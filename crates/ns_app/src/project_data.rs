//! Conversion between runtime editor resources and the portable project
//! archive. Keeping this in one module makes the save/load boundary explicit:
//! only authored data and rebuildable inputs cross it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use glam::Vec2;
use ns_bake::{
    BakeSettings, LightmapBackend, LightmapDenoiser, LightmapLightKind, LightmapLightingMode,
    LightmapPipeline, LightmapSceneLight, LightmapSettings, OidnDevice, PngBitDepth,
};
use ns_core::archive::{self, AssetKind, LoadedProject, ProjectAsset};
use ns_core::brush::BrushTip;
use ns_core::mesh_asset::{MeshAsset, MeshGroup};
use ns_core::project::{
    parse_layer_id, BakeSaveData, BakeSettingsSave, BrushSaveData, BrushTipSaveData,
    EditorSaveData, ExportSaveData, FontEntry, GridF32Save, GridVec3Save, LightmapLightSave,
    LightmapSaveData, LightmapSettingsSave, MeshEntry, MeshGroupSave, MeshRole, ProjectRoot,
    UdimSaveData, ViewerSaveData,
};
use ns_core::{Document, FontSource, Layer, LayerKind, ShapeSource};

use crate::app::{
    CanvasTool, EditorState, EnvironmentPreset, LightingPreset, MaskView, MaterialPreset,
    PaintMode, PreviewShape, RightTab, ViewportTab,
};
use crate::bake::BakeRuntime;
use crate::import::MeshImport;
use crate::lightmap::{LightmapFormat, LightmapRuntime};
use crate::mesh_io::{self, RawMesh};

/// Runtime resources needed to write a complete project.
pub(crate) struct SaveContext<'a> {
    pub mesh_import: &'a MeshImport,
    pub lightmap: &'a LightmapRuntime,
    pub bake: &'a BakeRuntime,
}

