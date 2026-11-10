//! Map catalogue, sizing and pixel encoding for the Blender bridge.
//!
//! The bridge speaks in map *ids* (`"normal"`, `"pbr_roughness"`, …) rather
//! than in stages, because that is what a Blender user recognises. Everything
//! that turns an id into bytes lives here so `bridge.rs` only deals with
//! sockets, sessions and pacing.
//!
//! Two families share one namespace:
//!
//! * `Document` maps come from the layer stack the editor is painting.
//! * `Pbr` maps come from the single-image PBR pipeline's stage cache.
//!
//! Payloads are raw RGBA in little-endian 8- or 16-bit words, which is the
//! exact layout Blender's `foreach_set` wants after one conversion. Nothing is
//! compressed: loopback moves a 2048² frame in a couple of milliseconds while
//! a compressor would spend ten times that on the same data, and the addon
//! would pay for the inverse.

use std::sync::Arc;

use glam::{UVec2, Vec3};
use ns_core::convention::{encode_normal, GreenChannel};
use ns_core::derived::{self, EdgeDetectParams, EdgeWearParams};
use ns_core::{Document, EvalCache, Grid, Wrap};
use ns_pbr::{PbrImage, Stage, StageOutput};

use crate::app::SharedDocument;

/// Hard ceiling on either side of a served frame, mirrored by the addon.
pub const MAX_SIDE: u32 = 4096;
/// The bridge's own evaluation budget. Larger stacks stream instead of
/// caching, so a preview can never double the editor's 256 MiB cache.
pub const CACHE_BUDGET: u64 = 64 * 1024 * 1024;

/// Which map a request refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MapId {
    // Document (layer stack).
    Normal,
    Height,
    Curvature,
    EdgeWear,
    EdgeDetect,
    IdMap,
    // Single-image PBR pipeline.
    PbrNormal,
    PbrHeight,
    PbrAo,
    PbrCavity,
    PbrCurvature,
    PbrEdgeWear,
    PbrRoughness,
    PbrMetallic,
    PbrDisplacement,
    PbrBaseColor,
    PbrOrm,
}

impl MapId {
    pub const ALL: [MapId; 17] = [
        MapId::Normal,
        MapId::Height,
        MapId::Curvature,
        MapId::EdgeWear,
        MapId::EdgeDetect,
        MapId::IdMap,
        MapId::PbrNormal,
        MapId::PbrHeight,
        MapId::PbrAo,
        MapId::PbrCavity,
        MapId::PbrCurvature,
        MapId::PbrEdgeWear,
        MapId::PbrRoughness,
        MapId::PbrMetallic,
        MapId::PbrDisplacement,
        MapId::PbrBaseColor,
        MapId::PbrOrm,
    ];

