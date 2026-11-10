//! Portable project metadata for the v2 save archive.
//!
//! The document itself stays in its existing RON form. This module describes
//! the data that used to be runtime-only: editor state, viewer state, mesh
//! metadata, lightmap inputs, and optional high-to-low bake inputs.
//!
//! All paths in this module are archive-relative asset IDs or relative paths.
//! Absolute paths are never authoritative.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::grid::Grid;
use crate::layers::LayerId;

/// Current project archive schema.
pub const PROJECT_FORMAT_VERSION: u32 = 2;

fn default_format_version() -> u32 {
    PROJECT_FORMAT_VERSION
}

fn default_document_path() -> String {
    "document.ron".into()
}

/// Root metadata written to `project.ron`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectRoot {
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default = "default_document_path")]
    pub document: String,
    #[serde(default)]
    pub editor: EditorSaveData,
    #[serde(default)]
    pub viewer: ViewerSaveData,
    #[serde(default)]
    pub meshes: Vec<MeshEntry>,
    /// Custom fonts used by text layers, mapped from their original path to an
    /// asset ID. The font bytes themselves live in the archive under `fonts/`.
    #[serde(default)]
    pub fonts: Vec<FontEntry>,
    #[serde(default)]
    pub lightmap: Option<LightmapSaveData>,
    #[serde(default)]
    pub bake: Option<BakeSaveData>,
}

impl ProjectRoot {
    pub fn new(app_version: impl Into<String>) -> Self {
        Self {
            format_version: PROJECT_FORMAT_VERSION,
            app_version: app_version.into(),
            document: default_document_path(),
            editor: EditorSaveData::default(),
            viewer: ViewerSaveData::default(),
            meshes: Vec::new(),
            fonts: Vec::new(),
            lightmap: None,
            bake: None,
        }
    }
}

impl Default for ProjectRoot {
    fn default() -> Self {
        Self::new("")
    }
}

/// Convert a saved layer ID string back to the typed UUID.
pub fn parse_layer_id(value: &str) -> Option<LayerId> {
    Uuid::parse_str(value).ok()
}

/// Convert a layer ID to its portable string form.
pub fn layer_id_string(value: LayerId) -> String {
    value.to_string()
}

/// Project-scoped editor state. Runtime-only fields are intentionally absent.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSaveData {
    pub active_tile: u32,
    pub selected_layers: Vec<String>,
    pub collapsed_groups: Vec<String>,
    pub canvas_zoom: f32,
    pub canvas_pan: [f32; 2],
    pub canvas_tool: String,
    pub show_height: bool,
    pub show_canvas_cursor: bool,
    pub show_uv_overlay_2d: bool,
    pub uv_overlay_opacity: f32,
    pub show_mask_overlay: bool,
    pub mask_overlay_opacity: f32,
    pub mask_view: String,
    pub viewport_tab: String,
    pub split_view: bool,
    pub split_stacked: bool,
    pub right_tab: String,
    pub show_layers_dock: bool,
    pub show_right_dock: bool,
    pub brush: BrushSaveData,
    pub export: ExportSaveData,
}

