//! White-albedo lightmap baking with optional HDR environments.
//!
//! The bake is a small path tracer in the spirit of Cycles' bake workflow:
//! every texel of the UV layout receives a diffuse white surface that
//! is lit by a directional sun and/or an HDR environment. An automatic
//! unshadowed fill derived from the environment's average radiance keeps
//! surfaces facing away from its bright regions readable. Geometry is
//! accelerated with the same BVH used by the normal baker, work is distributed
//! across a bounded rayon thread pool, and the result can be denoised either by
//! the built-in edge-aware filter or by a runtime-loaded Intel Open Image
//! Denoise library.
//!
//! An optional wgpu compute backend (`gpu` feature) shares the preparation,
//! denoising and output code in this module. It runs on DirectX 12/Vulkan/Metal
//! adapters from NVIDIA, AMD and Intel, with the CPU path as the fallback.
//!
//! The implementation is intentionally conservative so it runs on low-end
//! hardware: no mandatory GPU or CUDA dependency, bounded sample counts,
//! bounded bounce counts, and a memory budget tied to the output resolution.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use glam::Vec3;
use rayon::prelude::*;

use crate::bvh::Bvh;
use crate::mesh::normalize_or;
use crate::{BakeError, BakeMesh, Result};

mod denoise;
mod env;
mod geometry;
mod output;
mod prepare;
mod sampling;
mod trace;

#[cfg(test)]
mod tests;

use env::EnvSampler;
pub use env::EnvironmentMap;

pub(crate) use denoise::{denoise, denoise_v2};
#[cfg(feature = "gpu")]
pub(crate) use env::{build_cdf, sample_environment, SampledEnvironment};
pub(crate) use output::finish_lightmap;
pub(crate) use prepare::{prepare_lightmap, rasterize, rasterize_v2, GBuffer};
use sampling::Sampler;
use trace::{create_sun, trace_ao, trace_path, Rng};
pub(crate) use trace::{
    luminance, prepare_scene_lights, trace_ao_v2, trace_path_v2, TraceContext, Welford,
};
/// Denoiser selection for lightmap output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightmapDenoiser {
    None,
    /// Use OptiX when available, then OpenImageDenoise, then the built-in
    /// edge-aware filter.
    #[default]
    Auto,
    Builtin,
    /// NVIDIA OptiX HDR denoiser, loaded from the installed driver at runtime.
    Optix,
    /// Intel Open Image Denoise, loaded from the system at runtime.
    Oidn,
}

impl LightmapDenoiser {
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Auto,
        Self::Builtin,
        Self::Optix,
        Self::Oidn,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Auto => "Auto (best available)",
            Self::Builtin => "Built-in edge-aware",
            Self::Optix => "NVIDIA OptiX (RTX)",
            Self::Oidn => "OpenImageDenoise (optional)",
        }
    }
}

/// Device preference when [`LightmapDenoiser::Oidn`] is selected.
///
/// OIDN is loaded at runtime; a requested GPU device that is not available
/// falls back to the next candidate and finally to the CPU device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OidnDevice {
    /// Try CUDA, then SYCL, then CPU.
    #[default]
    Auto,
    Cpu,
    /// NVIDIA CUDA runtime (works on RTX and GTX cards with a CUDA build).
    Cuda,
    /// Intel SYCL runtime.
    Sycl,
}

impl OidnDevice {
    pub const ALL: [Self; 4] = [Self::Auto, Self::Cuda, Self::Sycl, Self::Cpu];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (GPU if available)",
            Self::Cpu => "CPU",
            Self::Cuda => "NVIDIA CUDA",
            Self::Sycl => "Intel SYCL",
        }
    }

    pub(crate) fn preference(self) -> i32 {
        match self {
            Self::Auto => -1,
            // Values are `OIDNDeviceType` enum values used by the C API.
            Self::Cpu => 1,
            Self::Sycl => 2,
            Self::Cuda => 3,
        }
    }
}

