//! GPU evaluation for the layer stack.
//!
//! A storage-buffer scene is built in the main world and extracted to the
//! render world. A compute node evaluates polygon SDF bevels, samples cached
//! normal/height textures for layers the shader cannot evaluate analytically
//! (rasters, smoothing, text, photo and painted layers), composites the stack,
//! and writes directly into the normal/height textures used by the 3D preview
//! and the egui canvas. Only boolean/vector-op groups, smart masks and
//! non-identity group opacity/blend still fall back to the CPU path.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bevy::asset::Assets;
use bevy::core_pipeline::schedule::camera_driver;
use bevy::prelude::*;
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{
    storage_buffer_read_only_sized, storage_buffer_sized, texture_storage_2d, uniform_buffer_sized,
};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayout, BindGroupLayoutDescriptor,
    BindGroupLayoutEntries, BindGroupLayoutEntry, Buffer, BufferDescriptor, BufferUsages,
    CachedComputePipelineId, ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache,
    ShaderStages, StorageTextureAccess, TextureFormat,
};
use bevy::render::renderer::{
    RenderContext, RenderDevice, RenderGraph, RenderGraphSystems, RenderQueue,
};
use bevy::render::texture::GpuImage;
use bevy::render::RenderApp;
use bevy::shader::Shader;
use ns_core::{BlendMode, Document, LayerKind, Profile, ShapeSource, Wrap};

/// Image handles shared by the render world, the 3D material and the canvas.
#[derive(Resource, Clone, ExtractResource)]
pub struct GpuOutputs {
    pub normal: Handle<Image>,
    pub height: Handle<Image>,
}

/// Most recent CPU-side GPU buffer upload time, shared from the render world so
/// the performance HUD can read it in the main world.
#[derive(Resource, Clone, Default)]
pub struct GpuUploadStats(pub Arc<Mutex<f32>>);

impl GpuUploadStats {
    pub fn last_ms(&self) -> f32 {
        self.0.lock().map(|value| *value).unwrap_or(0.0)
    }
}

/// Main-world scene description. A `None` payload means "use the CPU path".
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct GpuScene {
    pub generation: u64,
    pub data: Option<GpuSceneData>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GpuSceneOptions {
    /// Integrate the evaluated normals into the height texture on the GPU.
    pub integrate_height: bool,
}

/// Why the whole document could not use the GPU path.
///
/// The gate is fail-closed: any of these sends the entire document to the CPU
/// reference evaluator. Reporting every reason (rather than only the first)
/// makes the fallback ranking visible in the performance HUD and in the
/// benchmark corpus.
// Reserved variants are kept so the HUD, tests and the T1.1 sub-tasks share
// one vocabulary before every reason is emitted by the packer.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateReject {
    VectorCompound,
    BooleanOperand,
    SmartMask,
    SurfaceBoolean,
    NonIdentityGroup,
    GroupPaintedMask,
    MaskWithClipSource,
    ClipOnUnsupportedKind,
    TooManyLayers,
    TooManyVertices,
    TooManyContours,
    TooManyMasks,
    PixelBufferOverflow,
    FftSizeUnsupported,
    TooDeepGroups,
    /// The packer failed for a reason the diagnostic scan could not classify.
    /// Kept as an explicit fallback so the HUD never claims a false cause.
    UnsupportedLayer,
}

impl GateReject {
    #[allow(dead_code)]
    pub const ALL: [Self; 16] = [
        Self::VectorCompound,
        Self::BooleanOperand,
        Self::SmartMask,
        Self::SurfaceBoolean,
        Self::NonIdentityGroup,
        Self::GroupPaintedMask,
        Self::MaskWithClipSource,
        Self::ClipOnUnsupportedKind,
        Self::TooManyLayers,
        Self::TooManyVertices,
        Self::TooManyContours,
        Self::TooManyMasks,
        Self::PixelBufferOverflow,
        Self::FftSizeUnsupported,
        Self::TooDeepGroups,
        Self::UnsupportedLayer,
    ];

    /// Short label for a status-bar or HUD chip.
    pub fn label(self) -> &'static str {
        match self {
            Self::VectorCompound => "Vector compound",
            Self::BooleanOperand => "Boolean operand",
            Self::SmartMask => "Smart mask",
            Self::SurfaceBoolean => "Surface operation",
            Self::NonIdentityGroup => "Non-identity group",
            Self::GroupPaintedMask => "Group painted mask",
            Self::MaskWithClipSource => "Mask + clip source",
            Self::ClipOnUnsupportedKind => "Clip on unsupported kind",
            Self::TooManyLayers => "Layer cap",
            Self::TooManyVertices => "Vertex cap",
            Self::TooManyContours => "Contour cap",
            Self::TooManyMasks => "Mask cap",
            Self::PixelBufferOverflow => "Pixel buffer cap",
            Self::FftSizeUnsupported => "FFT size",
            Self::TooDeepGroups => "Group depth",
            Self::UnsupportedLayer => "Unsupported layer",
        }
    }

    /// Explanation shown in the HUD / status-bar tooltip.
    pub fn hint(self) -> &'static str {
        match self {
            Self::VectorCompound => {
                "A vector compound or boolean group must be resolved on the CPU first."
            }
            Self::BooleanOperand => "A layer uses legacy boolean operands.",
            Self::SmartMask => "A smart mask reads the surface composited below the layer.",
            Self::SurfaceBoolean => {
                "A boolean or partial-cut layer needs coverage-aware height compositing."
            }
            Self::NonIdentityGroup => {
                "A group has opacity or a blend mode other than Derivative."
            }
            Self::GroupPaintedMask => {
                "A group has a painted mask; the shader masks individual layers, not a finished group."
            }
            Self::MaskWithClipSource => {
                "A layer combines a painted mask with a clipping source."
            }
            Self::ClipOnUnsupportedKind => {
                "A clipping mask targets a layer kind the shader cannot mask."
            }
            Self::TooManyLayers => "The visible layer count exceeds the GPU buffer cap.",
            Self::TooManyVertices => "The polygon vertex count exceeds the GPU buffer cap.",
            Self::TooManyContours => "The contour count exceeds the GPU buffer cap.",
            Self::TooManyMasks => "The clipping/painted mask count exceeds the GPU buffer cap.",
            Self::PixelBufferOverflow => {
                "CPU-precomputed layer or mask pixels exceed the storage-buffer budget."
            }
            Self::FftSizeUnsupported => {
                "GPU height integration needs a power-of-two padded size up to 2048."
            }
            Self::TooDeepGroups => {
                "Group nesting exceeds the shader's accumulator stack."
            }
            Self::UnsupportedLayer => {
                "The scene could not be packed, and the cause was not classified."
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct GpuSceneData {
    pub width: u32,
    pub height: u32,
    pub height_scale: f32,
    pub layers: Vec<GpuLayer>,
    pub masks: Vec<GpuMask>,
    pub vertices: Vec<[f32; 2]>,
    /// One entry per closed contour. A polygon layer's `vertex_offset` and
    /// `vertex_count` select a range in this list; primitives don't use it.
    pub contours: Vec<GpuContour>,
    /// CPU-precomputed normal+height pixels for layers the shader cannot
    /// evaluate analytically (raster sources, smoothed outlines, text, photo
    /// and painted layers). One u32 per document pixel: RGBA8 normal, A=height.
    pub layer_pixels: Vec<Arc<Vec<u32>>>,
    pub layer_pixel_total: u32,
    /// Precomputed painted/static mask coverage, one u8 per document pixel in
    /// the low byte of each u32.
    pub mask_pixels: Vec<Arc<Vec<u32>>>,
    pub mask_pixel_total: u32,
    pub integrate_height: bool,
    pub repeat: bool,
}

/// CPU-precomputed pieces reused across GPU scene builds. The scene is rebuilt
/// whenever the document is dirty; without this cache every edit would
/// re-evaluate all raster/smoothed/masked layers on the CPU.
#[derive(Resource, Default)]
pub struct GpuSceneCache {
    entries: HashMap<ns_core::LayerId, CachedGpuLayer>,
}

struct CachedGpuLayer {
    size: (u32, u32),
    wrap: Wrap,
    normal_revision: Option<u64>,
    normal_pixels: Option<Arc<Vec<u32>>>,
    mask_revision: Option<(u64, u64)>,
    mask_pixels: Option<Arc<Vec<u32>>>,
}

impl CachedGpuLayer {
    fn new(size: (u32, u32), wrap: Wrap) -> Self {
        Self {
            size,
            wrap,
            normal_revision: None,
            normal_pixels: None,
            mask_revision: None,
            mask_pixels: None,
        }
    }
}

/// Must match `Contour` in `normal_studio.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuContour {
    pub vertex_offset: u32,
    pub vertex_count: u32,
    /// Conservative bounds used by the shader to skip distant islands.
    pub aabb_min: [f32; 2],
    pub aabb_max: [f32; 2],
}

/// Must match the `Layer` layout in `normal_studio.wgsl` (88 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuLayer {
    pub kind: u32,
    pub blend: u32,
    pub vertex_offset: u32,
    pub vertex_count: u32,
    pub flags: u32,
    pub mask_index: u32,
    pub radius: f32,
    pub height: f32,
    pub opacity: f32,
    pub chamfer_deg: f32,
    pub corner_radius: f32,
    pub softness: f32,
    pub stroke_width: f32,
    /// Width in pixels over which the bevel normal fades to flat at the
    /// silhouette. Replaces the old alignment padding slot.
    pub edge_aa: f32,
    pub center: [f32; 2],
    pub half_size: [f32; 2],
    pub aabb_min: [f32; 2],
    pub aabb_max: [f32; 2],
}

impl Default for GpuLayer {
    fn default() -> Self {
        Self {
            kind: 0,
            blend: 0,
            vertex_offset: 0,
            vertex_count: 0,
            flags: 0,
            mask_index: u32::MAX,
            radius: 0.0,
            height: 0.0,
            opacity: 1.0,
            chamfer_deg: 0.0,
            corner_radius: 0.0,
            softness: 0.0,
            stroke_width: 0.0,
            edge_aa: 0.0,
            center: [0.0; 2],
            half_size: [0.0; 2],
            aabb_min: [0.0; 2],
            aabb_max: [0.0; 2],
        }
    }
}

