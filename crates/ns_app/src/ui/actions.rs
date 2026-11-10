//! File dialogs, export, and SVG import actions.

use super::*;

use ns_core::io::{save_height, save_id_map, save_normal, save_project_ron, BitDepth};
use std::path::PathBuf;
pub(crate) fn export_project(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    cache: &mut EvalCache,
) {
    let _span = bevy::log::trace_span!("export").entered();
    let export_normal = state.export_normal;
    let export_height = state.export_height;
    let export_project = state.export_project;
    let export_edge_wear = state.export_edge_wear;
    let export_curvature = state.export_curvature;
    let export_edge_detect = state.export_edge_detect;
    let export_id_map = state.export_id_map;
    let edge_wear_params = state.edge_wear_params;
    let edge_detect_params = state.edge_detect_params;
    let has_derived = export_edge_wear || export_curvature || export_edge_detect;
    if !(export_normal || export_height || export_project || export_id_map || has_derived) {
        state.status = "Export failed: select at least one texture".into();
        return;
    }
    let resolution = if state.export_resolution == 0 {
        doc.0.size.x.max(doc.0.size.y)
    } else {
        state.export_resolution.clamp(512, 4096)
    };
    let bit_depth = if state.export_sixteen_bit {
        BitDepth::Sixteen
    } else {
        BitDepth::Eight
    };
    // The exported height must describe the same surface as the exported
    // normals. Analytic layer heights use max-compositing and per-layer height
    // scales, so the legacy default integrates the final normal field instead.
    // Auto keeps that integration only when imported normals (which have no
    // analytic height) are present.
    let height_source = state.export_height_source;
    let wrap = doc.0.wrap;
    let green = doc.0.green;
    let out_dir = state
        .export_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("output"));

    // One image per UDIM tile when the mesh (or the stack) defines tiles. A
    // layer tagged for a tile exports even when no mesh is loaded, so a saved
    // project keeps its per-tile maps. Without any tile information the export
    // stays the historical single set of maps.
    let original_tile = doc.0.active_tile;
    let mut tiles: Vec<u32> = if state.udim_enabled {
        state.udim_tiles.clone()
    } else {
        Vec::new()
    };
    for tile in doc.0.layer_tiles() {
        if !tiles.contains(&tile) {
            tiles.push(tile);
        }
    }
    if !tiles.is_empty() {
        if !tiles.contains(&original_tile) {
            tiles.push(original_tile);
        }
        tiles.sort_unstable();
        tiles.dedup();
    }
    let export_tiles: Vec<Option<u32>> = if tiles.is_empty() {
        vec![None]
    } else if state.export_all_tiles {
        tiles.iter().copied().map(Some).collect()
    } else {
        vec![Some(original_tile)]
    };

    let previous_cache_tile = cache.tile;
    let result = (|| -> ns_core::Result<Vec<&'static str>> {
        std::fs::create_dir_all(&out_dir)?;
        let mut exported = Vec::new();

        for tile in &export_tiles {
            // The cache override makes every evaluator read this tile without
            // mutating the document (and deep-cloning its layer grids).
            cache.tile = *tile;
            // The exported height must describe the same surface as the
            // exported normals. Analytic layer heights use max-compositing and
            // per-layer height scales, so the legacy default integrates the
            // final normal field instead. Auto keeps that integration only when
            // imported normals (which have no analytic height) are present.
            let has_imported_normals = match tile {
                Some(tile) => doc.0.has_imported_normals_for_tile(*tile),
                None => doc.0.has_imported_normals(),
            };
            let height_from_normals =
                export_height && height_source.integrates_height(has_imported_normals);

            // Integration needs the normal field even when only the height map
            // is selected, so evaluate it on demand and reuse it when both are
            // saved.
            let normal = if export_normal || height_from_normals || has_derived {
                Some(doc.0.evaluate_with(cache))
            } else {
                None
            };
            let normal_out = if export_normal {
                normal
                    .as_ref()
                    .map(|normal| normal.resized_normalized(resolution, resolution))
            } else {
                None
            };

            if export_normal {
                if let Some(normal) = &normal_out {
                    save_normal(
                        map_path(&out_dir, "normal", "png", *tile),
                        normal,
                        green,
                        bit_depth,
                    )?;
                    let label = if green.flips_y() {
                        "normal (DirectX)"
                    } else {
                        "normal (OpenGL)"
                    };
                    if !exported.contains(&label) {
                        exported.push(label);
                    }
                }
            }

            if export_height {
                let height = if height_from_normals {
                    let normal = normal.as_ref().expect("normal evaluated for integration");
                    // Integrate at document resolution; mirror-padded FFTs at 4K
                    // would need gigabytes. Resize afterwards just like the normal.
                    normal_to_height(
                        normal,
                        &NormalToHeightOptions {
                            wrap,
                            percentile_low: 0.0,
                            percentile_high: 1.0,
                            ..Default::default()
                        },
                    )
                    .resized(resolution, resolution)
                } else {
                    doc.0
                        .evaluate_height_with(cache)
                        .resized(resolution, resolution)
                };
                save_height(
                    map_path(&out_dir, "height", "png", *tile),
                    &height,
                    bit_depth,
                )?;
                let label = if height_from_normals {
                    "height (integrated)"
                } else {
                    "height (direct)"
                };
                if !exported.contains(&label) {
                    exported.push(label);
                }
            }

            if has_derived {
                // Curvature is a second derivative, so it is computed on the
                // native normal field and only then resized. Running the filter
                // on an already-downscaled normal map amplifies resampling noise
                // along every edge and makes edge wear/edge detect look chipped.
                let native = normal.as_ref().expect("normal evaluated for derived maps");
                let curvature = ns_core::derived::normal_curvature(native, wrap);
                if export_curvature {
                    let encoded = ns_core::derived::encode_curvature(&curvature);
                    save_height(
                        map_path(&out_dir, "curvature", "png", *tile),
                        &encoded.resized(resolution, resolution),
                        bit_depth,
                    )?;
                    if !exported.contains(&"curvature") {
                        exported.push("curvature");
                    }
                }
                if export_edge_wear {
                    let wear = ns_core::derived::edge_wear_with(&curvature, &edge_wear_params);
                    save_height(
                        map_path(&out_dir, "edge_wear", "png", *tile),
                        &wear.resized(resolution, resolution),
                        bit_depth,
                    )?;
                    if !exported.contains(&"edge wear") {
                        exported.push("edge wear");
                    }
                }
                if export_edge_detect {
                    let detect =
                        ns_core::derived::edge_detect_with(&curvature, &edge_detect_params);
                    save_height(
                        map_path(&out_dir, "edge_detect", "png", *tile),
                        &detect.resized(resolution, resolution),
                        bit_depth,
                    )?;
                    if !exported.contains(&"edge detect") {
                        exported.push("edge detect");
                    }
                }
            }

            if export_id_map {
                // Majority resize only: averaging IDs would blend two materials
                // along every shared edge, and plain nearest can drop a thin
                // feature that fills most of an output pixel. Majority keeps
                // every exported pixel exactly one of the assigned colours while
                // following the silhouette more faithfully when downscaling.
                let ids = doc
                    .0
                    .evaluate_id_map_for_tile([0.0, 0.0, 0.0], tile.unwrap_or(doc.0.active_tile))
                    .resample_majority(resolution, resolution);
                save_id_map(map_path(&out_dir, "id", "png", *tile), &ids)?;
                if !exported.contains(&"ID map") {
                    exported.push("ID map");
                }
            }
        }

        if export_project {
            save_project_ron(out_dir.join("project.nsproj"), &doc.0)?;
            exported.push("project");
        }
        Ok(exported)
    })();
    cache.tile = previous_cache_tile;

    let id_colors = doc.0.has_id_colors();
    let has_imported_normals = doc.0.has_imported_normals();
    let height_from_normals =
        export_height && height_source.integrates_height(has_imported_normals);
    state.status = match result {
        Ok(exported) => {
            let mut status = if export_tiles.len() > 1 {
                format!(
                    "Exported {} for {} UDIM tiles at {} px to {}",
                    exported.join(" + "),
                    export_tiles.len(),
                    resolution,
                    out_dir.display()
                )
            } else {
                format!(
                    "Exported {} at {} px to {}",
                    exported.join(" + "),
                    resolution,
                    out_dir.display()
                )
            };
            if export_id_map && !id_colors {
                status.push_str(" · no ID colors assigned");
            }
            if export_height && !height_from_normals && has_imported_normals {
                status.push_str(" · imported normals have no direct height");
            }
            status
        }
        Err(err) => format!("Export failed: {err}"),
    };
}