/// Denoiser that actually ran, reported back to the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DenoiserUsed {
    #[default]
    None,
    Builtin,
    Optix,
    OidnCpu,
    OidnCuda,
    OidnSycl,
}

impl DenoiserUsed {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No denoiser",
            Self::Builtin => "Built-in edge-aware",
            Self::Optix => "NVIDIA OptiX",
            Self::OidnCpu => "OpenImageDenoise · CPU",
            Self::OidnCuda => "OpenImageDenoise · CUDA",
            Self::OidnSycl => "OpenImageDenoise · SYCL",
        }
    }

    pub fn is_oidn(self) -> bool {
        matches!(self, Self::OidnCpu | Self::OidnCuda | Self::OidnSycl)
    }

    pub(crate) fn from_oidn_device(device: i32) -> Self {
        match device {
            3 => Self::OidnCuda,
            2 => Self::OidnSycl,
            _ => Self::OidnCpu,
        }
    }
}

/// Selects between the v1 behaviour and the v2 artifact fixes.
///
/// Every v2 branch in the baker tests [`Features`], never this enum directly,
/// so one fix can be disabled while debugging without touching the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightmapPipeline {
    /// Exact v1 behaviour in every code path touched by v2.
    Legacy,
    /// Artifact and banding fixes: Owen-Sobol sampling, edge extension,
    /// chart-aware denoising, TPDF dither and the geometric-normal offsets.
    #[default]
    V2,
}

impl LightmapPipeline {
    pub const ALL: [Self; 2] = [Self::Legacy, Self::V2];

    pub fn label(self) -> &'static str {
        match self {
            Self::Legacy => "Legacy (v1)",
            Self::V2 => "V2 (artifact fixes)",
        }
    }
}

/// Bit depth for saved PNG lightmaps. EXR is always linear float.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PngBitDepth {
    #[default]
    Eight,
    Sixteen,
}

impl PngBitDepth {
    pub const ALL: [Self; 2] = [Self::Eight, Self::Sixteen];

    pub fn label(self) -> &'static str {
        match self {
            Self::Eight => "8-bit sRGB",
            Self::Sixteen => "16-bit sRGB",
        }
    }
}

/// Derived, read-only switches for the v2 stages.
///
/// `soft_ao` also depends on `ao_max_distance`, so reading the settings once at
/// the start of a bake keeps every stage consistent.
#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub low_discrepancy: bool,
    pub geometric_offsets: bool,
    pub terminator_fix: bool,
    pub edge_extension: bool,
    pub variance_denoise: bool,
    pub soft_ao: bool,
    pub env_bilinear: bool,
    pub smooth_dilate: bool,
    pub tpdf_dither: bool,
    /// Multiply area lights by their Lambertian emitter cosine. Off in the
    /// legacy pipeline so existing bakes stay bit-identical.
    pub area_emitter_cosine: bool,
}

impl Features {
    pub const fn legacy() -> Self {
        Self {
            low_discrepancy: false,
            geometric_offsets: false,
            terminator_fix: false,
            edge_extension: false,
            variance_denoise: false,
            soft_ao: false,
            env_bilinear: false,
            smooth_dilate: false,
            tpdf_dither: false,
            area_emitter_cosine: false,
        }
    }
}

/// Path-tracing backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightmapBackend {
    /// Try the GPU path tracer, then fall back to the CPU.
    #[default]
    Auto,
    Cpu,
    /// Vendor-neutral wgpu compute backend (DirectX 12/Vulkan/Metal).
    Gpu,
}

impl LightmapBackend {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Gpu, Self::Cpu];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (GPU if available)",
            Self::Cpu => "CPU",
            Self::Gpu => "GPU (wgpu)",
        }
    }
}

/// Backend that actually produced the bake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendUsed {
    #[default]
    Cpu,
    Gpu,
}

impl BackendUsed {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu => "GPU (wgpu)",
        }
    }
}