/// Shape coverage used as a clipping mask. Must match `Mask` in the shader
/// (48 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuMask {
    pub kind: u32,
    pub vertex_offset: u32,
    pub vertex_count: u32,
    pub flags: u32,
    pub center: [f32; 2],
    pub half_size: [f32; 2],
    pub corner_radius: f32,
    pub opacity: f32,
    pub _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuHeader {
    width: u32,
    height: u32,
    layer_count: u32,
    vertex_count: u32,
    height_scale: f32,
    _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct PrepParams {
    src_w: u32,
    src_h: u32,
    pad_w: u32,
    pad_h: u32,
    repeat: u32,
    bits_x: u32,
    bits_y: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct FftParams {
    size_x: u32,
    size_y: u32,
    stage: u32,
    inverse: u32,
    axis: u32,
    bits: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct SpectrumParams {
    size_x: u32,
    size_y: u32,
    bits_x: u32,
    bits_y: u32,
    high_pass: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct FinalizeParams {
    width: u32,
    height: u32,
    stride: u32,
    scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct NormalizeParams {
    width: u32,
    height: u32,
    stride: u32,
    _pad: u32,
}

const MAX_LAYERS: usize = 256;
const MAX_VERTICES: usize = 1_048_576;
const MAX_CONTOURS: usize = 16_384;
const MAX_MASKS: usize = 256;
/// Keep each precomputed pixel buffer below the default 128 MiB storage
/// binding limit. Sixteen 1024² textures fit comfortably; a very large
/// photo/import stack falls back to the CPU instead of failing validation.
const MAX_PIXEL_TOTAL: u32 = 32 * 1024 * 1024 - 1;
/// Largest padded FFT side. A 1024² document mirror-pads to 2048², which is
/// 64 MiB for the two complex ping-pong buffers.
const MAX_FFT_SIDE: u32 = 2048;

fn fft_padded_size(width: u32, height: u32, wrap: Wrap) -> (u32, u32) {
    match wrap {
        Wrap::Repeat => (width, height),
        Wrap::Clamp => (width.saturating_mul(2), height.saturating_mul(2)),
    }
}

/// Whether the document can use the GPU FFT integrator. Sizes must be
/// power-of-two so the radix-2 passes have no remainder, and the padded
/// buffers stay within a predictable memory budget.
pub fn height_integration_supported(width: u32, height: u32, wrap: Wrap) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    let (pw, ph) = fft_padded_size(width, height, wrap);
    pw.is_power_of_two() && ph.is_power_of_two() && pw <= MAX_FFT_SIDE && ph <= MAX_FFT_SIDE
}

#[derive(Clone, Copy)]
struct PackedShape {
    kind: u32,
    center: [f32; 2],
    half_size: [f32; 2],
    corner_radius: f32,
    vertex_offset: u32,
    vertex_count: u32,
}

/// Push one closed contour and return its index. Rejects degenerate loops and
/// non-convex winding mixes so the shader's union-of-islands distance stays
/// equivalent to the CPU even-odd/non-zero rasterizer.
fn push_contour(
    polygon: &[glam::Vec2],
    vertices: &mut Vec<[f32; 2]>,
    contours: &mut Vec<GpuContour>,
    winding: &mut Option<f32>,
) -> Option<()> {
    let mut points = polygon.to_vec();
    if points.len() >= 2 && (points[0] - points[points.len() - 1]).length() < 1e-3 {
        points.pop();
    }
    if points.len() < 3 || contours.len() >= MAX_CONTOURS {
        return None;
    }
    let mut area = 0.0f32;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        area += a.x * b.y - b.x * a.y;
    }
    if area.abs() < 1e-4 {
        return None;
    }
    // Holes are encoded as opposite-winding loops; the shader union only
    // supports disjoint islands of the same winding.
    match winding {
        Some(sign) if *sign * area < 0.0 => return None,
        Some(_) => {}
        None => *winding = Some(area.signum()),
    }
    if vertices.len().checked_add(points.len())? > MAX_VERTICES {
        return None;
    }
    let mut aabb_min = points[0];
    let mut aabb_max = points[0];
    for point in &points[1..] {
        aabb_min = aabb_min.min(*point);
        aabb_max = aabb_max.max(*point);
    }
    let vertex_offset = vertices.len() as u32;
    vertices.extend(points.iter().map(|point| [point.x, point.y]));
    contours.push(GpuContour {
        vertex_offset,
        vertex_count: points.len() as u32,
        aabb_min: [aabb_min.x, aabb_min.y],
        aabb_max: [aabb_max.x, aabb_max.y],
    });
    Some(())
}

fn pack_source(
    source: &ns_core::ShapeSource,
    vertices: &mut Vec<[f32; 2]>,
    contours: &mut Vec<GpuContour>,
) -> Option<PackedShape> {
    match source {
        ns_core::ShapeSource::Primitive(ns_core::Primitive::Rect {
            center,
            size,
            corner_radius,
        }) => {
            if size.x.abs() < 1e-3 || size.y.abs() < 1e-3 {
                return None;
            }
            Some(PackedShape {
                kind: 3,
                center: [center.x, center.y],
                half_size: [size.x.abs() * 0.5, size.y.abs() * 0.5],
                corner_radius: corner_radius.max(0.0),
                vertex_offset: 0,
                vertex_count: 0,
            })
        }
        ns_core::ShapeSource::Primitive(ns_core::Primitive::Ellipse { center, radii }) => {
            if radii.x.abs() < 1e-3 || radii.y.abs() < 1e-3 {
                return None;
            }
            Some(PackedShape {
                kind: 4,
                center: [center.x, center.y],
                half_size: [radii.x.abs(), radii.y.abs()],
                corner_radius: 0.0,
                vertex_offset: 0,
                vertex_count: 0,
            })
        }
        ns_core::ShapeSource::Primitive(_) => {
            // Polygon and Star primitives are single closed loops.
            let polygon = source.as_polygon()?;
            let contour_offset = contours.len() as u32;
            let mut winding = None;
            push_contour(&polygon, vertices, contours, &mut winding)?;
            Some(PackedShape {
                kind: 1,
                center: [0.0; 2],
                half_size: [0.0; 2],
                corner_radius: 0.0,
                vertex_offset: contour_offset,
                vertex_count: 1,
            })
        }
        ns_core::ShapeSource::Path(path) => {
            if path.subpaths.is_empty() {
                return None;
            }
            // Tolerance-based flattening keeps straight edges at two points
            // instead of the CPU rasterizer's fixed 24 samples per segment.
            // That is the difference between 144 and 6 vertices per hex cell.
            let loops = ns_core::path::flatten_path_tolerance(path, 0.25);
            let loops: Vec<&Vec<glam::Vec2>> =
                loops.iter().filter(|polygon| polygon.len() >= 3).collect();
            // The shader evaluates the union of disjoint islands. Non-zero fill
            // matches that union when every loop winds the same way, which
            // `push_contour` enforces. Even-odd is only equivalent for a single
            // loop — the shape an open stroke band produces — while holes and
            // nested loops still need the CPU rasterizer.
            if loops.is_empty()
                || (path.fill_rule != ns_core::path::FillRule::NonZero && loops.len() != 1)
            {
                return None;
            }
            let contour_offset = contours.len() as u32;
            let mut winding = None;
            for polygon in loops {
                push_contour(polygon, vertices, contours, &mut winding)?;
            }
            let contour_count = contours.len() as u32 - contour_offset;
            if contour_count == 0 {
                return None;
            }
            Some(PackedShape {
                kind: 1,
                center: [0.0; 2],
                half_size: [0.0; 2],
                corner_radius: 0.0,
                vertex_offset: contour_offset,
                vertex_count: contour_count,
            })
        }
        ns_core::ShapeSource::Raster(_)
        | ns_core::ShapeSource::RasterRegion(_)
        | ns_core::ShapeSource::Text(_) => None,
    }
}

fn pack_mask(
    layer: &ns_core::Layer,
    vertices: &mut Vec<[f32; 2]>,
    contours: &mut Vec<GpuContour>,
) -> Option<GpuMask> {
    let LayerKind::Shape(shape) = &layer.kind else {
        return None;
    };
    // Match the CPU coverage path: an open stroked line is its stroke band.
    let band = shape.open_band();
    let (source, stroke, stroke_width) = match &band {
        Some(band) => (band, false, 0.0),
        None => (
            &shape.source,
            shape.style.is_stroke(),
            if shape.style.is_stroke() {
                shape.stroke_width()
            } else {
                0.0
            },
        ),
    };
    let packed = pack_source(source, vertices, contours)?;
    Some(GpuMask {
        kind: packed.kind,
        vertex_offset: packed.vertex_offset,
        vertex_count: packed.vertex_count,
        flags: u32::from(stroke),
        center: packed.center,
        half_size: packed.half_size,
        corner_radius: packed.corner_radius,
        opacity: layer.opacity.clamp(0.0, 1.0),
        _pad: [stroke_width, 0.0],
    })
}

/// Build a GPU scene with the default options. Used by tests; the editor
/// calls [`build_scene_cached`] so precomputed layer textures survive edits.
#[cfg(test)]
pub fn build_scene(doc: &Document) -> Option<GpuSceneData> {
    build_scene_with_options(doc, GpuSceneOptions::default())
}

/// Build a GPU scene with opt-in post-processing. Requesting height
/// integration on an unsupported size returns `None`, which invokes the CPU
/// fallback instead of silently producing analytic height.
#[cfg(test)]
pub fn build_scene_with_options(doc: &Document, options: GpuSceneOptions) -> Option<GpuSceneData> {
    build_scene_cached(doc, options, &mut GpuSceneCache::default())
}

/// Build a GPU scene, reusing CPU-precomputed layer textures from `cache`.
///
/// Layers the shader cannot evaluate analytically (raster sources, smoothed
/// outlines, text, photo/painted layers, holes) are evaluated once on the CPU,
/// uploaded as normal+height pixels and composited by the GPU. Painted static
/// masks are uploaded as coverage buffers and sampled by the same shader.
pub fn build_scene_cached(
    doc: &Document,
    options: GpuSceneOptions,
    cache: &mut GpuSceneCache,
) -> Option<GpuSceneData> {
    if options.integrate_height && !height_integration_supported(doc.size.x, doc.size.y, doc.wrap) {
        return None;
    }
    fn contains_vector_compound(layers: &[ns_core::Layer], tile: u32) -> bool {
        layers.iter().any(|layer| {
            layer.applies_to_tile(tile)
                && (layer.vector_op.is_some()
                    || layer
                        .children()
                        .is_some_and(|children| contains_vector_compound(children, tile)))
        })
    }
    if contains_vector_compound(&doc.layers, doc.active_tile) {
        return None;
    }
    let mut build = SceneBuild::new(doc, cache);
    build.push_layers(&doc.layers)?;
    Some(build.finish(options))
}

/// Build a GPU scene and, on failure, report every reason the document was
/// rejected instead of only the first one the packer hit.
///
/// The actual gate still runs through [`build_scene_cached`], so behaviour is
/// unchanged. The diagnostic scan runs only after a rejection, which keeps the
/// successful path free of extra work.
pub fn build_scene_reported(
    doc: &Document,
    options: GpuSceneOptions,
    cache: &mut GpuSceneCache,
) -> Result<GpuSceneData, Vec<GateReject>> {
    match build_scene_cached(doc, options, cache) {
        Some(scene) => Ok(scene),
        None => {
            let mut reasons = diagnose_gate(doc, options);
            if reasons.is_empty() {
                reasons.push(GateReject::UnsupportedLayer);
            }
            Err(reasons)
        }
    }
}

/// Scan a document for every condition that forces the whole stack onto the
/// CPU path. This mirrors the packer's rejection rules; it is diagnostic only
/// and never decides whether a scene is usable.
pub fn diagnose_gate(doc: &Document, options: GpuSceneOptions) -> Vec<GateReject> {
    let mut scan = GateScan {
        doc,
        reasons: Vec::new(),
        layers: 0,
        masks: 0,
        seen_masks: HashSet::new(),
        layer_pixels: 0,
        mask_pixels: 0,
    };
    if options.integrate_height && !height_integration_supported(doc.size.x, doc.size.y, doc.wrap) {
        scan.reject(GateReject::FftSizeUnsupported);
    }
    scan.scan_layers(&doc.layers);

    if scan.layers > MAX_LAYERS {
        scan.reject(GateReject::TooManyLayers);
    }
    if scan.masks > MAX_MASKS {
        scan.reject(GateReject::TooManyMasks);
    }
    if scan.layer_pixels.saturating_add(scan.mask_pixels) > u64::from(MAX_PIXEL_TOTAL) {
        scan.reject(GateReject::PixelBufferOverflow);
    }
    scan.reasons
}

struct GateScan<'a> {
    doc: &'a Document,
    reasons: Vec<GateReject>,
    layers: usize,
    masks: usize,
    seen_masks: HashSet<ns_core::LayerId>,
    layer_pixels: u64,
    mask_pixels: u64,
}

impl GateScan<'_> {
    fn reject(&mut self, reason: GateReject) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    fn scan_layers(&mut self, layers: &[ns_core::Layer]) {
        for layer in layers {
            // Layers tagged for another UDIM tile never reach the evaluator, so
            // they must not force this tile onto the CPU either.
            if !layer.applies_to_tile(self.doc.active_tile) {
                continue;
            }
            // The packer tests vector compounds before visibility, so match it
            // here even though a hidden compound cannot affect the image.
            if layer.vector_op.is_some() {
                self.reject(GateReject::VectorCompound);
            }
            if !layer.paints() {
                continue;
            }

            let mut rejected = false;
            if !layer.boolean_operands.is_empty() {
                self.reject(GateReject::BooleanOperand);
                rejected = true;
            }
            // Smart masks read the surface below; clip feather/invert is
            // likewise CPU-only.
            if layer.mask_settings.needs_base() || layer.mask_settings.clip_is_adjusted() {
                self.reject(GateReject::SmartMask);
                rejected = true;
            }

            let painted =
                layer.mask.is_some() || layer.mask_settings.has_effect(layer.mask.is_some());
            if matches!(layer.kind, LayerKind::Group { .. }) && painted {
                self.reject(GateReject::GroupPaintedMask);
                rejected = true;
            }

            if let Some(mask_id) = layer.mask_layer {
                if layer.mask.is_some() || layer.mask_settings.has_effect(false) {
                    self.reject(GateReject::MaskWithClipSource);
                    rejected = true;
                } else if !matches!(
                    layer.kind,
                    LayerKind::Shape(_) | LayerKind::Flat | LayerKind::Fill { .. }
                ) {
                    self.reject(GateReject::ClipOnUnsupportedKind);
                    rejected = true;
                } else if let Some(mask_layer) = self.doc.find_layer(mask_id) {
                    if !matches!(mask_layer.kind, LayerKind::Shape(_))
                        || mask_layer.mask.is_some()
                        || mask_layer.mask_layer.is_some()
                        || mask_layer.mask_settings.has_effect(false)
                        || mask_layer.mask_settings.needs_base()
                        || mask_layer.mask_settings.clip_is_adjusted()
                    {
                        self.reject(GateReject::ClipOnUnsupportedKind);
                        rejected = true;
                    } else if self.seen_masks.insert(mask_id) {
                        self.masks += 1;
                    }
                } else {
                    self.reject(GateReject::ClipOnUnsupportedKind);
                    rejected = true;
                }
            } else if painted {
                self.masks += 1;
                self.mask_pixels = self
                    .mask_pixels
                    .saturating_add(u64::from(self.doc.size.x) * u64::from(self.doc.size.y));
            }

            if layer.blend.uses_surface() {
                self.reject(GateReject::SurfaceBoolean);
                rejected = true;
            }

            match &layer.kind {
                LayerKind::Group { children } => {
                    let identity =
                        (layer.opacity - 1.0).abs() < 1e-6 && layer.blend == BlendMode::Derivative;
                    if !identity {
                        self.reject(GateReject::NonIdentityGroup);
                    }
                    self.scan_layers(children);
                }
                LayerKind::Flat | LayerKind::Fill { .. } => {
                    if !rejected {
                        self.layers += 1;
                    }
                }
                LayerKind::Shape(shape) => {
                    if !rejected {
                        self.layers += 1;
                        if shape_needs_texture(shape) {
                            self.layer_pixels = self.layer_pixels.saturating_add(
                                u64::from(self.doc.size.x) * u64::from(self.doc.size.y),
                            );
                        }
                    }
                }
                LayerKind::PhotoHeight(_)
                | LayerKind::ImportedNormal { .. }
                | LayerKind::NormalPaint { .. }
                | LayerKind::HeightPaint { .. } => {
                    if !rejected {
                        self.layers += 1;
                        self.layer_pixels = self.layer_pixels.saturating_add(
                            u64::from(self.doc.size.x) * u64::from(self.doc.size.y),
                        );
                    }
                }
            }
        }
    }
}

/// Layers that always take the CPU-precomputed texture path. Paths that fail
/// analytic packing later (holes, mixed winding) are classified as
/// [`GateReject::UnsupportedLayer`] if the packer still rejects the scene.
fn shape_needs_texture(shape: &ns_core::ShapeLayer) -> bool {
    shape.smooth_px > 1e-4
        || matches!(shape.profile, Profile::Custom(_))
        || matches!(
            &shape.source,
            ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) | ShapeSource::Text(_)
        )
        || matches!(&shape.source, ShapeSource::Path(path) if path.has_edge_profiles())
}