/// Path for one exported map. UDIM runs append the tile number before the
/// extension, matching the convention bake and lightmap batches already use.
fn map_path(out_dir: &std::path::Path, stem: &str, extension: &str, tile: Option<u32>) -> PathBuf {
    match tile {
        Some(tile) => out_dir.join(format!("{stem}_{tile}.{extension}")),
        None => out_dir.join(format!("{stem}.{extension}")),
    }
}

/// Folder the next project dialog opens in: the current project's folder, or
/// the last folder used by a project dialog.
fn project_dialog_dir(state: &EditorState) -> Option<PathBuf> {
    state.project_dir.clone().or_else(|| {
        state
            .project_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(std::path::Path::to_path_buf)
    })
}

fn project_name_or_default(state: &EditorState) -> String {
    state
        .project_name()
        .unwrap_or_else(|| "untitled.nsproj".to_string())
}

/// Require the project extension. A typed name without `.nsproj` gets it added;
/// another extension is replaced, so the dialog cannot silently save a `.ron`
/// that the Open filter will not show.
fn ensure_project_extension(path: PathBuf) -> PathBuf {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("nsproj"))
    {
        path
    } else {
        path.with_extension("nsproj")
    }
}

fn project_display_name(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project.nsproj")
        .to_string()
}

/// Open a `.nsproj` from a file dialog.
pub(crate) fn open_project_dialog(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    cache: &mut EvalCache,
) {
    let mut dialog = rfd::FileDialog::new()
        .add_filter("N-shade project", &["nsproj"])
        .set_title("Open N-shade project");
    if let Some(dir) = project_dialog_dir(state) {
        dialog = dialog.set_directory(dir);
    }
    if let Some(path) = dialog.pick_file() {
        open_project_file(state, doc, cache, &path);
    }
}