/// Which light contributions a lightmap bake writes.
///
/// The path tracer always works on a white diffuse surface, so the result is
/// lighting rather than albedo. Splitting the contributions lets a bake match
/// engines that already shade one of them themselves, such as Second Life's
/// Advanced Lighting Model, which supplies direct sunlight at runtime and wants
/// baked occlusion/indirect light in the texture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightmapLightingMode {
    /// Sun, scene lights, environment/sky and inter-surface bounces.
    #[default]
    Full,
    /// The directional sun, editable scene lights and their shadows only.
    Direct,
    /// Environment/sky and inter-surface bounces, without the sun.
    Indirect,
    /// Grayscale hemisphere visibility, the classic ambient-occlusion map.
    AmbientOcclusion,
}

impl LightmapLightingMode {
    pub const ALL: [Self; 4] = [
        Self::Full,
        Self::Direct,
        Self::Indirect,
        Self::AmbientOcclusion,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "Full (direct + indirect)",
            Self::Direct => "Direct lights only",
            Self::Indirect => "Indirect only",
            Self::AmbientOcclusion => "Ambient occlusion",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Full => "Sun, scene lights, sky/environment and inter-surface bounces. The physically based default.",
            Self::Direct => "The directional sun, scene lights and their shadows only; sky, ambient and bounces are ignored.",
            Self::Indirect => "Sky/environment, bounced light and scene lights without the sun. Use when the engine supplies direct sunlight.",
            Self::AmbientOcclusion => "Grayscale sky visibility for multiplying over a diffuse texture. Classic AO bake.",
        }
    }

    /// True when the directional sun contributes to this mode.
    pub fn uses_sun(self) -> bool {
        matches!(self, Self::Full | Self::Direct)
    }

    /// True when editable scene lights contribute to this mode.
    ///
    /// Scene lights are user-authored static fill, not the runtime sun that
    /// `Indirect` exists to exclude. They therefore bake in `Full`, `Direct`
    /// *and* `Indirect`, and only AO ignores them. This is what lets a Second
    /// Life Indirect/AO bake still carry deliberate fill lights for dark
    /// corners.
    pub fn uses_lights(self) -> bool {
        !self.is_ambient_occlusion()
    }

    /// True when the environment and solid ambient fill contribute.
    pub fn uses_environment(self) -> bool {
        matches!(self, Self::Full | Self::Indirect)
    }

    /// True when inter-surface bounces contribute.
    pub fn uses_bounces(self) -> bool {
        matches!(self, Self::Full | Self::Indirect)
    }

    pub fn is_ambient_occlusion(self) -> bool {
        matches!(self, Self::AmbientOcclusion)
    }
}

/// Shape of an editable scene light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightmapLightKind {
    /// Omnidirectional point light.
    #[default]
    Point,
    /// Point light restricted to a cone aimed at `target`.
    Spot,
    /// Rectangular area emitter aimed at `target`.
    Area,
}

impl LightmapLightKind {
    pub const ALL: [Self; 3] = [Self::Point, Self::Spot, Self::Area];

    pub fn label(self) -> &'static str {
        match self {
            Self::Point => "Point",
            Self::Spot => "Spot",
            Self::Area => "Area",
        }
    }
}

/// An editable light in the bake scene.
///
/// Positions, sizes and `range` live in **normalized mesh space**: the same
/// space the live 3D preview uses, where the mesh bounding box is centered on
/// the origin and its longest axis is one unit long. The tracer maps them into
/// the raw mesh coordinates, so a light rig is independent of whether the
/// source mesh is authored in millimetres or metres.
///
/// Two independent switches control a light:
///
/// * `enabled` decides whether it contributes to the bake.
/// * `visible_in_view` decides whether the live 3D preview draws it. A light
///   can be hidden in the viewport and still be baked, which is the usual
///   workflow for filling dark corners without cluttering the preview.
#[derive(Clone, Debug, PartialEq)]
pub struct LightmapSceneLight {
    /// True when the light contributes to the bake.
    pub enabled: bool,
    /// True when the live 3D preview shows the light. Independent of `enabled`.
    pub visible_in_view: bool,
    /// Display name shown in the editor.
    pub name: String,
    pub kind: LightmapLightKind,
    /// Center of the light in normalized mesh space.
    pub position: [f32; 3],
    /// Point a spot or area light aims at, in normalized mesh space. Unused by
    /// point lights.
    pub target: [f32; 3],
    pub color: [f32; 3],
    /// Radiant intensity: irradiance at one unit of distance for point and spot
    /// lights, emitted radiance for area lights. Uses the same arbitrary scale
    /// as `sun_intensity`.
    pub intensity: f32,
    /// Distance at which the light fades to zero. `0` disables the cutoff.
    pub range: f32,
    /// Full cone angle of a spot light, in degrees.
    pub spot_angle_deg: f32,
    /// Fraction of the spot cone used for the soft inner falloff, `0..1`.
    pub spot_blend: f32,
    /// Width and height of an area light, in normalized mesh units.
    pub area_size: [f32; 2],
    /// True when an area light emits from both faces.
    pub two_sided: bool,
}

