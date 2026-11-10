//! Central 3D/2D viewport shell and headers.

use super::*;

pub(crate) struct ViewportPanel;

impl Panel for ViewportPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        viewport_panel(
            ui,
            frame.state,
            frame.doc,
            frame.normal_tex,
            frame.height_tex,
            frame.mesh_import,
            frame.lightmap,
            frame.projection,
        );
    }
}

pub(crate) fn viewport_panel(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    normal_id: egui::TextureId,
    height_id: egui::TextureId,
    mesh_import: &MeshImport,
    lightmap: &mut LightmapRuntime,
    projection: &mut crate::projection::ProjectionRuntime,
) {
    if state.show_icon_sheet {
        icon_sheet(ui);
        return;
    }

    // A tab switch cross-fades the two views. Deriving the switch from the tab
    // itself means header clicks, shortcuts and programmatic switches all
    // animate without every call site having to announce the change.
    let now = ui.input(|input| input.time);
    if state.split_view || state.mask_edit_layer.is_some() {
        state.view_transition = None;
    } else if state.viewport_tab != state.last_viewport_tab {
        state.view_transition = Some(ViewTransition {
            from: state.last_viewport_tab,
            started: now,
        });
    }
    state.last_viewport_tab = state.viewport_tab;
    let blend = view_blend(state, now);
    if blend.is_some() {
        // The fade is time-driven, so keep frames coming until it lands.
        ui.ctx().request_repaint();
    }

    // The bar is its own 60 px panel: panel tone, 10 px side padding and the
    // theme's hairline separator along the bottom (guide §5).
    egui::Panel::top("viewport_bar")
        .exact_size(VIEWPORT_BAR_HEIGHT)
        .resizable(false)
        .show_separator_line(true)
        .frame(
            egui::Frame::new()
                .fill(CONTAINER_LOW)
                .inner_margin(egui::Margin::symmetric(10, 0)),
        )
        .show(ui, |ui| {
            viewport_header(ui, state, doc, mesh_import, projection)
        });
    if state.viewport_tab == ViewportTab::TwoD || state.mask_edit_layer.is_some() {
        mask_toolbar(ui, state, doc);
        node_options_bar(ui, state, doc);
        if state.mask_edit_layer.is_none() {
            ruler_options_bar(ui, state, doc);
            lasso_options_bar(ui, state);
        }
    }
    ui.add_space(4.0);
    if state.split_view {
        viewport_split(
            ui,
            state,
            doc,
            normal_id,
            height_id,
            mesh_import,
            lightmap,
            projection,
        );
    } else if let Some(alpha) = blend {
        // The viewport area is shared by both views during the fade.
        let area = ui.available_rect_before_wrap();
        if state.viewport_tab == ViewportTab::ThreeD {
            viewport_3d(ui, state, doc, mesh_import, false, lightmap, projection);
        } else {
            // Keep the 3D preview rendering under the canvas while it fades in.
            state.preview_rect = Some(area);
        }
        draw_canvas_transition(
            ui,
            state,
            doc,
            normal_id,
            height_id,
            mesh_import,
            area,
            alpha,
        );
    } else {
        match state.viewport_tab {
            ViewportTab::ThreeD => {
                viewport_3d(ui, state, doc, mesh_import, false, lightmap, projection)
            }
            ViewportTab::TwoD => {
                state.preview_rect = None;
                canvas_view(ui, state, doc, normal_id, height_id, mesh_import);
            }
        }
    }
}

/// Duration of the 3D <-> 2D switch, in seconds.
const VIEW_TRANSITION_SECS: f64 = 0.24;
/// How far the incoming view slides while it fades, in screen pixels.
const VIEW_TRANSITION_SLIDE: f32 = 16.0;

/// Opacity for the canvas during a running transition: it fades in when the
/// canvas is the target and out when the 3D preview is.
fn view_blend(state: &mut EditorState, now: f64) -> Option<f32> {
    let transition = state.view_transition?;
    let progress = ((now - transition.started) / VIEW_TRANSITION_SECS).clamp(0.0, 1.0) as f32;
    if progress >= 1.0 {
        state.view_transition = None;
        return None;
    }
    // Smoothstep: no jolt at either end.
    let eased = progress * progress * (3.0 - 2.0 * progress);
    Some(if state.viewport_tab == ViewportTab::TwoD {
        eased
    } else {
        1.0 - eased
    })
}

/// Draw the 2D canvas over the preview area at `alpha`, sliding into place as
/// it fades. Painting into an `Area` keeps it above the Bevy-rendered preview.
#[allow(clippy::too_many_arguments)]
fn draw_canvas_transition(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    normal_id: egui::TextureId,
    height_id: egui::TextureId,
    mesh_import: &MeshImport,
    area: egui::Rect,
    alpha: f32,
) {
    if area.width() < 1.0 || area.height() < 1.0 {
        return;
    }
    let offset = egui::vec2(0.0, (1.0 - alpha) * VIEW_TRANSITION_SLIDE);
    egui::Area::new(egui::Id::new("viewport_view_transition"))
        .order(egui::Order::Middle)
        .fixed_pos(area.min + offset)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(area);
            ui.set_opacity(alpha);
            ui.allocate_ui_with_layout(
                area.size(),
                egui::Layout::top_down(egui::Align::Min),
                |ui| canvas_view(ui, state, doc, normal_id, height_id, mesh_import),
            );
        });
}

/// Smallest usable split pane before the divider stops. Keeps the canvas
/// controls and the 3D preview from collapsing into a strip.
const MIN_SPLIT_PANE: f32 = 150.0;
/// Width of the draggable divider between the split panes.
const SPLIT_GAP: f32 = 8.0;

