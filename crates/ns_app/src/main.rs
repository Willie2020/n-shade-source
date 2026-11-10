mod app;
mod autosave;
mod bake;
mod bridge;
mod bridge_maps;
mod brushes;
mod cage;
mod gpu;
mod import;
mod lightmap;
mod lightmap_preview;
mod mesh_io;
mod operations;
#[cfg(test)]
mod parity;
mod pbr_ui;
mod pen_input;
mod preview;
mod project_data;
mod projection;
mod scifi;
mod snapping;
mod stats;
mod ui;

use std::path::PathBuf;

use bevy::asset::AssetPlugin;
use bevy::math::Vec2;
use bevy::prelude::*;
use bevy::window::WindowResolution;
use bevy_egui::EguiPlugin;
use ns_core::{
    BlendMode, FillRule, GreenChannel, Layer, LayerKind, Node, Path, Primitive, Profile,
    ShapeSource, ShapeStyle, SubPath, VectorOp, VectorRole,
};
use operations::{OperationEntry, OperationKind, SweepParams};

fn main() {
    let mut app = App::new();

    let args: Vec<String> = std::env::args().collect();
    let stress = args.iter().any(|arg| arg == "--stress");
    let empty = args.iter().any(|arg| arg == "--empty");
    let stress_layers = args
        .iter()
        .position(|arg| arg == "--stress-layers")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(100);
    let mesh = args
        .iter()
        .position(|arg| arg == "--mesh")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let uv_overlay = args.iter().any(|arg| arg == "--uv-overlay");
    let uv_overlay_2d = args.iter().any(|arg| arg == "--uv-overlay-2d");
    let uv_opacity = args
        .iter()
        .position(|arg| arg == "--uv-opacity")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f32>().ok());
    let bake_auto = args.iter().any(|arg| arg == "--bake");
    let bake_preview = args.iter().any(|arg| arg == "--bake-preview");
    let show_cage = args.iter().any(|arg| arg == "--show-cage");
    let bake_to_canvas = args.iter().any(|arg| arg == "--bake-to-canvas");
    let bake_high = args
        .iter()
        .position(|arg| arg == "--bake-high")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let bake_mode = args
        .iter()
        .position(|arg| arg == "--bake-mode")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let bake_out = args
        .iter()
        .position(|arg| arg == "--bake-out")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let bake_resolution = args
        .iter()
        .position(|arg| arg == "--bake-resolution")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<u32>().ok());
    let bake_radius = args
        .iter()
        .position(|arg| arg == "--bake-radius")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f32>().ok());
    let bake_intensity = args
        .iter()
        .position(|arg| arg == "--bake-intensity")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f32>().ok());
    let lightmap_auto = args.iter().any(|arg| arg == "--lightmap");
    let lightmap_preview = args.iter().any(|arg| arg == "--lightmap-preview");
    let lightmap_env = args
        .iter()
        .position(|arg| arg == "--lightmap-env")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let lightmap_out = args
        .iter()
        .position(|arg| arg == "--lightmap-out")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let lightmap_resolution = args
        .iter()
        .position(|arg| arg == "--lightmap-resolution")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<u32>().ok());
    let lightmap_samples = args
        .iter()
        .position(|arg| arg == "--lightmap-samples")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<u32>().ok());
    let lightmap_bounces = args
        .iter()
        .position(|arg| arg == "--lightmap-bounces")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<u32>().ok());
    let lightmap_fill = args
        .iter()
        .position(|arg| arg == "--lightmap-fill")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f32>().ok());
    let lightmap_format = args
        .iter()
        .position(|arg| arg == "--lightmap-format")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let lightmap_denoiser = args
        .iter()
        .position(|arg| arg == "--lightmap-denoiser")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let lightmap_lighting = args
        .iter()
        .position(|arg| arg == "--lightmap-lighting")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let lightmap_backend = args
        .iter()
        .position(|arg| arg == "--lightmap-backend")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let lightmap_oidn_device = args
        .iter()
        .position(|arg| arg == "--lightmap-oidn-device")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let lightmap_pipeline = args
        .iter()
        .position(|arg| arg == "--lightmap-pipeline")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let lightmap_png_depth = args
        .iter()
        .position(|arg| arg == "--lightmap-png-depth")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let lightmap_rolloff = args
        .iter()
        .position(|arg| arg == "--lightmap-rolloff")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f32>().ok());
    let lightmap_ao_distance = args
        .iter()
        .position(|arg| arg == "--lightmap-ao-distance")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f32>().ok());
    let export_now = args.iter().any(|arg| arg == "--export");
    let export_resolution = args
        .iter()
        .position(|arg| arg == "--export-resolution")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<u32>().ok());
    let export_textures = args
        .iter()
        .position(|arg| arg == "--export-textures")
        .and_then(|index| args.get(index + 1))
        .map(|value| value.to_ascii_lowercase());
    // Export height source; `--integrate-height` below only affects the 2D
    // height view, not the exported PNG.
    let export_height_source = args
        .iter()
        .position(|arg| arg == "--export-height")
        .and_then(|index| args.get(index + 1))
        .map(|value| value.to_ascii_lowercase())
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("--export-height="))
                .map(|value| value.to_ascii_lowercase())
        });
    let export_dir = args
        .iter()
        .position(|arg| arg == "--export-dir")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    // `--export-tiles 1001,1002` writes one set per tile; `all` reads the
    // listing from the mesh passed to `--mesh`.
    let export_tiles = args
        .iter()
        .position(|arg| arg == "--export-tiles")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let pbr = args
        .iter()
        .position(|arg| arg == "--pbr")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let pbr_auto = args.iter().any(|arg| arg == "--pbr-auto");
    let pbr_channel = args
        .iter()
        .position(|arg| arg == "--pbr-channel")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let view = args
        .iter()
        .position(|arg| arg == "--view")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let preview_shape = args
        .iter()
        .position(|arg| arg == "--preview-shape")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let preview_parallax = args
        .iter()
        .position(|arg| arg == "--preview-parallax")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let preview_height_scale = args
        .iter()
        .position(|arg| arg == "--preview-height-scale")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f32>().ok());
    let preview_tessellation = args
        .iter()
        .position(|arg| arg == "--preview-tessellation")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let split = args.iter().any(|arg| arg == "--split");
    let split_stacked = args.iter().any(|arg| arg == "--split-stacked");
    let gizmo = args
        .iter()
        .position(|arg| arg == "--gizmo")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let right_tab = args
        .iter()
        .position(|arg| arg == "--tab")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let height = args.iter().any(|arg| arg == "--height");
    let paint = args.iter().any(|arg| arg == "--paint");
    let paint_mask = args.iter().any(|arg| arg == "--paint-mask");
    let curve_editor = args.iter().any(|arg| arg == "--curve-editor");
    let integrate_height = args.iter().any(|arg| arg == "--integrate-height");
    let normal_directx = args.iter().any(|arg| arg == "--normal-directx");
    let normal_opengl = args.iter().any(|arg| arg == "--normal-opengl");
    let demo_shape = args.iter().any(|arg| arg == "--demo-shape");
    // Depth showcase: identical panels, different signed bevel depths.
    let demo_depth = args.iter().any(|arg| arg == "--demo-depth");
    let demo_groups = args.iter().any(|arg| arg == "--demo-groups");
    let demo_vector = args.iter().any(|arg| arg == "--demo-vector");
    let demo_boolean = args.iter().any(|arg| arg == "--demo-boolean");
    let demo_cut = args.iter().any(|arg| arg == "--demo-cut");
    let demo_ruler = args.iter().any(|arg| arg == "--demo-ruler");
    let demo_smooth = args.iter().any(|arg| arg == "--demo-smooth");
    // Partial cut / slash / clip divide showcase, each as an ordinary layer.
    let demo_cuts = args.iter().any(|arg| arg == "--demo-cuts");
    // Shape swept along a path guide, with tangent rotation.
    let demo_sweep = args.iter().any(|arg| arg == "--demo-sweep");
    let demo_guides = args.iter().any(|arg| arg == "--demo-guides");
    // Vector edge-profile showcase for the Advanced properties card.
    let demo_edge_bevel = args.iter().any(|arg| arg == "--demo-edge-bevel");
    // Cable showcase: one multi-bend tube so the terminal shapes and the
    // smooth tube shading can be inspected without drawing first.
    let demo_cable = args.iter().any(|arg| arg == "--demo-cable");
    // Live text and font showcase, also used by the manual captures.
    let demo_text = args.iter().any(|arg| arg == "--demo-text");
    // Optional generator window, parked outside the normal workflow.
    let open_scifi = args.iter().any(|arg| arg == "--scifi");
    // One flag per lasso variant keeps it easy to pass through launch scripts.
    let lasso_tool = if args.iter().any(|arg| arg == "--lasso-rect") {
        Some(app::CanvasTool::LassoRect)
    } else if args.iter().any(|arg| arg == "--lasso-ellipse") {
        Some(app::CanvasTool::LassoEllipse)
    } else if args.iter().any(|arg| arg == "--lasso-polygon") {
        Some(app::CanvasTool::LassoPolygon)
    } else if args.iter().any(|arg| arg == "--lasso-free") {
        Some(app::CanvasTool::LassoFree)
    } else if args.iter().any(|arg| arg == "--lasso-line") {
        Some(app::CanvasTool::LassoLine)
    } else {
        None
    };
    let demo_blend_modes = args.iter().any(|arg| arg == "--demo-blend-modes");
    let demo_mask_mixing = args.iter().any(|arg| arg == "--demo-mask-mixing");
    let demo_carve = args.iter().any(|arg| arg == "--demo-carve");
    let mask_overlay = args.iter().any(|arg| arg == "--mask-overlay");
    let mask_opacity = args
        .iter()
        .position(|arg| arg == "--mask-opacity")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<f32>().ok());
    let demo_mask_bind = args.iter().any(|arg| arg == "--demo-mask-bind");
    let demo_compound = args.iter().any(|arg| arg == "--demo-compound");
    let demo_combine = args.iter().any(|arg| arg == "--demo-combine");
    let icon_sheet = args.iter().any(|arg| arg == "--icon-sheet");
    let project = args
        .iter()
        .position(|arg| arg == "--project")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let svg = args
        .iter()
        .position(|arg| arg == "--svg")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let photo = args
        .iter()
        .position(|arg| arg == "--photo")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let alpha = args
        .iter()
        .position(|arg| arg == "--alpha")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let normal = args
        .iter()
        .position(|arg| arg == "--normal")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    // Blender bridge. Off unless asked for: no listener, no thread and no
    // pairing file exist without the flag.
    let bridge_enabled = args.iter().any(|arg| arg == "--bridge");
    let bridge_port = args
        .iter()
        .position(|arg| arg == "--bridge-port")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(bridge::DEFAULT_PORT);

    if let Some(index) = args.iter().position(|arg| arg == "--screenshot") {
        let path = args
            .get(index + 1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("n-shade.png"));
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        app.insert_resource(app::ScreenshotRequest { path, frame: 0 });
    }

    // Pin the asset root to the same directory the importer stages into.
    // Bevy's development default is the crate directory, which would disagree
    // with runtime imports and break in a shipped build.
    let asset_root = import::assets_dir();
    std::fs::create_dir_all(&asset_root).ok();

    let mut doc = if let Some(path) = &project {
        match ns_core::archive::load_project(path) {
            Ok(loaded) => {
                let mut loaded = loaded.document;
                ns_core::cable::migrate_cables(&mut loaded);
                loaded
            }
            Err(error) => {
                eprintln!("Could not open {}: {error}", path.display());
                ns_core::Document::new_with_base(bevy::math::UVec2::splat(1024))
            }
        }
    } else if stress {
        ns_core::stress_document(1024, stress_layers)
    } else if empty {
        ns_core::Document::new(bevy::math::UVec2::splat(1024))
    } else {
        // Plain startup: the neutral base layer only. Demo artwork is opt-in
        // through the --demo-* flags instead of being the default project.
        ns_core::Document::new_with_base(bevy::math::UVec2::splat(1024))
    };
    if normal_directx {
        doc.green = GreenChannel::DirectX;
    } else if normal_opengl {
        doc.green = GreenChannel::OpenGl;
    }
    let mut state = app::EditorState {
        show_icon_sheet: icon_sheet,
        show_perf_hud: args.iter().any(|arg| arg == "--perf-hud"),
        project_path: project.clone(),
        split_view: split || split_stacked,
        split_stacked,
        // Automated runs must never litter the user's folders with recovery
        // snapshots or overwrite their saved preferences.
        suppress_autosave: args.iter().any(|arg| {
            arg == "--screenshot"
                || arg == "--stress"
                || arg == "--icon-sheet"
                || arg == "--perf-hud"
                || arg.starts_with("--demo-")
        }),
        ..Default::default()
    };
    autosave::load_settings(&mut state);
    if let Some(source) = export_height_source.as_deref() {
        state.export_height_source = match source {
            "auto" => app::HeightSource::Auto,
            "direct" => app::HeightSource::Direct,
            "integrated" | "integrate" => app::HeightSource::Integrated,
            other => {
                eprintln!("unknown --export-height value: {other}");
                std::process::exit(2);
            }
        };
    }
    if let Some(mesh) = mesh.clone() {
        state.import_status = format!("Queued {}", mesh.display());
        state.import_request = Some(mesh);
        state.show_uv_overlay = uv_overlay;
        state.show_uv_overlay_2d = uv_overlay_2d;
    }
    if let Some(opacity) = uv_opacity {
        state.uv_overlay_opacity = opacity.clamp(0.05, 1.0);
    }
    let mut pbr_state = pbr_ui::PbrRuntime::default();
    if let Some(path) = pbr {
        state.show_pbr = true;
        pbr_state.pending_input = Some(path);
        pbr_state.auto_generate_on_load = pbr_auto;
        pbr_state.preview_maps = pbr_auto;
    }
    if let Some(channel) = pbr_channel {
        pbr_state.set_channel(pbr_ui::parse_map_kind(&channel));
    }
    if let Some(value) = preview_shape.as_deref() {
        state.preview_shape = match value.to_ascii_lowercase().replace('-', "").as_str() {
            "cube" => app::PreviewShape::Cube,
            "rounded" | "roundedcube" => app::PreviewShape::RoundedCube,
            "plane" => app::PreviewShape::Plane,
            "cylinder" => app::PreviewShape::Cylinder,
            "imported" => app::PreviewShape::Imported,
            other => {
                eprintln!("unknown --preview-shape value: {other}");
                std::process::exit(2);
            }
        };
    }
    if let Some(value) = preview_parallax.as_deref() {
        state.preview_parallax = match value.to_ascii_lowercase().as_str() {
            "off" | "none" => app::PreviewParallax::Off,
            "relief" => app::PreviewParallax::Relief,
            "occlusion" => app::PreviewParallax::Occlusion,
            other => {
                eprintln!("unknown --preview-parallax value: {other}");
                std::process::exit(2);
            }
        };
    }
    if let Some(value) = preview_height_scale {
        state.preview_height_scale = value.clamp(0.0, 4.0);
    }
    if let Some(value) = preview_tessellation.as_deref() {
        state.preview_tessellation = match value.to_ascii_lowercase().as_str() {
            "off" | "none" => app::PreviewTessellation::Off,
            "low" => app::PreviewTessellation::Low,
            "medium" => app::PreviewTessellation::Medium,
            "high" => app::PreviewTessellation::High,
            other => {
                eprintln!("unknown --preview-tessellation value: {other}");
                std::process::exit(2);
            }
        };
    }
    if let Some(value) = gizmo.as_deref() {
        state.preview_gizmo = match value.to_ascii_lowercase().as_str() {
            "move" => app::PreviewGizmo::Move,
            "stretch" => app::PreviewGizmo::Stretch,
            _ => app::PreviewGizmo::Off,
        };
    }
    if mask_overlay {
        state.show_mask_overlay = true;
    }
    if let Some(opacity) = mask_opacity {
        state.mask_overlay_opacity = opacity.clamp(0.05, 1.0);
    }
    if let Some(tab) = right_tab {
        state.right_tab = match tab.to_ascii_lowercase().as_str() {
            "mesh" => app::RightTab::Mesh,
            "bake" => app::RightTab::Bake,
            "lightmap" | "light" => app::RightTab::Lightmap,
            "pbr" => app::RightTab::Pbr,
            _ => app::RightTab::Properties,
        };
        state.show_right_dock = true;
        // Explicit tab requests focus the right dock, which matters on small
        // windows where only one dock is shown at a time.
        state.show_layers_dock = false;
    }
    if height {
        state.show_height = true;
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if integrate_height {
        state.integrate_height = true;
    }
    if demo_shape {
        let size = doc.size.x as f32;
        let scale = size / 1024.0;
        let mut layer = Layer::shape(
            "Rectangle",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(310.0, 785.0) * scale,
                size: Vec2::new(350.0, 230.0) * scale,
                corner_radius: 26.0 * scale,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 29.5 * scale;
            shape.height_px = 14.8 * scale;
        }
        doc.layers.push(layer);
        state.selected = Some(vec![doc.layers.len() - 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_depth {
        let scale = doc.size.x as f32 / 1024.0;
        for (index, depth) in [4.0_f32, 12.0, 24.0, -16.0].into_iter().enumerate() {
            let name = if depth < 0.0 {
                format!("Carve {:.0} px", -depth)
            } else {
                format!("Raise {:.0} px", depth)
            };
            let mut layer = Layer::shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center: Vec2::new(150.0 + index as f32 * 240.0, 512.0) * scale,
                    size: Vec2::new(180.0, 620.0) * scale,
                    corner_radius: 22.0 * scale,
                }),
            );
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = 26.0 * scale;
                shape.height_px = depth.abs();
                shape.invert = depth < 0.0;
            }
            doc.layers.push(layer);
        }
        state.selected = Some(vec![doc.layers.len() - 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
        state.status =
            "Depth showcase · same Radius, different Depth; the last one is carved".into();
    }
    if demo_groups {
        let scale = doc.size.x as f32 / 1024.0;
        let mut ring = Layer::shape(
            "Boolean ring",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: Vec2::new(430.0, 505.0) * scale,
                radii: Vec2::splat(122.0 * scale),
            }),
        );
        if let LayerKind::Shape(shape) = &mut ring.kind {
            shape.radius_px = 30.0 * scale;
            shape.height_px = 20.0 * scale;
        }
        ring.blend = BlendMode::BooleanUnion;

        let mut dot = Layer::shape(
            "Boolean dot",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: Vec2::new(568.0, 520.0) * scale,
                radii: Vec2::splat(74.0 * scale),
            }),
        );
        if let LayerKind::Shape(shape) = &mut dot.kind {
            shape.radius_px = 22.0 * scale;
            shape.height_px = 16.0 * scale;
        }
        dot.blend = BlendMode::BooleanDifference;

        let mut mask = Layer::shape(
            "Mask · ellipse",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: Vec2::new(512.0, 512.0) * scale,
                radii: Vec2::splat(262.0 * scale),
            }),
        );
        mask.visible = false;

        let mut group = Layer::group("Masked group", vec![Layer::flat("Group base"), ring, dot]);
        group.mask_layer = Some(mask.id);
        doc.layers.push(mask);
        doc.layers.push(group);
        state.selected = Some(vec![doc.layers.len() - 1]);
    }
    if demo_boolean {
        let scale = doc.size.x as f32 / 1024.0;
        let center = Vec2::splat(512.0 * scale);
        for (name, offset, size) in [
            ("Boolean A", Vec2::new(-95.0, 0.0), Vec2::new(260.0, 190.0)),
            ("Boolean B", Vec2::new(95.0, 30.0), Vec2::new(220.0, 240.0)),
        ] {
            let mut layer = Layer::shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center: center + offset * scale,
                    size: size * scale,
                    corner_radius: 34.0 * scale,
                }),
            );
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = 28.0 * scale;
                shape.height_px = 16.0 * scale;
            }
            doc.layers.push(layer);
        }
        let count = doc.layers.len();
        state.selected = Some(vec![count - 2]);
        state.multi_selected = vec![vec![count - 1]];
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_cut {
        let scale = doc.size.x as f32 / 1024.0;
        let mut panel = Layer::shape(
            "Panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(512.0 * scale),
                size: Vec2::new(640.0, 460.0) * scale,
                corner_radius: 40.0 * scale,
            }),
        );
        if let LayerKind::Shape(shape) = &mut panel.kind {
            shape.radius_px = 34.0 * scale;
            shape.height_px = 18.0 * scale;
            // Show the cut edges clean: diagonal bevels rib without this.
            shape.smooth_px = 3.0 * scale;
        }
        doc.layers.push(panel);

        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        for point in [
            Vec2::new(150.0, 800.0),
            Vec2::new(430.0, 560.0),
            Vec2::new(660.0, 690.0),
            Vec2::new(880.0, 430.0),
        ] {
            subpath.nodes.push(Node::corner(point * scale));
        }
        path.subpaths.push(subpath);
        let mut cutter = Layer::shape("Cut line", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut cutter.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 26.0 * scale;
            shape.radius_px = 8.0 * scale;
            shape.height_px = 12.0 * scale;
        }
        doc.layers.push(cutter);
        let cut = doc.cut_layers_below(&[doc.layers.len() - 1]);
        if let Some(outcome) = cut {
            state.selected = outcome.paths.first().cloned();
            state.multi_selected = outcome.paths.iter().skip(1).cloned().collect();
            state.status = format!("Cut into {} pieces", outcome.paths.len());
        }
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_ruler {
        let center = Vec2::splat(doc.size.x as f32 * 0.5);
        state.ruler = Some(app::RulerState {
            origin: center,
            angle: 30.0_f32.to_radians(),
        });
        state.canvas_tool = app::CanvasTool::Ruler;
        state.status = "Ruler at 30° · hold Shift while drawing to snap to it".into();
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_smooth {
        // Two identical diamonds: the left one beveled as-is, the right one
        // with outline smoothing, so the ribbing on diagonal edges shows.
        let scale = doc.size.x as f32 / 1024.0;
        for (name, center, smooth) in [
            ("Bevel · raw", Vec2::new(300.0, 512.0), 0.0_f32),
            ("Bevel · smooth", Vec2::new(724.0, 512.0), 4.0),
        ] {
            let mut path = Path::new(FillRule::NonZero);
            let mut subpath = SubPath::new(true);
            for point in [
                Vec2::new(0.0, -180.0),
                Vec2::new(180.0, 0.0),
                Vec2::new(0.0, 180.0),
                Vec2::new(-180.0, 0.0),
            ] {
                subpath.nodes.push(Node::corner((center + point) * scale));
            }
            path.subpaths.push(subpath);
            let mut layer = Layer::shape(name, ShapeSource::Path(path));
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = 40.0 * scale;
                shape.height_px = 60.0 * scale;
                shape.smooth_px = smooth;
            }
            doc.layers.push(layer);
        }
        state.selected = Some(vec![doc.layers.len() - 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_cuts {
        // The three cut operations are boolean groups nested base-first:
        // the seam splits the plate, the pocket cuts that result, then the
        // slash cuts the pocketed surface. Every cutter stays editable.
        let scale = doc.size.x as f32 / 1024.0;
        let mut plate = Layer::shape(
            "Plate",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(512.0 * scale),
                size: Vec2::new(760.0, 620.0) * scale,
                corner_radius: 60.0 * scale,
            }),
        );
        if let LayerKind::Shape(shape) = &mut plate.kind {
            shape.radius_px = 30.0 * scale;
            shape.height_px = 26.0 * scale;
            shape.profile = Profile::Round;
        }

        let mut seam = Layer::shape(
            "Divide seam",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(512.0 * scale),
                size: Vec2::new(700.0, 28.0) * scale,
                corner_radius: 14.0 * scale,
            }),
        );
        if let LayerKind::Shape(shape) = &mut seam.kind {
            shape.radius_px = 5.0 * scale;
            shape.height_px = 28.0 * scale;
            shape.profile = Profile::Round;
            shape.cut_step_px = 10.0 * scale;
        }

        let mut pocket = Layer::shape(
            "Pocket",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(330.0, 370.0) * scale,
                size: Vec2::new(240.0, 190.0) * scale,
                corner_radius: 36.0 * scale,
            }),
        );
        if let LayerKind::Shape(shape) = &mut pocket.kind {
            shape.radius_px = 22.0 * scale;
            shape.height_px = 12.0 * scale;
            shape.profile = Profile::Round;
        }

        let mut slash = Layer::shape(
            "Slash",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(520.0, 720.0) * scale,
                size: Vec2::new(560.0, 54.0) * scale,
                corner_radius: 27.0 * scale,
            }),
        );
        if let LayerKind::Shape(shape) = &mut slash.kind {
            shape.radius_px = 24.0 * scale;
            shape.height_px = 14.0 * scale;
            shape.profile = Profile::Round;
            shape.cut_taper_in = 0.2;
            shape.cut_taper_out = 0.2;
        }

        let mut clip = Layer::group("Clip divide", vec![plate, seam]);
        clip.vector_op = Some(VectorOp::ClipDivide);
        let mut pocket_group = Layer::group("Partial cut", vec![clip, pocket]);
        pocket_group.vector_op = Some(VectorOp::PartialCut);
        let mut slash_group = Layer::group("Partial slash", vec![pocket_group, slash]);
        slash_group.vector_op = Some(VectorOp::PartialSlash);
        doc.layers.push(slash_group);

        state.selected = Some(vec![doc.layers.len() - 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
        state.right_tab = app::RightTab::Properties;
        state.show_right_dock = true;
        state.status = "Boolean · partial cut / slash / clip divide".into();
    }
    if demo_sweep {
        // A stud swept along a vector path: the guide is drawn as a dashed
        // spine and never paints, so only the copies reach the normal map.
        let scale = doc.size.x as f32 / 1024.0;
        let mut stud = Layer::shape(
            "Stud",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(512.0 * scale),
                size: Vec2::new(64.0, 34.0) * scale,
                corner_radius: 16.0 * scale,
            }),
        );
        if let LayerKind::Shape(shape) = &mut stud.kind {
            shape.radius_px = 10.0 * scale;
            shape.height_px = 14.0 * scale;
        }

        let mut spine = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        for point in [
            Vec2::new(150.0, 760.0),
            Vec2::new(360.0, 420.0),
            Vec2::new(620.0, 700.0),
            Vec2::new(880.0, 340.0),
        ] {
            subpath.nodes.push(Node::corner(point * scale));
        }
        spine.subpaths.push(subpath);
        let mut guide = Layer::shape("Sweep path", ShapeSource::Path(spine));
        if let LayerKind::Shape(shape) = &mut guide.kind {
            shape.vector_role = VectorRole::Path;
        }
        let guide_id = guide.id;

        doc.layers.push(guide);
        doc.layers.push(stud);
        let stud_id = doc.layers.last().expect("stud").id;
        let mut entry = OperationEntry::new(OperationKind::Sweep);
        entry.sweep = SweepParams {
            copies: 9,
            path: Some(guide_id),
            ..Default::default()
        };
        state.operation_stacks.insert(stud_id, vec![entry]);
        state.selected = Some(vec![doc.layers.len() - 1]);
        // The demo opens with the on-model path handles ready so the workflow
        // is one click away when the viewport is switched to 3D.
        state.sweep_path_edit = true;
        state.auto_rotate = false;
        state.viewport_tab = app::ViewportTab::TwoD;
        state.right_tab = app::RightTab::Properties;
        state.show_right_dock = true;
        state.status = "Sweep · copies follow the path guide's tangent".into();
    }
    if demo_edge_bevel {
        let scale = doc.size.x as f32 / 1024.0;
        let mut path = Path::from_polygon(&[
            Vec2::new(210.0, 260.0),
            Vec2::new(820.0, 210.0),
            Vec2::new(870.0, 780.0),
            Vec2::new(240.0, 820.0),
        ]);
        path.set_edge_profile(0, 0, Some(Profile::Linear));
        let mut layer = Layer::shape("Edge bevels", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 30.0 * scale;
            shape.height_px = 24.0 * scale;
        }
        doc.layers.push(layer);
        state.selected = Some(vec![doc.layers.len() - 1]);
        state.canvas_tool = app::CanvasTool::Node;
        state.vector_tool = app::CanvasTool::Node;
        state.node_selection = vec![app::NodeRef {
            subpath: 0,
            node: 0,
        }];
        state.edge_selection = Some(app::EdgeRef {
            subpath: 0,
            edge: 0,
        });
        state.viewport_tab = app::ViewportTab::TwoD;
        state.right_tab = app::RightTab::Properties;
        state.show_right_dock = true;
        state.status =
            "Advanced properties · the top edge uses Linear, the other three stay Round".into();
    }
    if demo_guides {
        // A square-cornered panel plus a diagonal strut: enough geometry for
        // vertex, axis and parallel/perpendicular guides to show up.
        let scale = doc.size.x as f32 / 1024.0;
        let mut panel = Layer::shape(
            "Panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(512.0 * scale),
                size: Vec2::new(460.0, 320.0) * scale,
                corner_radius: 0.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut panel.kind {
            shape.radius_px = 26.0 * scale;
            shape.height_px = 14.0 * scale;
        }
        doc.layers.push(panel);

        let mut strut = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        for point in [Vec2::new(170.0, 790.0), Vec2::new(860.0, 230.0)] {
            subpath.nodes.push(Node::corner(point * scale));
        }
        strut.subpaths.push(subpath);
        let mut strut_layer = Layer::shape("Strut", ShapeSource::Path(strut));
        if let LayerKind::Shape(shape) = &mut strut_layer.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 8.0 * scale;
            shape.radius_px = 4.0 * scale;
            shape.height_px = 5.0 * scale;
        }
        doc.layers.push(strut_layer);

        state.canvas_tool = app::CanvasTool::Pen;
        state.viewport_tab = app::ViewportTab::TwoD;
        state.status = "Pen · hover near a corner or edge to see the alignment guides".into();
    }
    if demo_cable {
        let scale = doc.size.x as f32 / 1024.0;
        state.cable_width = 26.0 * scale;
        state.cable_height = 12.0 * scale;
        state.cable_cap = app::CableCap::Hex;
        let waypoints = [
            Vec2::new(180.0, 700.0),
            Vec2::new(340.0, 560.0),
            Vec2::new(540.0, 620.0),
            Vec2::new(730.0, 430.0),
            Vec2::new(900.0, 500.0),
        ]
        .map(|point| point * scale);
        // A Pen-style path with symmetric handles, converted into a cable.
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        for (index, point) in waypoints.iter().enumerate() {
            let previous = if index == 0 {
                *point
            } else {
                waypoints[index - 1]
            };
            let next = if index + 1 == waypoints.len() {
                *point
            } else {
                waypoints[index + 1]
            };
            let tangent = (next - previous) * 0.25;
            subpath.nodes.push(Node::smooth(*point, -tangent, tangent));
        }
        path.subpaths.push(subpath);
        if let Some(meta) = ns_core::CableMeta::from_path(
            &path,
            state.cable_width,
            state.cable_height,
            state.cable_cap,
            state.cable_cap_scale,
        ) {
            if let Some(layer) = ns_core::cable_layer(&meta) {
                doc.layers.push(layer);
                state.selected = Some(vec![doc.layers.len() - 1]);
            }
        }
        state.canvas_tool = app::CanvasTool::Cable;
        state.viewport_tab = app::ViewportTab::TwoD;
        state.right_tab = app::RightTab::Properties;
        state.show_right_dock = true;
        state.status =
            "Cable from a custom vector path · width, height and ends in Properties".into();
    }
    if demo_text {
        doc = ns_core::demo_text_document(doc.size.x);
        state.selected = doc
            .layers
            .iter()
            .position(|layer| layer.name == "N-SHADE")
            .map(|index| vec![index]);
        state.canvas_tool = app::CanvasTool::Select;
        state.viewport_tab = app::ViewportTab::TwoD;
        state.right_tab = app::RightTab::Properties;
        state.show_right_dock = true;
        state.status =
            "Live text · edit content, font, size, tracking and alignment in Properties".into();
    }
    if let Some(tool) = lasso_tool {
        state.lasso_tool = tool;
        state.canvas_tool = tool;
        state.status = if tool == app::CanvasTool::LassoLine {
            format!("{} · drag a straight cut", tool.label())
        } else {
            format!("{} · draw a region to clean up", tool.label())
        };
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if open_scifi {
        state.scifi.open = true;
    }
    if demo_blend_modes {
        // Two overlapping rounded squares per boolean mode: a Difference pair
        // top-left and a Union pair bottom-right, each in its own group so the
        // pairs do not interact.
        let scale = doc.size.x as f32 / 1024.0;
        let square = |name: &str, center: Vec2| {
            let mut layer = Layer::shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center: center * scale,
                    size: Vec2::splat(260.0 * scale),
                    corner_radius: 40.0 * scale,
                }),
            );
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = 12.0 * scale;
                shape.height_px = 10.0 * scale;
            }
            layer
        };
        let mut groups = Vec::new();
        for (label, mode, a, b) in [
            (
                "Difference",
                BlendMode::BooleanDifference,
                Vec2::new(200.0, 200.0),
                Vec2::new(370.0, 360.0),
            ),
            (
                "Union",
                BlendMode::BooleanUnion,
                Vec2::new(600.0, 650.0),
                Vec2::new(770.0, 810.0),
            ),
        ] {
            let mut top = square(&format!("{label} B"), b);
            top.blend = mode;
            groups.push(Layer::group(
                label,
                vec![square(&format!("{label} A"), a), top],
            ));
        }
        doc.layers.extend(groups);
        state.selected = Some(vec![doc.layers.len() - 1, 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_mask_mixing {
        // Raised panels with two detail normals mixed by smart masks: a grain
        // kept to the flat tops, and edge wear on the convex bevels broken up
        // by noise.
        let size = doc.size;
        let scale = size.x as f32 / 1024.0;
        doc.layers.clear();
        for (name, center, extent) in [
            ("Panel A", Vec2::new(330.0, 360.0), Vec2::new(420.0, 380.0)),
            ("Panel B", Vec2::new(700.0, 690.0), Vec2::new(380.0, 420.0)),
        ] {
            let mut panel = Layer::shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center: center * scale,
                    size: extent * scale,
                    corner_radius: 48.0 * scale,
                }),
            );
            if let LayerKind::Shape(shape) = &mut panel.kind {
                shape.radius_px = 30.0 * scale;
                shape.height_px = 18.0 * scale;
            }
            panel.blend = BlendMode::BooleanUnion;
            doc.layers.push(panel);
        }
        let grain_height = ns_core::mask::fbm_noise(size, 10.0 * scale, 3, 7, doc.wrap);
        let mut grain = Layer::height_paint("Grain (flat tops)", grain_height, 4.0);
        grain.mask_settings.generators.push(ui::preset_generator(0));
        doc.layers.push(grain);
        let wear_height = ns_core::mask::fbm_noise(size, 6.0 * scale, 2, 21, doc.wrap);
        let mut wear = Layer::height_paint("Edge wear", wear_height, 9.0);
        wear.mask_settings.generators.push(ui::preset_generator(2));
        wear.mask_settings.generators.push(ui::preset_generator(4));
        doc.layers.push(wear);
        state.selected = Some(vec![doc.layers.len() - 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_carve {
        // Raised (left) and carved (right) copies of the same bevel, to check
        // that invert flips the shading instead of repeating the same surface.
        let scale = doc.size.x as f32 / 1024.0;
        doc.layers.clear();
        for (name, x, invert) in [("Raised", 330.0, false), ("Carved", 700.0, true)] {
            let mut layer = Layer::shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center: Vec2::new(x, 512.0) * scale,
                    size: Vec2::new(320.0, 320.0) * scale,
                    corner_radius: 56.0 * scale,
                }),
            );
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = 34.0 * scale;
                shape.height_px = 22.0 * scale;
                shape.invert = invert;
            }
            doc.layers.push(layer);
        }
        state.selected = Some(vec![doc.layers.len() - 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_mask_bind {
        // A mask layer bound below a detail layer, with the clip grown past the
        // mask so the detail's bevel stays visible outside the source shape.
        let scale = doc.size.x as f32 / 1024.0;
        doc.layers.clear();
        let mut mask_layer = Layer::shape(
            "Mask · ellipse",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: Vec2::splat(512.0) * scale,
                radii: Vec2::splat(250.0) * scale,
            }),
        );
        mask_layer.visible = false;
        mask_layer.hidden_as_mask = true;
        let mask_id = mask_layer.id;
        let mut detail = Layer::shape(
            "Detail (clipped)",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(512.0) * scale,
                size: Vec2::new(620.0, 420.0) * scale,
                corner_radius: 40.0 * scale,
            }),
        );
        if let LayerKind::Shape(shape) = &mut detail.kind {
            shape.radius_px = 26.0 * scale;
            shape.height_px = 18.0 * scale;
        }
        detail.mask_layer = Some(mask_id);
        detail.mask_settings.clip_offset_px = 24.0 * scale;
        detail.mask_settings.clip_feather_px = 6.0 * scale;
        doc.layers.push(mask_layer);
        doc.layers.push(detail);
        state.selected = Some(vec![1]);
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if demo_compound {
        let scale = doc.size.x as f32 / 1024.0;
        doc.layers.clear();
        let shape = |name: &str, source: ShapeSource, radius: f32, height: f32| {
            let mut layer = Layer::shape(name, source);
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = radius * scale;
                shape.height_px = height * scale;
            }
            layer
        };
        let rect = |name: &str, center: Vec2, size: Vec2| {
            shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center: center * scale,
                    size: size * scale,
                    corner_radius: 34.0 * scale,
                }),
                24.0,
                18.0,
            )
        };
        let ellipse = |name: &str, center: Vec2, radii: Vec2| {
            shape(
                name,
                ShapeSource::Primitive(Primitive::Ellipse {
                    center: center * scale,
                    radii: radii * scale,
                }),
                22.0,
                16.0,
            )
        };

        // Four boolean groups, each keeping its shapes in the layer stack.
        let mut subtract = Layer::group(
            "Subtract",
            vec![
                rect(
                    "Subtract base",
                    Vec2::new(320.0, 735.0),
                    Vec2::new(330.0, 230.0),
                ),
                ellipse("Subtract hole", Vec2::new(320.0, 735.0), Vec2::splat(82.0)),
            ],
        );
        subtract.vector_op = Some(ns_core::VectorOp::Subtract);
        doc.layers.push(subtract);

        let mut divide = Layer::group(
            "Divide",
            vec![
                rect("Divide A", Vec2::new(700.0, 720.0), Vec2::new(250.0, 190.0)),
                rect("Divide B", Vec2::new(805.0, 790.0), Vec2::new(230.0, 190.0)),
            ],
        );
        divide.vector_op = Some(ns_core::VectorOp::Divide);
        doc.layers.push(divide);

        let mut xor = Layer::group(
            "XOR",
            vec![
                ellipse("XOR A", Vec2::new(720.0, 305.0), Vec2::splat(120.0)),
                ellipse("XOR B", Vec2::new(820.0, 370.0), Vec2::splat(120.0)),
            ],
        );
        xor.vector_op = Some(ns_core::VectorOp::Xor);
        doc.layers.push(xor);

        let mut add = Layer::group(
            "Add",
            vec![
                rect("Add base", Vec2::new(320.0, 305.0), Vec2::new(270.0, 200.0)),
                rect(
                    "Add overlap",
                    Vec2::new(430.0, 370.0),
                    Vec2::new(220.0, 180.0),
                ),
            ],
        );
        add.vector_op = Some(ns_core::VectorOp::Add);
        doc.layers.push(add);

        // Keep all four boolean groups visible in the dock while showing the
        // editable shapes only for the selected Add group.
        for layer in doc.layers.iter().take(3) {
            state.collapsed.insert(layer.id);
        }
        state.selected = Some(vec![doc.layers.len() - 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
        state.status = "Boolean groups · Add · Subtract · Intersect · XOR · Divide".into();
    }
    if demo_combine {
        let scale = doc.size.x as f32 / 1024.0;
        doc.layers.clear();
        let rect = |name: &str, center: Vec2, size: Vec2| {
            let mut layer = Layer::shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center: center * scale,
                    size: size * scale,
                    corner_radius: 34.0 * scale,
                }),
            );
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = 24.0 * scale;
                shape.height_px = 18.0 * scale;
            }
            layer
        };
        doc.layers.push(rect(
            "Left shape",
            Vec2::new(440.0, 512.0),
            Vec2::new(330.0, 260.0),
        ));
        doc.layers.push(rect(
            "Right shape",
            Vec2::new(600.0, 580.0),
            Vec2::new(360.0, 280.0),
        ));
        // Primary selection is the right shape; the left is added to the
        // multi-selection just like Ctrl/Cmd-clicking a second row.
        state.selected = Some(vec![1]);
        state.multi_selected = vec![vec![0]];
        state.viewport_tab = app::ViewportTab::TwoD;
        state.status = "Choose a Boolean operation to combine both shapes".into();
    }
    if demo_vector {
        let scale = doc.size.x as f32 / 1024.0;
        let center = Vec2::splat(512.0 * scale);
        let mut path = Path::new(FillRule::NonZero);
        let mut nodes = Vec::new();
        let count = 6;
        for i in 0..count {
            let angle =
                std::f32::consts::TAU * i as f32 / count as f32 - std::f32::consts::FRAC_PI_2;
            let radius = if i % 2 == 0 { 185.0 } else { 240.0 } * scale;
            let pos = center + Vec2::new(angle.cos(), angle.sin()) * radius;
            let tangent = Vec2::new(-angle.sin(), angle.cos()) * 86.0 * scale;
            nodes.push(Node::smooth(pos, -tangent, tangent));
        }
        path.subpaths.push(SubPath {
            nodes,
            closed: true,
        });
        let mut layer = Layer::shape("Vector blob", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 22.0 * scale;
            shape.height_px = 18.0 * scale;
        }
        doc.layers.push(layer);
        state.selected = Some(vec![doc.layers.len() - 1]);
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    if let Some(value) = view.as_deref() {
        state.viewport_tab = if value.eq_ignore_ascii_case("2d") {
            app::ViewportTab::TwoD
        } else {
            app::ViewportTab::ThreeD
        };
    }
    if let Some(svg) = svg {
        state.status = format!("Importing {}...", svg.display());
        state.svg_import_request = Some(svg);
    }
    if let Some(photo) = photo {
        state.status = format!("Importing photo {}...", photo.display());
        state.photo_import_request = Some(photo);
    }
    if let Some(alpha) = alpha {
        state.status = format!("Importing alpha {}...", alpha.display());
        state.alpha_import_request = Some(alpha);
    }
    if let Some(normal) = normal {
        state.status = format!("Importing normal map {}...", normal.display());
        state.normal_import_request = Some(normal);
    }
    if paint {
        state.canvas_tool = app::CanvasTool::Paint;
        state.show_height = true;
        state.viewport_tab = app::ViewportTab::TwoD;
    }
    let mut bake_state = bake::BakeRuntime::default();
    bake_state.auto_start = bake_auto;
    bake_state.preview_on_mesh = bake_preview;
    bake_state.show_cage = show_cage;
    bake_state.auto_canvas = bake_to_canvas;
    if let Some(path) = bake_high {
        bake_state.high_path = Some(path);
    }
    if let Some(mode) = bake_mode {
        bake_state.settings.mode = if mode.eq_ignore_ascii_case("bevel") {
            ns_bake::BakeMode::Bevel
        } else {
            ns_bake::BakeMode::HighToLow
        };
    }
    if let Some(path) = bake_out {
        bake_state.output_path = path.display().to_string();
    }
    if let Some(intensity) = bake_intensity {
        bake_state.settings.normal_intensity = intensity;
    }
    if let Some(resolution) = bake_resolution {
        bake_state.settings.resolution = resolution;
    }
    if let Some(radius) = bake_radius {
        bake_state.settings.bevel_radius_pct = radius.clamp(0.01, 2.0);
    }
    app.insert_resource(bake_state);

    let mut lightmap_state = lightmap::LightmapRuntime::default();
    lightmap_state.auto_start = lightmap_auto;
    lightmap_state.preview_enabled = lightmap_preview;
    lightmap_state.environment_path = lightmap_env;
    if lightmap_preview {
        state.right_tab = app::RightTab::Lightmap;
        state.show_right_dock = true;
    }
    if let Some(path) = lightmap_out {
        if lightmap_format.is_none()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exr"))
        {
            lightmap_state.format = lightmap::LightmapFormat::Exr;
        }
        lightmap_state.output_path = path.display().to_string();
    }
    if let Some(resolution) = lightmap_resolution {
        lightmap_state.settings.resolution = resolution;
    }
    if let Some(samples) = lightmap_samples {
        lightmap_state.settings.samples = samples;
    }
    if let Some(bounces) = lightmap_bounces {
        lightmap_state.settings.bounces = bounces;
    }
    if let Some(fill) = lightmap_fill {
        lightmap_state.settings.environment_fill = fill.max(0.0);
    }
    if let Some(format) = lightmap_format {
        lightmap_state.format = if format.eq_ignore_ascii_case("exr") {
            lightmap::LightmapFormat::Exr
        } else {
            lightmap::LightmapFormat::Png
        };
    }
    if let Some(lighting) = lightmap_lighting {
        lightmap_state.settings.lighting = match lighting.to_ascii_lowercase().as_str() {
            "full" | "all" => ns_bake::LightmapLightingMode::Full,
            "direct" | "sun" => ns_bake::LightmapLightingMode::Direct,
            "indirect" | "gi" | "sky" => ns_bake::LightmapLightingMode::Indirect,
            "ao" | "occlusion" | "ambient-occlusion" => {
                ns_bake::LightmapLightingMode::AmbientOcclusion
            }
            _ => ns_bake::LightmapLightingMode::Full,
        };
    }
    if let Some(denoiser) = lightmap_denoiser {
        lightmap_state.settings.denoiser = match denoiser.to_ascii_lowercase().as_str() {
            "none" | "off" => ns_bake::LightmapDenoiser::None,
            "auto" => ns_bake::LightmapDenoiser::Auto,
            "builtin" | "built-in" => ns_bake::LightmapDenoiser::Builtin,
            "optix" | "nvidia" => ns_bake::LightmapDenoiser::Optix,
            "oidn" | "intel" => ns_bake::LightmapDenoiser::Oidn,
            _ => ns_bake::LightmapDenoiser::Auto,
        };
    }
    if let Some(backend) = lightmap_backend {
        lightmap_state.settings.backend = match backend.to_ascii_lowercase().as_str() {
            "cpu" => ns_bake::LightmapBackend::Cpu,
            "gpu" => ns_bake::LightmapBackend::Gpu,
            _ => ns_bake::LightmapBackend::Auto,
        };
    }
    if let Some(device) = lightmap_oidn_device {
        lightmap_state.settings.oidn_device = match device.to_ascii_lowercase().as_str() {
            "cpu" => ns_bake::OidnDevice::Cpu,
            "cuda" | "nvidia" => ns_bake::OidnDevice::Cuda,
            "sycl" | "intel" => ns_bake::OidnDevice::Sycl,
            _ => ns_bake::OidnDevice::Auto,
        };
    }
    if let Some(pipeline) = lightmap_pipeline {
        lightmap_state.settings.pipeline = match pipeline.to_ascii_lowercase().as_str() {
            "legacy" | "v1" => ns_bake::LightmapPipeline::Legacy,
            "v2" => ns_bake::LightmapPipeline::V2,
            other => {
                eprintln!("unknown --lightmap-pipeline value: {other}");
                std::process::exit(2);
            }
        };
    }
    if let Some(depth) = lightmap_png_depth {
        lightmap_state.settings.png_bit_depth = match depth.to_ascii_lowercase().as_str() {
            "8" | "eight" => ns_bake::PngBitDepth::Eight,
            "16" | "sixteen" => ns_bake::PngBitDepth::Sixteen,
            other => {
                eprintln!("unknown --lightmap-png-depth value: {other}");
                std::process::exit(2);
            }
        };
    }
    if let Some(value) = lightmap_rolloff {
        lightmap_state.settings.highlight_rolloff = value.clamp(0.0, 0.95);
    }
    if let Some(value) = lightmap_ao_distance {
        lightmap_state.settings.ao_max_distance = value.max(0.0);
    }
    app.insert_resource(lightmap_state);

    if let Some(resolution) = export_resolution {
        state.export_resolution = resolution;
    }
    if let Some(textures) = export_textures {
        state.export_normal = false;
        state.export_height = false;
        state.export_project = false;
        state.export_edge_wear = false;
        state.export_curvature = false;
        state.export_edge_detect = false;
        state.export_id_map = false;
        for name in textures.split(',') {
            match name.trim() {
                "all" => {
                    state.export_normal = true;
                    state.export_height = true;
                    state.export_project = true;
                    state.export_edge_wear = true;
                    state.export_curvature = true;
                    state.export_edge_detect = true;
                    state.export_id_map = true;
                }
                "normal" => state.export_normal = true,
                "height" => state.export_height = true,
                "project" => state.export_project = true,
                "edge-wear" | "edge_wear" | "wear" => state.export_edge_wear = true,
                "curvature" | "curve" => state.export_curvature = true,
                "edge-detect" | "edge_detect" | "edges" => state.export_edge_detect = true,
                "id-map" | "id_map" | "ids" => state.export_id_map = true,
                _ => {}
            }
        }
    }
    if let Some(directory) = export_dir {
        state.export_dir = Some(directory);
    }
    if let Some(tiles) = export_tiles {
        let tiles = if tiles.eq_ignore_ascii_case("all") {
            mesh.as_ref()
                .map(|path| import::udim_tiles_for_mesh(path))
                .unwrap_or_default()
        } else {
            tiles
                .split(',')
                .filter_map(|tile| tile.trim().parse::<u32>().ok())
                .collect()
        };
        if tiles.is_empty() {
            eprintln!(
                "--export-tiles found no tiles; pass 1001,1002 or 'all' together with --mesh"
            );
        } else {
            state.udim_enabled = true;
            state.udim_tiles = tiles;
            state.export_all_tiles = true;
        }
    }
    let mut document_res = app::DocumentRes::new(doc);
    if paint_mask {
        if let Some(path) = state.selected.clone() {
            if let Some(layer_id) = document_res.0.layer_by_path(&path).map(|layer| layer.id) {
                ui::begin_mask_edit(&mut state, &mut document_res, layer_id);
            }
        }
    }
    if curve_editor {
        if let Some(path) = state.selected.clone() {
            if let Some(layer_id) = document_res.0.layer_by_path(&path).map(|layer| layer.id) {
                ui::open_curve_editor(&mut state, &document_res, vec![layer_id]);
            }
        }
    }
    if export_now {
        let mut cache = ns_core::EvalCache::new();
        ui::export_project(&mut state, &mut document_res, &mut cache);
        println!("{}", state.status);
    }
    app.insert_resource(document_res);
    // Start on the requested view without playing the switch animation.
    state.last_viewport_tab = state.viewport_tab;
    app.insert_resource(state);
    app.insert_resource(pbr_state);

    app.add_plugins((
        DefaultPlugins
            .set(AssetPlugin {
                file_path: asset_root.to_string_lossy().into_owned(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "N-shade — Normal Map Studio".into(),
                    resolution: WindowResolution::new(1440, 780),
                    ..default()
                }),
                ..default()
            }),
        EguiPlugin::default(),
        app::NormalStudioPlugin,
        gpu::NsGpuPlugin,
        import::MeshImportPlugin,
        stats::SystemStatsPlugin,
        bake::BakePlugin,
        cage::BakeCagePlugin,
        lightmap::LightmapPlugin,
        lightmap_preview::LightmapPreviewPlugin,
        projection::ProjectionPlugin,
        pbr_ui::PbrPlugin,
    ));
    // Always installed so the Settings window can open the listener at
    // runtime; a disabled bridge costs one boolean check per frame.
    app.add_plugins(bridge::BridgePlugin::new(bridge_port, bridge_enabled));
    app.run();
}