    /// Wire id. Stable: the addon stores selections by this string.
    pub fn id(self) -> &'static str {
        match self {
            MapId::Normal => "normal",
            MapId::Height => "height",
            MapId::Curvature => "curvature",
            MapId::EdgeWear => "edge_wear",
            MapId::EdgeDetect => "edge_detect",
            MapId::IdMap => "id_map",
            MapId::PbrNormal => "pbr_normal",
            MapId::PbrHeight => "pbr_height",
            MapId::PbrAo => "pbr_ao",
            MapId::PbrCavity => "pbr_cavity",
            MapId::PbrCurvature => "pbr_curvature",
            MapId::PbrEdgeWear => "pbr_edge_wear",
            MapId::PbrRoughness => "pbr_roughness",
            MapId::PbrMetallic => "pbr_metallic",
            MapId::PbrDisplacement => "pbr_displacement",
            MapId::PbrBaseColor => "pbr_base_color",
            MapId::PbrOrm => "pbr_orm",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MapId::Normal => "Normal",
            MapId::Height => "Height",
            MapId::Curvature => "Curvature",
            MapId::EdgeWear => "Edge Wear",
            MapId::EdgeDetect => "Edge Detect",
            MapId::IdMap => "ID Map",
            MapId::PbrNormal => "PBR Normal",
            MapId::PbrHeight => "PBR Height",
            MapId::PbrAo => "PBR AO",
            MapId::PbrCavity => "PBR Cavity",
            MapId::PbrCurvature => "PBR Curvature",
            MapId::PbrEdgeWear => "PBR Edge Wear",
            MapId::PbrRoughness => "PBR Roughness",
            MapId::PbrMetallic => "PBR Metallic",
            MapId::PbrDisplacement => "PBR Displacement",
            MapId::PbrBaseColor => "PBR Base Colour",
            MapId::PbrOrm => "PBR ORM (packed)",
        }
    }

    /// Group shown as a section header in the Blender panel.
    pub fn group(self) -> &'static str {
        if self.is_pbr() {
            "pbr"
        } else {
            "document"
        }
    }

    pub fn is_pbr(self) -> bool {
        matches!(
            self,
            MapId::PbrNormal
                | MapId::PbrHeight
                | MapId::PbrAo
                | MapId::PbrCavity
                | MapId::PbrCurvature
                | MapId::PbrEdgeWear
                | MapId::PbrRoughness
                | MapId::PbrMetallic
                | MapId::PbrDisplacement
                | MapId::PbrBaseColor
                | MapId::PbrOrm
        )
    }

    /// Colour space Blender must use for this map.
    pub fn space(self) -> &'static str {
        match self {
            MapId::PbrBaseColor => "srgb",
            _ => "linear",
        }
    }

    /// Maps whose precision matters (height fields), so the addon can default
    /// them to 16-bit.
    pub fn prefers_16(self) -> bool {
        matches!(
            self,
            MapId::Height | MapId::PbrHeight | MapId::PbrDisplacement
        )
    }

    pub fn stage(self) -> Option<Stage> {
        match self {
            MapId::PbrNormal | MapId::PbrHeight => Some(Stage::Geometry),
            MapId::PbrAo => Some(Stage::Ao),
            MapId::PbrCavity => Some(Stage::Cavity),
            MapId::PbrCurvature => Some(Stage::Curvature),
            MapId::PbrEdgeWear => Some(Stage::EdgeWear),
            MapId::PbrRoughness => Some(Stage::Roughness),
            MapId::PbrMetallic => Some(Stage::Metallic),
            MapId::PbrDisplacement => Some(Stage::Displacement),
            MapId::PbrBaseColor => Some(Stage::Delit),
            MapId::PbrOrm => Some(Stage::Roughness),
            _ => None,
        }
    }

    pub fn from_id(value: &str) -> Option<MapId> {
        let value = value.trim();
        MapId::ALL.into_iter().find(|map| map.id() == value)
    }
}

/// Sample width of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bits {
    Eight,
    Sixteen,
}

impl Bits {
    pub fn from_wire(value: u64) -> Bits {
        if value >= 16 {
            Bits::Sixteen
        } else {
            Bits::Eight
        }
    }

    pub fn format(self) -> &'static str {
        match self {
            Bits::Eight => "rgba8",
            Bits::Sixteen => "rgba16",
        }
    }
}

/// A finished image, ready to be framed and sent.
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub bits: Bits,
    pub payload: Vec<u8>,
}

/// Parameters the document's derived maps depend on. Snapshotted into the
/// background job so a slider drag mid-evaluation cannot produce a mixed frame.
#[derive(Clone, Copy, Debug)]
pub struct DocumentParams {
    pub wrap: Wrap,
    pub edge_wear: EdgeWearParams,
    pub edge_detect: EdgeDetectParams,
}

impl Default for DocumentParams {
    fn default() -> Self {
        Self {
            wrap: Wrap::Clamp,
            edge_wear: EdgeWearParams::default(),
            edge_detect: EdgeDetectParams::default(),
        }
    }
}