/// Tile the viewport into a 3D pane, a divider and a 2D pane. The divider is a
/// real gutter in the layout, so neither pane paints underneath it.
fn split_panes(
    area: egui::Rect,
    ratio: f32,
    stacked: bool,
) -> (egui::Rect, egui::Rect, egui::Rect) {
    let span = if stacked { area.height() } else { area.width() };
    let usable = (span - SPLIT_GAP).max(1.0);
    let min = (MIN_SPLIT_PANE / usable).clamp(0.0, 0.5);
    let ratio = ratio.clamp(min, 1.0 - min);
    let first = usable * ratio;
    if stacked {
        let three = egui::Rect::from_min_size(area.min, egui::vec2(area.width(), first));
        let gutter = egui::Rect::from_min_size(
            egui::pos2(area.min.x, three.bottom()),
            egui::vec2(area.width(), SPLIT_GAP),
        );
        let two = egui::Rect::from_min_max(egui::pos2(area.min.x, gutter.bottom()), area.max);
        (three, gutter, two)
    } else {
        let three = egui::Rect::from_min_size(area.min, egui::vec2(first, area.height()));
        let gutter = egui::Rect::from_min_size(
            egui::pos2(three.right(), area.min.y),
            egui::vec2(SPLIT_GAP, area.height()),
        );
        let two = egui::Rect::from_min_max(egui::pos2(gutter.right(), area.min.y), area.max);
        (three, gutter, two)
    }
}

/// Draw the 3D preview and the 2D canvas together. The focused pane owns the
/// header controls; the panes themselves stay interactive so neither view is a
/// thumbnail.
pub(crate) fn viewport_split(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    normal_id: egui::TextureId,
    height_id: egui::TextureId,
    mesh_import: &MeshImport,
    lightmap: &mut LightmapRuntime,
    projection: &mut crate::projection::ProjectionRuntime,
) {
    let size = ui.available_size();
    if size.x < 80.0 || size.y < 80.0 {
        return;
    }
    let (area, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter().with_clip_rect(area);
    let stacked = state.split_stacked;
    let (three_rect, gutter_rect, two_rect) = split_panes(area, state.split_ratio, stacked);
    // A wider invisible hit strip makes a 8 px divider comfortable to grab.
    let gutter_hit = if stacked {
        gutter_rect.expand2(egui::vec2(0.0, 4.0))
    } else {
        gutter_rect.expand2(egui::vec2(4.0, 0.0))
    };

    // A press inside either pane focuses it, which also swaps the contextual
    // toolbar above. The divider stays neutral.
    let pressed = ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary));
    if pressed {
        if let Some(pos) = ui.input(|input| input.pointer.interact_pos()) {
            if !gutter_hit.contains(pos) {
                if three_rect.contains(pos) {
                    state.viewport_tab = ViewportTab::ThreeD;
                } else if two_rect.contains(pos) {
                    state.viewport_tab = ViewportTab::TwoD;
                }
            }
        }
    }

    // The 3D pane stays transparent: the Bevy camera viewport is clipped to
    // exactly this rect and renders behind egui. The 2D pane paints its own
    // surface, so the window clear colour never shows through it.
    painter.rect_filled(gutter_rect, 0.0, APP_BG);
    painter.rect_filled(two_rect, RADIUS_XS as f32, SURFACE_LOWEST);

    state.preview_rect = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(three_rect), |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(three_rect));
        viewport_3d(ui, state, doc, mesh_import, true, lightmap, projection);
    });
    ui.scope_builder(egui::UiBuilder::new().max_rect(two_rect), |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(two_rect));
        canvas_view(ui, state, doc, normal_id, height_id, mesh_import);
    });

    let mode = if state.mask_edit_layer.is_some() {
        "Mask"
    } else if state.show_height {
        "Height"
    } else {
        "Normal"
    };
    draw_pane_badge(&painter, two_rect, &format!("2D · {mode}"));

    // The focused pane gets an accent border; the resting panes rely on the
    // viewport well and the gutter instead of an outline.
    let focus_rect = if state.viewport_tab == ViewportTab::ThreeD {
        three_rect
    } else {
        two_rect
    };
    painter.rect_stroke(
        focus_rect,
        4.0,
        egui::Stroke::new(1.5_f32, ACCENT),
        egui::StrokeKind::Inside,
    );

    let response = ui.interact(
        gutter_hit,
        ui.id().with("viewport_split_gutter"),
        egui::Sense::click_and_drag(),
    );
    let response = response.on_hover_cursor(if stacked {
        egui::CursorIcon::ResizeVertical
    } else {
        egui::CursorIcon::ResizeHorizontal
    });
    let response = response.on_hover_text("Drag to resize · double-click to reset");
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let span = if stacked { area.height() } else { area.width() };
            let usable = (span - SPLIT_GAP).max(1.0);
            let min = (MIN_SPLIT_PANE / usable).clamp(0.0, 0.5);
            let raw = if stacked {
                (pos.y - area.top()) / usable
            } else {
                (pos.x - area.left()) / usable
            };
            state.split_ratio = raw.clamp(min, 1.0 - min);
        }
    }
    if response.double_clicked() {
        state.split_ratio = 0.5;
    }

    let active = response.hovered() || response.dragged();
    painter.rect_filled(gutter_rect, 0.0, if active { HOVER_BG } else { APP_BG });
    let center = gutter_rect.center();
    let line = if stacked {
        [
            egui::pos2(gutter_rect.left(), center.y),
            egui::pos2(gutter_rect.right(), center.y),
        ]
    } else {
        [
            egui::pos2(center.x, gutter_rect.top()),
            egui::pos2(center.x, gutter_rect.bottom()),
        ]
    };
    painter.line_segment(
        line,
        egui::Stroke::new(1.0_f32, if active { ACCENT_SOFT } else { BORDER }),
    );
    let handle = if stacked {
        egui::Rect::from_center_size(center, egui::vec2(38.0, 4.0))
    } else {
        egui::Rect::from_center_size(center, egui::vec2(4.0, 38.0))
    };
    painter.rect_filled(handle, 1.5, if active { ACCENT } else { MUTED });
}

fn draw_pane_badge(painter: &egui::Painter, rect: egui::Rect, text: &str) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(TEXT_SMALL),
        TEXT,
    );
    let badge = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(10.0, 10.0),
        galley.size() + egui::vec2(12.0, 7.0),
    );
    painter.rect_filled(badge, 4.0, egui::Color32::from_black_alpha(150));
    painter.galley(badge.min + egui::vec2(6.0, 3.5), galley, TEXT);
}

