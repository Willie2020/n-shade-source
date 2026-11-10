//! Editor shell, shared UI context, and panel orchestration.
//!
//! Rust has no class inheritance; panels share behaviour through the
//! [`Panel`] trait (the equivalent of a base class) and receive all editor
//! resources through [`UiFrame`] (composition instead of inherited fields).

mod actions;
mod bake;
mod blender;
mod canvas;
mod curve_editor;
mod duplicate_array;
mod export;
mod gestures;
mod group_props;
mod icons;
mod labels;
mod layers;
mod lightmap;
mod lightmap_gizmo;
mod mask_editor;
mod mask_props;
mod mesh;
mod operations;
mod preview_gizmo;
mod projection_gizmo;
mod properties;
mod scifi;
mod settings;
mod shortcuts;
mod svg_path;
mod sweep_path;
mod theme;
mod toast;
mod top_bar;
mod viewport;
mod widgets;

use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiTextureHandle};

pub(crate) use bevy_egui::egui;
pub(crate) use glam::Vec2;
pub(crate) use ns_core::*;

pub(crate) use crate::app::*;
pub(crate) use crate::bake::BakeRuntime;
pub(crate) use crate::bridge::{BridgeRuntime, MapId};
pub(crate) use crate::import::MeshImport;
pub(crate) use crate::lightmap::{
    LightGizmoHandle, LightSelection, LightmapFormat, LightmapRuntime,
};
pub(crate) use crate::pbr_ui::PbrRuntime;
pub(crate) use crate::stats::{SystemStats, SystemStatsRes};

pub(crate) use actions::*;
pub(crate) use bake::*;
pub(crate) use blender::*;
pub(crate) use canvas::*;
pub(crate) use curve_editor::*;
pub(crate) use duplicate_array::*;
pub(crate) use export::*;
pub(crate) use gestures::*;
pub(crate) use group_props::*;
pub(crate) use icons::*;
pub(crate) use labels::*;
pub(crate) use layers::*;
pub(crate) use lightmap::*;
pub(crate) use lightmap_gizmo::*;
pub(crate) use mask_editor::*;
pub(crate) use mask_props::*;
pub(crate) use mesh::*;
pub(crate) use operations::*;
pub(crate) use preview_gizmo::*;
pub(crate) use projection_gizmo::*;
pub(crate) use properties::*;
pub(crate) use scifi::*;
pub(crate) use settings::*;
pub(crate) use shortcuts::*;
pub(crate) use svg_path::*;
pub(crate) use sweep_path::*;
pub(crate) use theme::*;
pub(crate) use toast::*;
pub(crate) use top_bar::*;
pub(crate) use viewport::*;
pub(crate) use widgets::*;

/// Shared mutable editor resources passed to every panel.
pub(crate) struct UiFrame<'a> {
    pub state: &'a mut EditorState,
    pub doc: &'a mut DocumentRes,
    pub cache: &'a mut EvalCache,
    pub mesh_import: &'a MeshImport,
    pub pbr: &'a mut PbrRuntime,
    pub bake: &'a mut BakeRuntime,
    pub lightmap: &'a mut LightmapRuntime,
    pub projection: &'a mut crate::projection::ProjectionRuntime,
    pub bridge: &'a mut BridgeRuntime,
    pub system_stats: SystemStats,
    pub normal_tex: egui::TextureId,
    pub height_tex: egui::TextureId,
}

/// Common interface implemented by every top-level UI section.
pub(crate) trait Panel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>);
}