struct SceneBuild<'a> {
    doc: &'a Document,
    layers: Vec<GpuLayer>,
    masks: Vec<GpuMask>,
    mask_cache: HashMap<ns_core::LayerId, u32>,
    vertices: Vec<[f32; 2]>,
    contours: Vec<GpuContour>,
    layer_pixels: Vec<Arc<Vec<u32>>>,
    layer_pixel_total: u32,
    mask_pixels: Vec<Arc<Vec<u32>>>,
    mask_pixel_total: u32,
    cache: &'a mut GpuSceneCache,
}

impl<'a> SceneBuild<'a> {
    fn new(doc: &'a Document, cache: &'a mut GpuSceneCache) -> Self {
        Self {
            doc,
            layers: Vec::new(),
            masks: Vec::new(),
            mask_cache: HashMap::new(),
            vertices: Vec::new(),
            contours: Vec::new(),
            layer_pixels: Vec::new(),
            layer_pixel_total: 0,
            mask_pixels: Vec::new(),
            mask_pixel_total: 0,
            cache,
        }
    }

    fn finish(self, options: GpuSceneOptions) -> GpuSceneData {
        let doc = self.doc;
        self.cache
            .entries
            .retain(|id, _| doc.find_layer(*id).is_some());
        GpuSceneData {
            width: doc.size.x,
            height: doc.size.y,
            height_scale: doc.height_scale_px,
            layers: self.layers,
            masks: self.masks,
            vertices: self.vertices,
            contours: self.contours,
            layer_pixels: self.layer_pixels,
            layer_pixel_total: self.layer_pixel_total,
            mask_pixels: self.mask_pixels,
            mask_pixel_total: self.mask_pixel_total,
            integrate_height: options.integrate_height,
            repeat: doc.wrap == Wrap::Repeat,
        }
    }

    fn push_layers(&mut self, layers: &[ns_core::Layer]) -> Option<()> {
        for layer in layers {
            if !layer.applies_to_tile(self.doc.active_tile) {
                continue;
            }
            if layer.vector_op.is_some() {
                return None;
            }
            if !layer.paints() {
                continue;
            }
            if !layer.boolean_operands.is_empty() {
                return None;
            }
            // Smart generators read the surface below, which only the CPU tree
            // evaluator tracks; clip feather/invert is likewise CPU-only.
            if layer.mask_settings.needs_base() || layer.mask_settings.clip_is_adjusted() {
                return None;
            }
            if layer.mask_layer.is_some()
                && !matches!(
                    layer.kind,
                    LayerKind::Shape(_) | LayerKind::Flat | LayerKind::Fill { .. }
                )
            {
                return None;
            }
            let painted =
                layer.mask.is_some() || layer.mask_settings.has_effect(layer.mask.is_some());
            if matches!(layer.kind, LayerKind::Group { .. }) && painted {
                // A group mask applies to the group's finished composite; the
                // shader masks individual layers, so keep this on the CPU.
                return None;
            }
            let mask_index = self.push_mask(layer)?;
            let blend = match layer.blend {
                BlendMode::Derivative => 0,
                BlendMode::Rnm => 1,
                BlendMode::Replace => 2,
                BlendMode::Whiteout => 3,
                BlendMode::Udn => 4,
                BlendMode::BooleanUnion
                | BlendMode::BooleanIntersect
                | BlendMode::BooleanDifference => return None,
                // Cut modes are gated to the CPU before packing.
                BlendMode::PartialCut | BlendMode::PartialSlash | BlendMode::ClipDivide => {
                    return None
                }
            };
            match &layer.kind {
                LayerKind::Group { children } => {
                    let identity =
                        (layer.opacity - 1.0).abs() < 1e-6 && layer.blend == BlendMode::Derivative;
                    if !identity {
                        return None;
                    }
                    self.push_layers(children)?;
                }
                LayerKind::Flat => self.layers.push(GpuLayer {
                    kind: 0,
                    blend,
                    mask_index,
                    opacity: layer.opacity,
                    ..Default::default()
                }),
                LayerKind::Fill { height } => self.layers.push(GpuLayer {
                    kind: 2,
                    blend,
                    mask_index,
                    height: *height,
                    opacity: layer.opacity,
                    ..Default::default()
                }),
                LayerKind::Shape(shape) => {
                    // Analytic SDF is exact and cheap; anything the shader
                    // cannot represent falls back to a precomputed texture.
                    let analytic = shape.smooth_px <= 1e-4
                        && !matches!(shape.profile, Profile::Custom(_))
                        && !matches!(&shape.source, ShapeSource::Path(path) if path.has_edge_profiles());
                    let packed = if analytic {
                        self.pack_analytic_shape(shape)
                    } else {
                        None
                    };
                    match packed {
                        Some((packed, stroke, stroke_width, bounds)) => {
                            self.push_analytic(
                                layer,
                                shape,
                                packed,
                                stroke,
                                stroke_width,
                                bounds,
                                mask_index,
                                blend,
                            );
                        }
                        None => self.push_textured(layer, mask_index, blend)?,
                    }
                }
                LayerKind::PhotoHeight(_)
                | LayerKind::ImportedNormal { .. }
                | LayerKind::NormalPaint { .. }
                | LayerKind::HeightPaint { .. } => self.push_textured(layer, mask_index, blend)?,
            }
            if self.layers.len() > MAX_LAYERS {
                return None;
            }
        }
        Some(())
    }

    /// Resolve this layer's clipping or painted mask into a shader mask index.
    fn push_mask(&mut self, layer: &ns_core::Layer) -> Option<u32> {
        if let Some(mask_id) = layer.mask_layer {
            if layer.mask.is_some() || layer.mask_settings.has_effect(false) {
                // A painted mask and a clipping source together need the CPU
                // mask pipeline.
                return None;
            }
            if let Some(&index) = self.mask_cache.get(&mask_id) {
                return Some(index);
            }
            let mask_layer = self.doc.find_layer(mask_id)?;
            if mask_layer.mask.is_some()
                || mask_layer.mask_layer.is_some()
                || mask_layer.mask_settings.has_effect(false)
                || mask_layer.mask_settings.needs_base()
                || mask_layer.mask_settings.clip_is_adjusted()
            {
                return None;
            }
            if self.masks.len() >= MAX_MASKS {
                return None;
            }
            let index = self.masks.len() as u32;
            self.masks.push(pack_mask(
                mask_layer,
                &mut self.vertices,
                &mut self.contours,
            )?);
            self.mask_cache.insert(mask_id, index);
            return Some(index);
        }

        let painted = layer.mask.is_some() || layer.mask_settings.has_effect(layer.mask.is_some());
        if !painted {
            return Some(u32::MAX);
        }
        let Some(pixels) = self.painted_mask_pixels(layer)? else {
            return Some(u32::MAX);
        };
        if self.masks.len() >= MAX_MASKS {
            return None;
        }
        let offset = self.mask_pixel_total;
        self.mask_pixel_total = self
            .mask_pixel_total
            .checked_add(pixels.len() as u32)
            .filter(|total| *total <= MAX_PIXEL_TOTAL)?;
        self.mask_pixels.push(pixels);
        let index = self.masks.len() as u32;
        self.masks.push(GpuMask {
            kind: 5,
            vertex_offset: offset,
            opacity: 1.0,
            ..Default::default()
        });
        Some(index)
    }

    fn painted_mask_pixels(&mut self, layer: &ns_core::Layer) -> Option<Option<Arc<Vec<u32>>>> {
        let size = (self.doc.size.x, self.doc.size.y);
        let revision = (layer.revision, layer.mask_revision);
        let entry = self
            .cache
            .entries
            .entry(layer.id)
            .or_insert_with(|| CachedGpuLayer::new(size, self.doc.wrap));
        if entry.size != size || entry.wrap != self.doc.wrap {
            *entry = CachedGpuLayer::new(size, self.doc.wrap);
        }
        if entry.mask_revision != Some(revision) {
            let mask = ns_core::mask::static_mask(
                layer.mask.as_ref(),
                &layer.mask_settings,
                self.doc.size,
                self.doc.wrap,
            );
            entry.mask_pixels = mask.map(|mask| Arc::new(encode_mask(&mask)));
            entry.mask_revision = Some(revision);
        }
        Some(entry.mask_pixels.clone())
    }