impl LightmapSceneLight {
    /// A light of `kind` with a small, mesh-relative default placement.
    ///
    /// The defaults put the light roughly one mesh extent in front of the
    /// model, aimed at the origin, so it visibly lights the bake immediately
    /// after being added.
    pub fn new(kind: LightmapLightKind) -> Self {
        let (position, target, intensity, range) = match kind {
            LightmapLightKind::Point => ([0.6, 0.7, 0.8], [0.0, 0.0, 0.0], 2.0, 6.0),
            LightmapLightKind::Spot => ([0.0, 0.8, 1.2], [0.0, 0.0, 0.0], 3.0, 8.0),
            LightmapLightKind::Area => ([0.0, 0.7, 1.1], [0.0, 0.0, 0.0], 2.0, 8.0),
        };
        Self {
            enabled: true,
            visible_in_view: true,
            name: kind.label().to_string(),
            kind,
            position,
            target,
            color: [1.0, 1.0, 1.0],
            intensity,
            range,
            spot_angle_deg: 45.0,
            spot_blend: 0.4,
            area_size: [0.5, 0.5],
            two_sided: false,
        }
    }
}

impl Default for LightmapSceneLight {
    fn default() -> Self {
        Self::new(LightmapLightKind::Point)
    }
}

/// Lightmap bake settings.
#[derive(Clone, Debug)]
pub struct LightmapSettings {
    /// Output edge length in pixels, clamped to 64..=4096.
    pub resolution: u32,
    /// Path tracer backend. Auto prefers the GPU and falls back to the CPU.
    pub backend: LightmapBackend,
    /// Which light contributions are written.
    pub lighting: LightmapLightingMode,
    /// Paths per texel. More samples reduce noise and increase bake time.
    pub samples: u32,
    /// Indirect surface bounces. `0` bakes direct light plus environment only.
    pub bounces: u32,
    /// UDIM tile to bake. `1001` is used for ordinary 0–1 UV layouts.
    pub tile: u32,
    /// Rasterize only triangles whose [`BakeMesh::triangle_group`] matches.
    /// `None` bakes the whole tile. The BVH always contains every triangle, so
    /// other materials or objects still occlude the selected one.
    pub group: Option<u32>,
    /// Transparent texels copied around UV islands to hide seams.
    pub padding: u32,
    /// True when the low-poly source uses a bottom-left UV origin
    /// (OBJ/FBX/COLLADA/USD). glTF is top-left.
    pub uv_bottom_left: bool,
    pub denoiser: LightmapDenoiser,
    /// Preferred OIDN device. Ignored by the built-in denoiser.
    pub oidn_device: OidnDevice,
    /// Number of edge-aware filter passes for [`LightmapDenoiser::Builtin`].
    pub denoise_passes: u32,
    /// Exposure in stops applied to the PNG preview (EXR output stays linear).
    pub exposure: f32,
    pub sun_enabled: bool,
    /// Compass angle of the sun, in degrees. 0° points along +X, 90° along +Z.
    pub sun_azimuth_deg: f32,
    /// Height of the sun above the horizon, in degrees.
    pub sun_elevation_deg: f32,
    /// Angular diameter of the sun disk. Larger values soften shadows.
    pub sun_size_deg: f32,
    pub sun_color: [f32; 3],
    pub sun_intensity: f32,
    /// Editable point, spot and area lights baked as direct lighting.
    ///
    /// Positions are in normalized mesh space; see [`LightmapSceneLight`].
    pub lights: Vec<LightmapSceneLight>,
    /// Color of the unshadowed fill. With an HDR environment this is the fill
    /// color; without one it is the solid ambient color.
    pub ambient_color: [f32; 3],
    /// Base strength of the unshadowed fill.
    pub ambient_intensity: f32,
    /// Multiplier applied to the HDR environment.
    pub environment_strength: f32,
    /// Multiplier for the automatic fill derived from the environment's average
    /// radiance. `1.0` keeps the base fill and scales it with bright HDRIs;
    /// `0.0` lights the mesh with the HDR alone.
    pub environment_fill: f32,
    /// Seed for the per-texel sample sequences. Re-bake with another seed to
    /// average out any remaining noise manually.
    pub seed: u32,
    /// Legacy keeps v1 behaviour; V2 enables the artifact fixes.
    pub pipeline: LightmapPipeline,
    /// PNG bit depth. The in-memory preview stays 8-bit sRGB.
    pub png_bit_depth: PngBitDepth,
    /// Soft shoulder for values above `1 - highlight_rolloff`. 0 hard-clips.
    pub highlight_rolloff: f32,
    /// AO ray length in normalized mesh units. 0 is an infinite, binary Ray.
    pub ao_max_distance: f32,
}

