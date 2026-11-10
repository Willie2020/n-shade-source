//! CPU normal-map and lightmap baking.
//!
//! Two normal-map workflows are supported:
//!
//! * **High to low** — for every texel covered by the low-poly UV layout, cast a
//!   two-sided cage ray and transfer the closest high-poly shading normal into
//!   the low-poly tangent frame.
//! * **Bevel** — no high-poly mesh. Rounded-edge normals are synthesized from
//!   the low-poly geometry by blending each flat surface toward the normals of
//!   adjacent surfaces near its boundary edges. Coplanar triangles are grouped
//!   first so triangulation diagonals do not produce fake bevels.
//!
//! A third, optional workflow bakes a white-albedo lightmap with a directional
//! sun, HDR environment lighting, an automatic back-side fill derived from the
//! environment and path-traced shadows. It shares the BVH but is otherwise
//! independent, so the normal baker is unaffected.
//!
//! The implementation is deliberately dependency-light (glam, rayon and
//! optional runtime-loaded OIDN and OptiX) so it can run on a worker thread
//! while the editor stays interactive. Geometry is accelerated with a binary BVH and
//! texels are rendered in parallel horizontal bands. Output is a tangent-space
//! or object-space RGBA normal map with configurable supersampling, edge
//! padding and UDIM tile selection, or a linear lightmap.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

mod bvh;
mod lightmap;
#[cfg(feature = "gpu")]
mod lightmap_gpu;
mod mesh;
mod oidn;
mod optix;
mod raster;

#[cfg(test)]
mod tests;

pub use lightmap::{
    bake_lightmap, BackendUsed, DenoiserUsed, EnvironmentMap, Features, LightmapBackend,
    LightmapDenoiser, LightmapLightKind, LightmapLightingMode, LightmapOutput, LightmapPipeline,
    LightmapSceneLight, LightmapSettings, LightmapStats, OidnDevice, PngBitDepth,
};

#[cfg(feature = "gpu")]
pub use lightmap_gpu::bake_lightmap_gpu;

/// True when the optional Intel Open Image Denoise shared library is
/// available. Lightmap bakes fall back to the built-in denoiser when it is not.
pub fn oidn_available() -> bool {
    oidn::available()
}

/// True when the optional NVIDIA OptiX denoiser runtime can be loaded.
pub fn optix_available() -> bool {
    optix::available()
}

/// Which normal source to bake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BakeMode {
    /// Project a separate high-poly mesh onto the low-poly UVs.
    #[default]
    HighToLow,
    /// Synthesize rounded-edge normals from the low-poly geometry only.
    Bevel,
}

impl BakeMode {
    pub const ALL: [Self; 2] = [Self::HighToLow, Self::Bevel];

    pub fn label(self) -> &'static str {
        match self {
            Self::HighToLow => "High to Low",
            Self::Bevel => "Bevel (low poly)",
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Self::HighToLow => "normal_high",
            Self::Bevel => "normal_bevel",
        }
    }
}

/// Normal-map output space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BakeSpace {
    #[default]
    TangentOpenGl,
    TangentDirectX,
    Object,
}

impl BakeSpace {
    pub const ALL: [Self; 3] = [Self::TangentOpenGl, Self::TangentDirectX, Self::Object];

    pub fn label(self) -> &'static str {
        match self {
            Self::TangentOpenGl => "Tangent · OpenGL",
            Self::TangentDirectX => "Tangent · DirectX",
            Self::Object => "Object space",
        }
    }
}

/// Bake configuration. Distances are percentages of the low-poly mesh extent so
/// the same settings work for meshes authored in millimetres or metres.
#[derive(Clone, Debug)]
pub struct BakeSettings {
    pub mode: BakeMode,
    /// Output edge length in pixels (clamped to 64..=4096).
    pub resolution: u32,
    /// Linear supersampling factor: 1, 2 or 4. Automatically clamped so the
    /// supersampled buffer stays within the memory budget.
    pub anti_alias: u32,
    pub space: BakeSpace,
    /// UDIM tile to bake. `1001` is used for ordinary 0–1 UV layouts.
    pub tile: u32,
    /// Distance above/below the low-poly surface from which cage rays start.
    pub cage_offset_pct: f32,
    /// How far beyond the cage the rays search for high-poly geometry.
    pub ray_distance_pct: f32,
    /// World-space width of the synthesized bevel, as a percentage of the mesh
    /// extent. Clamped to 0.01–2.0.
    pub bevel_radius_pct: f32,
    /// Falloff exponent for the bevel blend. Lower values spread the bevel
    /// further; higher values concentrate it at the edge.
    pub bevel_falloff: f32,
    /// Multiplier for the tangent-space slope of the baked normals. `1.0` is
    /// the raw ray result, `0.0` flattens the map, values above 1 exaggerate
    /// the relief. Object-space bakes ignore it.
    pub normal_intensity: f32,
    /// Number of transparent texels to fill around UV islands. Fractional
    /// values are allowed in the editor and round to whole texels when baking.
    pub padding: f32,
    /// True when the low-poly source uses a bottom-left UV origin
    /// (OBJ/FBX/COLLADA/USD). glTF is top-left. This changes image row
    /// placement only; the tangent basis always uses the raw UVs.
    pub uv_bottom_left: bool,
}

impl Default for BakeSettings {
    fn default() -> Self {
        Self {
            mode: BakeMode::HighToLow,
            resolution: 1024,
            anti_alias: 1,
            space: BakeSpace::TangentOpenGl,
            tile: 1001,
            cage_offset_pct: 1.0,
            ray_distance_pct: 5.0,
            bevel_radius_pct: 2.0,
            bevel_falloff: 1.0,
            normal_intensity: 1.0,
            padding: 8.0,
            uv_bottom_left: false,
        }
    }
}