/// Load a project and replace the document. Undo history and transient canvas
/// state belong to the old document, so they are cleared; the cache is dropped
/// because loaded layers can reuse ids from the previous document.
pub(crate) fn open_project_file(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    cache: &mut EvalCache,
    path: &std::path::Path,
) -> bool {
    let _span = bevy::log::trace_span!("project.load").entered();
    match ns_core::archive::load_project(path) {
        Ok(loaded) => {
            reset_after_project_open(state, path);
            let warnings = crate::project_data::apply_loaded(state, &mut doc.0, loaded);
            cache.invalidate_all();
            state.request_eval_now();
            state.status = if warnings.is_empty() {
                format!("Opened {}", project_display_name(path))
            } else {
                format!(
                    "Opened {} ({} warning(s))",
                    project_display_name(path),
                    warnings.len()
                )
            };
            true
        }
        Err(error) => {
            state.status = format!("Open failed: {error}");
            false
        }
    }
}

/// Clear the state that belongs to the document being replaced: undo history,
/// selection, drafts, and viewport framing. Shared by New and Open.
fn reset_after_document_replaced(state: &mut EditorState) {
    state.undo.clear();
    state.redo.clear();
    state.undo_gesture = None;
    state.undo_gesture_committed = false;
    state.selected = None;
    state.multi_selected.clear();
    state.collapsed.clear();
    state.node_selection.clear();
    state.node_hover = None;
    state.canvas_has_focus = false;
    state.canvas_drag = None;
    state.vector_draft = None;
    state.lasso_draft = None;
    state.mask_edit_layer = None;
    state.mask_overlay_key = None;
    state.mask_overlay_grid = None;
    state.mask_overlay_tex_key = None;
    state.mask_overlay_tex = None;
    state.mask_tex = None;
    state.curve_editor = None;
    state.ruler = None;
    state.ruler_dragging = false;
    state.snap_session = None;
    state.active_guides.clear();
    state.show_duplicate_array = false;
    state.canvas_tool = CanvasTool::Select;
    state.viewport_tab = ViewportTab::TwoD;
    state.split_view = false;
    state.show_height = false;
    state.zoom = 1.0;
    state.pan = egui::Vec2::ZERO;
    state.pending_lightmap = None;
    state.pending_lightmap_environment = None;
    state.pending_lightmap_preview = None;
    state.pending_bake = None;
    state.pending_bake_high = None;
}

fn reset_after_project_open(state: &mut EditorState, path: &std::path::Path) {
    reset_after_document_replaced(state);
    state.project_path = Some(path.to_path_buf());
    state.project_dir = path.parent().map(std::path::Path::to_path_buf);
    state.mark_dirty();
    state.project_saved_serial = state.edit_serial;
}

/// Start a blank project at the current resolution. The project has no file
/// yet, so the next Save asks for a path and autosave uses the untitled folder.
pub(crate) fn new_project(state: &mut EditorState, doc: &mut DocumentRes, cache: &mut EvalCache) {
    let size = doc.0.size;
    doc.0 = Document::new_with_base(size).into();
    cache.invalidate_all();
    reset_after_document_replaced(state);
    state.project_path = None;
    state.mark_dirty();
    state.project_saved_serial = state.edit_serial;
    state.status = format!("New project · {} × {}", size.x, size.y);
}

/// Save to the current path, or ask for one. `save_as` always asks.
pub(crate) fn save_project_dialog(
    state: &mut EditorState,
    doc: &DocumentRes,
    save_as: bool,
    context: Option<&crate::project_data::SaveContext<'_>>,
) {
    let existing = if save_as {
        None
    } else {
        state.project_path.clone()
    };
    let path = match existing {
        Some(path) => path,
        None => {
            let mut dialog = rfd::FileDialog::new()
                .add_filter("N-shade project", &["nsproj"])
                .set_title("Save N-shade project")
                .set_file_name(project_name_or_default(state));
            if let Some(dir) = project_dialog_dir(state) {
                dialog = dialog.set_directory(dir);
            }
            let Some(chosen) = dialog.save_file() else {
                return;
            };
            ensure_project_extension(chosen)
        }
    };
    save_project_to_with_context(state, doc, path, context);
}