    fn pack_analytic_shape(
        &mut self,
        shape: &ns_core::ShapeLayer,
    ) -> Option<(PackedShape, bool, f32, ns_core::Bounds)> {
        let band = shape.open_band();
        let (source, stroke, stroke_width) = match &band {
            Some(band) => (band, false, 0.0),
            None => (
                &shape.source,
                shape.style.is_stroke(),
                if shape.style.is_stroke() {
                    shape.stroke_width()
                } else {
                    0.0
                },
            ),
        };
        let vertex_len = self.vertices.len();
        let contour_len = self.contours.len();
        let packed = pack_source(source, &mut self.vertices, &mut self.contours);
        let bounds = source.bounds();
        match (packed, bounds) {
            (Some(packed), Some(bounds)) => Some((packed, stroke, stroke_width, bounds)),
            _ => {
                // `pack_source` may have pushed the first contours before a
                // later one was rejected; undo that before using the texture.
                self.vertices.truncate(vertex_len);
                self.contours.truncate(contour_len);
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_analytic(
        &mut self,
        layer: &ns_core::Layer,
        shape: &ns_core::ShapeLayer,
        packed: PackedShape,
        stroke: bool,
        stroke_width: f32,
        bounds: ns_core::Bounds,
        mask_index: u32,
        blend: u32,
    ) {
        let (profile, chamfer_deg) = match &shape.profile {
            Profile::Round => (0u32, 0.0),
            Profile::Chamfer { degrees } => (1, *degrees),
            Profile::Linear => (2, 0.0),
            Profile::Convex => (3, 0.0),
            Profile::Concave => (4, 0.0),
            Profile::Smooth => (5, 0.0),
            Profile::Custom(_) => unreachable!("custom profiles use the texture path"),
        };
        let softness = shape.softness_px.max(0.0);
        let expand = shape.radius_px.max(0.0) + softness * 3.0 + stroke_width * 0.5 + 2.0;
        // Match `ShapeLayer::region_rect` exactly: floor the expanded minimum and
        // ceil the expanded maximum. A raw float AABB can lose the first/last
        // pixel of an inverted shape's plateau, while a blanket extra pixel
        // would add one beyond the CPU region.
        let aabb_min = [
            (bounds.min.x - expand).floor(),
            (bounds.min.y - expand).floor(),
        ];
        let aabb_max = [
            (bounds.max.x + expand).ceil(),
            (bounds.max.y + expand).ceil(),
        ];
        self.layers.push(GpuLayer {
            kind: packed.kind,
            blend,
            vertex_offset: packed.vertex_offset,
            vertex_count: packed.vertex_count,
            flags: u32::from(shape.invert) | (u32::from(stroke) << 2) | (profile << 3),
            mask_index,
            radius: shape.radius_px.max(0.5),
            height: shape.height_px,
            opacity: layer.opacity,
            chamfer_deg,
            corner_radius: packed.corner_radius,
            softness,
            stroke_width,
            edge_aa: shape.edge_aa_px.max(0.0),
            center: packed.center,
            half_size: packed.half_size,
            aabb_min,
            aabb_max,
        });
    }

    fn push_textured(&mut self, layer: &ns_core::Layer, mask_index: u32, blend: u32) -> Option<()> {
        let pixels = self.texture_pixels(layer)?;
        let offset = self.layer_pixel_total;
        self.layer_pixel_total = self
            .layer_pixel_total
            .checked_add(pixels.len() as u32)
            .filter(|total| *total <= MAX_PIXEL_TOTAL)?;
        self.layer_pixels.push(pixels);
        self.layers.push(GpuLayer {
            kind: 5,
            blend,
            vertex_offset: offset,
            mask_index,
            opacity: layer.opacity,
            ..Default::default()
        });
        Some(())
    }

    fn texture_pixels(&mut self, layer: &ns_core::Layer) -> Option<Arc<Vec<u32>>> {
        let size = (self.doc.size.x, self.doc.size.y);
        let revision = layer.content_revision();
        let entry = self
            .cache
            .entries
            .entry(layer.id)
            .or_insert_with(|| CachedGpuLayer::new(size, self.doc.wrap));
        if entry.size != size || entry.wrap != self.doc.wrap {
            *entry = CachedGpuLayer::new(size, self.doc.wrap);
        }
        if entry.normal_revision != Some(revision) {
            let (normal, height) = self.doc.evaluate_layer_contribution(layer.id)?;
            entry.normal_pixels = Some(Arc::new(encode_layer_pixels(&normal, &height)));
            entry.normal_revision = Some(revision);
        }
        entry.normal_pixels.clone()
    }
}

/// Pack a layer normal and height into one u32 per pixel: RGBA8, A = height.
fn encode_layer_pixels(
    normal: &ns_core::Grid<glam::Vec3>,
    height: &ns_core::Grid<f32>,
) -> Vec<u32> {
    let mut out = Vec::with_capacity((normal.w as usize) * (normal.h as usize));
    for (n, h) in normal.data.iter().zip(height.data.iter()) {
        let n = if n.length_squared() > 1.0e-12 {
            n.normalize()
        } else {
            glam::Vec3::Z
        };
        let r = ((n.x * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u32;
        let g = ((n.y * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u32;
        let b = ((n.z * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u32;
        let a = (h.clamp(0.0, 1.0) * 255.0).round() as u32;
        out.push(r | (g << 8) | (b << 16) | (a << 24));
    }
    out
}

/// Pack mask coverage into the low byte of each u32.
fn encode_mask(mask: &ns_core::Grid<f32>) -> Vec<u32> {
    mask.data
        .iter()
        .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u32)
        .collect()
}

pub struct NsGpuPlugin;

impl Plugin for NsGpuPlugin {
    fn build(&self, app: &mut App) {
        let upload_stats = GpuUploadStats::default();
        app.insert_resource(upload_stats.clone())
            .init_resource::<GpuScene>()
            .init_resource::<GpuSceneCache>()
            .add_plugins(ExtractResourcePlugin::<GpuScene>::default())
            .add_plugins(ExtractResourcePlugin::<GpuOutputs>::default());

        let (shader, height_shaders) = {
            let mut shaders = app.world_mut().resource_mut::<Assets<Shader>>();
            let shader = shaders.add(Shader::from_wgsl(
                include_str!("normal_studio.wgsl"),
                file!(),
            ));
            let height_shaders = NsHeightShaders {
                prep: shaders.add(Shader::from_wgsl(include_str!("height_prep.wgsl"), file!())),
                fft: shaders.add(Shader::from_wgsl(include_str!("height_fft.wgsl"), file!())),
                spectrum: shaders.add(Shader::from_wgsl(
                    include_str!("height_spectrum.wgsl"),
                    file!(),
                )),
                finalize: shaders.add(Shader::from_wgsl(
                    include_str!("height_finalize.wgsl"),
                    file!(),
                )),
                normalize: shaders.add(Shader::from_wgsl(
                    include_str!("height_normalize.wgsl"),
                    file!(),
                )),
            };
            (shader, height_shaders)
        };
        app.insert_resource(NsShader(shader));
        app.insert_resource(height_shaders);

        let render_app = app.sub_app_mut(RenderApp);
        render_app.insert_resource(upload_stats);
        render_app.init_resource::<GpuPrepared>();
        render_app.add_systems(
            RenderGraph,
            (prepare_gpu, dispatch_gpu_compute)
                .chain()
                .in_set(RenderGraphSystems::Render)
                .before(camera_driver),
        );
    }

    fn finish(&self, app: &mut App) {
        let shader = app.world().resource::<NsShader>().0.clone();
        let height = app.world().resource::<NsHeightShaders>();
        let height_render = NsHeightShadersRender {
            prep: height.prep.clone(),
            fft: height.fft.clone(),
            spectrum: height.spectrum.clone(),
            finalize: height.finalize.clone(),
            normalize: height.normalize.clone(),
        };
        let render_app = app.sub_app_mut(RenderApp);
        render_app.insert_resource(NsShaderRender(shader));
        render_app.insert_resource(height_render);
        render_app.init_resource::<GpuPipeline>();
        render_app.init_resource::<GpuIntegrationPipelines>();
    }
}

#[derive(Resource)]
struct NsShader(Handle<Shader>);

#[derive(Resource)]
struct NsShaderRender(Handle<Shader>);

#[derive(Resource)]
struct NsHeightShaders {
    prep: Handle<Shader>,
    fft: Handle<Shader>,
    spectrum: Handle<Shader>,
    finalize: Handle<Shader>,
    normalize: Handle<Shader>,
}

#[derive(Resource)]
struct NsHeightShadersRender {
    prep: Handle<Shader>,
    fft: Handle<Shader>,
    spectrum: Handle<Shader>,
    finalize: Handle<Shader>,
    normalize: Handle<Shader>,
}

#[derive(Resource)]
struct GpuPipeline {
    layout: BindGroupLayout,
    pipeline: CachedComputePipelineId,
}

impl FromWorld for GpuPipeline {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();
        let entries = BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                uniform_buffer_sized(false, None),
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
                texture_storage_2d(TextureFormat::Rgba8Unorm, StorageTextureAccess::WriteOnly),
                texture_storage_2d(TextureFormat::Rgba8Unorm, StorageTextureAccess::WriteOnly),
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
            ),
        );
        let layout = render_device.create_bind_group_layout("ns_gpu_layout", &entries);
        let shader = world.resource::<NsShaderRender>().0.clone();
        let pipeline_cache = world.resource::<PipelineCache>();
        let pipeline = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some(Cow::Borrowed("ns_gpu_eval")),
            layout: vec![BindGroupLayoutDescriptor::new("ns_gpu_layout", &entries)],
            immediate_size: 0,
            shader,
            shader_defs: Vec::new(),
            entry_point: Some(Cow::Borrowed("main")),
            zero_initialize_workgroup_memory: false,
        });
        Self { layout, pipeline }
    }
}

#[derive(Resource)]
struct GpuIntegrationPipelines {
    _prep_layout: BindGroupLayout,
    _fft_layout: BindGroupLayout,
    _finalize_layout: BindGroupLayout,
    _normalize_layout: BindGroupLayout,
    prep: CachedComputePipelineId,
    fft: CachedComputePipelineId,
    bitrev: CachedComputePipelineId,
    spectrum: CachedComputePipelineId,
    finalize: CachedComputePipelineId,
    normalize: CachedComputePipelineId,
}

impl FromWorld for GpuIntegrationPipelines {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();
        let prep_entries = BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                texture_storage_2d(TextureFormat::Rgba8Unorm, StorageTextureAccess::ReadOnly),
                storage_buffer_sized(false, None),
            ),
        );
        let fft_entries = BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                storage_buffer_read_only_sized(false, None),
                storage_buffer_sized(false, None),
            ),
        );
        let finalize_entries = BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                storage_buffer_read_only_sized(false, None),
                storage_buffer_sized(false, None),
                storage_buffer_sized(false, None),
            ),
        );
        let normalize_entries = BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
                texture_storage_2d(TextureFormat::Rgba8Unorm, StorageTextureAccess::WriteOnly),
            ),
        );

        let shaders = world.resource::<NsHeightShadersRender>();
        let pipeline_cache = world.resource::<PipelineCache>();
        let queue = |label: &'static str,
                     shader: &Handle<Shader>,
                     entries: &[BindGroupLayoutEntry],
                     entry: &'static str| {
            let layout = render_device.create_bind_group_layout(label, entries);
            let id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
                label: Some(Cow::Borrowed(label)),
                layout: vec![BindGroupLayoutDescriptor::new(label, entries)],
                immediate_size: 32,
                shader: shader.clone(),
                shader_defs: Vec::new(),
                entry_point: Some(Cow::Borrowed(entry)),
                zero_initialize_workgroup_memory: false,
            });
            (layout, id)
        };
        let (prep_layout, prep) = queue("ns_height_prep", &shaders.prep, &prep_entries, "prep");
        let (fft_layout, fft) = queue("ns_height_fft", &shaders.fft, &fft_entries, "fft");
        let (_, bitrev) = queue("ns_height_bitrev", &shaders.fft, &fft_entries, "bitrev");
        let (_, spectrum) = queue(
            "ns_height_spectrum",
            &shaders.spectrum,
            &fft_entries,
            "spectrum",
        );
        let (finalize_layout, finalize) = queue(
            "ns_height_finalize",
            &shaders.finalize,
            &finalize_entries,
            "finalize",
        );
        let (normalize_layout, normalize) = queue(
            "ns_height_normalize",
            &shaders.normalize,
            &normalize_entries,
            "normalize",
        );

        Self {
            _prep_layout: prep_layout,
            _fft_layout: fft_layout,
            _finalize_layout: finalize_layout,
            _normalize_layout: normalize_layout,
            prep,
            fft,
            bitrev,
            spectrum,
            finalize,
            normalize,
        }
    }
}