impl BakeSettings {
    /// Effective supersampling factor after the memory budget is applied.
    pub fn effective_aa(&self) -> u32 {
        let resolution = self.resolution.clamp(64, 4096) as u64;
        let max_pixels = 16_777_216u64;
        let max_aa = ((max_pixels / (resolution * resolution)).max(1) as f64).sqrt() as u32;
        self.anti_alias.clamp(1, 4).min(max_aa.max(1))
    }

    /// Output edge length after clamping.
    pub fn output_size(&self) -> u32 {
        self.resolution.clamp(64, 4096)
    }
}

/// Engine-agnostic triangle mesh used as bake input.
#[derive(Clone, Debug, Default)]
pub struct BakeMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex UVs. The low-poly mesh must have one entry per position.
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// Group index per triangle, used by separated lightmap bakes to rasterize
    /// one material or object at a time. Empty means every triangle is in
    /// group 0. The BVH always keeps every triangle, so geometry from other
    /// groups still casts shadows.
    pub triangle_groups: Vec<u32>,
}

impl BakeMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Group of one triangle. Missing entries read as group 0.
    pub fn triangle_group(&self, triangle: usize) -> u32 {
        self.triangle_groups.get(triangle).copied().unwrap_or(0)
    }
}

/// Statistics reported by a completed bake.
#[derive(Clone, Copy, Debug, Default)]
pub struct BakeStats {
    pub resolution: u32,
    pub effective_aa: u32,
    pub tile: u32,
    pub low_triangles: u32,
    pub high_triangles: u32,
    pub rays: u64,
    pub hits: u64,
    pub misses: u64,
    pub elapsed_ms: f32,
}

/// Baked RGBA image, top-down, alpha 255 outside the dilated UV islands.
#[derive(Debug)]
pub struct BakeOutput {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub stats: BakeStats,
}

/// Triplanar projection of a source normal map onto a low-poly mesh.
///
/// The bounding box is expressed in **normalized mesh space**: the mesh is
/// centered on its bounds and scaled so its largest extent is one unit. This
/// matches the 3D preview's normalization, so the box gizmo and the rasterizer
/// use the same coordinates.
#[derive(Clone, Debug)]
pub struct TriplanarSettings {
    /// Output UV-map edge length.
    pub resolution: u32,
    /// Pixels of nearest-normal padding around UV islands.
    pub padding: u32,
    pub uv_bottom_left: bool,
    /// Bounding-box centre in normalized mesh space.
    pub center: [f32; 3],
    /// Full bounding-box size in normalized mesh space.
    pub size: [f32; 3],
    /// Orientation as an `[x, y, z, w]` quaternion.
    pub rotation: [f32; 4],
    /// Triplanar weight exponent. Higher values keep each axis crisper.
    pub sharpness: f32,
    /// Soft edge width outside the box, in normalized units.
    pub edge_feather: f32,
    /// Only generate normals inside this 0–1 sub-rectangle of UV space.
    /// `[0, 0, 1, 1]` covers the whole map.
    pub uv_window: [f32; 4],
}

impl Default for TriplanarSettings {
    fn default() -> Self {
        Self {
            resolution: 512,
            padding: 6,
            uv_bottom_left: false,
            center: [0.0; 3],
            size: [1.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            sharpness: 4.0,
            edge_feather: 0.04,
            uv_window: [0.0, 0.0, 1.0, 1.0],
        }
    }
}

/// Tangent-space normals produced by a triplanar projection.
#[derive(Clone, Debug)]
pub struct TriplanarOutput {
    pub width: u32,
    pub height: u32,
    /// Canonical OpenGL tangent-space normals in row-major order.
    pub normals: Vec<[f32; 3]>,
    /// Box coverage after the soft edge and UV island mask.
    pub coverage: Vec<f32>,
}

#[derive(Debug, thiserror::Error)]
pub enum BakeError {
    #[error("cancelled")]
    Cancelled,
    #[error("GPU path tracing unavailable: {0}")]
    GpuUnavailable(String),
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, BakeError>;

/// Bake a normal map.
///
/// `high` is consumed because building the BVH copies its geometry into an
/// acceleration structure; pass `None` for [`BakeMode::Bevel`].
/// Project a source normal map onto the mesh through a transformed bounding
/// box. `sample` maps UV in 0–1 to a canonical tangent-space normal and is
/// called for each of the three triplanar planes.
pub fn project_triplanar<F>(
    low: &BakeMesh,
    settings: &TriplanarSettings,
    sample: F,
) -> Result<TriplanarOutput>
where
    F: Fn([f32; 2]) -> [f32; 3],
{
    raster::project_triplanar(low, settings, sample)
}

pub fn bake(
    low: &BakeMesh,
    high: Option<BakeMesh>,
    settings: &BakeSettings,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<BakeOutput> {
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }
    if settings.mode == BakeMode::HighToLow && high.is_none() {
        return Err(BakeError::Message(
            "A high-poly mesh is required for a high-to-low bake".into(),
        ));
    }

    let start = Instant::now();
    progress.store(150, Ordering::Relaxed);
    let low = mesh::prepare_low(low, settings)?;
    progress.store(250, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }

    let high = match settings.mode {
        BakeMode::HighToLow => Some(bvh::Bvh::new(high.expect("checked above"))?),
        BakeMode::Bevel => None,
    };
    progress.store(300, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }

    let mut output = raster::render(&low, high.as_ref(), settings, progress, cancel)?;
    output.stats.elapsed_ms = start.elapsed().as_secs_f32() * 1000.0;
    Ok(output)
}