/// Save a plain v1 RON document for older N-shade builds. The legacy file
/// intentionally omits mesh, editor, viewer and lightmap state because those
/// builds cannot read the v2 archive.
pub(crate) fn save_legacy_project_dialog(state: &mut EditorState, doc: &DocumentRes) {
    let mut dialog = rfd::FileDialog::new()
        .add_filter("N-shade v1 project", &["nsproj"])
        .set_title("Save legacy N-shade project")
        .set_file_name(project_name_or_default(state));
    if let Some(dir) = project_dialog_dir(state) {
        dialog = dialog.set_directory(dir);
    }
    let Some(chosen) = dialog.save_file() else {
        return;
    };
    let path = ensure_project_extension(chosen);
    match save_project_ron(&path, &doc.0) {
        Ok(()) => {
            state.status = format!("Saved legacy {}", project_display_name(&path));
        }
        Err(error) => {
            state.status = format!("Legacy save failed: {error}");
        }
    }
}

/// Write a complete v2 project and remember it as the current save target.
pub(crate) fn save_project_to_with_context(
    state: &mut EditorState,
    doc: &DocumentRes,
    path: PathBuf,
    context: Option<&crate::project_data::SaveContext<'_>>,
) {
    let _span = bevy::log::trace_span!("project.save").entered();
    let path = ensure_project_extension(path);
    match crate::project_data::save_bundle(state, &doc.0, &path, context) {
        Ok(warnings) => {
            state.project_path = Some(path.clone());
            state.project_dir = path.parent().map(std::path::Path::to_path_buf);
            state.project_saved_serial = state.edit_serial;
            state.status = if warnings.is_empty() {
                format!("Saved {}", project_display_name(&path))
            } else {
                format!(
                    "Saved {} ({} embedded asset warning(s))",
                    project_display_name(&path),
                    warnings.len()
                )
            };
        }
        Err(error) => {
            state.status = format!("Save failed: {error}");
        }
    }
}

/// Load the newest autosave for the current project (or the untitled recovery
/// folder) without making the snapshot the new save target. Recovered work
/// stays marked as unsaved so the next Ctrl+S writes the real project file.
pub(crate) fn recover_latest_autosave(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    cache: &mut EvalCache,
) -> bool {
    let dir = crate::autosave::autosave_dir(state.project_path.as_deref());
    let stem = crate::autosave::snapshot_stem(state.project_path.as_deref());
    match crate::autosave::latest_snapshot(&dir, &stem) {
        Ok(Some(path)) => {
            let save_target = state.project_path.clone();
            if !open_project_file(state, doc, cache, &path) {
                return false;
            }
            state.project_path = save_target.clone();
            state.project_dir = save_target
                .as_ref()
                .and_then(|path| path.parent())
                .map(std::path::Path::to_path_buf);
            // `open_project_file` treats the snapshot as clean. Recovered work
            // is not the project file yet, so keep the unsaved marker.
            state.mark_dirty();
            state.status = format!("Recovered {}", project_display_name(&path));
            true
        }
        Ok(None) => {
            state.status = format!("No autosave found in {}", dir.display());
            false
        }
        Err(error) => {
            state.status = format!("Could not read {}: {error}", dir.display());
            false
        }
    }
}

pub(crate) fn open_mesh_dialog(state: &mut EditorState) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter(
            "3D meshes",
            &[
                "glb", "gltf", "fbx", "obj", "dae", "usd", "usda", "usdc", "usdz",
            ],
        )
        .pick_file()
    {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mesh");
        state.import_status = format!("Queued {name}");
        state.import_request = Some(path);
    }
}

pub(crate) fn open_svg_dialog(state: &mut EditorState) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("SVG vectors", &["svg"])
        .pick_file()
    {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("SVG");
        state.status = format!("Importing {name}...");
        state.svg_import_request = Some(path);
    }
}

pub(crate) fn open_photo_dialog(state: &mut EditorState) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter(
            "Photos",
            &[
                "png", "jpg", "jpeg", "tga", "exr", "hdr", "bmp", "tif", "tiff", "webp",
            ],
        )
        .pick_file()
    {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("photo");
        state.status = format!("Importing photo {name}...");
        state.photo_import_request = Some(path);
    }
}

pub(crate) fn open_alpha_dialog(state: &mut EditorState) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter(
            "Alpha masks",
            &[
                "png", "jpg", "jpeg", "tga", "exr", "hdr", "bmp", "tif", "tiff", "webp",
            ],
        )
        .pick_file()
    {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("alpha");
        state.status = format!("Importing alpha {name}...");
        state.alpha_import_request = Some(path);
    }
}

pub(crate) fn open_normal_dialog(state: &mut EditorState) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter(
            "Normal maps",
            &[
                "png", "jpg", "jpeg", "tga", "exr", "bmp", "tif", "tiff", "webp",
            ],
        )
        .pick_file()
    {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("normal");
        state.status = format!("Importing normal map {name}...");
        state.normal_import_request = Some(path);
    }
}