impl Default for EditorSaveData {
    fn default() -> Self {
        Self {
            active_tile: 1001,
            selected_layers: Vec::new(),
            collapsed_groups: Vec::new(),
            canvas_zoom: 1.0,
            canvas_pan: [0.0, 0.0],
            canvas_tool: "Select".into(),
            show_height: false,
            show_canvas_cursor: true,
            show_uv_overlay_2d: false,
            uv_overlay_opacity: 1.0,
            show_mask_overlay: false,
            mask_overlay_opacity: 0.5,
            mask_view: "Overlay".into(),
            viewport_tab: "ThreeD".into(),
            split_view: false,
            split_stacked: false,
            right_tab: "Properties".into(),
            show_layers_dock: true,
            show_right_dock: true,
            brush: BrushSaveData::default(),
            export: ExportSaveData::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushSaveData {
    pub radius: f32,
    pub hardness: f32,
    pub flow: f32,
    pub opacity: f32,
    pub mode: String,
    pub angle_deg: f32,
    pub symmetry: String,
    pub symmetry_radial_count: u32,
    pub symmetry_pivot: [f32; 2],
    pub mask_white: bool,
    pub flatten_target: f32,
    pub stroke_width: f32,
    pub lasso_line_width: f32,
    pub tips: Vec<BrushTipSaveData>,
    pub selected_tip: usize,
}

impl Default for BrushSaveData {
    fn default() -> Self {
        Self {
            radius: 48.0,
            hardness: 0.55,
            flow: 0.35,
            opacity: 1.0,
            mode: "Raise".into(),
            angle_deg: 0.0,
            symmetry: "Off".into(),
            symmetry_radial_count: 6,
            symmetry_pivot: [0.5, 0.5],
            mask_white: false,
            flatten_target: 0.5,
            stroke_width: 8.0,
            lasso_line_width: 4.0,
            tips: Vec::new(),
            selected_tip: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushTipSaveData {
    pub name: String,
    pub alpha: Option<GridF32Save>,
    pub normal: Option<GridVec3Save>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSaveData {
    pub normal: bool,
    pub height: bool,
    pub project: bool,
    pub edge_wear: bool,
    pub curvature: bool,
    pub edge_detect: bool,
    pub id_map: bool,
    pub resolution: u32,
    pub sixteen_bit: bool,
    pub all_tiles: bool,
    pub relative_dir: String,
}

impl ExportSaveData {
    pub fn default_for_project() -> Self {
        Self {
            normal: true,
            height: true,
            project: true,
            edge_wear: false,
            curvature: false,
            edge_detect: false,
            id_map: false,
            resolution: 0,
            sixteen_bit: true,
            all_tiles: true,
            relative_dir: "output".into(),
        }
    }
}

impl Default for ExportSaveData {
    fn default() -> Self {
        Self::default_for_project()
    }
}

/// State of the 3D viewer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerSaveData {
    pub mesh_id: Option<String>,
    pub preview_shape: String,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub auto_rotate: bool,
    pub surface_rotation: [f32; 4],
    pub camera_yaw: f32,
    pub camera_pitch: f32,
    pub camera_distance: f32,
    pub tiling: f32,
    pub show_uv_overlay: bool,
    pub show_light_gizmos: bool,
    pub material_preset: String,
    pub lighting_preset: String,
    pub environment_preset: String,
    pub lightmap_preview: bool,
}

impl Default for ViewerSaveData {
    fn default() -> Self {
        Self {
            mesh_id: None,
            preview_shape: "Cube".into(),
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            auto_rotate: false,
            surface_rotation: [0.0, 0.0, 0.0, 1.0],
            camera_yaw: 0.35,
            camera_pitch: 0.15,
            camera_distance: 3.0,
            tiling: 1.0,
            show_uv_overlay: false,
            show_light_gizmos: true,
            material_preset: "MatteGray".into(),
            lighting_preset: "Studio".into(),
            environment_preset: "Neutral".into(),
            lightmap_preview: false,
        }
    }
}

/// Role a mesh plays in the project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeshRole {
    Viewer,
    LightmapLow,
    LightmapHigh,
    BakeLow,
    BakeHigh,
}

/// Mesh metadata stored beside the binary `.nsmesh` asset.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MeshEntry {
    pub id: String,
    pub roles: Vec<MeshRole>,
    /// Asset ID in the archive manifest.
    pub asset: String,
    /// Optional embedded original source asset ID.
    pub source: Option<String>,
    pub source_format: String,
    pub source_complete: bool,
    pub uv_bottom_left: bool,
    pub vertex_count: u32,
    pub triangle_count: u32,
    pub udim: UdimSaveData,
    pub groups: Vec<MeshGroupSave>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MeshGroupSave {
    pub name: String,
    pub object: String,
    pub material: Option<String>,
    pub first_index: u32,
    pub index_count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UdimSaveData {
    pub enabled: bool,
    pub base_tile: u32,
    pub tiles: Vec<u32>,
}

impl Default for UdimSaveData {
    fn default() -> Self {
        Self {
            enabled: false,
            base_tile: 1001,
            tiles: Vec::new(),
        }
    }
}

/// Mapping from a document font path to an embedded font asset.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FontEntry {
    pub original_path: String,
    pub asset: String,
}

/// Lightmap inputs and settings. No baked image is stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LightmapSaveData {
    pub mesh_id: Option<String>,
    pub settings: LightmapSettingsSave,
    /// Environment asset ID, if one is used.
    pub environment: Option<String>,
    pub output_format: String,
    pub output_relative_dir: String,
    pub output_file_stem: String,
}

impl Default for LightmapSaveData {
    fn default() -> Self {
        Self {
            mesh_id: None,
            settings: LightmapSettingsSave::default(),
            environment: None,
            output_format: "Png".into(),
            output_relative_dir: "output/lightmaps".into(),
            output_file_stem: "{mesh}_lightmap".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LightmapSettingsSave {
    pub resolution: u32,
    pub backend: String,
    pub lighting: String,
    pub samples: u32,
    pub bounces: u32,
    pub tile: u32,
    pub group: Option<u32>,
    pub padding: u32,
    pub denoiser: String,
    pub oidn_device: String,
    pub denoise_passes: u32,
    pub exposure: f32,
    pub sun_enabled: bool,
    pub sun_azimuth_deg: f32,
    pub sun_elevation_deg: f32,
    pub sun_size_deg: f32,
    pub sun_color: [f32; 3],
    pub sun_intensity: f32,
    pub lights: Vec<LightmapLightSave>,
    pub ambient_color: [f32; 3],
    pub ambient_intensity: f32,
    pub environment_strength: f32,
    pub environment_fill: f32,
    pub seed: u32,
    pub pipeline: String,
    pub png_bit_depth: String,
    pub highlight_rolloff: f32,
    pub ao_max_distance: f32,
}

impl Default for LightmapSettingsSave {
    fn default() -> Self {
        Self {
            resolution: 1024,
            backend: "Auto".into(),
            lighting: "Full".into(),
            samples: 128,
            bounces: 2,
            tile: 1001,
            group: None,
            padding: 8,
            denoiser: "Builtin".into(),
            oidn_device: "Auto".into(),
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
            environment_fill: 0.0,
            seed: 1,
            pipeline: "V2".into(),
            png_bit_depth: "Eight".into(),
            highlight_rolloff: 0.0,
            ao_max_distance: 0.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LightmapLightSave {
    pub enabled: bool,
    pub visible_in_view: bool,
    pub name: String,
    pub kind: String,
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub color: [f32; 3],
    pub intensity: f32,
    pub range: f32,
    pub spot_angle_deg: f32,
    pub spot_blend: f32,
    pub area_size: [f32; 2],
    pub two_sided: bool,
}

impl Default for LightmapLightSave {
    fn default() -> Self {
        Self {
            enabled: true,
            visible_in_view: true,
            name: String::new(),
            kind: "Point".into(),
            position: [0.0, 0.0, 0.0],
            target: [0.0, 0.0, 0.0],
            color: [1.0, 1.0, 1.0],
            intensity: 1.0,
            range: 0.0,
            spot_angle_deg: 45.0,
            spot_blend: 0.4,
            area_size: [0.5, 0.5],
            two_sided: false,
        }
    }
}

/// Optional high-to-low bake inputs. The baked map is not stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BakeSaveData {
    pub low_mesh_id: Option<String>,
    pub high_mesh_id: Option<String>,
    pub settings: BakeSettingsSave,
    pub output_relative_dir: String,
    pub output_file_stem: String,
}

impl Default for BakeSaveData {
    fn default() -> Self {
        Self {
            low_mesh_id: None,
            high_mesh_id: None,
            settings: BakeSettingsSave::default(),
            output_relative_dir: "output/bake".into(),
            output_file_stem: "{mesh}_normal".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BakeSettingsSave {
    pub mode: String,
    pub resolution: u32,
    pub anti_alias: u32,
    pub space: String,
    pub tile: u32,
    pub cage_offset_pct: f32,
    pub ray_distance_pct: f32,
    pub bevel_radius_pct: f32,
    pub bevel_falloff: f32,
    pub normal_intensity: f32,
    pub padding: f32,
    pub uv_bottom_left: bool,
}

impl Default for BakeSettingsSave {
    fn default() -> Self {
        Self {
            mode: "HighToLow".into(),
            resolution: 1024,
            anti_alias: 1,
            space: "TangentOpenGl".into(),
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

/// Portable form of a scalar grid used by custom brush tips.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GridF32Save {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

impl From<&Grid<f32>> for GridF32Save {
    fn from(grid: &Grid<f32>) -> Self {
        Self {
            width: grid.w,
            height: grid.h,
            data: grid.data.clone(),
        }
    }
}

impl GridF32Save {
    pub fn to_grid(&self) -> Option<Grid<f32>> {
        if self.width == 0
            || self.height == 0
            || self.data.len() != self.width as usize * self.height as usize
        {
            return None;
        }
        Some(Grid::from_vec(self.width, self.height, self.data.clone()))
    }
}

/// Portable form of a normal grid used by custom brush tips.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GridVec3Save {
    pub width: u32,
    pub height: u32,
    pub data: Vec<[f32; 3]>,
}

impl From<&Grid<glam::Vec3>> for GridVec3Save {
    fn from(grid: &Grid<glam::Vec3>) -> Self {
        Self {
            width: grid.w,
            height: grid.h,
            data: grid.data.iter().map(|value| value.to_array()).collect(),
        }
    }
}

impl GridVec3Save {
    pub fn to_grid(&self) -> Option<Grid<glam::Vec3>> {
        if self.width == 0
            || self.height == 0
            || self.data.len() != self.width as usize * self.height as usize
        {
            return None;
        }
        Some(Grid::from_vec(
            self.width,
            self.height,
            self.data
                .iter()
                .map(|value| glam::Vec3::from_array(*value))
                .collect(),
        ))
    }
}