struct IntegrationPrepared {
    doc_size: (u32, u32),
    padded: (u32, u32),
    bits_x: u32,
    bits_y: u32,
    repeat: bool,
    prep: BindGroup,
    fft: [BindGroup; 2],
    finalize: [BindGroup; 2],
    normalize: BindGroup,
    minmax: Buffer,
    _fft_a: Buffer,
    _fft_b: Buffer,
    _raw: Buffer,
}

#[derive(Resource, Default)]
struct GpuPrepared {
    generation: u64,
    dispatched_generation: u64,
    bind_group: Option<BindGroup>,
    buffers: Option<(Buffer, Buffer, Buffer, Buffer, Buffer, Buffer, Buffer)>,
    integration: Option<IntegrationPrepared>,
    workgroups: (u32, u32),
}

fn reset_minmax(queue: &RenderQueue, minmax: &Buffer) {
    let initial = [0xffff_ffffu32, 0, 0, 0];
    queue.write_buffer(minmax, 0, bytemuck::cast_slice(&initial));
}

fn prepare_integration(
    data: &GpuSceneData,
    normal_image: &GpuImage,
    height_image: &GpuImage,
    render_device: &RenderDevice,
    render_queue: &RenderQueue,
    pipelines: &GpuIntegrationPipelines,
    existing: Option<IntegrationPrepared>,
) -> Option<IntegrationPrepared> {
    let wrap = if data.repeat {
        Wrap::Repeat
    } else {
        Wrap::Clamp
    };
    if !height_integration_supported(data.width, data.height, wrap) {
        return None;
    }
    let padded = fft_padded_size(data.width, data.height, wrap);
    let doc_size = (data.width, data.height);
    if let Some(existing) = existing {
        if existing.doc_size == doc_size && existing.padded == padded {
            reset_minmax(render_queue, &existing.minmax);
            return Some(existing);
        }
    }

    let (pad_w, pad_h) = padded;
    let bits_x = pad_w.trailing_zeros();
    let bits_y = pad_h.trailing_zeros();
    let fft_bytes = u64::from(pad_w) * u64::from(pad_h) * 8;
    let raw_bytes = u64::from(data.width) * u64::from(data.height) * 4;

    let fft_a = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_height_fft_a"),
        size: fft_bytes,
        usage: BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let fft_b = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_height_fft_b"),
        size: fft_bytes,
        usage: BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let raw = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_height_raw"),
        size: raw_bytes,
        usage: BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let minmax = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_height_minmax"),
        size: 16,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    reset_minmax(render_queue, &minmax);

    let prep = render_device.create_bind_group(
        None,
        &pipelines._prep_layout,
        &BindGroupEntries::sequential((&normal_image.texture_view, fft_a.as_entire_binding())),
    );
    let fft_ab = render_device.create_bind_group(
        None,
        &pipelines._fft_layout,
        &BindGroupEntries::sequential((fft_a.as_entire_binding(), fft_b.as_entire_binding())),
    );
    let fft_ba = render_device.create_bind_group(
        None,
        &pipelines._fft_layout,
        &BindGroupEntries::sequential((fft_b.as_entire_binding(), fft_a.as_entire_binding())),
    );
    let finalize_a = render_device.create_bind_group(
        None,
        &pipelines._finalize_layout,
        &BindGroupEntries::sequential((
            fft_a.as_entire_binding(),
            raw.as_entire_binding(),
            minmax.as_entire_binding(),
        )),
    );
    let finalize_b = render_device.create_bind_group(
        None,
        &pipelines._finalize_layout,
        &BindGroupEntries::sequential((
            fft_b.as_entire_binding(),
            raw.as_entire_binding(),
            minmax.as_entire_binding(),
        )),
    );
    let normalize = render_device.create_bind_group(
        None,
        &pipelines._normalize_layout,
        &BindGroupEntries::sequential((
            raw.as_entire_binding(),
            minmax.as_entire_binding(),
            &height_image.texture_view,
        )),
    );

    Some(IntegrationPrepared {
        doc_size,
        padded,
        bits_x,
        bits_y,
        repeat: data.repeat,
        prep,
        fft: [fft_ab, fft_ba],
        finalize: [finalize_a, finalize_b],
        normalize,
        minmax,
        _fft_a: fft_a,
        _fft_b: fft_b,
        _raw: raw,
    })
}

#[allow(clippy::too_many_arguments)]
fn prepare_gpu(
    scene: Res<GpuScene>,
    outputs: Option<Res<GpuOutputs>>,
    upload_stats: Option<Res<GpuUploadStats>>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    pipeline: Res<GpuPipeline>,
    integration_pipelines: Res<GpuIntegrationPipelines>,
    mut prepared: ResMut<GpuPrepared>,
) {
    let Some(outputs) = outputs else {
        return;
    };
    let Some(data) = &scene.data else {
        if prepared.generation != scene.generation {
            prepared.generation = scene.generation;
            prepared.bind_group = None;
            prepared.buffers = None;
            prepared.integration = None;
        }
        return;
    };
    if prepared.generation == scene.generation && prepared.bind_group.is_some() {
        return;
    }

    let Some(normal_image) = gpu_images.get(&outputs.normal) else {
        return;
    };
    let Some(height_image) = gpu_images.get(&outputs.height) else {
        return;
    };
    let _span = bevy::log::trace_span!("gpu.upload").entered();
    let upload_started = Instant::now();

    let header = GpuHeader {
        width: data.width,
        height: data.height,
        layer_count: data.layers.len() as u32,
        vertex_count: data.vertices.len() as u32,
        height_scale: data.height_scale,
        _pad: [0; 3],
    };
    let header_bytes = bytemuck::bytes_of(&header);
    let header_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_gpu_header"),
        size: header_bytes.len() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    render_queue.write_buffer(&header_buffer, 0, header_bytes);

    let dummy_layer = GpuLayer::default();
    let layer_slice: &[GpuLayer] = if data.layers.is_empty() {
        std::slice::from_ref(&dummy_layer)
    } else {
        &data.layers
    };
    let layer_bytes = bytemuck::cast_slice(layer_slice);
    let layer_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_gpu_layers"),
        size: layer_bytes.len() as u64,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    render_queue.write_buffer(&layer_buffer, 0, layer_bytes);

    let dummy_vertex = [0.0f32, 0.0f32];
    let vertex_slice: &[[f32; 2]] = if data.vertices.is_empty() {
        std::slice::from_ref(&dummy_vertex)
    } else {
        &data.vertices
    };
    let vertex_bytes = bytemuck::cast_slice(vertex_slice);
    let vertex_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_gpu_vertices"),
        size: vertex_bytes.len() as u64,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    render_queue.write_buffer(&vertex_buffer, 0, vertex_bytes);

    let dummy_mask = GpuMask {
        kind: 0,
        opacity: 0.0,
        ..Default::default()
    };
    let mask_slice: &[GpuMask] = if data.masks.is_empty() {
        std::slice::from_ref(&dummy_mask)
    } else {
        &data.masks
    };
    let mask_bytes = bytemuck::cast_slice(mask_slice);
    let mask_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_gpu_masks"),
        size: mask_bytes.len() as u64,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    render_queue.write_buffer(&mask_buffer, 0, mask_bytes);

    let dummy_contour = GpuContour::default();
    let contour_slice: &[GpuContour] = if data.contours.is_empty() {
        std::slice::from_ref(&dummy_contour)
    } else {
        &data.contours
    };
    let contour_bytes = bytemuck::cast_slice(contour_slice);
    let contour_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_gpu_contours"),
        size: contour_bytes.len() as u64,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    render_queue.write_buffer(&contour_buffer, 0, contour_bytes);

    // Precomputed layer and mask pixels share one storage buffer each; each
    // textured layer's `vertex_offset` is its pixel offset into the layer
    // buffer, and each painted mask's `vertex_offset` into the mask buffer.
    let layer_pixel_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_gpu_layer_pixels"),
        size: u64::from(data.layer_pixel_total.max(1)) * 4,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut pixel_offset = 0u64;
    for pixels in &data.layer_pixels {
        if !pixels.is_empty() {
            render_queue.write_buffer(
                &layer_pixel_buffer,
                pixel_offset,
                bytemuck::cast_slice(pixels.as_slice()),
            );
            pixel_offset += (pixels.len() * 4) as u64;
        }
    }
    let mask_pixel_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("ns_gpu_mask_pixels"),
        size: u64::from(data.mask_pixel_total.max(1)) * 4,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut mask_offset = 0u64;
    for pixels in &data.mask_pixels {
        if !pixels.is_empty() {
            render_queue.write_buffer(
                &mask_pixel_buffer,
                mask_offset,
                bytemuck::cast_slice(pixels.as_slice()),
            );
            mask_offset += (pixels.len() * 4) as u64;
        }
    }

    let bind_group = render_device.create_bind_group(
        None,
        &pipeline.layout,
        &BindGroupEntries::sequential((
            header_buffer.as_entire_binding(),
            layer_buffer.as_entire_binding(),
            vertex_buffer.as_entire_binding(),
            &normal_image.texture_view,
            &height_image.texture_view,
            mask_buffer.as_entire_binding(),
            contour_buffer.as_entire_binding(),
            layer_pixel_buffer.as_entire_binding(),
            mask_pixel_buffer.as_entire_binding(),
        )),
    );

    prepared.bind_group = Some(bind_group);
    prepared.buffers = Some((
        header_buffer,
        layer_buffer,
        vertex_buffer,
        mask_buffer,
        contour_buffer,
        layer_pixel_buffer,
        mask_pixel_buffer,
    ));
    let existing = prepared.integration.take();
    prepared.integration = if data.integrate_height {
        prepare_integration(
            data,
            normal_image,
            height_image,
            &render_device,
            &render_queue,
            &integration_pipelines,
            existing,
        )
    } else {
        None
    };
    prepared.generation = scene.generation;
    prepared.workgroups = (
        data.width.div_ceil(8).max(1),
        data.height.div_ceil(8).max(1),
    );
    if let Some(stats) = upload_stats {
        if let Ok(mut last) = stats.0.lock() {
            *last = upload_started.elapsed().as_secs_f32() * 1000.0;
        }
    }
}