/// Write a v2 archive and return non-fatal warnings.
pub(crate) fn save_bundle(
    state: &EditorState,
    doc: &Document,
    path: &Path,
    context: Option<&SaveContext<'_>>,
) -> Result<Vec<String>, String> {
    let mut project = ProjectRoot::new(env!("CARGO_PKG_VERSION"));
    project.editor = editor_save(state, doc);
    project.viewer = viewer_save(state, context);

    let mut assets = Vec::new();
    let mut warnings = Vec::new();
    let mut viewer_mesh_id: Option<String> = None;
    let mut high_mesh_id: Option<String> = None;

    if let Some(context) = context {
        if let Some(source) = &context.mesh_import.source_path {
            let raw = mesh_io::load_any(source)
                .map_err(|error| format!("could not reload mesh {}: {error}", source.display()))?;
            let asset = mesh_asset_from_raw(&raw, context.mesh_import.uv_bottom_left)?;
            let id = "mesh-viewer".to_string();
            let bytes = asset.encode()?;
            let source_format = source
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("mesh")
                .to_ascii_lowercase();
            project.meshes.push(mesh_entry(
                &id,
                vec![MeshRole::Viewer, MeshRole::LightmapLow],
                &asset,
                context.mesh_import,
                &source_format,
            ));
            assets.push(ProjectAsset {
                id: id.clone(),
                kind: AssetKind::Mesh,
                path: format!("meshes/{id}.nsmesh"),
                bytes,
            });
            viewer_mesh_id = Some(id);
        }

        if let Some(high) = &context.bake.high_path {
            match mesh_io::load_any(high) {
                Ok(raw) => {
                    let asset = mesh_asset_from_raw(&raw, context.bake.settings.uv_bottom_left)?;
                    let id = "mesh-high".to_string();
                    let bytes = asset.encode()?;
                    let source_format = high
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .unwrap_or("mesh")
                        .to_ascii_lowercase();
                    project.meshes.push(mesh_entry(
                        &id,
                        vec![MeshRole::BakeHigh],
                        &asset,
                        context.mesh_import,
                        &source_format,
                    ));
                    assets.push(ProjectAsset {
                        id: id.clone(),
                        kind: AssetKind::Mesh,
                        path: format!("meshes/{id}.nsmesh"),
                        bytes,
                    });
                    high_mesh_id = Some(id);
                }
                Err(error) => warnings.push(format!(
                    "High-poly bake mesh {} was not embedded: {error}",
                    high.display()
                )),
            }
        }

        if high_mesh_id.is_some() {
            if let Some(entry) = project
                .meshes
                .iter_mut()
                .find(|entry| entry.id == "mesh-viewer")
            {
                entry.roles.push(MeshRole::BakeLow);
            }
        }

        let mut lightmap = lightmap_save(context.lightmap, viewer_mesh_id.clone());
        if let Some(environment_path) = &context.lightmap.environment_path {
            match std::fs::read(environment_path) {
                Ok(bytes) => {
                    let id = "env-main".to_string();
                    let file_name = environment_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("environment.hdr");
                    assets.push(ProjectAsset {
                        id: id.clone(),
                        kind: AssetKind::Environment,
                        path: format!("environments/{id}-{file_name}"),
                        bytes,
                    });
                    lightmap.environment = Some(id);
                }
                Err(error) => warnings.push(format!(
                    "Lightmap environment {} was not embedded: {error}",
                    environment_path.display()
                )),
            }
        }
        project.lightmap = Some(lightmap);

        if high_mesh_id.is_some() {
            project.bake = Some(bake_save(
                &context.bake.settings,
                viewer_mesh_id.clone(),
                high_mesh_id.clone(),
            ));
        }
    }

    project.viewer.mesh_id = viewer_mesh_id;
    let font_paths = collect_font_paths(doc);
    for (index, original_path) in font_paths.into_iter().enumerate() {
        match std::fs::read(&original_path) {
            Ok(bytes) => {
                let id = format!("font-{index}");
                let file_name = Path::new(&original_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("font.ttf");
                assets.push(ProjectAsset {
                    id: id.clone(),
                    kind: AssetKind::Font,
                    path: format!("fonts/{id}-{file_name}"),
                    bytes,
                });
                project.fonts.push(FontEntry {
                    original_path,
                    asset: id,
                });
            }
            Err(error) => warnings.push(format!(
                "Custom font {} was not embedded; text will fall back: {error}",
                original_path
            )),
        }
    }

    archive::save_project(path, &project, doc, &assets)
        .map_err(|error| format!("could not write project: {error}"))?;
    Ok(warnings)
}

/// Apply a loaded archive to the editor and document. Mesh and environment
/// bytes are materialized into a cache folder so existing importers and bake
/// workers can use normal file paths.
pub(crate) fn apply_loaded(
    state: &mut EditorState,
    doc: &mut Document,
    loaded: LoadedProject,
) -> Vec<String> {
    let LoadedProject {
        project,
        document,
        meshes,
        fonts,
        environments,
        legacy: _,
        mut warnings,
    } = loaded;
    *doc = document;
    ns_core::cable::migrate_cables(doc);

    apply_editor(state, doc, &project.editor);
    apply_viewer(state, &project.viewer);
    state.pending_lightmap = project.lightmap.clone();
    state.pending_lightmap_preview = Some(project.viewer.lightmap_preview);
    state.pending_bake = project.bake.clone();

    let cache = cache_dir();
    if let Err(error) = std::fs::create_dir_all(&cache) {
        warnings.push(format!("Could not create project cache: {error}"));
    } else {
        if !fonts.is_empty() {
            let mut paths = HashMap::new();
            for font in &fonts {
                let path = cache.join(safe_file_name(&font.file_name));
                match std::fs::write(&path, &font.bytes) {
                    Ok(()) => {
                        paths.insert(font.original_path.clone(), path);
                    }
                    Err(error) => warnings.push(format!(
                        "Could not materialize font {}: {error}",
                        font.file_name
                    )),
                }
            }
            if !paths.is_empty() {
                rewrite_font_paths(&mut doc.layers, &paths);
            }
        }

        let viewer_mesh = project.viewer.mesh_id.as_deref();
        let mesh = viewer_mesh
            .and_then(|id| meshes.iter().find(|mesh| mesh.entry.id == id))
            .or_else(|| {
                meshes
                    .iter()
                    .find(|mesh| mesh.entry.roles.contains(&MeshRole::Viewer))
            });
        if let Some(mesh) = mesh {
            let path = cache.join(format!("{}.nsmesh", safe_file_name(&mesh.entry.id)));
            match mesh.asset.encode() {
                Ok(bytes) => match std::fs::write(&path, bytes) {
                    Ok(()) => {
                        state.import_request = Some(path);
                        state.import_status = "Restoring saved mesh...".into();
                    }
                    Err(error) => warnings.push(format!("Could not restore mesh: {error}")),
                },
                Err(error) => warnings.push(format!("Could not encode restored mesh: {error}")),
            }
        }

        if let Some(high) = project
            .bake
            .as_ref()
            .and_then(|bake| bake.high_mesh_id.as_deref())
            .and_then(|id| meshes.iter().find(|mesh| mesh.entry.id == id))
            .or_else(|| {
                meshes
                    .iter()
                    .find(|mesh| mesh.entry.roles.contains(&MeshRole::BakeHigh))
            })
        {
            let path = cache.join(format!("{}.nsmesh", safe_file_name(&high.entry.id)));
            match high.asset.encode() {
                Ok(bytes) => match std::fs::write(&path, bytes) {
                    Ok(()) => state.pending_bake_high = Some(path),
                    Err(error) => warnings.push(format!("Could not restore high mesh: {error}")),
                },
                Err(error) => warnings.push(format!("Could not encode high mesh: {error}")),
            }
        }

        if let Some(environment_id) = project
            .lightmap
            .as_ref()
            .and_then(|lightmap| lightmap.environment.as_deref())
        {
            if let Some(environment) = environments
                .iter()
                .find(|environment| environment.id == environment_id)
            {
                let path = cache.join(safe_file_name(&environment.file_name));
                match std::fs::write(&path, &environment.bytes) {
                    Ok(()) => state.pending_lightmap_environment = Some(path),
                    Err(error) => {
                        warnings.push(format!("Could not restore lightmap environment: {error}"))
                    }
                }
            } else {
                warnings.push("Lightmap environment asset is missing from the archive".into());
            }
        }
    }

    warnings
}

pub(crate) fn apply_bake(runtime: &mut BakeRuntime, save: &BakeSaveData) {
    runtime.settings = bake_settings_from_save(&save.settings);
    let directory = if save.output_relative_dir.trim().is_empty() {
        "output/bake"
    } else {
        save.output_relative_dir.trim()
    };
    let stem = if save.output_file_stem.trim().is_empty() {
        "normal"
    } else {
        save.output_file_stem.trim()
    };
    runtime.output_path = format!("{directory}/{stem}");
}

pub(crate) fn apply_lightmap(runtime: &mut LightmapRuntime, save: &LightmapSaveData) {
    runtime.settings = lightmap_settings_from_save(&save.settings);
    runtime.format = if save.output_format.eq_ignore_ascii_case("exr") {
        LightmapFormat::Exr
    } else {
        LightmapFormat::Png
    };
    let directory = if save.output_relative_dir.trim().is_empty() {
        "output/lightmaps"
    } else {
        save.output_relative_dir.trim()
    };
    let stem = if save.output_file_stem.trim().is_empty() {
        "lightmap"
    } else {
        save.output_file_stem.trim()
    };
    runtime.output_path = format!("{directory}/{stem}");
}

fn editor_save(state: &EditorState, doc: &Document) -> EditorSaveData {
    let mut selected_layers = Vec::new();
    if let Some(path) = &state.selected {
        if let Some(layer) = doc.layer_by_path(path) {
            selected_layers.push(layer.id.to_string());
        }
    }
    for path in &state.multi_selected {
        if let Some(layer) = doc.layer_by_path(path) {
            selected_layers.push(layer.id.to_string());
        }
    }

    let tips = state
        .brush_library
        .tips
        .iter()
        .map(|tip| BrushTipSaveData {
            name: tip.name.clone(),
            alpha: tip.alpha.as_ref().map(GridF32Save::from),
            normal: tip.normal.as_ref().map(GridVec3Save::from),
        })
        .collect();

    EditorSaveData {
        active_tile: doc.active_tile,
        selected_layers,
        collapsed_groups: state
            .collapsed
            .iter()
            .copied()
            .map(|id| id.to_string())
            .collect(),
        canvas_zoom: state.zoom,
        canvas_pan: [state.pan.x, state.pan.y],
        canvas_tool: format!("{:?}", state.canvas_tool),
        show_height: state.show_height,
        show_canvas_cursor: state.show_canvas_cursor,
        show_uv_overlay_2d: state.show_uv_overlay_2d,
        uv_overlay_opacity: state.uv_overlay_opacity,
        show_mask_overlay: state.show_mask_overlay,
        mask_overlay_opacity: state.mask_overlay_opacity,
        mask_view: format!("{:?}", state.mask_view),
        viewport_tab: format!("{:?}", state.viewport_tab),
        split_view: state.split_view,
        split_stacked: state.split_stacked,
        right_tab: format!("{:?}", state.right_tab),
        show_layers_dock: state.show_layers_dock,
        show_right_dock: state.show_right_dock,
        brush: BrushSaveData {
            radius: state.brush_radius,
            hardness: state.brush_hardness,
            flow: state.brush_flow,
            opacity: state.brush_opacity,
            mode: format!("{:?}", state.brush_mode),
            angle_deg: state.brush_angle_deg,
            symmetry: format!("{:?}", state.symmetry),
            symmetry_radial_count: state.symmetry_radial_count,
            symmetry_pivot: state.symmetry_pivot.to_array(),
            mask_white: state.mask_white,
            flatten_target: state.brush_flatten_target,
            stroke_width: state.stroke_width,
            lasso_line_width: state.lasso_line_width,
            tips,
            selected_tip: state.brush_library.selected_index(),
        },
        export: ExportSaveData {
            normal: state.export_normal,
            height: state.export_height,
            project: state.export_project,
            edge_wear: state.export_edge_wear,
            curvature: state.export_curvature,
            edge_detect: state.export_edge_detect,
            id_map: state.export_id_map,
            resolution: state.export_resolution,
            sixteen_bit: state.export_sixteen_bit,
            all_tiles: state.export_all_tiles,
            relative_dir: "output".into(),
        },
    }
}

fn apply_editor(state: &mut EditorState, doc: &mut Document, save: &EditorSaveData) {
    doc.active_tile = save.active_tile.max(1001);

    let mut paths = Vec::new();
    for value in &save.selected_layers {
        if let Some(id) = parse_layer_id(value) {
            if let Some(path) = doc.find_path(id) {
                paths.push(path);
            }
        }
    }
    state.selected = paths.first().cloned();
    state.multi_selected = paths.into_iter().skip(1).collect();
    state.collapsed = save
        .collapsed_groups
        .iter()
        .filter_map(|value| parse_layer_id(value))
        .collect();

    state.canvas_tool = parse_canvas_tool(&save.canvas_tool);
    state.show_height = save.show_height;
    state.show_canvas_cursor = save.show_canvas_cursor;
    state.show_uv_overlay_2d = save.show_uv_overlay_2d;
    state.uv_overlay_opacity = save.uv_overlay_opacity.clamp(0.0, 1.0);
    state.show_mask_overlay = save.show_mask_overlay;
    state.mask_overlay_opacity = save.mask_overlay_opacity.clamp(0.0, 1.0);
    state.mask_view = parse_mask_view(&save.mask_view);
    state.zoom = save.canvas_zoom.max(0.01);
    state.pan = bevy_egui::egui::vec2(save.canvas_pan[0], save.canvas_pan[1]);

    state.viewport_tab = parse_viewport_tab(&save.viewport_tab);
    state.split_view = save.split_view;
    state.split_stacked = save.split_stacked;
    state.right_tab = parse_right_tab(&save.right_tab);
    state.show_layers_dock = save.show_layers_dock;
    state.show_right_dock = save.show_right_dock;

    state.brush_radius = save.brush.radius.max(0.5);
    state.brush_hardness = save.brush.hardness.clamp(0.0, 1.0);
    state.brush_flow = save.brush.flow.clamp(0.0, 1.0);
    state.brush_opacity = save.brush.opacity.clamp(0.0, 1.0);
    state.brush_mode = parse_paint_mode(&save.brush.mode);
    state.brush_angle_deg = save.brush.angle_deg;
    state.symmetry = parse_symmetry(&save.brush.symmetry);
    state.symmetry_radial_count = save.brush.symmetry_radial_count.clamp(2, 32);
    state.symmetry_pivot = Vec2::new(
        save.brush.symmetry_pivot[0].clamp(0.0, 1.0),
        save.brush.symmetry_pivot[1].clamp(0.0, 1.0),
    );
    state.mask_white = save.brush.mask_white;
    state.brush_flatten_target = save.brush.flatten_target;
    state.stroke_width = save.brush.stroke_width.max(0.5);
    state.lasso_line_width = save.brush.lasso_line_width.max(0.5);

    if !save.brush.tips.is_empty() {
        let mut library = crate::brushes::BrushLibrary::default();
        library.tips.clear();
        for tip in &save.brush.tips {
            library.push(BrushTip {
                name: tip.name.clone(),
                alpha: tip.alpha.as_ref().and_then(GridF32Save::to_grid),
                normal: tip.normal.as_ref().and_then(GridVec3Save::to_grid),
            });
        }
        if library.tips.is_empty() {
            library.push(BrushTip::round());
        }
        library.select(save.brush.selected_tip);
        state.brush_library = library;
    }

    state.export_normal = save.export.normal;
    state.export_height = save.export.height;
    state.export_project = save.export.project;
    state.export_edge_wear = save.export.edge_wear;
    state.export_curvature = save.export.curvature;
    state.export_edge_detect = save.export.edge_detect;
    state.export_id_map = save.export.id_map;
    state.export_resolution = save.export.resolution;
    state.export_sixteen_bit = save.export.sixteen_bit;
    state.export_all_tiles = save.export.all_tiles;
    if !save.export.relative_dir.trim().is_empty() {
        state.export_dir = Some(PathBuf::from(save.export.relative_dir.trim()));
    }
}

fn viewer_save(state: &EditorState, context: Option<&SaveContext<'_>>) -> ViewerSaveData {
    let mesh_import = context.map(|context| context.mesh_import);
    ViewerSaveData {
        mesh_id: mesh_import
            .and_then(|import| import.source_path.as_ref())
            .map(|_| "mesh-viewer".to_string()),
        preview_shape: format!("{:?}", state.preview_shape),
        translation: [0.0, 0.0, 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0, 1.0, 1.0],
        auto_rotate: state.auto_rotate,
        surface_rotation: mesh_import
            .map(|import| import.surface_rotation.to_array())
            .unwrap_or([0.0, 0.0, 0.0, 1.0]),
        camera_yaw: state.orbit_yaw,
        camera_pitch: state.orbit_pitch,
        camera_distance: state.orbit_distance,
        tiling: state.tiling,
        show_uv_overlay: state.show_uv_overlay,
        show_light_gizmos: state.show_light_gizmos,
        material_preset: state.material_preset.label().into(),
        lighting_preset: state.lighting_preset.label().into(),
        environment_preset: state.environment_preset.label().into(),
        lightmap_preview: context
            .map(|context| context.lightmap.preview_enabled)
            .unwrap_or(false),
    }
}

fn apply_viewer(state: &mut EditorState, save: &ViewerSaveData) {
    state.preview_shape = parse_preview_shape(&save.preview_shape);
    state.auto_rotate = save.auto_rotate;
    state.orbit_yaw = save.camera_yaw;
    state.orbit_pitch = save.camera_pitch;
    state.orbit_distance = save.camera_distance.max(0.1);
    state.tiling = save.tiling.max(0.01);
    state.show_uv_overlay = save.show_uv_overlay;
    state.show_light_gizmos = save.show_light_gizmos;
    state.material_preset = parse_material_preset(&save.material_preset);
    state.lighting_preset = parse_lighting_preset(&save.lighting_preset);
    state.environment_preset = parse_environment_preset(&save.environment_preset);
}

fn lightmap_save(runtime: &LightmapRuntime, mesh_id: Option<String>) -> LightmapSaveData {
    let output_path = Path::new(&runtime.output_path);
    let stem = output_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("{mesh}_lightmap")
        .to_string();
    LightmapSaveData {
        mesh_id,
        settings: lightmap_settings_save(&runtime.settings),
        environment: None,
        output_format: format!("{:?}", runtime.format),
        output_relative_dir: "output/lightmaps".into(),
        output_file_stem: stem,
    }
}

fn lightmap_settings_save(settings: &LightmapSettings) -> LightmapSettingsSave {
    LightmapSettingsSave {
        resolution: settings.resolution,
        backend: format!("{:?}", settings.backend),
        lighting: format!("{:?}", settings.lighting),
        samples: settings.samples,
        bounces: settings.bounces,
        tile: settings.tile,
        group: settings.group,
        padding: settings.padding,
        denoiser: format!("{:?}", settings.denoiser),
        oidn_device: format!("{:?}", settings.oidn_device),
        denoise_passes: settings.denoise_passes,
        exposure: settings.exposure,
        sun_enabled: settings.sun_enabled,
        sun_azimuth_deg: settings.sun_azimuth_deg,
        sun_elevation_deg: settings.sun_elevation_deg,
        sun_size_deg: settings.sun_size_deg,
        sun_color: settings.sun_color,
        sun_intensity: settings.sun_intensity,
        lights: settings.lights.iter().map(light_save).collect(),
        ambient_color: settings.ambient_color,
        ambient_intensity: settings.ambient_intensity,
        environment_strength: settings.environment_strength,
        environment_fill: settings.environment_fill,
        seed: settings.seed,
        pipeline: format!("{:?}", settings.pipeline),
        png_bit_depth: format!("{:?}", settings.png_bit_depth),
        highlight_rolloff: settings.highlight_rolloff,
        ao_max_distance: settings.ao_max_distance,
    }
}

fn light_save(light: &LightmapSceneLight) -> LightmapLightSave {
    LightmapLightSave {
        enabled: light.enabled,
        visible_in_view: light.visible_in_view,
        name: light.name.clone(),
        kind: format!("{:?}", light.kind),
        position: light.position,
        target: light.target,
        color: light.color,
        intensity: light.intensity,
        range: light.range,
        spot_angle_deg: light.spot_angle_deg,
        spot_blend: light.spot_blend,
        area_size: light.area_size,
        two_sided: light.two_sided,
    }
}

fn lightmap_settings_from_save(save: &LightmapSettingsSave) -> LightmapSettings {
    let mut settings = LightmapSettings::default();
    settings.resolution = save.resolution;
    settings.backend = parse_lightmap_backend(&save.backend);
    settings.lighting = parse_lighting_mode(&save.lighting);
    settings.samples = save.samples;
    settings.bounces = save.bounces;
    settings.tile = save.tile;
    settings.group = save.group;
    settings.padding = save.padding;
    settings.denoiser = parse_denoiser(&save.denoiser);
    settings.oidn_device = parse_oidn_device(&save.oidn_device);
    settings.denoise_passes = save.denoise_passes;
    settings.exposure = save.exposure;
    settings.sun_enabled = save.sun_enabled;
    settings.sun_azimuth_deg = save.sun_azimuth_deg;
    settings.sun_elevation_deg = save.sun_elevation_deg;
    settings.sun_size_deg = save.sun_size_deg;
    settings.sun_color = save.sun_color;
    settings.sun_intensity = save.sun_intensity;
    settings.lights = save.lights.iter().map(light_from_save).collect();
    settings.ambient_color = save.ambient_color;
    settings.ambient_intensity = save.ambient_intensity;
    settings.environment_strength = save.environment_strength;
    settings.environment_fill = save.environment_fill;
    settings.seed = save.seed;
    settings.pipeline = if save.pipeline.eq_ignore_ascii_case("legacy") {
        LightmapPipeline::Legacy
    } else {
        LightmapPipeline::V2
    };
    settings.png_bit_depth = if save.png_bit_depth.eq_ignore_ascii_case("sixteen") {
        PngBitDepth::Sixteen
    } else {
        PngBitDepth::Eight
    };
    settings.highlight_rolloff = save.highlight_rolloff;
    settings.ao_max_distance = save.ao_max_distance;
    settings
}

fn light_from_save(save: &LightmapLightSave) -> LightmapSceneLight {
    let mut light = LightmapSceneLight::default();
    light.enabled = save.enabled;
    light.visible_in_view = save.visible_in_view;
    light.name = save.name.clone();
    light.kind = parse_light_kind(&save.kind);
    light.position = save.position;
    light.target = save.target;
    light.color = save.color;
    light.intensity = save.intensity;
    light.range = save.range;
    light.spot_angle_deg = save.spot_angle_deg;
    light.spot_blend = save.spot_blend;
    light.area_size = save.area_size;
    light.two_sided = save.two_sided;
    light
}

fn bake_settings_from_save(save: &BakeSettingsSave) -> BakeSettings {
    let mut settings = BakeSettings::default();
    settings.mode = match save.mode.as_str() {
        "Bevel" => ns_bake::BakeMode::Bevel,
        _ => ns_bake::BakeMode::HighToLow,
    };
    settings.resolution = save.resolution;
    settings.anti_alias = save.anti_alias;
    settings.space = match save.space.as_str() {
        "TangentDirectX" => ns_bake::BakeSpace::TangentDirectX,
        "Object" => ns_bake::BakeSpace::Object,
        _ => ns_bake::BakeSpace::TangentOpenGl,
    };
    settings.tile = save.tile;
    settings.cage_offset_pct = save.cage_offset_pct;
    settings.ray_distance_pct = save.ray_distance_pct;
    settings.bevel_radius_pct = save.bevel_radius_pct;
    settings.bevel_falloff = save.bevel_falloff;
    settings.normal_intensity = save.normal_intensity;
    settings.padding = save.padding;
    settings.uv_bottom_left = save.uv_bottom_left;
    settings
}

fn bake_save(
    settings: &BakeSettings,
    low_mesh_id: Option<String>,
    high_mesh_id: Option<String>,
) -> BakeSaveData {
    BakeSaveData {
        low_mesh_id,
        high_mesh_id,
        settings: BakeSettingsSave {
            mode: format!("{:?}", settings.mode),
            resolution: settings.resolution,
            anti_alias: settings.anti_alias,
            space: format!("{:?}", settings.space),
            tile: settings.tile,
            cage_offset_pct: settings.cage_offset_pct,
            ray_distance_pct: settings.ray_distance_pct,
            bevel_radius_pct: settings.bevel_radius_pct,
            bevel_falloff: settings.bevel_falloff,
            normal_intensity: settings.normal_intensity,
            padding: settings.padding,
            uv_bottom_left: settings.uv_bottom_left,
        },
        output_relative_dir: "output/bake".into(),
        output_file_stem: "{mesh}_normal".into(),
    }
}

fn mesh_asset_from_raw(raw: &[RawMesh], uv_bottom_left: bool) -> Result<MeshAsset, String> {
    if raw.is_empty() {
        return Err("the mesh file contains no geometry".into());
    }
    let has_uvs = raw.iter().all(|mesh| {
        mesh.uvs
            .as_ref()
            .is_some_and(|uvs| uvs.len() == mesh.positions.len())
    });
    let mut asset = MeshAsset {
        name: raw
            .first()
            .map(|mesh| mesh.name.clone())
            .unwrap_or_else(|| "mesh".into()),
        uvs: has_uvs.then(|| Vec::new()),
        uv_bottom_left,
        ..Default::default()
    };
    for mesh in raw {
        let base_vertex = asset.positions.len() as u32;
        let first_index = asset.indices.len() as u32;
        let normals = if mesh.normals.len() == mesh.positions.len() {
            mesh.normals.clone()
        } else {
            mesh_io::smooth_normals(&mesh.positions, &mesh.indices)
        };
        asset.positions.extend_from_slice(&mesh.positions);
        asset.normals.extend_from_slice(&normals);
        if let Some(uvs) = &mut asset.uvs {
            if let Some(mesh_uvs) = &mesh.uvs {
                uvs.extend_from_slice(mesh_uvs);
            } else {
                uvs.extend(std::iter::repeat([0.0, 0.0]).take(mesh.positions.len()));
            }
        }
        asset.indices.extend(
            mesh.indices
                .iter()
                .map(|index| base_vertex.saturating_add(*index)),
        );
        asset.groups.push(MeshGroup {
            name: mesh.name.clone(),
            object: mesh.object_name().to_string(),
            material: mesh.material.clone(),
            first_index,
            index_count: mesh.indices.len() as u32,
        });
    }
    asset.validate()?;
    Ok(asset)
}

fn mesh_entry(
    id: &str,
    roles: Vec<MeshRole>,
    asset: &MeshAsset,
    import: &MeshImport,
    source_format: &str,
) -> MeshEntry {
    MeshEntry {
        id: id.into(),
        roles,
        asset: id.into(),
        source: None,
        source_format: source_format.into(),
        source_complete: false,
        uv_bottom_left: import.uv_bottom_left,
        vertex_count: asset.vertex_count() as u32,
        triangle_count: asset.triangle_count() as u32,
        udim: UdimSaveData {
            enabled: import.udim.enabled,
            base_tile: import.udim.base_tile(),
            tiles: import.udim.tiles.clone(),
        },
        groups: asset
            .groups
            .iter()
            .map(|group| MeshGroupSave {
                name: group.name.clone(),
                object: group.object.clone(),
                material: group.material.clone(),
                first_index: group.first_index,
                index_count: group.index_count,
            })
            .collect(),
    }
}

fn collect_font_paths(doc: &Document) -> Vec<String> {
    fn visit(layers: &[Layer], out: &mut Vec<String>, seen: &mut HashSet<String>) {
        for layer in layers {
            match &layer.kind {
                LayerKind::Group { children } => visit(children, out, seen),
                LayerKind::Shape(shape) => {
                    if let ShapeSource::Text(text) = &shape.source {
                        if let FontSource::File { path, .. } = &text.font {
                            if seen.insert(path.clone()) {
                                out.push(path.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    visit(&doc.layers, &mut paths, &mut seen);
    paths
}

fn rewrite_font_paths(layers: &mut [Layer], paths: &HashMap<String, PathBuf>) {
    for layer in layers {
        match &mut layer.kind {
            LayerKind::Group { children } => rewrite_font_paths(children, paths),
            LayerKind::Shape(shape) => {
                if let ShapeSource::Text(text) = &mut shape.source {
                    if let FontSource::File { path, .. } = &mut text.font {
                        if let Some(new_path) = paths.get(path) {
                            *path = new_path.to_string_lossy().into_owned();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("n-shade-project-cache")
}

fn safe_file_name(name: &str) -> String {
    let filtered: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if filtered.trim_matches('.').is_empty() {
        "asset".into()
    } else {
        filtered
    }
}

fn parse_canvas_tool(value: &str) -> CanvasTool {
    match value {
        "Rectangle" => CanvasTool::Rectangle,
        "Ellipse" => CanvasTool::Ellipse,
        "Star" => CanvasTool::Star,
        "Polygon" => CanvasTool::Polygon,
        "Cable" => CanvasTool::Cable,
        "Text" => CanvasTool::Text,
        "Ruler" => CanvasTool::Ruler,
        "LassoRect" => CanvasTool::LassoRect,
        "LassoEllipse" => CanvasTool::LassoEllipse,
        "LassoPolygon" => CanvasTool::LassoPolygon,
        "LassoFree" => CanvasTool::LassoFree,
        "LassoLine" => CanvasTool::LassoLine,
        "Pen" => CanvasTool::Pen,
        "Node" => CanvasTool::Node,
        "AddAnchor" => CanvasTool::AddAnchor,
        "DeleteAnchor" => CanvasTool::DeleteAnchor,
        "ConvertAnchor" => CanvasTool::ConvertAnchor,
        "Paint" => CanvasTool::Paint,
        _ => CanvasTool::Select,
    }
}

fn parse_viewport_tab(value: &str) -> ViewportTab {
    if value == "TwoD" {
        ViewportTab::TwoD
    } else {
        ViewportTab::ThreeD
    }
}

fn parse_right_tab(value: &str) -> RightTab {
    match value {
        "Mesh" => RightTab::Mesh,
        "Bake" => RightTab::Bake,
        "Lightmap" => RightTab::Lightmap,
        "Pbr" => RightTab::Pbr,
        _ => RightTab::Properties,
    }
}

fn parse_mask_view(value: &str) -> MaskView {
    match value {
        "Mask" => MaskView::Mask,
        "Result" => MaskView::Result,
        _ => MaskView::Overlay,
    }
}

fn parse_paint_mode(value: &str) -> PaintMode {
    match value {
        "Lower" => PaintMode::Lower,
        "Smooth" => PaintMode::Smooth,
        "Flatten" => PaintMode::Flatten,
        _ => PaintMode::Raise,
    }
}

fn parse_symmetry(value: &str) -> ns_core::brush::Symmetry {
    match value {
        "MirrorX" => ns_core::brush::Symmetry::MirrorX,
        "MirrorY" => ns_core::brush::Symmetry::MirrorY,
        "MirrorXY" => ns_core::brush::Symmetry::MirrorXY,
        "Radial" => ns_core::brush::Symmetry::Radial,
        _ => ns_core::brush::Symmetry::Off,
    }
}

fn parse_preview_shape(value: &str) -> PreviewShape {
    match value {
        "RoundedCube" => PreviewShape::RoundedCube,
        "Plane" => PreviewShape::Plane,
        "Cylinder" => PreviewShape::Cylinder,
        "Imported" => PreviewShape::Imported,
        _ => PreviewShape::Cube,
    }
}

fn parse_material_preset(value: &str) -> MaterialPreset {
    match value {
        "Clay" => MaterialPreset::Clay,
        "Steel" => MaterialPreset::Steel,
        _ => MaterialPreset::MatteGray,
    }
}

fn parse_lighting_preset(value: &str) -> LightingPreset {
    match value {
        "Soft" => LightingPreset::Soft,
        "Dramatic" => LightingPreset::Dramatic,
        _ => LightingPreset::Studio,
    }
}

fn parse_environment_preset(value: &str) -> EnvironmentPreset {
    match value {
        "Cool" => EnvironmentPreset::Cool,
        "Warm" => EnvironmentPreset::Warm,
        _ => EnvironmentPreset::Neutral,
    }
}

fn parse_lightmap_backend(value: &str) -> LightmapBackend {
    match value {
        "Cpu" => LightmapBackend::Cpu,
        "Gpu" => LightmapBackend::Gpu,
        _ => LightmapBackend::Auto,
    }
}

fn parse_lighting_mode(value: &str) -> LightmapLightingMode {
    match value {
        "Direct" => LightmapLightingMode::Direct,
        "Indirect" => LightmapLightingMode::Indirect,
        "AmbientOcclusion" => LightmapLightingMode::AmbientOcclusion,
        _ => LightmapLightingMode::Full,
    }
}

fn parse_denoiser(value: &str) -> LightmapDenoiser {
    match value {
        "None" => LightmapDenoiser::None,
        "Optix" => LightmapDenoiser::Optix,
        "Oidn" => LightmapDenoiser::Oidn,
        "Builtin" => LightmapDenoiser::Builtin,
        _ => LightmapDenoiser::Auto,
    }
}

fn parse_oidn_device(value: &str) -> OidnDevice {
    match value {
        "Cpu" => OidnDevice::Cpu,
        "Cuda" => OidnDevice::Cuda,
        "Sycl" => OidnDevice::Sycl,
        _ => OidnDevice::Auto,
    }
}

fn parse_light_kind(value: &str) -> LightmapLightKind {
    match value {
        "Spot" => LightmapLightKind::Spot,
        "Area" => LightmapLightKind::Area,
        _ => LightmapLightKind::Point,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ns_core::project::ProjectRoot;

    #[test]
    fn mesh_asset_merges_raw_groups() {
        let raw = vec![RawMesh {
            name: "panel".into(),
            object: "panel".into(),
            material: Some("metal".into()),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: Some(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            indices: vec![0, 1, 2],
        }];
        let asset = mesh_asset_from_raw(&raw, false).unwrap();
        assert_eq!(asset.vertex_count(), 3);
        assert_eq!(asset.triangle_count(), 1);
        assert_eq!(asset.groups[0].material.as_deref(), Some("metal"));
    }

    #[test]
    fn bundle_embeds_viewer_mesh_and_lightmap_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let mesh_path = directory.path().join("panel.obj");
        std::fs::write(
            &mesh_path,
            "v 0 0 0\nv 1 0 0\nv 0 1 0\n\
             vt 0 0\nvt 1 0\nvt 0 1\n\
             vn 0 0 1\n\
             f 1/1/1 2/2/1 3/3/1\n",
        )
        .unwrap();
        let mut mesh_import = MeshImport::default();
        mesh_import.source_path = Some(mesh_path);
        mesh_import.uv_bottom_left = true;
        let lightmap = LightmapRuntime::default();
        let bake = BakeRuntime::default();
        let context = SaveContext {
            mesh_import: &mesh_import,
            lightmap: &lightmap,
            bake: &bake,
        };
        let document = ns_core::Document::new_with_base(glam::UVec2::splat(64));
        let path = directory.path().join("panel.nsproj");
        let mut state = crate::app::EditorState::default();
        state.show_light_gizmos = false;
        let warnings = save_bundle(&state, &document, &path, Some(&context)).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let loaded = ns_core::archive::load_project(&path).unwrap();
        assert!(!loaded.legacy);
        assert_eq!(loaded.meshes.len(), 1);
        assert_eq!(loaded.meshes[0].asset.triangle_count(), 1);
        assert!(loaded.meshes[0].entry.uv_bottom_left);
        assert!(loaded.project.lightmap.is_some());
        assert!(
            !loaded.project.viewer.show_light_gizmos,
            "the 3D light gizmo toggle must survive the bundle"
        );
    }

    #[test]
    fn editor_selection_round_trips_by_id() {
        let mut doc = ns_core::demo_document(64);
        let state = EditorState::default();
        let save = editor_save(&state, &doc);
        apply_editor(&mut EditorState::default(), &mut doc, &save);
        assert!(ProjectRoot::new("test").format_version >= 2);
    }
}