/// How much of the viewport bar the select and the contextual icon buttons may
/// use: returns the select width (0 = hide it) and how many 34 px icon buttons
/// fit beside it while keeping room for the fit button on the right.
///
/// The reference layout (230 px select + four icons) needs about 430 px after
/// the tabs. The editor also runs at 1440 x 780 with both docks open, where
/// only ~250 px are left, so the select collapses to 112 px before either
/// control is dropped: the channel/mode select is the primary control.
pub(crate) fn bar_budget(room: f32) -> (f32, usize) {
    const WIDE_SELECT: f32 = 230.0;
    const COMPACT_SELECT: f32 = 112.0;
    // 40 px per icon (34 + 6 gap), the select, and the 34 px fit button plus
    // its 6 px gap, with 6 px of slack so glyphs never touch the separator.
    let cost = |icons: usize, select: f32| icons as f32 * 40.0 + select + 46.0;
    for (icons, select) in [
        (4, WIDE_SELECT),
        (3, WIDE_SELECT),
        (2, WIDE_SELECT),
        (2, COMPACT_SELECT),
        (1, COMPACT_SELECT),
        (0, COMPACT_SELECT),
    ] {
        if room >= cost(icons, select) {
            return (select, icons);
        }
    }
    (0.0, 0)
}

/// Short label for the 3D shape select when the bar is compact: the full name
/// would widen the combo past its budget and push the fit button off the bar.
fn compact_shape_label(shape: PreviewShape) -> &'static str {
    match shape {
        PreviewShape::Cube => "Cube",
        PreviewShape::RoundedCube => "Rnd Cube",
        PreviewShape::Plane => "Plane",
        PreviewShape::Cylinder => "Cylinder",
        PreviewShape::Imported => "Imported",
    }
}