impl Default for LightmapSettings {
    fn default() -> Self {
        Self {
            resolution: 1024,
            backend: LightmapBackend::Auto,
            lighting: LightmapLightingMode::Full,
            samples: 128,
            bounces: 2,
            tile: 1001,
            group: None,
            padding: 8,
            uv_bottom_left: false,
            denoiser: LightmapDenoiser::Builtin,
            oidn_device: OidnDevice::Auto,
            denoise_passes: 4,
            exposure: 0.0,
            sun_enabled: true,
            sun_azimuth_deg: 35.0,
            sun_elevation_deg: 55.0,
            sun_size_deg: 1.5,
            sun_color: [1.0, 1.0, 1.0],
            sun_intensity: 1.0,
            lights: Vec::new(),
            ambient_color: [0.55, 0.62, 0.75],
            ambient_intensity: 0.25,
            environment_strength: 1.0,
            // Off by default: an HDR bake should match the environment alone.
            // Raise it only for an artistic unshadowed fill.
            environment_fill: 0.0,
            seed: 1,
            pipeline: LightmapPipeline::V2,
            png_bit_depth: PngBitDepth::Eight,
            highlight_rolloff: 0.0,
            ao_max_distance: 0.0,
        }
    }
}

impl LightmapSettings {
    pub fn output_size(&self) -> u32 {
        self.resolution.clamp(64, 4096)
    }

    pub fn effective_samples(&self) -> u32 {
        self.samples.clamp(1, 4096)
    }

    pub fn effective_bounces(&self) -> u32 {
        self.bounces.min(8)
    }

    pub fn sun_direction(&self) -> Vec3 {
        let azimuth = self.sun_azimuth_deg.to_radians();
        let elevation = self.sun_elevation_deg.clamp(-89.0, 89.0).to_radians();
        Vec3::new(
            elevation.cos() * azimuth.cos(),
            elevation.sin(),
            elevation.cos() * azimuth.sin(),
        )
        .normalize_or_zero()
    }