#[allow(clippy::too_many_arguments)]
pub fn editor_ui(
    mut contexts: EguiContexts,
    mut state: ResMut<EditorState>,
    mut doc: ResMut<DocumentRes>,
    mut cache: ResMut<EvalCacheRes>,
    handles: Res<PreviewHandles>,
    mesh_import: Res<MeshImport>,
    mut pbr: ResMut<PbrRuntime>,
    mut bake: ResMut<BakeRuntime>,
    mut lightmap: ResMut<LightmapRuntime>,
    mut projection: ResMut<crate::projection::ProjectionRuntime>,
    mut bridge: ResMut<BridgeRuntime>,
    stats: Res<SystemStatsRes>,
    diagnostics: Option<Res<bevy::diagnostic::DiagnosticsStore>>,
    gpu_upload: Option<Res<crate::gpu::GpuUploadStats>>,
) {
    // The camera viewport is derived from the UI layout. Reset it every frame
    // and let the viewport panel publish it again; an early return or a tiny
    // pane must never leave the camera rendering at a stale rectangle.
    state.preview_rect = None;
    if let Some(path) = state.project_open_request.take() {
        open_project_file(&mut state, &mut doc, &mut cache.0, &path);
    }
    if let Some(save) = state.pending_lightmap.take() {
        crate::project_data::apply_lightmap(&mut lightmap, &save);
    }
    if let Some(path) = state.pending_lightmap_environment.take() {
        lightmap.environment_path = Some(path);
    }
    if let Some(preview) = state.pending_lightmap_preview.take() {
        lightmap.preview_enabled = preview;
    }
    if let Some(save) = state.pending_bake.take() {
        crate::project_data::apply_bake(&mut bake, &save);
    }
    if let Some(path) = state.pending_bake_high.take() {
        bake.high_path = Some(path);
    }
    // Mirror the mesh's UDIM layout so layer creation and export can tag and
    // split per-tile maps without borrowing the mesh resource themselves.
    state.udim_enabled = mesh_import.udim.enabled;
    if state.udim_tiles != mesh_import.udim.tiles {
        state.udim_tiles.clone_from(&mesh_import.udim.tiles);
    }
    let normal_id = match state.normal_tex_id {
        Some(id) => id,
        None => {
            let id = contexts.add_image(EguiTextureHandle::Strong(handles.normal_texture.clone()));
            state.normal_tex_id = Some(id);
            id
        }
    };
    let height_id = match state.height_tex_id {
        Some(id) => id,
        None => {
            let id = contexts.add_image(EguiTextureHandle::Strong(handles.height_texture.clone()));
            state.height_tex_id = Some(id);
            id
        }
    };
    let uv_current = mesh_import.uv_texture.as_ref().map(|handle| handle.id());
    let uv_shown = state.uv_tex_handle.as_ref().map(|handle| handle.id());
    if uv_current != uv_shown {
        if let Some(old) = state.uv_tex_handle.take() {
            let _ = contexts.remove_image(old.id());
        }
        state.uv_tex_handle = mesh_import.uv_texture.clone();
        state.uv_tex_id = mesh_import
            .uv_texture
            .as_ref()
            .map(|texture| contexts.add_image(EguiTextureHandle::Strong(texture.clone())));
        if mesh_import.uv_texture.is_some() && !mesh_import.udim.tiles.contains(&doc.0.active_tile)
        {
            doc.0.active_tile = mesh_import.udim.base_tile();
            state.request_eval_now();
        }
    }
    let inset_current = mesh_import
        .uv_inset_texture
        .as_ref()
        .map(|handle| handle.id());
    let inset_shown = state.uv_inset_tex_handle.as_ref().map(|handle| handle.id());
    if inset_current != inset_shown {
        if let Some(old) = state.uv_inset_tex_handle.take() {
            let _ = contexts.remove_image(old.id());
        }
        state.uv_inset_tex_handle = mesh_import.uv_inset_texture.clone();
        state.uv_inset_tex_id = mesh_import
            .uv_inset_texture
            .as_ref()
            .map(|texture| contexts.add_image(EguiTextureHandle::Strong(texture.clone())));
    }
    if bake.result_tex_id.is_none() {
        if let Some(result) = &bake.result {
            let image = result.image.clone();
            if let Some(old) = bake.result_tex_handle.replace(image.clone()) {
                let _ = contexts.remove_image(old.id());
            }
            bake.result_tex_id = Some(contexts.add_image(EguiTextureHandle::Strong(image)));
        }
    }
    if lightmap.result_tex_id.is_none() {
        if let Some(result) = &lightmap.result {
            let image = result.image.clone();
            if let Some(old) = lightmap.result_tex_handle.replace(image.clone()) {
                let _ = contexts.remove_image(old.id());
            }
            lightmap.result_tex_id = Some(contexts.add_image(EguiTextureHandle::Strong(image)));
        }
    }
    crate::pbr_ui::ensure_texture_ids(&mut contexts, &mut pbr);

    if let Some(path) = state.svg_import_request.take() {
        import_svg_document(&mut doc, &mut state, &path);
    }
    if let Some(path) = state.photo_import_request.take() {
        import_photo_document(&mut doc, &mut state, &path);
    }
    if let Some(path) = state.alpha_import_request.take() {
        import_alpha_document(&mut doc, &mut state, &path);
    }
    if let Some(path) = state.normal_import_request.take() {
        import_normal_document(&mut doc, &mut state, &path);
    }

    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    update_pen_pressure(ctx, &mut state);
    if !state.ui_initialized {
        apply_theme(ctx);
        state.ui_initialized = true;
        if state.show_pbr {
            state.right_tab = RightTab::Pbr;
            state.show_right_dock = true;
            if ctx.viewport_rect().width() < 860.0 {
                state.show_layers_dock = false;
            }
        }
    }
    let mut viewport_ui = egui::Ui::new(
        ctx.clone(),
        "viewport".into(),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );
    // Capture before any shortcut or panel can mutate the document, so the
    // end-of-frame commit sees keyboard-driven edits as well as pointer ones.
    let frame_edit_serial = state.edit_serial;

    let widget_focus = ctx.memory(|memory| memory.focused().is_some());
    let text_focus = ctx.text_edit_focused();
    let focus_elsewhere = widget_focus && !state.canvas_has_focus;
    if !text_focus {
        let redo = ctx.input_mut(|input| {
            input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            ) || input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y)
        });
        if redo {
            state.redo(&mut doc);
        } else {
            let undo =
                ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z));
            if undo {
                state.undo(&mut doc);
            }
        }

        // Layer clipboard and grouping. These are document edits, so grouping
        // and paste take an explicit undo checkpoint (keyboard input does not
        // start a pointer gesture).
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::G)) {
            if state.selected.is_some() {
                apply_layer_action(LayerAction::GroupSelected, &mut state, &mut doc);
            } else {
                state.status = "Select layers to group".into();
            }
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::C)) {
            copy_selected_layers(&mut state, &doc);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::V)) {
            paste_layers(&mut state, &mut doc);
        }

        // Project shortcuts. Check Shift+S before S so the shifted combo is not
        // consumed by the plain save path.
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::N)) {
            new_project(&mut state, &mut doc, &mut cache.0);
        }
        if ctx.input_mut(|input| {
            input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
            )
        }) {
            let context = crate::project_data::SaveContext {
                mesh_import: &*mesh_import,
                lightmap: &*lightmap,
                bake: &*bake,
            };
            save_project_dialog(&mut state, &doc, true, Some(&context));
        } else if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            let context = crate::project_data::SaveContext {
                mesh_import: &*mesh_import,
                lightmap: &*lightmap,
                bake: &*bake,
            };
            save_project_dialog(&mut state, &doc, false, Some(&context));
        } else if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
            open_project_dialog(&mut state, &mut doc, &mut cache.0);
        }

        // Ctrl/Cmd+E exports with the current settings. Export is synchronous,
        // so the toast is the completion signal and carries no Undo action.
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::E)) {
            export_project(&mut state, &mut doc, &mut cache.0);
            if state.status.starts_with("Exported") {
                queue_toast(ctx, "Export finished", false);
            }
        }

        // Right-panel tabs: Ctrl/Cmd+1..5. These stay available even though the
        // tab strip is click-driven, so Mesh/Bake/Lightmap/PBR never require
        // hunting for a small target.
        let tab_keys = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
        ];
        for (key, tab) in tab_keys.into_iter().zip(RightTab::ALL) {
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, key)) {
                state.right_tab = tab;
                state.show_right_dock = true;
            }
        }

        // Shortcut cheat sheet. `?` is Shift+/ on most layouts, so match the
        // logical key rather than requiring an exact modifier set.
        if ctx.input(|input| input.key_pressed(egui::Key::Questionmark)) {
            toggle_shortcuts(ctx);
        }

        // Single-key tool switches. Plain keys only: the
        // command shortcuts above already consumed their events. A popup or an
        // active canvas drag keeps the keys for itself.
        if !ctx.any_popup_open()
            && state.canvas_drag.is_none()
            && state.vector_draft.is_none()
            && state.lasso_draft.is_none()
            && state.mask_edit_layer.is_none()
        {
            let tool_key = |key: egui::Key| {
                ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key))
            };
            if tool_key(egui::Key::V) {
                activate_tool(&mut state, CanvasTool::Select);
            } else if tool_key(egui::Key::R) {
                activate_tool(&mut state, CanvasTool::Rectangle);
            } else if tool_key(egui::Key::O) {
                activate_tool(&mut state, CanvasTool::Ellipse);
            } else if tool_key(egui::Key::S) {
                activate_tool(&mut state, CanvasTool::Star);
            } else if tool_key(egui::Key::G) {
                activate_tool(&mut state, CanvasTool::Polygon);
            } else if tool_key(egui::Key::C) {
                activate_tool(&mut state, CanvasTool::Cable);
            } else if tool_key(egui::Key::T) {
                activate_tool(&mut state, CanvasTool::Text);
            } else if tool_key(egui::Key::P) {
                let tool = state.vector_tool;
                activate_tool(&mut state, tool);
            } else if tool_key(egui::Key::L) {
                let tool = state.lasso_tool;
                activate_tool(&mut state, tool);
            } else if tool_key(egui::Key::B) {
                activate_tool(&mut state, CanvasTool::Paint);
            } else if tool_key(egui::Key::H) {
                state.canvas_pan_tool = !state.canvas_pan_tool;
                state.vector_draft = None;
                state.lasso_draft = None;
                state.viewport_tab = ViewportTab::TwoD;
            }
        }
    }

    // Context menus and other secondary-button clicks must not create undo
    // gestures: they can clone the whole document without editing it. Gestures
    // now start at the edit site (canvas press, properties press, layer-row
    // controls, actions), so a plain selection click is free.

    // Delete/Backspace removes the selected layers, the same as the row menu
    // does. Text fields, node editing (which deletes anchors), an in-progress
    // pen draft and an open mask edit keep the key for themselves.
    let layer_delete_key = !text_focus
        && (!widget_focus || state.canvas_has_focus)
        && delete_key_deletes_layers(&state)
        && state.lasso_draft.is_none()
        && ctx.input(|input| {
            input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
        });
    if layer_delete_key {
        delete_selected_layers(&mut state, &mut doc);
        queue_toast(ctx, "Layer deleted", true);
    }

    // Arrow keys nudge: the selected layers, or the selected anchors while a
    // node tool is active. One document pixel per press, ten with Shift, and
    // 0.1 with Alt/Ctrl for sub-pixel tuning. Auto-repeat is allowed, and the
    // first press of a burst takes the undo checkpoint so a held key still
    // undoes as a single step.
    let arrows_down = ctx.input(|input| {
        input.key_down(egui::Key::ArrowLeft)
            || input.key_down(egui::Key::ArrowRight)
            || input.key_down(egui::Key::ArrowUp)
            || input.key_down(egui::Key::ArrowDown)
    });
    if !arrows_down {
        state.nudge_burst = false;
    }
    let popup_open = ctx.any_popup_open();
    if !text_focus
        && !popup_open
        && !focus_elsewhere
        && state.canvas_drag.is_none()
        && state.vector_draft.is_none()
        && state.lasso_draft.is_none()
        && state.mask_edit_layer.is_none()
        && !state.canvas_pan_tool
    {
        let fine = ctx.input(|input| input.modifiers.alt || input.modifiers.command);
        let shift = ctx.input(|input| input.modifiers.shift);
        let step = if fine {
            0.1
        } else if shift {
            10.0
        } else {
            1.0
        };
        let pressed = ctx.input(|input| {
            [
                (egui::Key::ArrowLeft, Vec2::new(-1.0, 0.0), "left"),
                (egui::Key::ArrowRight, Vec2::new(1.0, 0.0), "right"),
                (egui::Key::ArrowUp, Vec2::new(0.0, -1.0), "up"),
                (egui::Key::ArrowDown, Vec2::new(0.0, 1.0), "down"),
            ]
            .into_iter()
            .find(|(key, _, _)| input.key_pressed(*key))
        });
        if let Some((_, direction, label)) = pressed {
            let checkpoint = !state.nudge_burst;
            let moved = if state.canvas_tool.is_node_edit_tool() && !state.node_selection.is_empty()
            {
                nudge_selected_nodes(&mut state, &mut doc, direction * step, checkpoint)
            } else {
                // A whole path/line selected in vector mode still nudges as a
                // layer when no anchor is selected.
                nudge_selection(&mut state, &mut doc, direction * step, checkpoint)
            };
            if moved {
                state.nudge_burst = true;
                let step_text = if step < 1.0 {
                    format!("{step:.1}")
                } else {
                    format!("{step:.0}")
                };
                state.status = format!("Nudged {step_text} px {label}");
            }
        }
    }

    let screen_width = ctx.viewport_rect().width();
    // Docks are draggable like Blender editors. They open at the reference
    // widths and can be squeezed or widened from their inner edge; the cap
    // keeps one dock from swallowing the canvas.
    let left_default = 420.0_f32.min(screen_width * 0.34);
    let right_default = 420.0_f32.min(screen_width * 0.36);
    let left_max = (screen_width * 0.40).max(left_default);
    let right_max = (screen_width * 0.40).max(right_default);
    // Below this width both docks would squeeze the viewport to nothing, so
    // keep one dock open at a time. The toolbar toggles swap them.
    let compact_docks = screen_width < 860.0;
    let (show_layers, show_right) = if compact_docks {
        match (state.show_layers_dock, state.show_right_dock) {
            (true, true) => {
                if state.right_tab == RightTab::Properties {
                    (true, false)
                } else {
                    (false, true)
                }
            }
            (layers, right) => (layers, right),
        }
    } else {
        (state.show_layers_dock, state.show_right_dock)
    };
    // Bevy renders the 3D scene through the central panel, so that frame must
    // stay transparent. The 2D canvas draws its own texture and can sit on the
    // viewport well fill.
    let central_frame = if state.viewport_tab == ViewportTab::ThreeD || state.split_view {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(8))
            .outer_margin(egui::Margin::same(4))
    } else {
        viewport_frame()
    };

    let mut frame = UiFrame {
        state: &mut state,
        doc: &mut doc,
        cache: &mut cache.0,
        mesh_import: &mesh_import,
        pbr: &mut pbr,
        bake: &mut bake,
        lightmap: &mut lightmap,
        projection: &mut projection,
        bridge: bridge.as_mut(),
        system_stats: stats.snapshot(),
        normal_tex: normal_id,
        height_tex: height_id,
    };

    egui::Panel::top("app_top_bar")
        .exact_size(60.0)
        .frame(
            egui::Frame::new()
                .fill(CONTAINER_LOW)
                .inner_margin(egui::Margin::symmetric(20, 8)),
        )
        .show_separator_line(true)
        .show(&mut viewport_ui, |ui| TopBarPanel.show(ui, &mut frame));

    egui::Panel::bottom("app_status_bar")
        .exact_size(30.0)
        .frame(
            egui::Frame::new()
                .fill(SURFACE)
                .corner_radius(egui::CornerRadius::same(RADIUS_SM))
                .inner_margin(egui::Margin::symmetric(12, 2))
                .outer_margin(egui::Margin::same(4)),
        )
        .show_separator_line(false)
        .show(&mut viewport_ui, |ui| StatusBarPanel.show(ui, &mut frame));

    egui::Panel::left("tool_rail")
        .resizable(false)
        .exact_size(72.0)
        .frame(
            egui::Frame::new()
                .fill(CONTAINER_LOW)
                .inner_margin(egui::Margin::symmetric(4, 12)),
        )
        .show_separator_line(true)
        .show(&mut viewport_ui, |ui| ToolRailPanel.show(ui, &mut frame));

    if show_layers {
        egui::Panel::left("layers_panel")
            .resizable(true)
            .default_size(left_default)
            .size_range(200.0..=left_max)
            .frame(dock_frame())
            .show_separator_line(true)
            .show(&mut viewport_ui, |ui| left_panel(ui, &mut frame));
    }

    if show_right {
        egui::Panel::right("properties_panel")
            .resizable(true)
            .default_size(right_default)
            .size_range(240.0..=right_max)
            .frame(right_dock_frame())
            .show_separator_line(true)
            .show(&mut viewport_ui, |ui| right_panel(ui, &mut frame));
    }

    egui::CentralPanel::default()
        .frame(central_frame)
        .show(&mut viewport_ui, |ui| ViewportPanel.show(ui, &mut frame));
    if frame.state.viewport_tab == ViewportTab::ThreeD && !frame.state.split_view {
        frame.state.canvas_has_focus = false;
    }

    if frame.state.show_settings {
        settings_window(ctx, frame.state, frame.bridge);
    }
    if frame.state.show_bridge {
        blender_window(ctx, frame.state, frame.bridge, frame.pbr);
    }
    if frame.state.curve_editor.is_some() {
        curve_editor_window(ctx, frame.state, frame.doc);
    }
    if frame.state.show_duplicate_array {
        duplicate_array_window(ctx, frame.state, frame.doc);
    }
    if frame.state.scifi.open {
        scifi_window(ctx, &mut frame.state.scifi);
    }
    let gpu_eval_ms = gpu_timing(&diagnostics, "ns_gpu_eval");
    let gpu_height_ms = gpu_timing(&diagnostics, "ns_gpu_height");
    let gpu_upload_ms = gpu_upload.as_ref().map(|stats| f64::from(stats.last_ms()));
    perf_window(
        ctx,
        frame.state,
        &frame.doc.0,
        gpu_eval_ms,
        gpu_height_ms,
        gpu_upload_ms,
    );
    shortcuts_ui(ctx);

    // Toasts sit above every panel; Undo needs the document resource, so the
    // widget only reports the click.
    if toast_ui(ctx) {
        frame.state.undo(frame.doc);
    }

    if frame.state.edit_serial != frame_edit_serial {
        frame.state.commit_undo_gesture();
    }
    if !ctx.input(|input| input.pointer.any_down()) && !ctx.text_edit_focused() {
        frame.state.finish_undo_gesture();
    }

    drop(frame);
    // The operation preview spans every layer's enabled stack, so it must be
    // refreshed even when the Properties tab is hidden or the selection moved
    // to a layer without operations.
    sync_operation_preview(&mut state, &doc);
}