pub(crate) fn viewport_header(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    mesh_import: &MeshImport,
    projection: &mut crate::projection::ProjectionRuntime,
) {
    let compact = ui.available_width() < 330.0;
    let content_size = ui.available_size();
    ui.horizontal_centered(|ui| {
        // ── View tabs: 4 px apart, then the bar's 6 px gap to the controls ──
        ui.spacing_mut().item_spacing.x = 4.0;
        let (label_3d, label_2d) = if compact {
            ("3D", "2D")
        } else {
            ("3D\nView", "2D\nCanvas")
        };
        if bar_tab(
            ui,
            label_3d,
            !state.split_view && state.viewport_tab == ViewportTab::ThreeD,
        )
        .on_hover_text(if state.split_view {
            "Focus the 3D pane"
        } else {
            ViewportTab::ThreeD.label()
        })
        .clicked()
        {
            state.viewport_tab = ViewportTab::ThreeD;
            if state.dirty {
                state.request_eval_now();
            }
        }
        if bar_tab(
            ui,
            label_2d,
            !state.split_view && state.viewport_tab == ViewportTab::TwoD,
        )
        .on_hover_text(if state.split_view {
            "Focus the 2D pane"
        } else {
            ViewportTab::TwoD.label()
        })
        .clicked()
        {
            state.viewport_tab = ViewportTab::TwoD;
        }
        if bar_tab(ui, "Split", state.split_view)
            .on_hover_text(if state.split_view {
                "Exit split view (the focused pane fills the viewport)"
            } else {
                "Show the 3D preview and 2D canvas together"
            })
            .clicked()
        {
            state.split_view = !state.split_view;
            if state.split_view {
                // Prefer columns on a wide central area and stack when the
                // viewport is taller than it is wide, where two columns would
                // leave both panes unnecessarily thin.
                state.split_stacked = content_size.x < content_size.y * 0.95;
                if state.dirty {
                    state.request_eval_now();
                }
                state.status = "Split view · drag the divider to resize".into();
            } else {
                state.status = state.viewport_tab.label().into();
            }
        }
        let projecting = projection.enabled;
        if bar_glyph_button(
            ui,
            Icon::Projection,
            projecting,
            true,
            if projecting {
                "Close 3D projection (Esc)"
            } else {
                "3D projection · move, scale and rotate the selected layer on the model; Apply bakes it back to the 2D canvas"
            },
        )
        .clicked()
        {
            if projection.enabled {
                projection.disable();
                state.status = "3D projection closed".into();
            } else if mesh_import.source_path.is_none() {
                state.status = "3D projection needs an imported mesh (Mesh tab)".into();
            } else if let Some(selected) = state
                .selected
                .as_deref()
                .and_then(|path| doc.0.layer_by_path(path))
                .cloned()
            {
                if crate::projection::supports_projection(&selected) && !selected.is_path_guide() {
                    projection.enable_for(selected.id);
                    state.preview_gizmo = PreviewGizmo::Off;
                    state.sweep_path_edit = false;
                    state.sweep_path_drag = None;
                    state.viewport_tab = ViewportTab::ThreeD;
                    state.show_right_dock = true;
                    state.right_tab = RightTab::Properties;
                    state.status =
                        "3D projection · drag the cage handles · Apply bakes to 2D".into();
                } else {
                    state.status =
                        "3D projection: select a shape, group or normal layer".into();
                }
            } else {
                state.status = "3D projection: select a layer first".into();
            }
        }
        // Sweep-path handles are only meaningful while an enabled Sweep entry
        // and an imported surface both exist. The icon sits right after the
        // projection cage so the two on-model tools read as one group.
        let sweep_guide = sweep_path_target(state, &doc.0);
        let can_sweep_path = sweep_guide.is_some() && !mesh_import.surface.is_empty();
        if state.sweep_path_edit && !can_sweep_path {
            state.sweep_path_edit = false;
            state.sweep_path_drag = None;
            state.sweep_path_hover = None;
            state.sweep_path_readout.clear();
        }
        if sweep_guide.is_some() {
            let sweep_tip = if state.sweep_path_edit {
                "Stop editing the sweep path on the model".to_string()
            } else if mesh_import.surface.is_empty() {
                "Sweep path: import a UV-mapped mesh first".to_string()
            } else {
                "Edit the sweep path on the model · drag anchors and Bezier handles, \
                 snapped to the surface · Alt breaks handles"
                    .to_string()
            };
            if bar_glyph_button(
                ui,
                Icon::NodeEdit,
                state.sweep_path_edit,
                can_sweep_path,
                &sweep_tip,
            )
            .clicked()
                && can_sweep_path
            {
                state.sweep_path_edit = !state.sweep_path_edit;
                if state.sweep_path_edit {
                    state.preview_gizmo = PreviewGizmo::Off;
                    // A moving target cannot be grabbed precisely.
                    state.auto_rotate = false;
                    state.viewport_tab = ViewportTab::ThreeD;
                    state.status =
                        "Sweep path · drag anchors/handles on the model · surface snap · Alt breaks handles"
                            .into();
                } else {
                    state.sweep_path_drag = None;
                    state.sweep_path_hover = None;
                    state.sweep_path_readout.clear();
                    state.status = "Sweep path editor closed".into();
                }
            }
        }
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.x = 6.0;

        // ── Contextual controls, in the slots the reference gives crop / cam /
        // ops / channel. Each tier drops the least important control first and
        // always leaves room for the right-hand cluster. ──
        if state.split_view && ui.available_width() >= 80.0 {
            let (icon, tip) = if state.split_stacked {
                (Icon::Split, "Switch to side-by-side panes")
            } else {
                (Icon::SplitStacked, "Stack the panes vertically")
            };
            if bar_glyph_button(ui, icon, false, true, tip).clicked() {
                state.split_stacked = !state.split_stacked;
                state.split_ratio = 0.5;
                state.status = if state.split_stacked {
                    "Split view · stacked"
                } else {
                    "Split view · side by side"
                }
                .into();
            }
        }
        match state.viewport_tab {
            ViewportTab::ThreeD => {
                let (select_width, mut icons) = bar_budget(ui.available_width());
                if icons >= 1 {
                    icons -= 1;
                    if bar_icon_button(ui, "cube", false, true, "Import a preview mesh").clicked() {
                        open_mesh_dialog(state);
                    }
                }
                if icons >= 1 {
                    icons -= 1;
                    // Drag a layer directly on the model; the 2D canvas follows
                    // because both views read the same document.
                    let moving = state.preview_gizmo == PreviewGizmo::Move;
                    if bar_icon_button(
                        ui,
                        "pan",
                        moving,
                        true,
                        "Move the selected layer by dragging on the model (right-drag orbits)",
                    )
                    .clicked()
                    {
                        state.preview_gizmo = if moving {
                            PreviewGizmo::Off
                        } else {
                            PreviewGizmo::Move
                        };
                        if !moving {
                            state.sweep_path_edit = false;
                            state.sweep_path_drag = None;
                        }
                        state.status = if moving {
                            "3D layer gizmo off".into()
                        } else {
                            format!(
                                "3D {} layer · drag on the model",
                                PreviewGizmo::Move.label()
                            )
                        };
                    }
                }
                if icons >= 1 {
                    icons -= 1;
                    let stretching = state.preview_gizmo == PreviewGizmo::Stretch;
                    if bar_glyph_button(
                        ui,
                        Icon::Stretch,
                        stretching,
                        true,
                        "Stretch the selected layer by dragging on the model (right-drag orbits)",
                    )
                    .clicked()
                    {
                        state.preview_gizmo = if stretching {
                            PreviewGizmo::Off
                        } else {
                            PreviewGizmo::Stretch
                        };
                        if !stretching {
                            state.sweep_path_edit = false;
                            state.sweep_path_drag = None;
                        }
                        state.status = if stretching {
                            "3D layer gizmo off".into()
                        } else {
                            format!(
                                "3D {} layer · drag on the model",
                                PreviewGizmo::Stretch.label()
                            )
                        };
                    }
                }
                if icons >= 1
                    && bar_glyph_button(
                        ui,
                        Icon::Rotate,
                        state.auto_rotate,
                        true,
                        "Auto-rotate the preview",
                    )
                    .clicked()
                {
                    state.auto_rotate = !state.auto_rotate;
                }
                if select_width > 0.0 {
                    let selected_text = if select_width >= 230.0 {
                        state.preview_shape.label()
                    } else {
                        compact_shape_label(state.preview_shape)
                    };
                    bar_select(ui, "viewport_mesh", selected_text, select_width, |ui| {
                        for shape in PreviewShape::BUILTIN {
                            ui.selectable_value(&mut state.preview_shape, shape, shape.label());
                        }
                        if state.imported_mesh.is_some() {
                            ui.selectable_value(
                                &mut state.preview_shape,
                                PreviewShape::Imported,
                                PreviewShape::Imported.label(),
                            );
                        }
                    });
                }
            }
            ViewportTab::TwoD => {
                // Reserve the UDIM tile selector in the header budget so it
                // survives at the default window size; the budget drops the
                // least important icon instead of hiding the tile choice.
                let show_tiles = mesh_import.udim.enabled && mesh_import.udim.tiles.len() > 1;
                let tile_reserve = if show_tiles { 92.0 } else { 0.0 };
                let (select_width, mut icons) =
                    bar_budget((ui.available_width() - tile_reserve).max(0.0));
                let mask_editing = state.mask_edit_layer.is_some();
                let selected_mask = state
                    .selected
                    .as_deref()
                    .and_then(|path| doc.0.layer_by_path(path))
                    .map(|layer| (layer.id, layer.mask.is_some()));
                let has_mask = state
                    .selected
                    .as_deref()
                    .and_then(|path| doc.0.layer_by_path(path))
                    .is_some_and(|layer| {
                        layer.mask.is_some()
                            || layer.mask_layer.is_some()
                            || !layer.mask_settings.generators.is_empty()
                    });

                if icons >= 1 {
                    icons -= 1;
                    if bar_icon_button(
                        ui,
                        "pan",
                        state.canvas_pan_tool,
                        true,
                        "Pan tool (Space also pans)",
                    )
                    .clicked()
                    {
                        state.canvas_pan_tool = !state.canvas_pan_tool;
                    }
                }

                // Mask overlay: inspect the selected layer's mask without
                // entering mask editing, and toggle it off for normal work.
                if icons >= 1 {
                    icons -= 1;
                    if bar_glyph_button(
                        ui,
                        Icon::Mask,
                        state.show_mask_overlay && has_mask,
                        has_mask,
                        if has_mask {
                            "Show the selected layer's mask over the normal map (M)"
                        } else {
                            "The selected layer has no mask yet"
                        },
                    )
                    .clicked()
                    {
                        state.show_mask_overlay = !state.show_mask_overlay;
                        state.status = if state.show_mask_overlay {
                            "Mask overlay on".into()
                        } else {
                            "Mask overlay off".into()
                        };
                    }
                }
                if state.show_mask_overlay && has_mask && select_width >= 230.0 {
                    ui.spacing_mut().slider_width = 58.0;
                    ui.add(
                        egui::Slider::new(&mut state.mask_overlay_opacity, 0.05..=1.0)
                            .show_value(false)
                            .trailing_fill(true),
                    )
                    .on_hover_text(format!(
                        "Mask overlay opacity {:.0}%",
                        state.mask_overlay_opacity * 100.0
                    ));
                }

                let uv_ready =
                    state.preview_shape == PreviewShape::Imported && state.uv_tex_id.is_some();
                let uv_active = state.show_uv_overlay_2d && uv_ready;
                if icons >= 1 {
                    if bar_glyph_button(
                        ui,
                        Icon::Uv,
                        uv_active,
                        uv_ready,
                        "UV island overlay on the 2D canvas",
                    )
                    .clicked()
                    {
                        state.show_uv_overlay_2d = !state.show_uv_overlay_2d;
                    }
                }
                if uv_active && select_width >= 230.0 {
                    ui.spacing_mut().slider_width = 58.0;
                    ui.add(
                        egui::Slider::new(&mut state.uv_overlay_opacity, 0.05..=1.0)
                            .show_value(false)
                            .trailing_fill(true),
                    )
                    .on_hover_text(format!(
                        "UV overlay opacity {:.0}%",
                        state.uv_overlay_opacity * 100.0
                    ));
                }
                // Tile selection drives the whole document, not just the UV
                // overlay: layers tagged for another tile stop contributing to
                // the canvas, the 3D preview and exports.
                if show_tiles && select_width > 0.0 {
                    let selected = if mesh_import.udim.tiles.contains(&doc.0.active_tile) {
                        doc.0.active_tile
                    } else {
                        mesh_import.udim.base_tile()
                    };
                    let tile_text = selected.to_string();
                    let previous = doc.0.active_tile;
                    bar_select(ui, "udim_tile", &tile_text, 86.0, |ui| {
                        for tile in &mesh_import.udim.tiles {
                            ui.selectable_value(&mut doc.0.active_tile, *tile, tile.to_string());
                        }
                    });
                    if doc.0.active_tile != previous {
                        let tile = doc.0.active_tile;
                        state.set_active_tile(doc, tile);
                    }
                }

                if select_width > 0.0 {
                    let mode_text = if mask_editing {
                        "Mask"
                    } else if state.show_height {
                        "Height"
                    } else {
                        "Normal"
                    };
                    bar_select(ui, "canvas_mode", mode_text, select_width, |ui| {
                        if ui
                            .selectable_label(!mask_editing && !state.show_height, "Normal")
                            .clicked()
                        {
                            let changed = state.show_height || mask_editing;
                            state.show_height = false;
                            state.mask_edit_layer = None;
                            if changed {
                                state.request_eval_now();
                            }
                        }
                        if ui
                            .selectable_label(!mask_editing && state.show_height, "Height")
                            .clicked()
                        {
                            let changed = !state.show_height || mask_editing;
                            state.show_height = true;
                            state.mask_edit_layer = None;
                            if changed {
                                state.request_eval_now();
                            }
                        }
                        if let Some((id, has_mask)) = selected_mask {
                            let label = if has_mask { "Mask" } else { "Mask (new)" };
                            if ui.selectable_label(mask_editing, label).clicked() && !mask_editing {
                                begin_mask_edit(state, doc, id);
                            }
                        }
                    });
                }
            }
        }

        // ── Right cluster: fit, then the zoom stepper, exactly as the reference
        // puts them (right-to-left in egui means adding in reverse). ──
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            match state.viewport_tab {
                ViewportTab::TwoD => {
                    let stepper = ui.available_width() >= 170.0;
                    if stepper && bar_icon_button(ui, "zoom_in", false, true, "Zoom in").clicked() {
                        state.zoom = (state.zoom * 1.25).clamp(MIN_CANVAS_ZOOM, MAX_CANVAS_ZOOM);
                    }
                    if stepper {
                        ui.label(
                            egui::RichText::new(format!("{:.0}%", state.zoom * 100.0))
                                .font(mono(12.0))
                                .color(ON_SURFACE_VAR),
                        )
                        .on_hover_text(
                            "Wheel, pinch or Ctrl/Cmd+wheel zooms; two-finger touchpad \
                                 scroll pans (Settings → Input)",
                        );
                    }
                    if stepper && bar_icon_button(ui, "zoom_out", false, true, "Zoom out").clicked()
                    {
                        state.zoom = (state.zoom * 0.8).clamp(MIN_CANVAS_ZOOM, MAX_CANVAS_ZOOM);
                    }
                    if ui.available_width() >= 40.0
                        && bar_icon_button(ui, "fit", false, true, "Fit to view").clicked()
                    {
                        state.zoom = 1.0;
                        state.pan = egui::Vec2::ZERO;
                    }
                }
                ViewportTab::ThreeD => {
                    if ui.available_width() >= 40.0
                        && bar_icon_button(ui, "fit", false, true, "Reset the 3D camera").clicked()
                    {
                        reset_camera(state);
                    }
                }
            }
        });
    });
}