    /// Read the v2 switches once for this bake.
    pub fn features(&self) -> Features {
        let v2 = self.pipeline == LightmapPipeline::V2;
        Features {
            low_discrepancy: v2,
            geometric_offsets: v2,
            terminator_fix: v2,
            edge_extension: v2,
            variance_denoise: v2,
            soft_ao: v2 && self.ao_max_distance > 0.0,
            env_bilinear: v2,
            smooth_dilate: v2,
            tpdf_dither: v2,
            area_emitter_cosine: v2,
        }
    }
}
/// Statistics reported by a completed lightmap bake.
#[derive(Clone, Copy, Debug, Default)]
pub struct LightmapStats {
    pub resolution: u32,
    pub lighting: LightmapLightingMode,
    pub samples: u32,
    pub bounces: u32,
    pub texels: u32,
    pub triangles: u32,
    pub rays: u64,
    pub hits: u64,
    pub elapsed_ms: f32,
    pub denoiser: DenoiserUsed,
    pub backend: BackendUsed,
    /// Covered texels whose preview value exceeds 1.0 and therefore clips to
    /// white. Counted before padding dilation.
    pub clipped: u32,
    /// Pipeline that produced this bake.
    pub pipeline: LightmapPipeline,
    /// Texels covered only by the v2 edge extension (zero under Legacy).
    pub edge_texels: u32,
    /// Connected surface patches (charts) in the baked mesh.
    pub charts: u32,
}

/// Linear HDR result plus an sRGB-encoded preview.
#[derive(Debug)]
pub struct LightmapOutput {
    pub width: u32,
    pub height: u32,
    /// Top-down RGBA8 preview. Alpha is the dilated coverage mask.
    pub pixels: Vec<u8>,
    /// Top-down RGBA16 sRGB when `png_bit_depth` is sixteen bits; `None`
    /// otherwise. Alpha is the dilated coverage mask.
    pub pixels16: Option<Vec<u16>>,
    /// Top-down linear RGB radiance, padded like `pixels`.
    pub hdr: Vec<f32>,
    pub stats: LightmapStats,
    /// Adapter/backend description for GPU bakes, e.g. `RTX 3070 Ti · Vulkan`.
    pub backend_name: Option<String>,
}
/// Fill radiance added on top of the environment lighting.
///
/// The solid ambient setting is the base. With an HDR environment the base is
/// scaled by the environment's average radiance relative to `1.0`, so the fill
/// follows the HDRI exposure instead of being replaced by it. High-dynamic-range
/// environments often leave surfaces facing away from their bright regions
/// almost black; this unshadowed term keeps those backs readable.
///
/// `environment_fill` scales the automatic part, `0.0` disables it.
pub(crate) fn environment_fill_radiance(
    settings: &LightmapSettings,
    average_radiance: f32,
) -> Vec3 {
    let color = Vec3::from_array(settings.ambient_color).max(Vec3::ZERO);
    let intensity = settings.ambient_intensity.max(0.0);
    let brightness = average_radiance.max(0.0) * settings.environment_strength.max(0.0);
    color * intensity * brightness.max(1.0) * settings.environment_fill.max(0.0)
}