pub(crate) fn import_alpha_document(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    path: &std::path::Path,
) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("alpha");
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(file_name);
    match ns_core::io::load_alpha(path) {
        Ok(alpha) => {
            let layer = Layer::alpha_height(name, alpha, 1.0);
            state.checkpoint_document(&doc.0);
            add_layer(doc, state, layer);
            state.viewport_tab = ViewportTab::TwoD;
            state.show_height = false;
            state.canvas_tool = CanvasTool::Select;
            state.status = format!("Imported alpha {name}");
        }
        Err(error) => {
            state.status = format!("Alpha import failed: {error}");
        }
    }
}

/// Import an existing normal map as a referenced normal layer. The project's
/// green-channel convention is used as the source convention, so an imported
/// map matches the rest of the document. Shapes added above it blend normally.
pub(crate) fn import_normal_document(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    path: &std::path::Path,
) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("normal");
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(file_name);
    let convention = doc.0.green;
    match ns_core::io::load_normal(path, convention) {
        Ok(normal) => {
            let layer = Layer::imported_normal(name, normal);
            state.checkpoint_document(&doc.0);
            add_layer(doc, state, layer);
            state.viewport_tab = ViewportTab::TwoD;
            state.show_height = false;
            state.canvas_tool = CanvasTool::Select;
            state.status = format!("Imported normal map {name}");
        }
        Err(error) => {
            state.status = format!("Normal map import failed: {error}");
        }
    }
}

pub(crate) fn import_photo_document(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    path: &std::path::Path,
) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("photo");
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(file_name);
    match ns_core::io::load_height(path) {
        Ok(height) => {
            let scale = height.w as f32 / 1024.0;
            let (strength, radii, weights) = PhotoPreset::default().params(scale);
            let layer = Layer::photo_height(name, height, strength, radii, weights);
            state.checkpoint_document(&doc.0);
            add_layer(doc, state, layer);
            state.viewport_tab = ViewportTab::TwoD;
            state.show_height = false;
            state.canvas_tool = CanvasTool::Select;
            state.status = format!("Imported photo {name}");
        }
        Err(error) => {
            state.status = format!("Photo import failed: {error}");
        }
    }
}