/// Development view (`--icon-sheet`): every glyph at several sizes so the
/// drawing weight and optical size can be checked together.
pub(crate) fn icon_sheet(ui: &mut egui::Ui) {
    let icons = [
        (Icon::Select, "Select"),
        (Icon::Rectangle, "Rectangle"),
        (Icon::Ellipse, "Ellipse"),
        (Icon::Star, "Star"),
        (Icon::Pen, "Pen"),
        (Icon::Brush, "Paint"),
        (Icon::Pan, "Pan"),
        (Icon::Grid, "2D"),
        (Icon::Cube, "3D"),
        (Icon::NodeEdit, "Node"),
        (Icon::Mask, "Mask"),
        (Icon::Trash, "Trash"),
        (Icon::Split, "Split"),
        (Icon::SplitStacked, "Stack"),
        (Icon::PathAdd, "Add"),
        (Icon::PathSubtract, "Subtract"),
        (Icon::PathIntersect, "Intersect"),
        (Icon::PathXor, "XOR"),
        (Icon::PathDivide, "Divide"),
        (Icon::Detach, "Detach"),
    ];
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(
            egui::RichText::new("ICON SHEET · on panel and on a selected tile")
                .size(TEXT_LABEL)
                .color(MUTED),
        );
        for size in [20.0_f32, 28.0, 48.0, 96.0] {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                for (icon, label) in &icons {
                    let tile = size + 14.0;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(tile, tile + 16.0), egui::Sense::hover());
                    let tile_rect = egui::Rect::from_min_size(rect.min, egui::vec2(tile, tile));
                    ui.painter().rect_filled(tile_rect, 6.0, CONTAINER_HIGH);
                    draw_icon(
                        ui.painter(),
                        egui::Rect::from_center_size(tile_rect.center(), egui::Vec2::splat(size)),
                        *icon,
                        TEXT,
                    );
                    ui.painter().text(
                        tile_rect.center_bottom() + egui::vec2(0.0, 9.0),
                        egui::Align2::CENTER_CENTER,
                        *label,
                        egui::FontId::proportional(TEXT_CAPTION),
                        MUTED,
                    );
                }
            });
        }

        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("RAIL STATES · normal, hover, selected")
                .size(TEXT_LABEL)
                .color(MUTED),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (icon, _) in &icons {
                for (fill, border, glyph) in [
                    (egui::Color32::TRANSPARENT, BORDER, TEXT),
                    (HOVER_BG, BORDER, TEXT),
                    (SELECTED_BG, ACCENT, ACCENT_TEXT),
                ] {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
                    let tile = rect.shrink(2.0);
                    if fill != egui::Color32::TRANSPARENT {
                        ui.painter().rect_filled(tile, 5.0, fill);
                    }
                    ui.painter().rect_stroke(
                        tile,
                        5.0,
                        egui::Stroke::new(1.0_f32, border),
                        egui::StrokeKind::Inside,
                    );
                    draw_icon(
                        ui.painter(),
                        egui::Rect::from_center_size(tile.center(), egui::Vec2::splat(20.0)),
                        *icon,
                        glyph,
                    );
                }
            }
        });
    });
}