fn dispatch_gpu_compute(
    pipeline: Res<GpuPipeline>,
    integration_pipelines: Res<GpuIntegrationPipelines>,
    pipeline_cache: Res<PipelineCache>,
    mut prepared: ResMut<GpuPrepared>,
    mut render_context: RenderContext,
) {
    if prepared.generation == 0
        || prepared.generation == prepared.dispatched_generation
        || prepared.bind_group.is_none()
    {
        return;
    }
    let Some(bind_group) = &prepared.bind_group else {
        return;
    };
    let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline.pipeline) else {
        return;
    };
    let _span = bevy::log::trace_span!("gpu.dispatch").entered();

    // If integration is enabled, wait for all of its pipelines before
    // dispatching anything. Leaving the generation unmarked retries next frame.
    if prepared.integration.is_some()
        && (pipeline_cache
            .get_compute_pipeline(integration_pipelines.prep)
            .is_none()
            || pipeline_cache
                .get_compute_pipeline(integration_pipelines.fft)
                .is_none()
            || pipeline_cache
                .get_compute_pipeline(integration_pipelines.bitrev)
                .is_none()
            || pipeline_cache
                .get_compute_pipeline(integration_pipelines.spectrum)
                .is_none()
            || pipeline_cache
                .get_compute_pipeline(integration_pipelines.finalize)
                .is_none()
            || pipeline_cache
                .get_compute_pipeline(integration_pipelines.normalize)
                .is_none())
    {
        return;
    }

    let diagnostics = render_context.diagnostic_recorder();
    {
        let mut pass = render_context
            .command_encoder()
            .begin_compute_pass(&ComputePassDescriptor::default());
        let span = diagnostics
            .as_ref()
            .map(|recorder| recorder.pass_span(&mut pass, "ns_gpu_eval"));
        pass.set_pipeline(compute_pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.dispatch_workgroups(prepared.workgroups.0, prepared.workgroups.1, 1);
        if let Some(span) = span {
            span.end(&mut pass);
        }
    }

    if let Some(integration) = &prepared.integration {
        let Some(prep_pipeline) = pipeline_cache.get_compute_pipeline(integration_pipelines.prep)
        else {
            return;
        };
        let Some(fft_pipeline) = pipeline_cache.get_compute_pipeline(integration_pipelines.fft)
        else {
            return;
        };
        let Some(bitrev_pipeline) =
            pipeline_cache.get_compute_pipeline(integration_pipelines.bitrev)
        else {
            return;
        };
        let Some(spectrum_pipeline) =
            pipeline_cache.get_compute_pipeline(integration_pipelines.spectrum)
        else {
            return;
        };
        let Some(finalize_pipeline) =
            pipeline_cache.get_compute_pipeline(integration_pipelines.finalize)
        else {
            return;
        };
        let Some(normalize_pipeline) =
            pipeline_cache.get_compute_pipeline(integration_pipelines.normalize)
        else {
            return;
        };

        let (doc_w, doc_h) = integration.doc_size;
        let (pad_w, pad_h) = integration.padded;
        let padded_total = pad_w * pad_h;
        let pairs = padded_total / 2;
        let pairs_groups = pairs.div_ceil(256).max(1);
        let padded_groups = padded_total.div_ceil(256).max(1);
        let doc_total = doc_w * doc_h;
        let doc_groups = doc_total.div_ceil(256).max(1);

        let mut pass = render_context
            .command_encoder()
            .begin_compute_pass(&ComputePassDescriptor::default());
        let span = diagnostics
            .as_ref()
            .map(|recorder| recorder.pass_span(&mut pass, "ns_gpu_height"));

        pass.set_pipeline(prep_pipeline);
        pass.set_bind_group(0, &integration.prep, &[]);
        let params = PrepParams {
            src_w: doc_w,
            src_h: doc_h,
            pad_w,
            pad_h,
            repeat: u32::from(integration.repeat),
            bits_x: integration.bits_x,
            bits_y: integration.bits_y,
            _pad: 0,
        };
        pass.set_immediates(0, bytemuck::bytes_of(&params));
        pass.dispatch_workgroups(padded_groups, 1, 1);

        let mut current = 0usize;
        for stage in 0..integration.bits_x {
            pass.set_pipeline(fft_pipeline);
            pass.set_bind_group(0, &integration.fft[current], &[]);
            let params = FftParams {
                size_x: pad_w,
                size_y: pad_h,
                stage,
                inverse: 0,
                axis: 0,
                bits: integration.bits_y,
                _pad0: 0,
                _pad1: 0,
            };
            pass.set_immediates(0, bytemuck::bytes_of(&params));
            pass.dispatch_workgroups(pairs_groups, 1, 1);
            current ^= 1;
        }

        pass.set_pipeline(bitrev_pipeline);
        pass.set_bind_group(0, &integration.fft[current], &[]);
        let params = FftParams {
            size_x: pad_w,
            size_y: pad_h,
            stage: 0,
            inverse: 0,
            axis: 1,
            bits: integration.bits_y,
            _pad0: 0,
            _pad1: 0,
        };
        pass.set_immediates(0, bytemuck::bytes_of(&params));
        pass.dispatch_workgroups(padded_groups, 1, 1);
        current ^= 1;

        for stage in 0..integration.bits_y {
            pass.set_pipeline(fft_pipeline);
            pass.set_bind_group(0, &integration.fft[current], &[]);
            let params = FftParams {
                size_x: pad_w,
                size_y: pad_h,
                stage,
                inverse: 0,
                axis: 1,
                bits: integration.bits_y,
                _pad0: 0,
                _pad1: 0,
            };
            pass.set_immediates(0, bytemuck::bytes_of(&params));
            pass.dispatch_workgroups(pairs_groups, 1, 1);
            current ^= 1;
        }

        pass.set_pipeline(spectrum_pipeline);
        pass.set_bind_group(0, &integration.fft[current], &[]);
        let params = SpectrumParams {
            size_x: pad_w,
            size_y: pad_h,
            bits_x: integration.bits_x,
            bits_y: integration.bits_y,
            high_pass: 0.0,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        pass.set_immediates(0, bytemuck::bytes_of(&params));
        pass.dispatch_workgroups(padded_groups, 1, 1);
        current ^= 1;

        for stage in 0..integration.bits_x {
            pass.set_pipeline(fft_pipeline);
            pass.set_bind_group(0, &integration.fft[current], &[]);
            let params = FftParams {
                size_x: pad_w,
                size_y: pad_h,
                stage,
                inverse: 1,
                axis: 0,
                bits: integration.bits_y,
                _pad0: 0,
                _pad1: 0,
            };
            pass.set_immediates(0, bytemuck::bytes_of(&params));
            pass.dispatch_workgroups(pairs_groups, 1, 1);
            current ^= 1;
        }

        for stage in 0..integration.bits_y {
            pass.set_pipeline(fft_pipeline);
            pass.set_bind_group(0, &integration.fft[current], &[]);
            let params = FftParams {
                size_x: pad_w,
                size_y: pad_h,
                stage,
                inverse: 1,
                axis: 1,
                bits: integration.bits_y,
                _pad0: 0,
                _pad1: 0,
            };
            pass.set_immediates(0, bytemuck::bytes_of(&params));
            pass.dispatch_workgroups(pairs_groups, 1, 1);
            current ^= 1;
        }

        pass.set_pipeline(finalize_pipeline);
        pass.set_bind_group(0, &integration.finalize[current], &[]);
        let params = FinalizeParams {
            width: doc_w,
            height: doc_h,
            stride: pad_w,
            scale: 1.0 / (padded_total as f32),
        };
        pass.set_immediates(0, bytemuck::bytes_of(&params));
        pass.dispatch_workgroups(doc_groups, 1, 1);

        pass.set_pipeline(normalize_pipeline);
        pass.set_bind_group(0, &integration.normalize, &[]);
        let params = NormalizeParams {
            width: doc_w,
            height: doc_h,
            stride: doc_w,
            _pad: 0,
        };
        pass.set_immediates(0, bytemuck::bytes_of(&params));
        pass.dispatch_workgroups(doc_groups, 1, 1);
        if let Some(span) = span {
            span.end(&mut pass);
        }
    }

    prepared.dispatched_generation = prepared.generation;
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{UVec2, Vec2, Vec3};
    use ns_core::{
        demo_document, Grid, Layer, LayerKind, Primitive, ShapeSource, ShapeStyle, VectorOp,
    };

    #[test]
    fn gate_reject_metadata_is_complete() {
        let mut labels = HashSet::new();
        for reason in GateReject::ALL {
            assert!(!reason.label().is_empty(), "missing label for {reason:?}");
            assert!(!reason.hint().is_empty(), "missing hint for {reason:?}");
            assert!(
                labels.insert(reason.label()),
                "duplicate label: {}",
                reason.label()
            );
        }
    }

    #[test]
    fn layouts_match_the_shader() {
        assert_eq!(std::mem::size_of::<GpuLayer>(), 88);
        assert_eq!(std::mem::size_of::<GpuMask>(), 48);
        assert_eq!(std::mem::size_of::<GpuContour>(), 24);
        assert_eq!(std::mem::size_of::<GpuHeader>(), 32);
        assert_eq!(std::mem::size_of::<PrepParams>(), 32);
        assert_eq!(std::mem::size_of::<FftParams>(), 32);
        assert_eq!(std::mem::size_of::<SpectrumParams>(), 32);
        assert_eq!(std::mem::size_of::<FinalizeParams>(), 16);
        assert_eq!(std::mem::size_of::<NormalizeParams>(), 16);
    }

    #[test]
    fn height_integration_pads_and_flags_the_scene() {
        let doc = demo_document(128);
        let scene = build_scene_with_options(
            &doc,
            GpuSceneOptions {
                integrate_height: true,
            },
        )
        .expect("power-of-two documents should use the GPU integrator");
        assert!(scene.integrate_height);
        assert!(!scene.repeat);
        assert_eq!(fft_padded_size(128, 128, Wrap::Clamp), (256, 256));
        assert!(height_integration_supported(128, 128, Wrap::Clamp));
    }

    #[test]
    fn non_power_of_two_integration_falls_back_to_cpu() {
        let doc = Document::new(UVec2::splat(192));
        assert!(build_scene_with_options(
            &doc,
            GpuSceneOptions {
                integrate_height: true,
            },
        )
        .is_none());
        assert!(!height_integration_supported(192, 192, Wrap::Clamp));
    }

    #[test]
    fn demo_scene_is_gpu_compatible() {
        let doc = demo_document(256);
        let scene = build_scene(&doc).expect("demo should use the GPU path");
        assert_eq!(scene.width, 256);
        assert_eq!(scene.height, 256);
        assert_eq!(scene.layers.len(), 4);
        // Rect and ellipse are analytic; only the five-point star contributes
        // polygon vertices (10 points, no duplicate closing vertex).
        assert_eq!(scene.vertices.len(), 10);
        assert_eq!(
            scene
                .layers
                .iter()
                .filter(|layer| matches!(layer.kind, 1 | 3 | 4))
                .count(),
            3
        );
    }

    #[test]
    fn gpu_scene_skips_layers_for_other_udim_tiles() {
        let mut doc = demo_document(128);
        doc.layers[1].tile = Some(1002);
        doc.layers[2].tile = Some(1002);

        doc.active_tile = 1001;
        let first = build_scene(&doc).expect("the remaining shapes are GPU compatible");
        assert_eq!(first.layers.len(), 2);

        doc.active_tile = 1002;
        let second = build_scene(&doc).expect("the other tile is GPU compatible");
        assert_eq!(second.layers.len(), 4);
    }

    #[test]
    fn off_tile_rejections_do_not_force_the_cpu_path() {
        let mut doc = demo_document(64);
        doc.layers[1].blend = BlendMode::BooleanUnion;
        doc.layers[1].tile = Some(1002);
        doc.active_tile = 1001;
        assert!(diagnose_gate(&doc, GpuSceneOptions::default()).is_empty());
        assert!(build_scene(&doc).is_some());
    }

    #[test]
    fn hundred_layer_stress_scene_uses_the_gpu() {
        let doc = ns_core::stress_document(256, 100);
        let scene = build_scene(&doc).expect("100-shape stress scene should use the GPU path");
        assert_eq!(scene.layers.len(), 101);
        assert!(scene.vertices.len() >= 330);
    }

    #[test]
    fn gpu_scene_honors_the_layer_cap() {
        // The stress builder adds one flat base layer, so 255 shapes exactly
        // fill the 256-layer cap and one more forces the CPU fallback.
        assert!(build_scene(&ns_core::stress_document(64, 255)).is_some());
        assert!(build_scene(&ns_core::stress_document(64, 256)).is_none());
    }

    #[test]
    fn gate_reports_every_reason_not_just_the_first() {
        let mut doc = demo_document(64);
        let children = doc.layers.clone();
        let mut group = Layer::group("group", children);
        group.opacity = 0.5;
        group.vector_op = Some(ns_core::VectorOp::Add);
        group.mask = Some(Grid::new(64, 64, 1.0));
        doc.layers = vec![group];

        let reasons = diagnose_gate(&doc, GpuSceneOptions::default());
        assert!(reasons.contains(&GateReject::VectorCompound));
        assert!(reasons.contains(&GateReject::NonIdentityGroup));
        assert!(reasons.contains(&GateReject::GroupPaintedMask));

        let Err(reported) = build_scene_reported(
            &doc,
            GpuSceneOptions::default(),
            &mut GpuSceneCache::default(),
        ) else {
            panic!("the compound/group stack must be rejected");
        };
        assert_eq!(reported, reasons);
    }

    #[test]
    fn gate_reports_surface_booleans_and_unsupported_masks() {
        let mut doc = demo_document(64);
        let mut children = doc.layers.clone();
        children[1].blend = BlendMode::BooleanUnion;
        doc.layers = vec![Layer::group("group", children)];
        assert!(
            diagnose_gate(&doc, GpuSceneOptions::default()).contains(&GateReject::SurfaceBoolean)
        );

        // A flat layer cannot be packed as a clipping source.
        let mut doc = demo_document(64);
        let mask_id = doc.layers[0].id;
        doc.layers[1].mask_layer = Some(mask_id);
        let reasons = diagnose_gate(&doc, GpuSceneOptions::default());
        assert!(reasons.contains(&GateReject::ClipOnUnsupportedKind));
        assert!(build_scene(&doc).is_none());
    }

    #[test]
    fn gate_reports_smart_masks_and_unsupported_fft_sizes() {
        let mut doc = demo_document(64);
        doc.layers[1]
            .mask_settings
            .generators
            .push(ns_core::MaskGenerator::new(ns_core::GeneratorKind::slope()));
        assert!(diagnose_gate(&doc, GpuSceneOptions::default()).contains(&GateReject::SmartMask));
        assert!(build_scene(&doc).is_none());

        let doc = Document::new(UVec2::splat(192));
        let reasons = diagnose_gate(
            &doc,
            GpuSceneOptions {
                integrate_height: true,
            },
        );
        assert_eq!(reasons, vec![GateReject::FftSizeUnsupported]);
    }

    #[test]
    fn gate_reports_the_layer_cap_and_success_has_no_reasons() {
        let doc = ns_core::stress_document(64, 256);
        assert!(
            diagnose_gate(&doc, GpuSceneOptions::default()).contains(&GateReject::TooManyLayers)
        );

        let doc = demo_document(64);
        assert!(diagnose_gate(&doc, GpuSceneOptions::default()).is_empty());
        assert!(build_scene_reported(
            &doc,
            GpuSceneOptions::default(),
            &mut GpuSceneCache::default()
        )
        .is_ok());
    }

    #[test]
    fn softness_and_painted_masks_use_gpu_or_fall_back_correctly() {
        let mut doc = demo_document(128);
        let mut soft = Layer::shape(
            "soft",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: glam::Vec2::splat(64.0),
                radii: glam::Vec2::splat(20.0),
            }),
        );
        if let LayerKind::Shape(shape) = &mut soft.kind {
            shape.softness_px = 2.0;
        }
        doc.layers.push(soft);
        let scene = build_scene(&doc).expect("softness should use the GPU SDF path");
        assert!(
            scene.layers.iter().any(|layer| layer.softness > 0.0),
            "softness must be packed for the shader"
        );

        let mut doc = demo_document(128);
        let mut masked = Layer::shape(
            "masked",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: glam::Vec2::splat(64.0),
                radii: glam::Vec2::splat(20.0),
            }),
        );
        masked.mask = Some(Grid::new(128, 128, 1.0));
        doc.layers.push(masked);
        let scene = build_scene(&doc).expect("painted masks use the GPU mask buffer");
        assert!(
            scene.masks.iter().any(|mask| mask.kind == 5),
            "the painted mask is packed as coverage"
        );
        assert_eq!(scene.mask_pixel_total, 128 * 128);
    }

    #[test]
    fn stroke_shapes_pack_the_outline_band() {
        let mut doc = Document::new(UVec2::splat(64));
        let mut layer = Layer::shape(
            "ring",
            ShapeSource::Primitive(Primitive::Rect {
                center: glam::Vec2::splat(32.0),
                size: glam::Vec2::splat(32.0),
                corner_radius: 4.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 12.0;
        }
        doc.layers.push(layer);

        let scene = build_scene(&doc).expect("stroke shapes should use the GPU SDF path");
        let gpu = scene
            .layers
            .iter()
            .find(|layer| layer.kind == 3)
            .expect("rounded rect layer");
        assert_ne!(gpu.flags & 4, 0, "stroke flag must be packed");
        assert!((gpu.stroke_width - 12.0).abs() < 1e-6);
    }

    #[test]
    fn stroke_masks_pack_the_outline_band() {
        let mut doc = demo_document(128);
        let mask_id = doc.layers[1].id;
        if let LayerKind::Shape(shape) = &mut doc.layers[1].kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 10.0;
        }
        doc.layers[2].mask_layer = Some(mask_id);

        let scene = build_scene(&doc).expect("stroke clipping masks should use the GPU");
        assert_eq!(scene.masks.len(), 1);
        assert_ne!(
            scene.masks[0].flags & 1,
            0,
            "stroke mask flag must be packed"
        );
        assert!((scene.masks[0]._pad[0] - 10.0).abs() < 1e-6);
    }

    #[test]
    fn shape_clipping_masks_use_gpu_sdf() {
        let mut doc = demo_document(128);
        let mask_id = doc.layers[1].id;
        doc.layers[2].mask_layer = Some(mask_id);
        let scene = build_scene(&doc).expect("shape clipping masks should use the GPU");
        assert_eq!(scene.masks.len(), 1);
        assert_eq!(scene.masks[0].kind, 3);
        assert!(scene.layers.iter().any(|layer| layer.mask_index == 0));
    }

    #[test]
    fn footer_mask_shortcut_keeps_the_gpu_path() {
        let mut doc = demo_document(1024);
        let dial_id = doc.layers[2].id;
        doc.layers[3].mask_layer = Some(dial_id);
        let scene =
            build_scene(&doc).expect("masking the emblem by the dial should stay on the GPU");
        assert_eq!(scene.masks.len(), 1);
        assert_eq!(scene.layers.len(), 4);
        assert_eq!(scene.layers[3].mask_index, 0);
    }

    #[test]
    fn all_analytic_profiles_stay_on_the_gpu() {
        for (profile, id) in [
            (Profile::Round, 0u32),
            (Profile::Chamfer { degrees: 45.0 }, 1),
            (Profile::Linear, 2),
            (Profile::Convex, 3),
            (Profile::Concave, 4),
            (Profile::Smooth, 5),
        ] {
            let mut doc = demo_document(64);
            let LayerKind::Shape(shape) = &mut doc.layers[1].kind else {
                panic!("expected a shape")
            };
            shape.profile = profile.clone();
            let scene =
                build_scene(&doc).unwrap_or_else(|| panic!("{profile:?} should be GPU-renderable"));
            let gpu = scene
                .layers
                .iter()
                .find(|layer| layer.kind == 3)
                .expect("rounded rect layer");
            assert_eq!((gpu.flags >> 3) & 7, id, "profile id must be packed");
        }

        // Custom LUTs use a CPU-precomputed texture layer instead of the CPU
        // fallback; the shader samples the finished normals.
        let mut doc = demo_document(64);
        let LayerKind::Shape(shape) = &mut doc.layers[1].kind else {
            panic!("expected a shape")
        };
        shape.profile = Profile::Custom(vec![0.0, 0.5, 1.0]);
        let scene = build_scene(&doc).expect("custom LUTs use a textured layer");
        assert!(scene.layers.iter().any(|layer| layer.kind == 5));
    }

    #[test]
    fn compound_islands_stay_on_the_gpu() {
        use ns_core::path::{FillRule, Node, NodeKind, Path, SubPath};

        let square = |x: f32, y: f32| {
            [
                glam::Vec2::new(x, y),
                glam::Vec2::new(x + 24.0, y),
                glam::Vec2::new(x + 24.0, y + 24.0),
                glam::Vec2::new(x, y + 24.0),
            ]
        };
        let mut path = Path::new(FillRule::NonZero);
        for points in [square(8.0, 8.0), square(40.0, 8.0)] {
            path.subpaths.push(SubPath {
                nodes: points
                    .iter()
                    .map(|point| Node {
                        pos: *point,
                        handle_in: glam::Vec2::ZERO,
                        handle_out: glam::Vec2::ZERO,
                        kind: NodeKind::Corner,
                        bevel: None,
                    })
                    .collect(),
                closed: true,
            });
        }

        let mut doc = Document::new(UVec2::splat(64));
        doc.layers
            .push(Layer::shape("compound", ShapeSource::Path(path)));
        let scene = build_scene(&doc).expect("disjoint islands should use the GPU");
        assert_eq!(scene.contours.len(), 2);
        assert_eq!(scene.contours[0].aabb_min, [8.0, 8.0]);
        assert_eq!(scene.contours[0].aabb_max, [32.0, 32.0]);
        assert_eq!(scene.vertices.len(), 8);
        assert_eq!(scene.layers[0].kind, 1);
        assert_eq!(scene.layers[0].vertex_count, 2, "two contours");
    }

    #[test]
    fn smoothed_shapes_use_a_precomputed_texture() {
        let mut doc = demo_document(128);
        assert!(
            build_scene(&doc).is_some(),
            "the demo alone is GPU-renderable"
        );
        let LayerKind::Shape(shape) = &mut doc.layers[1].kind else {
            panic!("expected a shape")
        };
        shape.smooth_px = 2.0;
        let scene = build_scene(&doc).expect("smoothing uses a textured layer");
        assert!(scene.layers.iter().any(|layer| layer.kind == 5));
        assert_eq!(scene.layer_pixel_total, 128 * 128);
    }

    #[test]
    #[ignore = "prints the T0.1 corpus gate report"]
    fn perf_corpus_gate_report() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/scenes");
        let mut projects: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("bench/scenes exists; run ns_cli bench first")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "nsproj"))
            .collect();
        projects.sort();
        for path in projects {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("scene");
            let doc = ns_core::io::load_project_ron(&path).expect("load corpus project");
            let started = Instant::now();
            let mut cache = GpuSceneCache::default();
            let result = build_scene_reported(&doc, GpuSceneOptions::default(), &mut cache);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            match result {
                Ok(scene) => println!(
                    "{name}: GPU · {} layers · {} masks · {} verts · {ms:.1} ms",
                    scene.layers.len(),
                    scene.masks.len(),
                    scene.vertices.len()
                ),
                Err(reasons) => println!("{name}: CPU · {reasons:?} · {ms:.1} ms"),
            }
        }
    }

    #[test]
    fn the_saved_showcase_project_uses_the_gpu() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../showcase/scif_panel_baked.nsproj");
        if !path.is_file() {
            // The baked showcase is local-only (`.gitignore` excludes it). A
            // fresh CI checkout must not fail because the fixture is absent.
            eprintln!("{} is missing; skipping", path.display());
            return;
        }
        let doc = ns_core::io::load_project_ron(&path).expect("load project");
        let scene = build_scene(&doc).expect("the saved showcase project should use the GPU path");
        assert!(
            scene.layers.iter().any(|layer| layer.kind == 5),
            "baked rasters and smoothed outlines use precomputed textures"
        );
        assert!(
            scene.masks.iter().any(|mask| mask.kind == 5),
            "painted masks use coverage buffers"
        );
    }

    #[test]
    fn trimmed_single_loops_stay_analytic_and_holes_use_a_texture() {
        let mut doc = ns_core::Document::new(UVec2::splat(128));
        doc.layers.push(Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: glam::Vec2::splat(64.0),
                size: glam::Vec2::splat(80.0),
                corner_radius: 0.0,
            }),
        ));
        let strip = vec![vec![
            glam::Vec2::new(60.0, 0.0),
            glam::Vec2::new(68.0, 0.0),
            glam::Vec2::new(68.0, 128.0),
            glam::Vec2::new(60.0, 128.0),
        ]];
        doc.trim_layers_with_loops(&[vec![0]], &strip)
            .expect("panel trimmed");
        assert!(
            build_scene(&doc).is_some(),
            "single-loop pieces stay packable by the GPU"
        );

        // A loop in the middle leaves a hole. The shader cannot represent it
        // analytically, so the CPU precomputes this one layer as a texture.
        let mut holed = ns_core::Document::new(UVec2::splat(128));
        holed.layers.push(Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: glam::Vec2::splat(64.0),
                size: glam::Vec2::splat(80.0),
                corner_radius: 0.0,
            }),
        ));
        let hole = vec![vec![
            glam::Vec2::new(44.0, 44.0),
            glam::Vec2::new(84.0, 44.0),
            glam::Vec2::new(84.0, 84.0),
            glam::Vec2::new(44.0, 84.0),
        ]];
        holed
            .trim_layers_with_loops(&[vec![0]], &hole)
            .expect("hole cut");
        let scene = build_scene(&holed).expect("holes use a texture layer");
        assert!(scene.layers.iter().any(|layer| layer.kind == 5));
    }

    #[test]
    fn texture_and_paint_layers_use_precomputed_gpu_textures() {
        let mut doc = demo_document(64);
        doc.layers.push(Layer::photo_height(
            "photo",
            Grid::new(64, 64, 0.5),
            1.0,
            [0.0; 3],
            [0.0; 3],
        ));
        let scene = build_scene(&doc).expect("photo layers use a texture layer");
        assert!(scene.layers.iter().any(|layer| layer.kind == 5));

        let mut doc = demo_document(64);
        doc.layers
            .push(Layer::imported_normal("normal", Grid::new(64, 64, Vec3::Z)));
        let scene = build_scene(&doc).expect("imported normals use a texture layer");
        assert!(scene.layers.iter().any(|layer| layer.kind == 5));
    }

    #[test]
    fn textured_layers_store_the_cpu_normal_and_height() {
        let mut doc = Document::new(UVec2::splat(64));
        let mut layer = Layer::shape(
            "smooth",
            ShapeSource::Primitive(Primitive::Rect {
                center: glam::Vec2::splat(32.0),
                size: glam::Vec2::splat(40.0),
                corner_radius: 4.0,
            }),
        );
        let id = layer.id;
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.smooth_px = 2.0;
            shape.radius_px = 6.0;
            shape.height_px = 8.0;
        }
        doc.layers.push(layer);
        let (normal, height) = doc
            .evaluate_layer_contribution(id)
            .expect("layer contribution");
        let scene = build_scene(&doc).expect("smooth shape uses a texture layer");
        let textured = scene
            .layers
            .iter()
            .find(|layer| layer.kind == 5)
            .expect("textured layer");
        let pixels = &scene.layer_pixels[0];
        let base = textured.vertex_offset as usize;
        for (x, y) in [(10u32, 32u32), (20, 32), (32, 32)] {
            let packed = pixels[base + (y * 64 + x) as usize];
            let decoded = glam::Vec3::new(
                (packed & 255) as f32 / 127.5 - 1.0,
                ((packed >> 8) & 255) as f32 / 127.5 - 1.0,
                ((packed >> 16) & 255) as f32 / 127.5 - 1.0,
            );
            let cpu = *normal.get(x, y);
            assert!(
                (decoded - cpu).length() < 0.03,
                "pixel ({x},{y}): encoded {decoded:?} vs CPU {cpu:?}"
            );
            let encoded_height = ((packed >> 24) & 255) as f32 / 255.0;
            assert!(
                (encoded_height - height.get(x, y)).abs() < 1.0 / 128.0,
                "pixel ({x},{y}) height differs"
            );
        }
    }

    #[test]
    fn cut_booleans_fall_back_to_the_cpu() {
        let mut doc = Document::new(UVec2::splat(64));
        let mut group = Layer::group(
            "cut",
            vec![
                Layer::shape(
                    "plate",
                    ShapeSource::Primitive(Primitive::Rect {
                        center: Vec2::splat(32.0),
                        size: Vec2::splat(48.0),
                        corner_radius: 4.0,
                    }),
                ),
                Layer::shape(
                    "cutter",
                    ShapeSource::Primitive(Primitive::Rect {
                        center: Vec2::splat(32.0),
                        size: Vec2::splat(20.0),
                        corner_radius: 2.0,
                    }),
                ),
            ],
        );
        group.vector_op = Some(VectorOp::PartialCut);
        doc.layers.push(group);

        assert!(build_scene(&doc).is_none(), "cut groups stay on the CPU");
        assert!(
            diagnose_gate(&doc, GpuSceneOptions::default()).contains(&GateReject::VectorCompound)
        );
    }

    #[test]
    fn painted_masks_store_the_static_coverage() {
        let mut doc = demo_document(64);
        let mut layer = Layer::shape(
            "masked",
            ShapeSource::Primitive(Primitive::Rect {
                center: glam::Vec2::splat(32.0),
                size: glam::Vec2::splat(30.0),
                corner_radius: 0.0,
            }),
        );
        layer.mask = Some(Grid::from_fn(64, 64, |x, _| if x < 32 { 0.0 } else { 1.0 }));
        doc.layers.push(layer);

        let scene = build_scene(&doc).expect("painted mask uses a coverage buffer");
        let mask = scene
            .masks
            .iter()
            .find(|mask| mask.kind == 5)
            .expect("painted mask");
        let pixels = &scene.mask_pixels[0];
        let base = mask.vertex_offset as usize;
        assert_eq!(pixels[base + 10], 0, "left half is masked out");
        assert_eq!(pixels[base + 40], 255, "right half is revealed");
    }

    #[test]
    fn empty_and_fill_scenes_are_supported() {
        let mut doc = Document::new(UVec2::splat(64));
        let empty = build_scene(&doc).unwrap();
        assert!(empty.layers.is_empty());

        doc.layers.push(Layer::fill("fill", 0.5));
        let scene = build_scene(&doc).unwrap();
        assert_eq!(scene.layers.len(), 1);
        assert_eq!(scene.layers[0].kind, 2);
        assert!((scene.layers[0].height - 0.5).abs() < 1e-6);
    }

    #[test]
    fn simple_groups_flatten_on_the_gpu() {
        let mut doc = demo_document(128);
        let children = doc.layers.clone();
        doc.layers = vec![Layer::group("group", children)];
        let scene = build_scene(&doc).expect("identity groups should flatten");
        assert_eq!(scene.layers.len(), 4);
        assert_eq!(scene.vertices.len(), 10);
    }

    #[test]
    fn the_cable_tool_stays_on_the_gpu_path() {
        let meta = ns_core::CableMeta::new(
            vec![Vec2::new(20.0, 40.0), Vec2::new(120.0, 80.0)],
            26.0,
            12.0,
            ns_core::CableCap::Hex,
            1.5,
        );
        let layer = ns_core::cable_layer(&meta).expect("cable");
        let mut doc = Document::new(UVec2::splat(256));
        doc.layers.push(layer);
        let scene = build_scene(&doc).expect("the baked cable should use the GPU");
        assert_eq!(scene.layers.len(), 3, "body and both terminals");
    }

    #[test]
    fn complex_groups_and_boolean_blends_fall_back_to_cpu() {
        let mut doc = demo_document(64);
        let children = doc.layers.clone();
        let mut group = Layer::group("group", children);
        group.opacity = 0.5;
        doc.layers = vec![group];
        assert!(
            build_scene(&doc).is_none(),
            "group opacity needs the tree path"
        );

        let mut doc = demo_document(64);
        let mut children = doc.layers.clone();
        children[1].blend = BlendMode::BooleanUnion;
        doc.layers = vec![Layer::group("group", children)];
        assert!(
            build_scene(&doc).is_none(),
            "boolean normal blending needs the tree path"
        );

        let mut doc = demo_document(64);
        let mask_id = doc.layers[0].id;
        doc.layers[1].mask_layer = Some(mask_id);
        assert!(
            build_scene(&doc).is_none(),
            "a flat mask source needs the tree path"
        );
    }

    /// A pen path: `closed` false is what Enter and double-click produce.
    fn pen_path(points: &[glam::Vec2], closed: bool) -> Layer {
        use ns_core::path::{Node, Path, SubPath};
        let mut path = Path::new(ns_core::path::FillRule::NonZero);
        path.subpaths.push(SubPath {
            nodes: points.iter().map(|point| Node::corner(*point)).collect(),
            closed,
        });
        Layer::shape("pen", ShapeSource::Path(path))
    }

    #[test]
    fn open_pen_cut_lines_stay_on_the_gpu() {
        let mut cut = pen_path(
            &[
                glam::Vec2::new(8.0, 32.0),
                glam::Vec2::new(56.0, 40.0),
                glam::Vec2::new(40.0, 56.0),
            ],
            false,
        );
        if let LayerKind::Shape(shape) = &mut cut.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 6.0;
        }
        let mut doc = Document::new(UVec2::splat(64));
        doc.layers.push(cut);

        let scene = build_scene(&doc).expect("an open stroked pen line should use the GPU");
        assert_eq!(scene.layers.len(), 1);
        assert!(!scene.vertices.is_empty(), "the stroke band was packed");
        // The band is filled geometry, not a shader-side stroke around a chord.
        assert_eq!(scene.layers[0].flags & 4, 0);
        assert_eq!(scene.layers[0].stroke_width, 0.0);
    }

    #[test]
    fn open_filled_paths_pack_as_implicitly_closed_loops() {
        let mut doc = Document::new(UVec2::splat(64));
        doc.layers.push(pen_path(
            &[
                glam::Vec2::new(12.0, 12.0),
                glam::Vec2::new(52.0, 16.0),
                glam::Vec2::new(30.0, 50.0),
            ],
            false,
        ));
        let scene = build_scene(&doc).expect("an open filled path is implicitly closed");
        assert_eq!(scene.vertices.len(), 3);
        assert_eq!(scene.contours.len(), 1);
    }

    #[test]
    fn many_pen_layers_and_cut_lines_stay_on_the_gpu() {
        let mut doc = Document::new(UVec2::splat(256));
        for index in 0..24 {
            let x = 8.0 + (index % 6) as f32 * 40.0;
            let y = 8.0 + (index / 6) as f32 * 40.0;
            doc.layers.push(pen_path(
                &[
                    glam::Vec2::new(x, y),
                    glam::Vec2::new(x + 28.0, y + 4.0),
                    glam::Vec2::new(x + 24.0, y + 30.0),
                ],
                true,
            ));
        }
        for index in 0..4 {
            let y = 20.0 + index as f32 * 50.0;
            let mut cut = pen_path(
                &[
                    glam::Vec2::new(10.0, y),
                    glam::Vec2::new(120.0, y + 12.0),
                    glam::Vec2::new(240.0, y),
                ],
                false,
            );
            if let LayerKind::Shape(shape) = &mut cut.kind {
                shape.style = ShapeStyle::Stroke;
                shape.stroke_width_px = 4.0;
            }
            doc.layers.push(cut);
        }

        let scene = build_scene(&doc).expect("a pen stack with cut lines should stay on the GPU");
        assert_eq!(scene.layers.len(), 28);
        assert!(scene.vertices.len() > 24 * 3);
    }

    #[test]
    fn multi_loop_even_odd_paths_use_a_texture_layer() {
        use ns_core::path::{FillRule, Node, Path, SubPath};
        let square = |x: f32, y: f32| {
            [
                glam::Vec2::new(x, y),
                glam::Vec2::new(x + 24.0, y),
                glam::Vec2::new(x + 24.0, y + 24.0),
                glam::Vec2::new(x, y + 24.0),
            ]
        };
        let mut path = Path::new(FillRule::EvenOdd);
        for points in [square(8.0, 8.0), square(16.0, 16.0)] {
            path.subpaths.push(SubPath {
                nodes: points.iter().map(|point| Node::corner(*point)).collect(),
                closed: true,
            });
        }
        let mut doc = Document::new(UVec2::splat(64));
        doc.layers
            .push(Layer::shape("xor", ShapeSource::Path(path)));
        let scene = build_scene(&doc).expect("nested loops use a texture layer");
        assert!(scene.layers.iter().any(|layer| layer.kind == 5));
    }
}