pub(crate) fn import_svg_document(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    path: &std::path::Path,
) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("SVG");
    match ns_core::load_svg(path, doc.0.size) {
        Ok(shapes) if !shapes.is_empty() => {
            let scale = doc.0.size.x as f32 / 1024.0;
            let count = shapes.len();
            state.checkpoint_document(&doc.0);
            for shape in shapes {
                let name = if shape.name.trim().is_empty() {
                    file_name.to_string()
                } else {
                    shape.name
                };
                let mut layer = Layer::shape(name, ShapeSource::Path(shape.path));
                if let LayerKind::Shape(shape) = &mut layer.kind {
                    // Imported paths are often detailed; keep the bevel tight.
                    shape.radius_px = 12.0 * scale;
                    shape.height_px = 14.0 * scale;
                }
                add_layer(doc, state, layer);
            }
            state.viewport_tab = ViewportTab::TwoD;
            state.canvas_tool = CanvasTool::Select;
            state.vector_draft = None;
            state.mark_dirty();
            state.status = format!(
                "Imported {count} SVG shape{}",
                if count == 1 { "" } else { "s" }
            );
        }
        Ok(_) => {
            state.status = format!("{file_name}: no filled shapes found");
        }
        Err(error) => {
            state.status = format!("SVG import failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_archive_opens_through_the_editor_loader() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v2.nsproj");
        let document = Document::new_with_base(glam::UVec2::splat(64));
        let state = EditorState::default();
        crate::project_data::save_bundle(&state, &document, &path, None).unwrap();

        let mut loaded_state = EditorState::default();
        let mut loaded_doc = DocumentRes::new(Document::new(glam::UVec2::splat(16)));
        let mut cache = EvalCache::default();
        assert!(open_project_file(
            &mut loaded_state,
            &mut loaded_doc,
            &mut cache,
            &path
        ));
        assert_eq!(loaded_doc.0.size, document.size);
        assert_eq!(loaded_state.project_path.as_deref(), Some(path.as_path()));
        assert!(!loaded_state.has_unsaved_changes());
    }

    #[test]
    fn v2_archive_restores_mesh_and_lightmap_pending_state() {
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
        let path = directory.path().join("mesh.nsproj");
        let document = Document::new_with_base(glam::UVec2::splat(64));
        let mut mesh_import = crate::import::MeshImport::default();
        mesh_import.source_path = Some(mesh_path);
        let lightmap = crate::lightmap::LightmapRuntime::default();
        let bake = crate::bake::BakeRuntime::default();
        let context = crate::project_data::SaveContext {
            mesh_import: &mesh_import,
            lightmap: &lightmap,
            bake: &bake,
        };
        crate::project_data::save_bundle(&EditorState::default(), &document, &path, Some(&context))
            .unwrap();

        let mut state = EditorState::default();
        let mut loaded_doc = DocumentRes::new(Document::new(glam::UVec2::splat(16)));
        let mut cache = EvalCache::default();
        assert!(open_project_file(
            &mut state,
            &mut loaded_doc,
            &mut cache,
            &path
        ));
        assert!(state.import_request.is_some());
        assert!(state.pending_lightmap.is_some());
    }

    #[test]
    fn new_project_replaces_the_document_and_forgets_the_file() {
        let mut doc = DocumentRes::new(ns_core::demo_document(128));
        let mut state = EditorState {
            project_path: Some(std::path::PathBuf::from("demo.nsproj")),
            selected: Some(vec![0]),
            ..Default::default()
        };
        state.mark_dirty();
        state.checkpoint_document(&doc.0);
        let mut cache = EvalCache::default();

        new_project(&mut state, &mut doc, &mut cache);

        assert_eq!(doc.0.size, glam::UVec2::splat(128));
        assert_eq!(doc.0.layers.len(), 1);
        assert_eq!(doc.0.layers[0].name, "base");
        assert!(state.project_path.is_none(), "Save must ask for a path");
        assert!(state.undo.is_empty());
        assert!(state.redo.is_empty());
        assert!(state.selected.is_none());
        assert!(!state.has_unsaved_changes());
        assert!(state.status.starts_with("New project"), "{}", state.status);
    }

    #[test]
    fn udim_export_writes_one_map_set_per_tile() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(16)));
        let tile_a = glam::Vec3::new(0.6, 0.0, 0.8).normalize();
        let tile_b = glam::Vec3::new(0.0, 0.6, 0.8).normalize();
        let mut a = Layer::imported_normal("a", Grid::new(16, 16, tile_a));
        a.tile = Some(1001);
        let mut b = Layer::imported_normal("b", Grid::new(16, 16, tile_b));
        b.tile = Some(1002);
        doc.0.layers = vec![a, b];
        doc.0.active_tile = 1001;
        let mut state = EditorState {
            export_normal: true,
            export_height: false,
            export_project: false,
            export_sixteen_bit: false,
            export_resolution: 0,
            udim_enabled: true,
            udim_tiles: vec![1001, 1002],
            export_all_tiles: true,
            export_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let mut cache = EvalCache::default();

        export_project(&mut state, &mut doc, &mut cache);

        assert!(state.status.starts_with("Exported"), "{}", state.status);
        assert!(
            !dir.path().join("normal.png").exists(),
            "a UDIM export must not write an untiled normal.png"
        );
        let first =
            ns_core::io::load_normal(&dir.path().join("normal_1001.png"), doc.0.green).unwrap();
        let second =
            ns_core::io::load_normal(&dir.path().join("normal_1002.png"), doc.0.green).unwrap();
        assert!((*first.get(8, 8) - tile_a).length() < 0.01);
        assert!((*second.get(8, 8) - tile_b).length() < 0.01);
        assert_eq!(
            doc.0.active_tile, 1001,
            "export must restore the canvas tile"
        );
    }

    #[test]
    fn udim_export_can_write_only_the_active_tile() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(16)));
        let mut a = Layer::imported_normal(
            "a",
            Grid::new(16, 16, glam::Vec3::new(0.6, 0.0, 0.8).normalize()),
        );
        a.tile = Some(1001);
        let mut b = Layer::imported_normal(
            "b",
            Grid::new(16, 16, glam::Vec3::new(0.0, 0.6, 0.8).normalize()),
        );
        b.tile = Some(1002);
        doc.0.layers = vec![a, b];
        doc.0.active_tile = 1002;
        let mut state = EditorState {
            export_normal: true,
            export_height: false,
            export_project: false,
            export_sixteen_bit: false,
            export_resolution: 0,
            udim_enabled: true,
            udim_tiles: vec![1001, 1002],
            export_all_tiles: false,
            export_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let mut cache = EvalCache::default();

        export_project(&mut state, &mut doc, &mut cache);

        assert!(dir.path().join("normal_1002.png").exists());
        assert!(!dir.path().join("normal_1001.png").exists());
        assert_eq!(doc.0.active_tile, 1002);
    }

    #[test]
    fn exported_height_matches_integrated_normals_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = DocumentRes::new(ns_core::demo_document(128));
        let mut state = EditorState {
            export_normal: true,
            export_height: true,
            export_project: false,
            export_height_source: HeightSource::Integrated,
            export_resolution: 0,
            export_sixteen_bit: false,
            export_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let mut cache = EvalCache::default();
        export_project(&mut state, &mut doc, &mut cache);
        assert!(state.status.starts_with("Exported"), "{}", state.status);

        let normals = doc.0.evaluate();
        let expected = normal_to_height(
            &normals,
            &NormalToHeightOptions {
                wrap: doc.0.wrap,
                percentile_low: 0.0,
                percentile_high: 1.0,
                ..Default::default()
            },
        );
        let loaded = ns_core::io::load_height(dir.path().join("height.png")).unwrap();
        assert_eq!((loaded.w, loaded.h), (expected.w, expected.h));
        let mut max_diff = 0.0f32;
        for (saved, source) in loaded.iter().zip(expected.iter()) {
            max_diff = max_diff.max((*saved - *source).abs());
        }
        assert!(
            max_diff <= 1.0 / 255.0 + 1e-4,
            "saved height differed from normal integration by {max_diff}"
        );
    }

    #[test]
    fn direct_height_export_matches_layer_evaluation() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = DocumentRes::new(ns_core::demo_document(128));
        let mut state = EditorState {
            export_normal: false,
            export_height: true,
            export_project: false,
            export_height_source: HeightSource::Direct,
            export_resolution: 0,
            export_sixteen_bit: false,
            export_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let mut cache = EvalCache::default();
        export_project(&mut state, &mut doc, &mut cache);
        assert!(state.status.starts_with("Exported"), "{}", state.status);
        assert!(state.status.contains("height (direct)"), "{}", state.status);

        let saved = ns_core::io::load_height(dir.path().join("height.png")).unwrap();
        let expected = doc.0.evaluate_height_with(&mut EvalCache::default());
        assert_eq!((saved.w, saved.h), (expected.w, expected.h));
        let mut max_diff = 0.0f32;
        for (saved, source) in saved.iter().zip(expected.iter()) {
            max_diff = max_diff.max((*saved - *source).abs());
        }
        assert!(
            max_diff <= 1.0 / 255.0 + 1e-4,
            "direct height differed from the layer evaluator by {max_diff}"
        );
    }

    #[test]
    fn auto_height_integrates_when_imported_normals_are_visible() {
        let dir = tempfile::tempdir().unwrap();
        let mut document = ns_core::demo_document(64);
        document.layers.push(Layer::imported_normal(
            "imported normal",
            Grid::new(64, 64, glam::Vec3::Z),
        ));
        let mut doc = DocumentRes::new(document);
        let mut state = EditorState {
            export_normal: false,
            export_height: true,
            export_project: false,
            export_height_source: HeightSource::Auto,
            export_resolution: 0,
            export_sixteen_bit: false,
            export_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let mut cache = EvalCache::default();
        export_project(&mut state, &mut doc, &mut cache);
        assert!(state.status.starts_with("Exported"), "{}", state.status);
        assert!(
            state.status.contains("height (integrated)"),
            "Auto must integrate an imported-normal stack: {}",
            state.status
        );
    }

    #[test]
    fn derived_normal_maps_export() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = DocumentRes::new(ns_core::demo_document(128));
        let mut state = EditorState {
            export_normal: false,
            export_height: false,
            export_project: false,
            export_edge_wear: true,
            export_curvature: true,
            export_edge_detect: true,
            export_resolution: 0,
            export_sixteen_bit: false,
            export_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let mut cache = EvalCache::default();
        export_project(&mut state, &mut doc, &mut cache);
        assert!(state.status.starts_with("Exported"), "{}", state.status);

        for name in ["curvature.png", "edge_wear.png", "edge_detect.png"] {
            let path = dir.path().join(name);
            assert!(path.exists(), "{name} was not written");
            let map = ns_core::io::load_height(&path).unwrap();
            assert_eq!((map.w, map.h), (128, 128), "{name} size");
            let min = map.iter().cloned().fold(1.0f32, f32::min);
            let max = map.iter().cloned().fold(0.0f32, f32::max);
            assert!(max - min > 0.01, "{name} is flat: {min}..{max}");
        }
    }

    #[test]
    fn text_layers_export_normal_height_and_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = DocumentRes::new(ns_core::demo_text_document(256));
        if let Some(title) = doc
            .0
            .layers
            .iter_mut()
            .find(|layer| layer.name == "N-SHADE")
        {
            title.id_color = Some([0.9, 0.2, 0.2]);
        }
        let mut state = EditorState {
            export_normal: true,
            export_height: true,
            export_id_map: true,
            export_project: false,
            export_height_source: HeightSource::Direct,
            export_resolution: 0,
            export_sixteen_bit: false,
            export_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let mut cache = EvalCache::default();
        export_project(&mut state, &mut doc, &mut cache);
        assert!(state.status.starts_with("Exported"), "{}", state.status);
        for name in ["normal.png", "height.png", "id.png"] {
            assert!(dir.path().join(name).exists(), "{name} was not written");
        }
        let normals =
            ns_core::io::load_normal(&dir.path().join("normal.png"), doc.0.green).unwrap();
        assert!(
            normals.iter().any(|normal| normal.z < 0.99),
            "text glyphs must contribute beveled normals to the export"
        );
    }

    #[test]
    fn id_map_export_keeps_exact_colours_after_resize() {
        let dir = tempfile::tempdir().unwrap();
        let mut document = ns_core::demo_document(64);
        let mut layer = Layer::shape(
            "id panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(32.0),
                size: Vec2::splat(32.0),
                corner_radius: 4.0,
            }),
        );
        layer.id_color = Some([0.9, 0.2, 0.2]);
        document.layers.push(layer);

        let mut doc = DocumentRes::new(document);
        let mut state = EditorState {
            export_normal: false,
            export_height: false,
            export_project: false,
            export_id_map: true,
            export_resolution: 512,
            export_sixteen_bit: false,
            export_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let mut cache = EvalCache::default();
        export_project(&mut state, &mut doc, &mut cache);
        assert!(state.status.starts_with("Exported"), "{}", state.status);

        let image = image::open(dir.path().join("id.png")).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (512, 512));
        // Majority resize means every exported pixel is exactly one of the
        // assigned colours: no averaged hue along the shared silhouettes.
        let mut colors = std::collections::HashSet::new();
        for pixel in image.pixels() {
            colors.insert(pixel.0);
        }
        assert_eq!(colors.len(), 2, "id.png blended colours: {colors:?}");
        assert!(colors.contains(&[0u8, 0, 0]));
        assert!(colors.contains(&[230u8, 51, 51]));
    }

    #[test]
    fn normal_export_switches_between_opengl_and_directx() {
        let gl_dir = tempfile::tempdir().unwrap();
        let dx_dir = tempfile::tempdir().unwrap();
        let mut doc = DocumentRes::new(ns_core::demo_document(64));
        let mut state = EditorState {
            export_normal: true,
            export_height: false,
            export_project: false,
            export_resolution: 512,
            export_sixteen_bit: true,
            ..Default::default()
        };
        let mut cache = EvalCache::default();

        doc.0.green = GreenChannel::OpenGl;
        state.export_dir = Some(gl_dir.path().to_path_buf());
        export_project(&mut state, &mut doc, &mut cache);
        assert!(state.status.contains("OpenGL"), "{}", state.status);

        doc.0.green = GreenChannel::DirectX;
        state.export_dir = Some(dx_dir.path().to_path_buf());
        export_project(&mut state, &mut doc, &mut cache);
        assert!(state.status.contains("DirectX"), "{}", state.status);

        let gl_path = gl_dir.path().join("normal.png");
        let dx_path = dx_dir.path().join("normal.png");
        assert_ne!(
            std::fs::read(&gl_path).unwrap(),
            std::fs::read(&dx_path).unwrap(),
            "the two exports should not be byte-identical"
        );
        let gl = ns_core::io::load_normal(&gl_path, GreenChannel::OpenGl).unwrap();
        let dx = ns_core::io::load_normal(&dx_path, GreenChannel::DirectX).unwrap();
        assert_eq!((gl.w, gl.h), (dx.w, dx.h));
        let mut max_diff = 0.0f32;
        for (a, b) in gl.iter().zip(dx.iter()) {
            max_diff = max_diff.max((*a - *b).length());
        }
        // Decoded with their own convention both files describe the same
        // canonical surface; only the stored green channel differs.
        assert!(
            max_diff < 2e-3,
            "convention round trip drifted by {max_diff}"
        );
    }

    #[test]
    fn recovering_an_autosave_restores_the_document_and_keeps_it_unsaved() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("demo.nsproj");
        let snapshot_dir = dir.path().join("demo.autosave");
        std::fs::create_dir_all(&snapshot_dir).unwrap();

        let mut snapshot = Document::new_with_base(glam::UVec2::splat(64));
        snapshot.layers[0].name = "recovered".into();
        ns_core::io::save_project_ron(snapshot_dir.join("demo-20260101-000000.nsproj"), &snapshot)
            .unwrap();

        let mut state = EditorState {
            project_path: Some(project.clone()),
            ..Default::default()
        };
        let mut doc = DocumentRes::new(Document::new_with_base(glam::UVec2::splat(64)));
        let mut cache = EvalCache::default();

        assert!(recover_latest_autosave(&mut state, &mut doc, &mut cache));

        assert_eq!(doc.0.layers[0].name, "recovered");
        // Ctrl+S must still write the project file, not the snapshot.
        assert_eq!(state.project_path.as_deref(), Some(project.as_path()));
        assert!(
            state.has_unsaved_changes(),
            "recovered work must stay unsaved"
        );
        assert!(state.status.contains("Recovered"), "{}", state.status);
    }

    #[test]
    fn recovering_without_a_snapshot_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = EditorState {
            project_path: Some(dir.path().join("demo.nsproj")),
            ..Default::default()
        };
        let mut doc = DocumentRes::new(Document::new_with_base(glam::UVec2::splat(64)));
        let original = doc.0.layers[0].name.clone();
        let mut cache = EvalCache::default();

        assert!(!recover_latest_autosave(&mut state, &mut doc, &mut cache));

        assert_eq!(doc.0.layers[0].name, original);
        assert!(state.status.contains("No autosave"), "{}", state.status);
    }
}