/// Bake a white diffuse lightmap.
///
/// `environment` is optional. When present it is importance-sampled with
/// multiple-importance sampling against the cosine-weighted diffuse lobe, and
/// an automatic unshadowed fill derived from its average radiance keeps
/// surfaces facing away from its bright regions lit. Without an environment the
/// solid ambient settings provide the fill. The mesh must contain UVs;
/// only triangles in `settings.tile` (and, for a separated bake,
/// `settings.group`) are rasterized, while the BVH includes every triangle so
/// geometry from other tiles, materials and objects can still cast shadows.
pub fn bake_lightmap(
    mesh: &BakeMesh,
    environment: Option<&EnvironmentMap>,
    settings: &LightmapSettings,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<LightmapOutput> {
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }
    let start = Instant::now();
    let prepared = prepare_lightmap(mesh, settings)?;
    progress.store(100, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }

    let resolution = settings.output_size();
    let features = settings.features();
    let v2 = settings.pipeline == LightmapPipeline::V2;
    let gbuffer = if v2 {
        rasterize_v2(&prepared, resolution, &features)
    } else {
        rasterize(&prepared, resolution)
    };
    progress.store(180, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }

    let texel_count = resolution as usize * resolution as usize;
    let covered_count = gbuffer.covered.iter().filter(|covered| **covered).count();
    if covered_count == 0 {
        return Err(BakeError::Message(
            "No UV texels were covered. Check the UV layout and UDIM tile.".into(),
        ));
    }

    let bvh = Bvh::from_mesh(mesh)?;
    let environment_sampler = if settings.lighting.uses_environment() {
        environment.and_then(|map| EnvSampler::new(map, features.env_bilinear))
    } else {
        None
    };
    let sun = create_sun(settings, settings.lighting.uses_sun());
    let lights = prepare_scene_lights(settings, prepared.extent, prepared.center);
    let samples = settings.effective_samples();
    let bounces = if settings.lighting.uses_bounces() {
        settings.effective_bounces()
    } else {
        0
    };
    let ambient = if settings.lighting.uses_environment() && environment.is_none() {
        Vec3::from_array(settings.ambient_color) * settings.ambient_intensity.max(0.0)
    } else {
        Vec3::ZERO
    };
    let fill = if settings.lighting.uses_environment() {
        environment
            .map(|map| environment_fill_radiance(settings, map.average_radiance()))
            .unwrap_or(Vec3::ZERO)
    } else {
        Vec3::ZERO
    };
    let env_strength = if settings.lighting.uses_environment() {
        settings.environment_strength.max(0.0)
    } else {
        0.0
    };
    let extent = prepared.extent;
    let eps = (extent * 5.0e-4).max(1.0e-5);
    let shadow_distance = (extent * 8.0).max(1.0);
    let context = TraceContext {
        bvh: &bvh,
        environment: environment_sampler.as_ref(),
        sun: &sun,
        lights: &lights,
        ambient,
        fill,
        environment_strength: env_strength,
        bounces,
        extent,
        shadow_distance,
        area_emitter_cosine: features.area_emitter_cosine,
    };

    let mut radiance = vec![Vec3::ZERO; texel_count];
    let mut variance = if v2 {
        vec![0.0f32; texel_count]
    } else {
        Vec::new()
    };
    let band_size = (resolution as usize * 4).max(1);
    let band_count = texel_count.div_ceil(band_size);
    let traced = AtomicU32::new(0);

    // Keep one core free for the editor. The pool is created once and reused
    // across bakes. Rows are processed in bands so progress updates are cheap
    // and the radiance slice stays cache-friendly.
    let pool = lightmap_pool();

    let mut trace = || {
        if v2 {
            radiance
                .par_chunks_mut(band_size)
                .zip(variance.par_chunks_mut(band_size))
                .enumerate()
                .map(|(band_index, (chunk, variance_chunk))| {
                    if cancel.load(Ordering::Relaxed) {
                        return (0u64, 0u64);
                    }
                    let start = band_index * band_size;
                    let mut rays = 0u64;
                    let mut hits = 0u64;
                    for (local, (output, variance_output)) in
                        chunk.iter_mut().zip(variance_chunk.iter_mut()).enumerate()
                    {
                        let index = start + local;
                        if !gbuffer.covered[index] {
                            continue;
                        }
                        let position = gbuffer.positions[index];
                        let base_normal = normalize_or(gbuffer.normals[index], Vec3::Y);
                        let geometric = gbuffer.geo_normals[index];
                        let mut sum = Vec3::ZERO;
                        let mut welford = Welford::default();
                        for sample_index in 0..samples {
                            if sample_index % 16 == 0 && cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            let mut sampler = Sampler::new(
                                settings.seed,
                                index as u32,
                                sample_index,
                                samples,
                                features.low_discrepancy,
                            );
                            let (value, sample_rays, sample_hits) =
                                if settings.lighting.is_ambient_occlusion() {
                                    trace_ao_v2(
                                        &mut sampler,
                                        position,
                                        base_normal,
                                        geometric,
                                        &bvh,
                                        extent,
                                        settings.ao_max_distance,
                                    )
                                } else {
                                    trace_path_v2(
                                        &mut sampler,
                                        position,
                                        base_normal,
                                        geometric,
                                        &context,
                                    )
                                };
                            if value.is_finite() {
                                sum += value;
                                welford.push(luminance(value));
                            }
                            rays += sample_rays;
                            hits += sample_hits;
                        }
                        *output = sum / samples as f32;
                        *variance_output = welford.var_of_mean();
                    }
                    let done = traced.fetch_add(1, Ordering::Relaxed) + 1;
                    let fraction = done as f32 / band_count as f32;
                    progress.store(180 + (fraction * 650.0) as u32, Ordering::Relaxed);
                    (rays, hits)
                })
                .reduce(
                    || (0u64, 0u64),
                    |left, right| (left.0 + right.0, left.1 + right.1),
                )
        } else {
            radiance
                .par_chunks_mut(band_size)
                .enumerate()
                .map(|(band_index, chunk)| {
                    if cancel.load(Ordering::Relaxed) {
                        return (0u64, 0u64);
                    }
                    let start = band_index * band_size;
                    let mut rays = 0u64;
                    let mut hits = 0u64;
                    for (local, output) in chunk.iter_mut().enumerate() {
                        let index = start + local;
                        if !gbuffer.covered[index] {
                            continue;
                        }
                        let position = gbuffer.positions[index];
                        let base_normal = normalize_or(gbuffer.normals[index], Vec3::Y);
                        let mut rng = Rng::new(settings.seed as u64, index as u64);
                        let mut sum = Vec3::ZERO;
                        for sample_index in 0..samples {
                            if sample_index % 16 == 0 && cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            let (value, sample_rays, sample_hits) =
                                if settings.lighting.is_ambient_occlusion() {
                                    trace_ao(position, base_normal, &bvh, eps, &mut rng)
                                } else {
                                    trace_path(
                                        position,
                                        base_normal,
                                        &bvh,
                                        environment_sampler.as_ref(),
                                        &sun,
                                        &lights,
                                        ambient,
                                        fill,
                                        env_strength,
                                        bounces,
                                        eps,
                                        shadow_distance,
                                        &mut rng,
                                    )
                                };
                            if value.is_finite() {
                                sum += value;
                            }
                            rays += sample_rays;
                            hits += sample_hits;
                        }
                        *output = sum / samples as f32;
                    }
                    let done = traced.fetch_add(1, Ordering::Relaxed) + 1;
                    let fraction = done as f32 / band_count as f32;
                    progress.store(180 + (fraction * 650.0) as u32, Ordering::Relaxed);
                    (rays, hits)
                })
                .reduce(
                    || (0u64, 0u64),
                    |left, right| (left.0 + right.0, left.1 + right.1),
                )
        }
    };
    let (rays, hits) = if let Some(pool) = pool {
        pool.install(trace)
    } else {
        trace()
    };

    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }
    progress.store(850, Ordering::Relaxed);

    let denoiser = if let Some(pool) = pool {
        pool.install(|| {
            if v2 {
                denoise_v2(
                    &mut radiance,
                    &mut variance,
                    &gbuffer,
                    extent,
                    resolution,
                    settings,
                )
            } else {
                denoise(&mut radiance, &gbuffer, extent, resolution, settings)
            }
        })
    } else if v2 {
        denoise_v2(
            &mut radiance,
            &mut variance,
            &gbuffer,
            extent,
            resolution,
            settings,
        )
    } else {
        denoise(&mut radiance, &gbuffer, extent, resolution, settings)
    };
    progress.store(930, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }

    progress.store(980, Ordering::Relaxed);
    let output = finish_lightmap(
        &mut radiance,
        &gbuffer,
        resolution,
        settings,
        start.elapsed().as_secs_f32() * 1000.0,
        covered_count,
        prepared.triangle_count(),
        rays,
        hits,
        denoiser,
        BackendUsed::Cpu,
    );
    progress.store(1000, Ordering::Relaxed);
    Ok(output)
}

fn lightmap_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let thread_count = std::thread::available_parallelism()
            .map(|count| count.get().saturating_sub(1))
            .unwrap_or(1)
            .max(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(thread_count)
            .thread_name(|index| format!("lightmap-{index}"))
            .build()
            .ok()
    })
    .as_ref()
}