fn gpu_timing(store: &Option<Res<bevy::diagnostic::DiagnosticsStore>>, pass: &str) -> Option<f64> {
    let store = store.as_ref()?;
    let path = bevy::diagnostic::DiagnosticPath::from_components(["render", pass, "elapsed_gpu"]);
    store.get(&path).and_then(|diagnostic| diagnostic.value())
}

pub(crate) fn left_panel(ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
    // The export card sizes itself to its content (non-resizable panels keep
    // last frame's content height), so wrapped chips never clip the primary
    // action. It starts collapsed so the layer stack keeps a usable height.
    let dock_height = ui.available_height();
    let export_max = (dock_height - LAYERS_MIN_HEIGHT).max(EXPORT_COLLAPSED_HEIGHT);
    egui::Panel::bottom("left_export_panel")
        .resizable(false)
        .default_size(EXPORT_COLLAPSED_HEIGHT)
        .size_range(0.0..=export_max)
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            export_card(ui, frame.state, frame.doc, frame.cache, frame.bridge)
        });

    egui::Panel::bottom("layer_footer_panel")
        .resizable(false)
        .exact_size(34.0)
        .frame(egui::Frame::NONE)
        .show(ui, |ui| LayerFooterPanel.show(ui, frame));

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| LayersPanel.show(ui, frame));
}