pub(crate) fn reset_camera(state: &mut EditorState) {
    state.orbit_yaw = 0.35;
    state.orbit_pitch = 0.15;
    state.orbit_distance = 3.0;
}

/// Orbit speed per screen pixel of drag.
const ORBIT_SPEED: f32 = 0.008;
/// Keep the camera out of the poles, where the view flips over.
const MAX_ORBIT_PITCH: f32 = 1.35;

/// Apply one orbit drag in grab semantics: the scene follows the pointer, just
/// like the grabbing-hand cursor and the 2D canvas pan. Dragging right turns
/// the scene right (the camera orbits left); dragging down tips the front of
/// the scene down (the camera orbits up, revealing the top).
fn orbit_drag(yaw: &mut f32, pitch: &mut f32, delta: egui::Vec2) {
    *yaw -= delta.x * ORBIT_SPEED;
    *pitch = (*pitch + delta.y * ORBIT_SPEED).clamp(-MAX_ORBIT_PITCH, MAX_ORBIT_PITCH);
}

pub(crate) fn viewport_3d(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    mesh_import: &MeshImport,
    split: bool,
    lightmap: &mut LightmapRuntime,
    projection: &mut crate::projection::ProjectionRuntime,
) {
    let size = ui.available_size();
    if size.x < 24.0 || size.y < 24.0 {
        return;
    }
    let (response, painter) = ui.allocate_painter(size, egui::Sense::click_and_drag());
    let rect = response.rect;
    state.preview_rect = Some(rect);

    // Projection handles come first: a drag that starts on the cage must not
    // orbit the camera, move a light or move the layer gizmo.
    let projection_input = if projection.enabled {
        projection_gizmo_input(ui, &response, rect, state, projection)
    } else {
        ProjectionGizmoInput::default()
    };

    // Light handles come next: a drag that starts on one must not orbit the
    // camera or move the selected normal layer.
    let lightmap_gizmos = light_gizmos_visible(state, lightmap);
    let light_input = if lightmap_gizmos && !projection_input.dragging {
        light_gizmo_input(ui, &response, rect, state, lightmap)
    } else {
        lightmap.dragging_light = None;
        LightGizmoInput::default()
    };

    // Sweep-path handles share the model with the other gizmos. They take
    // priority after the projection cage and light handles so grabbing a path
    // node never orbits the camera or moves a layer.
    let sweep_active = state.sweep_path_edit && sweep_path_target(state, &doc.0).is_some();
    let sweep_input = if sweep_active && !projection_input.dragging && !light_input.dragging {
        sweep_path_input(ui, &response, rect, state, doc, mesh_import)
    } else {
        state.sweep_path_drag = None;
        SweepPathInput::default()
    };

    // With a layer gizmo active, the primary button edits the selected layer
    // and the right/middle buttons orbit. Otherwise any drag orbits as before.
    let gizmo_active =
        state.preview_gizmo != PreviewGizmo::Off && !projection.enabled && !sweep_active;
    let orbiting = if projection_input.dragging || light_input.dragging || sweep_input.dragging {
        false
    } else if gizmo_active {
        response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
    } else {
        response.dragged()
    };
    if orbiting {
        let delta = response.drag_delta();
        orbit_drag(&mut state.orbit_yaw, &mut state.orbit_pitch, delta);
    }
    if gizmo_active
        && !light_input.dragging
        && !projection_input.dragging
        && !sweep_input.dragging
        && response.dragged_by(egui::PointerButton::Primary)
    {
        drag_selected_layer(state, doc, rect.size(), response.drag_delta());
    }
    if response.hovered() {
        // Pinch and Ctrl/Cmd+wheel zoom; two-finger horizontal scrolling
        // orbits, vertical scrolling dollies, and a touch-screen two-finger
        // drag orbits directly.
        let gesture = scroll_gesture(ui);
        gesture.remember_device(state);
        if gesture.has_zoom() {
            state.orbit_distance = (state.orbit_distance / gesture.zoom).clamp(1.8, 8.0);
        } else if gesture.has_scroll() {
            if gesture.delta.x != 0.0 {
                state.orbit_yaw -= gesture.delta.x * 0.006;
            }
            if gesture.delta.y != 0.0 {
                state.orbit_distance =
                    (state.orbit_distance * (1.0 - gesture.delta.y * 0.0015)).clamp(1.8, 8.0);
            }
        }
        if gesture.touch_pan != egui::Vec2::ZERO {
            orbit_drag(
                &mut state.orbit_yaw,
                &mut state.orbit_pitch,
                gesture.touch_pan,
            );
        }
    }
    let dragging = response.dragged();
    let _ = response.on_hover_cursor(
        if projection_input.dragging || light_input.dragging || sweep_input.dragging {
            egui::CursorIcon::Grabbing
        } else if sweep_input.hovering.is_some() {
            egui::CursorIcon::PointingHand
        } else if projection_input.hovering.is_some() {
            egui::CursorIcon::PointingHand
        } else if light_input.hovering.is_some() {
            egui::CursorIcon::PointingHand
        } else if dragging {
            egui::CursorIcon::Grabbing
        } else if gizmo_active {
            egui::CursorIcon::Move
        } else {
            egui::CursorIcon::Grab
        },
    );

    let painter = painter.with_clip_rect(rect);
    if projection.enabled {
        paint_projection_gizmo(&painter, rect, state, projection, projection_input.hovering);
    }
    if lightmap_gizmos {
        paint_light_gizmos(&painter, rect, state, lightmap, light_input.hovering);
    }
    if sweep_active {
        paint_sweep_path_gizmo(&painter, rect, state, &doc.0, mesh_import, &sweep_input);
    }
    // No base outline: the viewport well separates the pane from the shell.
    // Both corner hints need ~330 px; on narrow viewports keep only the
    // lighting readout instead of letting the two overlap.
    if lightmap_gizmos && rect.width() >= 360.0 {
        painter.text(
            rect.left_bottom() + egui::vec2(10.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "drag body: move · aim arrow: rotate · ring: turn · right-drag: orbit",
            egui::FontId::proportional(TEXT_SMALL),
            ACCENT_TEXT,
        );
    } else if projection.enabled {
        painter.text(
            rect.left_bottom() + egui::vec2(10.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "drag cage · arrows: axis · square: scale · ring: rotate",
            egui::FontId::proportional(TEXT_SMALL),
            ACCENT_TEXT,
        );
    } else if sweep_active {
        painter.text(
            rect.left_bottom() + egui::vec2(10.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            sweep_path_hint(state),
            egui::FontId::proportional(TEXT_SMALL),
            ACCENT_TEXT,
        );
    } else if gizmo_active {
        painter.text(
            rect.left_bottom() + egui::vec2(10.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            match state.preview_gizmo {
                PreviewGizmo::Move => "drag: move layer · right-drag: orbit",
                PreviewGizmo::Stretch => "drag: stretch layer · right-drag: orbit",
                PreviewGizmo::Off => "",
            },
            egui::FontId::proportional(TEXT_SMALL),
            ACCENT_TEXT,
        );
    } else if rect.width() >= 360.0 {
        painter.text(
            rect.left_bottom() + egui::vec2(10.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "drag to orbit · scroll or pinch to zoom",
            egui::FontId::proportional(TEXT_SMALL),
            MUTED,
        );
    }

    painter.text(
        rect.right_bottom() + egui::vec2(-10.0, -8.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("Lighting · {}", state.lighting_preset.label()),
        egui::FontId::proportional(TEXT_SMALL),
        MUTED,
    );

    let chip = if split {
        format!(
            "3D · {} · {:.2}×",
            state.preview_shape.label(),
            state.tiling
        )
    } else {
        format!("{} · {:.2}×", state.preview_shape.label(), state.tiling)
    };
    let galley = painter.layout_no_wrap(chip, egui::FontId::proportional(TEXT_SMALL), TEXT);
    let chip_rect = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(10.0, 10.0),
        galley.size() + egui::vec2(16.0, 8.0),
    );
    painter.rect_filled(chip_rect, 4.0, egui::Color32::from_black_alpha(150));
    painter.galley(chip_rect.min + egui::vec2(8.0, 4.0), galley, TEXT);

    draw_uv_inset(&painter, rect, state, mesh_import);
}

pub(crate) fn draw_uv_inset(
    painter: &egui::Painter,
    rect: egui::Rect,
    state: &EditorState,
    mesh_import: &MeshImport,
) {
    if !state.show_uv_overlay || state.preview_shape != PreviewShape::Imported {
        return;
    }
    let Some(texture_id) = state.uv_inset_tex_id.or(state.uv_tex_id) else {
        return;
    };
    if rect.width() < 320.0 || rect.height() < 240.0 {
        return;
    }

    let udim = &mesh_import.udim;
    let (cols, rows) = if udim.enabled {
        (udim.cols.max(1), udim.rows.max(1))
    } else {
        (1, 1)
    };
    let max_width = (rect.width() * 0.30).clamp(110.0, 230.0);
    let max_height = (rect.height() * 0.30).clamp(90.0, 200.0);
    let aspect = cols as f32 / rows as f32;
    let mut image_width = max_width;
    let mut image_height = image_width / aspect;
    if image_height > max_height {
        image_height = max_height;
        image_width = image_height * aspect;
    }
    let title = if udim.enabled {
        format!("UDIM · {} tiles", udim.tiles.len())
    } else {
        "UV 0–1 · islands".to_string()
    };
    let size = egui::vec2(image_width + 12.0, image_height + 30.0);
    let background =
        egui::Rect::from_min_size(rect.right_top() + egui::vec2(-12.0 - size.x, 12.0), size);
    painter.rect_filled(background, 4.0, egui::Color32::from_black_alpha(215));
    painter.text(
        background.min + egui::vec2(7.0, 6.0),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::proportional(TEXT_SMALL),
        TEXT,
    );
    let image_rect = egui::Rect::from_min_size(
        background.min + egui::vec2(6.0, 22.0),
        egui::vec2(image_width, image_height),
    );
    painter.image(
        texture_id,
        image_rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::from_white_alpha((state.uv_overlay_opacity.clamp(0.0, 1.0) * 255.0) as u8),
    );
    painter.rect_stroke(
        image_rect,
        0.0,
        egui::Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Outside,
    );
    if udim.enabled {
        let cell_width = image_rect.width() / cols as f32;
        let cell_height = image_rect.height() / rows as f32;
        for row in 0..rows {
            for column in 0..cols {
                let cell = egui::Rect::from_min_size(
                    image_rect.min
                        + egui::vec2(column as f32 * cell_width, row as f32 * cell_height),
                    egui::vec2(cell_width, cell_height),
                );
                painter.rect_stroke(
                    cell,
                    0.0,
                    egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(120, 124, 132, 110),
                    ),
                    egui::StrokeKind::Inside,
                );
                if cell_width >= 26.0 && cell_height >= 13.0 {
                    painter.text(
                        cell.center(),
                        egui::Align2::CENTER_CENTER,
                        udim.tile_number(column, row).to_string(),
                        egui::FontId::proportional(TEXT_CAPTION),
                        MUTED,
                    );
                }
            }
        }
    }
}
/// N-shade brand mark: an N monogram with an offset shadow, on the accent tile.
pub(crate) fn draw_brand_logo(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect.expand(2.0),
        10.0,
        egui::Color32::from_rgba_unmultiplied(0x00, 0xD6, 0xFC, 48),
    );
    painter.rect_filled(rect, 8.0, PRIMARY_CONTAINER);

    // Draw the N as one connected stroke so the joins stay crisp. The offset
    // translucent copy reads as a cast shadow, matching the N-shade name.
    let inner = rect.shrink(8.5);
    let points = [
        egui::pos2(inner.left(), inner.bottom()),
        egui::pos2(inner.left(), inner.top()),
        egui::pos2(inner.right(), inner.bottom()),
        egui::pos2(inner.right(), inner.top()),
    ];
    let shadow_offset = egui::vec2(1.2, 1.4);
    let shadow: Vec<egui::Pos2> = points.iter().map(|point| *point + shadow_offset).collect();
    painter.add(egui::Shape::line(
        shadow,
        egui::Stroke::new(3.2_f32, egui::Color32::from_black_alpha(70)),
    ));
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(3.2_f32, ON_PRIMARY),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 500.0))
    }

    #[test]
    fn the_view_switch_eases_the_canvas_in_and_out() {
        // 3D -> 2D: the canvas starts invisible, is half-way through the run and
        // ends opaque, after which the transition is retired.
        let mut state = EditorState {
            viewport_tab: ViewportTab::TwoD,
            ..Default::default()
        };
        state.view_transition = Some(ViewTransition {
            from: ViewportTab::ThreeD,
            started: 10.0,
        });
        assert!(view_blend(&mut state, 10.0).unwrap() < 0.01);
        let middle = view_blend(&mut state, 10.0 + VIEW_TRANSITION_SECS * 0.5).unwrap();
        assert!((middle - 0.5).abs() < 0.01, "mid-fade was {middle}");
        assert_eq!(view_blend(&mut state, 10.0 + VIEW_TRANSITION_SECS), None);
        assert!(state.view_transition.is_none());

        // 2D -> 3D: the canvas starts opaque and fades away.
        let mut state = EditorState {
            viewport_tab: ViewportTab::ThreeD,
            ..Default::default()
        };
        state.view_transition = Some(ViewTransition {
            from: ViewportTab::TwoD,
            started: 4.0,
        });
        assert!((view_blend(&mut state, 4.0).unwrap() - 1.0).abs() < 0.01);
        assert!(view_blend(&mut state, 4.0 + VIEW_TRANSITION_SECS * 0.5).unwrap() < 0.51);
    }

    #[test]
    fn the_bar_keeps_its_primary_controls_at_the_default_window_size() {
        // A 1440 x 780 window with both docks open leaves about 290 px after
        // the tabs: the select collapses but stays visible with two icons.
        assert_eq!(bar_budget(290.0), (112.0, 2));
        // A wide viewport gets the reference layout.
        assert_eq!(bar_budget(900.0), (230.0, 4));
        // Too narrow for a select: only the right-hand fit button remains.
        assert_eq!(bar_budget(120.0), (0.0, 0));
        // The compact select is still preferred over hiding it.
        assert_eq!(bar_budget(160.0), (112.0, 0));
    }

    #[test]
    fn side_by_side_split_tiles_the_area() {
        let (three, gutter, two) = split_panes(area(), 0.35, false);
        assert_eq!(three.min, area().min);
        assert_eq!(two.max, area().max);
        assert!((three.right() - gutter.left()).abs() < 1e-3);
        assert!((gutter.right() - two.left()).abs() < 1e-3);
        assert!((gutter.width() - SPLIT_GAP).abs() < 1e-3);
        assert!((three.width() + gutter.width() + two.width() - area().width()).abs() < 1e-3);
    }

    #[test]
    fn stacked_split_tiles_the_area() {
        let (three, gutter, two) = split_panes(area(), 0.65, true);
        assert_eq!(three.min, area().min);
        assert_eq!(two.max, area().max);
        assert!((three.bottom() - gutter.top()).abs() < 1e-3);
        assert!((gutter.bottom() - two.top()).abs() < 1e-3);
        assert!((gutter.height() - SPLIT_GAP).abs() < 1e-3);
        assert!((three.height() + gutter.height() + two.height() - area().height()).abs() < 1e-3);
    }

    #[test]
    fn split_ratio_keeps_both_panes_usable() {
        let (three, _, two) = split_panes(area(), 0.0, false);
        assert!(three.width() >= MIN_SPLIT_PANE - 1e-3);
        assert!(two.width() >= MIN_SPLIT_PANE - 1e-3);

        let (three, _, two) = split_panes(area(), 1.0, false);
        assert!(three.width() >= MIN_SPLIT_PANE - 1e-3);
        assert!(two.width() >= MIN_SPLIT_PANE - 1e-3);
    }

    #[test]
    fn orbit_drag_follows_the_grabbing_hand() {
        // Horizontal: dragging right turns the scene right (camera left).
        let mut yaw = 0.0;
        let mut pitch = 0.0;
        orbit_drag(&mut yaw, &mut pitch, egui::vec2(10.0, 0.0));
        assert!(yaw < 0.0, "dragging right must turn the scene right");

        // Vertical: dragging down tips the front of the scene down, so the
        // camera moves up and the top comes into view. This was inverted.
        let mut yaw = 0.0;
        let mut pitch = 0.0;
        orbit_drag(&mut yaw, &mut pitch, egui::vec2(0.0, 10.0));
        assert!(pitch > 0.0, "dragging down must tip the scene down");

        // Pitch never crosses the poles.
        let mut yaw = 0.0;
        let mut pitch = 1.0;
        orbit_drag(&mut yaw, &mut pitch, egui::vec2(0.0, 1000.0));
        assert!((pitch - MAX_ORBIT_PITCH).abs() < 1e-6);
    }
}