impl DocumentParams {
    /// Folded into the cache key so a parameter change invalidates a delivered
    /// frame even though the document itself did not change.
    pub fn hash(self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |value: f32| {
            let bits = value.to_bits() as u64;
            hash ^= bits;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for params in [
            self.edge_wear.threshold,
            self.edge_wear.softness,
            self.edge_wear.strength,
        ] {
            eat(params);
        }
        for params in [
            self.edge_detect.threshold,
            self.edge_detect.softness,
            self.edge_detect.strength,
        ] {
            eat(params);
        }
        hash
    }
}

/// What a background job encodes. Owned data only: the task runs on Bevy's
/// compute pool and must not borrow editor resources.
pub enum JobSource {
    Document {
        /// Shared with the main thread; cloned into a private snapshot on the
        /// worker, exactly like the editor's own background evaluation, so a
        /// UI edit never has to deep-clone the document.
        shared: SharedDocument,
        params: DocumentParams,
        /// Full-resolution source extent, used for the aspect ratio.
        size: UVec2,
    },
    Pbr {
        output: Arc<StageOutput>,
        size: UVec2,
    },
    /// Three scalar stages packed into one RGB texture (R=AO, G=rough, B=metal).
    Orm {
        ao: Option<Arc<StageOutput>>,
        roughness: Arc<StageOutput>,
        metallic: Arc<StageOutput>,
        size: UVec2,
    },
}

impl JobSource {
    pub fn size(&self) -> UVec2 {
        match self {
            JobSource::Document { size, .. }
            | JobSource::Pbr { size, .. }
            | JobSource::Orm { size, .. } => *size,
        }
    }
}

/// Encode a job's map at `width`×`height`.
///
/// `cache` is threaded through the document path so the bridge reuses the
/// editor's own evaluation cache layout; it comes back to the caller when the
/// job finishes.
pub fn render(
    map: MapId,
    source: &JobSource,
    width: u32,
    height: u32,
    bits: Bits,
    cache: &mut EvalCache,
) -> Option<Frame> {
    let width = width.max(1);
    let height = height.max(1);
    match source {
        JobSource::Document { shared, params, .. } => {
            let document: Document = (**shared).clone();
            render_document(map, &document, params, width, height, bits, cache)
        }
        JobSource::Pbr { output, .. } => render_pbr(map, output, width, height, bits),
        JobSource::Orm {
            ao,
            roughness,
            metallic,
            ..
        } => {
            let roughness = scalars(roughness)?;
            let metallic = scalars(metallic)?;
            let ao = ao.as_ref().and_then(|output| scalars(output));
            Some(Frame {
                w: width,
                h: height,
                bits,
                payload: encode_orm(ao, roughness, metallic, width, height, bits),
            })
        }
    }
}

fn render_document(
    map: MapId,
    document: &Document,
    params: &DocumentParams,
    width: u32,
    height: u32,
    bits: Bits,
    cache: &mut EvalCache,
) -> Option<Frame> {
    let payload = match map {
        MapId::Normal => {
            let normals = document.evaluate_auto(cache, CACHE_BUDGET);
            encode_normal_map(&normals.resized_normalized(width, height), bits)
        }
        MapId::Height => {
            let height_field = document.evaluate_height_auto(cache, CACHE_BUDGET);
            encode_scalar(&height_field.resized(width, height), bits)
        }
        // Second derivative: computed at native resolution, then resized. The
        // export path does the same and explains why: filtering an already
        // downscaled normal field chips every edge.
        MapId::Curvature | MapId::EdgeWear | MapId::EdgeDetect => {
            let normals = document.evaluate_auto(cache, CACHE_BUDGET);
            let curvature = derived::normal_curvature(&normals, params.wrap);
            match map {
                MapId::Curvature => encode_scalar(
                    &derived::encode_curvature(&curvature).resized(width, height),
                    bits,
                ),
                MapId::EdgeWear => encode_scalar(
                    &derived::edge_wear_with(&curvature, &params.edge_wear).resized(width, height),
                    bits,
                ),
                _ => encode_scalar(
                    &derived::edge_detect_with(&curvature, &params.edge_detect)
                        .resized(width, height),
                    bits,
                ),
            }
        }
        MapId::IdMap => {
            let ids = document.evaluate_id_map([0.0, 0.0, 0.0]);
            encode_rgb(&ids.resample_majority(width, height), bits)
        }
        _ => return None,
    };
    Some(Frame {
        w: width,
        h: height,
        bits,
        payload,
    })
}

fn render_pbr(
    map: MapId,
    output: &StageOutput,
    width: u32,
    height: u32,
    bits: Bits,
) -> Option<Frame> {
    let payload = match map {
        MapId::PbrNormal => {
            encode_normal_map(&output.normal()?.resized_normalized(width, height), bits)
        }
        MapId::PbrHeight => encode_scalar(&output.height()?.resized(width, height), bits),
        MapId::PbrDisplacement => {
            encode_scalar(&output.displacement()?.map.resized(width, height), bits)
        }
        MapId::PbrCurvature => encode_scalar(&output.curvature()?.map.resized(width, height), bits),
        MapId::PbrAo
        | MapId::PbrCavity
        | MapId::PbrEdgeWear
        | MapId::PbrRoughness
        | MapId::PbrMetallic => encode_scalar(&output.scalar()?.resized(width, height), bits),
        MapId::PbrBaseColor => encode_color(&output.delit_image()?.resized(width, height), bits),
        _ => return None,
    };
    Some(Frame {
        w: width,
        h: height,
        bits,
        payload,
    })
}

fn scalars(output: &StageOutput) -> Option<&Grid<f32>> {
    output.scalar()
}

/// Longest side capped at [`MAX_SIDE`], aspect preserved. `max_size == 0`
/// means "native", which is still capped so an 8K document cannot push a
/// 256 MiB frame through the socket.
pub fn preview_size(size: UVec2, max_size: u32) -> (u32, u32) {
    let longest = size.x.max(size.y).max(1);
    let limit = if max_size == 0 {
        MAX_SIDE
    } else {
        max_size.min(MAX_SIDE)
    };
    if limit >= longest {
        return (size.x.max(1), size.y.max(1));
    }
    let scale = limit as f32 / longest as f32;
    (
        ((size.x as f32 * scale).round() as u32).max(1),
        ((size.y as f32 * scale).round() as u32).max(1),
    )
}

fn to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn to_u16(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

fn encode_scalar(grid: &Grid<f32>, bits: Bits) -> Vec<u8> {
    let count = grid.data.len();
    match bits {
        Bits::Eight => {
            let mut out = vec![0u8; count * 4];
            for (index, value) in grid.data.iter().enumerate() {
                let byte = to_u8(*value);
                let base = index * 4;
                out[base] = byte;
                out[base + 1] = byte;
                out[base + 2] = byte;
                out[base + 3] = u8::MAX;
            }
            out
        }
        Bits::Sixteen => {
            let mut out = vec![0u8; count * 8];
            let full = u16::MAX.to_le_bytes();
            for (index, value) in grid.data.iter().enumerate() {
                let word = to_u16(*value).to_le_bytes();
                let base = index * 8;
                out[base..base + 2].copy_from_slice(&word);
                out[base + 2..base + 4].copy_from_slice(&word);
                out[base + 4..base + 6].copy_from_slice(&word);
                out[base + 6..base + 8].copy_from_slice(&full);
            }
            out
        }
    }
}

fn encode_normal_map(normals: &Grid<Vec3>, bits: Bits) -> Vec<u8> {
    let count = normals.data.len();
    match bits {
        Bits::Eight => {
            let mut out = vec![0u8; count * 4];
            for (index, normal) in normals.data.iter().enumerate() {
                let encoded = encode_normal(*normal, GreenChannel::OpenGl);
                let base = index * 4;
                out[base] = to_u8(encoded.x);
                out[base + 1] = to_u8(encoded.y);
                out[base + 2] = to_u8(encoded.z);
                out[base + 3] = u8::MAX;
            }
            out
        }
        Bits::Sixteen => {
            let mut out = vec![0u8; count * 8];
            let full = u16::MAX.to_le_bytes();
            for (index, normal) in normals.data.iter().enumerate() {
                let encoded = encode_normal(*normal, GreenChannel::OpenGl);
                let base = index * 8;
                for (channel, value) in [encoded.x, encoded.y, encoded.z].into_iter().enumerate() {
                    out[base + channel * 2..base + channel * 2 + 2]
                        .copy_from_slice(&to_u16(value).to_le_bytes());
                }
                out[base + 6..base + 8].copy_from_slice(&full);
            }
            out
        }
    }
}

fn encode_rgb(grid: &Grid<[f32; 3]>, bits: Bits) -> Vec<u8> {
    let count = grid.data.len();
    match bits {
        Bits::Eight => {
            let mut out = vec![0u8; count * 4];
            for (index, color) in grid.data.iter().enumerate() {
                let base = index * 4;
                out[base] = to_u8(color[0]);
                out[base + 1] = to_u8(color[1]);
                out[base + 2] = to_u8(color[2]);
                out[base + 3] = u8::MAX;
            }
            out
        }
        Bits::Sixteen => {
            let mut out = vec![0u8; count * 8];
            let full = u16::MAX.to_le_bytes();
            for (index, color) in grid.data.iter().enumerate() {
                let base = index * 8;
                for (channel, value) in color.iter().enumerate() {
                    out[base + channel * 2..base + channel * 2 + 2]
                        .copy_from_slice(&to_u16(*value).to_le_bytes());
                }
                out[base + 6..base + 8].copy_from_slice(&full);
            }
            out
        }
    }
}

/// Base colour is stored linear and must reach Blender as sRGB-encoded data,
/// exactly like `base_color.png` on disk.
fn encode_color(image: &PbrImage, bits: Bits) -> Vec<u8> {
    match bits {
        Bits::Eight => image.to_srgb_rgba8(),
        Bits::Sixteen => {
            let mut out = Vec::with_capacity(image.len() * 8);
            let full = u16::MAX.to_le_bytes();
            for color in &image.data {
                for channel in [color.x, color.y, color.z] {
                    out.extend_from_slice(&to_u16(srgb_encode(channel)).to_le_bytes());
                }
                out.extend_from_slice(&full);
            }
            out
        }
    }
}

/// R=AO, G=roughness, B=metallic — the glTF/ORM convention Blender's
/// Principled BSDF can be driven from with a single Separate Colour node.
fn encode_orm(
    ao: Option<&Grid<f32>>,
    roughness: &Grid<f32>,
    metallic: &Grid<f32>,
    width: u32,
    height: u32,
    bits: Bits,
) -> Vec<u8> {
    let ao = ao.map(|grid| grid.resized(width, height));
    let roughness = roughness.resized(width, height);
    let metallic = metallic.resized(width, height);
    let ao: Option<&Grid<f32>> = ao.as_ref();
    let count = (width as usize) * (height as usize);
    let sample = |grid: &Grid<f32>, x: u32, y: u32, fallback: f32| {
        if x < grid.w && y < grid.h {
            *grid.get(x, y)
        } else {
            fallback
        }
    };
    match bits {
        Bits::Eight => {
            let mut out = vec![0u8; count * 4];
            for y in 0..height {
                for x in 0..width {
                    let index = (y as usize * width as usize + x as usize) * 4;
                    out[index] = to_u8(ao.map_or(1.0, |grid| sample(grid, x, y, 1.0)));
                    out[index + 1] = to_u8(sample(&roughness, x, y, 0.5));
                    out[index + 2] = to_u8(sample(&metallic, x, y, 0.0));
                    out[index + 3] = u8::MAX;
                }
            }
            out
        }
        Bits::Sixteen => {
            let mut out = vec![0u8; count * 8];
            let full = u16::MAX.to_le_bytes();
            for y in 0..height {
                for x in 0..width {
                    let index = (y as usize * width as usize + x as usize) * 8;
                    let values = [
                        ao.map_or(1.0, |grid| sample(grid, x, y, 1.0)),
                        sample(&roughness, x, y, 0.5),
                        sample(&metallic, x, y, 0.0),
                    ];
                    for (channel, value) in values.into_iter().enumerate() {
                        out[index + channel * 2..index + channel * 2 + 2]
                            .copy_from_slice(&to_u16(value).to_le_bytes());
                    }
                    out[index + 6..index + 8].copy_from_slice(&full);
                }
            }
            out
        }
    }
}

/// sRGB transfer function, matching `ns_pbr::pixels::srgb_encode` without
/// pulling the module into this file's public surface.
fn srgb_encode(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_stay_unique() {
        let mut seen = std::collections::HashSet::new();
        for map in MapId::ALL {
            assert!(seen.insert(map.id()), "duplicate id {}", map.id());
            assert_eq!(MapId::from_id(map.id()), Some(map));
        }
        assert_eq!(MapId::from_id("nope"), None);
    }

    #[test]
    fn preview_size_keeps_ratio_and_respects_the_cap() {
        assert_eq!(preview_size(UVec2::splat(1024), 0), (1024, 1024));
        assert_eq!(preview_size(UVec2::splat(1024), 512), (512, 512));
        assert_eq!(preview_size(UVec2::new(2048, 1024), 512), (512, 256));
        assert_eq!(preview_size(UVec2::splat(8192), 0), (4096, 4096));
        assert_eq!(preview_size(UVec2::new(64, 32), 2048), (64, 32));
    }

    #[test]
    fn scalar_frames_are_rgba_and_16_bit_is_twice_as_wide() {
        let grid = Grid::from_fn(4, 2, |x, _| x as f32 / 3.0);
        let eight = encode_scalar(&grid, Bits::Eight);
        let sixteen = encode_scalar(&grid, Bits::Sixteen);
        assert_eq!(eight.len(), 4 * 2 * 4);
        assert_eq!(sixteen.len(), 4 * 2 * 8);
        assert_eq!(eight[3], 255);
        assert_eq!(&sixteen[6..8], &[0xff, 0xff]);
        // r, g and b carry the same sample.
        assert_eq!(eight[0], eight[1]);
        assert_eq!(eight[1], eight[2]);
    }

    #[test]
    fn orm_packs_ao_roughness_and_metallic_in_that_order() {
        let ao = Grid::new(2, 2, 1.0);
        let rough = Grid::new(2, 2, 0.5);
        let metal = Grid::new(2, 2, 0.0);
        let bytes = encode_orm(Some(&ao), &rough, &metal, 2, 2, Bits::Eight);
        assert_eq!(bytes.len(), 2 * 2 * 4);
        assert_eq!(bytes[0], 255);
        assert_eq!(bytes[1], 128);
        assert_eq!(bytes[2], 0);
        assert_eq!(bytes[3], 255);
    }

    #[test]
    fn missing_ao_defaults_to_white() {
        let rough = Grid::new(1, 1, 0.5);
        let metal = Grid::new(1, 1, 1.0);
        let bytes = encode_orm(None, &rough, &metal, 1, 1, Bits::Eight);
        assert_eq!(bytes[0], 255);
        assert_eq!(bytes[2], 255);
    }

    #[test]
    fn document_maps_render_at_the_requested_size() {
        let document = Document::new_with_base(UVec2::splat(16));
        let shared = SharedDocument::from(document.clone());
        let mut cache = EvalCache::default();
        let params = DocumentParams::default();
        for (map, expect) in [
            (MapId::Normal, "rgba8"),
            (MapId::Height, "rgba8"),
            (MapId::Curvature, "rgba8"),
            (MapId::EdgeWear, "rgba8"),
        ] {
            let source = JobSource::Document {
                shared: shared.clone(),
                params,
                size: document.size,
            };
            let frame = render(map, &source, 8, 8, Bits::Eight, &mut cache)
                .unwrap_or_else(|| panic!("{} produced no frame", map.id()));
            assert_eq!((frame.w, frame.h), (8, 8));
            assert_eq!(frame.bits.format(), expect);
            assert_eq!(frame.payload.len(), 8 * 8 * 4);
        }
    }

    #[test]
    fn pbr_stage_outputs_render_every_map() {
        use ns_core::height::height_to_normal;
        use ns_pbr::geometry::GeometryOutput;
        use ns_pbr::pipeline::StageOutput;

        let height = Grid::from_fn(8, 8, |x, y| (x + y) as f32 / 14.0);
        let normal = height_to_normal(&height, 4.0, Wrap::Clamp, false);
        let geometry = StageOutput::Geometry(GeometryOutput {
            height: height.clone(),
            normal,
        });
        let source = JobSource::Pbr {
            output: Arc::new(geometry),
            size: UVec2::splat(8),
        };
        let mut cache = EvalCache::default();
        for (map, expected) in [(MapId::PbrNormal, 8 * 8 * 4), (MapId::PbrHeight, 8 * 8 * 4)] {
            let frame = render(map, &source, 4, 4, Bits::Eight, &mut cache).unwrap();
            assert_eq!((frame.w, frame.h), (4, 4));
            assert_eq!(frame.payload.len(), expected / 4);
        }
    }

    #[test]
    fn orm_source_packs_without_a_geometry_stage() {
        let rough = Arc::new(StageOutput::Roughness(Grid::new(4, 4, 0.25)));
        let metal = Arc::new(StageOutput::Metallic(Grid::new(4, 4, 0.75)));
        let source = JobSource::Orm {
            ao: None,
            roughness: rough,
            metallic: metal,
            size: UVec2::splat(4),
        };
        let mut cache = EvalCache::default();
        let frame = render(MapId::PbrOrm, &source, 4, 4, Bits::Eight, &mut cache).unwrap();
        assert_eq!(frame.payload[1], 64);
        assert_eq!(frame.payload[2], 191);
    }
}