/// Height the layer stack keeps before the export card stops growing.
const LAYERS_MIN_HEIGHT: f32 = 150.0;
/// Header, primary button and the Send-to-Blender row of the collapsed card.
const EXPORT_COLLAPSED_HEIGHT: f32 = 108.0;

pub(crate) fn right_panel(ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
    right_tab_strip(ui, &mut frame.state.right_tab);
    ui.add_space(10.0);

    let tab = frame.state.right_tab;
    egui::ScrollArea::vertical()
        // Each tab keeps its own scroll offset, so switching from a long Bake
        // card to Properties starts at the top instead of mid-section.
        .id_salt(("right_dock_scroll", tab.label()))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            inspector_style(ui);
            // Leave room for the scrollbar so right-aligned labels never clip.
            ui.set_width((ui.available_width() - 8.0).max(80.0));
            match tab {
                RightTab::Properties => PropertiesPanel.show(ui, frame),
                RightTab::Mesh => MeshPanel.show(ui, frame),
                RightTab::Bake => BakePanel.show(ui, frame),
                RightTab::Lightmap => LightmapPanel.show(ui, frame),
                RightTab::Pbr => crate::pbr_ui::pbr_panel(ui, frame.pbr, frame.bridge),
            }
        });
}

fn right_tab_icon(tab: RightTab) -> Icon {
    match tab {
        RightTab::Properties => Icon::Sliders,
        RightTab::Mesh => Icon::Cube,
        RightTab::Bake => Icon::Bake,
        RightTab::Lightmap => Icon::Sun,
        RightTab::Pbr => Icon::Pbr,
    }
}

pub(crate) fn right_tab_strip(ui: &mut egui::Ui, current: &mut RightTab) {
    let spacing = 2.0;
    let count = RightTab::ALL.len() as f32;
    // 36 px is enough for the compact labels; lower than that the strip would
    // push past the dock's minimum width and clip the last tab.
    let tab_width = ((ui.available_width() - spacing * (count - 1.0)) / count).max(36.0);
    let height = 54.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = spacing;
        for (index, tab) in RightTab::ALL.into_iter().enumerate() {
            let selected = *current == tab;
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(tab_width, height), egui::Sense::click());
            let painter = ui.painter();

            if !selected && response.hovered() {
                painter.rect_filled(
                    rect,
                    egui::CornerRadius {
                        nw: RADIUS_SM,
                        ne: RADIUS_SM,
                        sw: 0,
                        se: 0,
                    },
                    egui::Color32::from_white_alpha(8),
                );
            }

            let ink = if selected { PRIMARY } else { INSPECTOR_MUTED };
            let icon_rect = egui::Rect::from_center_size(
                egui::pos2(rect.center().x, rect.top() + 17.0),
                egui::vec2(17.0, 17.0),
            );
            draw_icon(painter, icon_rect, right_tab_icon(tab), ink);

            let font = if selected {
                bold(12.0)
            } else {
                egui::FontId::proportional(12.0)
            };
            // Measure instead of guessing: "Properties" fits in the default
            // dock, but must collapse before it clips on narrow ones.
            let fits = painter
                .layout_no_wrap(tab.label().to_owned(), font.clone(), ink)
                .size()
                .x
                <= tab_width - 4.0;
            let label = if fits {
                tab.label()
            } else {
                compact_tab_label(tab)
            };
            let galley = painter.layout_no_wrap(label.to_owned(), font, ink);
            painter.galley(
                egui::pos2(
                    rect.center().x - galley.size().x * 0.5,
                    rect.bottom() - 9.0 - galley.size().y,
                ),
                galley,
                ink,
            );
            if selected {
                // Reference tabs: a 2 px accent underline, no selected chip.
                painter.hline(
                    rect.x_range(),
                    rect.bottom() - 1.0,
                    egui::Stroke::new(2.0_f32, PRIMARY),
                );
            }

            let response = response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(format!("{} (Ctrl+{})", tab.label(), index + 1));
            if response.clicked() {
                *current = tab;
            }
        }
    });
    let bottom = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        bottom,
        egui::Stroke::new(1.0_f32, INSPECTOR_LINE),
    );
}

/// Shortest unambiguous label for a right-dock tab when the full name does not
/// fit the measured slot.
fn compact_tab_label(tab: RightTab) -> &'static str {
    match tab {
        RightTab::Properties => "Props",
        RightTab::Lightmap => "Light",
        other => other.label(),
    }
}

/// Whether Delete/Backspace should remove layers instead of anchors or a draft
/// node. Node editing owns the key while a node tool is active, and while a
/// stroke or a mask edit is in progress the key belongs to that gesture.
fn delete_key_deletes_layers(state: &EditorState) -> bool {
    state.selected.is_some()
        && !state.canvas_tool.is_node_edit_tool()
        && state.vector_draft.is_none()
        && state.mask_edit_layer.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_state() -> EditorState {
        EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        }
    }

    #[test]
    fn delete_removes_layers_for_a_plain_selection() {
        assert!(delete_key_deletes_layers(&selected_state()));
    }

    #[test]
    fn delete_is_left_alone_without_a_selection() {
        assert!(!delete_key_deletes_layers(&EditorState::default()));
    }

    #[test]
    fn node_editing_keeps_the_delete_key_for_anchors() {
        let state = EditorState {
            canvas_tool: CanvasTool::Node,
            ..selected_state()
        };
        assert!(!delete_key_deletes_layers(&state));
    }

    #[test]
    fn a_pen_draft_or_mask_edit_keeps_the_delete_key() {
        let drafting = EditorState {
            vector_draft: Some(VectorDraft::default()),
            ..selected_state()
        };
        assert!(!delete_key_deletes_layers(&drafting));

        let masking = EditorState {
            mask_edit_layer: Some(LayerId::nil()),
            ..selected_state()
        };
        assert!(!delete_key_deletes_layers(&masking));
    }
}
