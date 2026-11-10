//! Painted-mask editing: dedicated 2D canvas mode, brush toolbar, and preview
//! texture upload.

use super::*;

/// Enters mask-editing mode for `layer_id`, creating a fully visible painted
/// mask when the layer does not have one yet.
pub(crate) fn begin_mask_edit(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    layer_id: LayerId,
) -> bool {
    if doc.0.is_locked(layer_id) {
        state.status = "Layer is locked · unlock it to paint its mask".into();
        return false;
    }
    state.ensure_undo_gesture(&doc.0);
    let size = doc.0.size;
    let Some(layer) = doc.0.find_layer_mut(layer_id) else {
        return false;
    };
    let fresh = layer.mask.is_none();
    if fresh {
        layer.mask = Some(Grid::new(size.x, size.y, 1.0));
        layer.touch_mask();
        state.mark_dirty();
        // A painted mask disables the GPU shape path.
        state.using_gpu = false;
    }
    // Editing a disabled mask would paint with no visible effect.
    if !layer.mask_settings.enabled {
        layer.mask_settings.enabled = true;
        layer.touch_mask();
        state.mark_dirty();
    }
    let name = layer.name.clone();
    if let Some(path) = doc.0.find_path(layer_id) {
        state.selected = Some(path);
        state.multi_selected.clear();
    }
    state.mask_edit_layer = Some(layer_id);
    state.viewport_tab = ViewportTab::TwoD;
    state.canvas_tool = CanvasTool::Paint;
    // A fresh mask is fully white, so start by painting black (hide);
    // otherwise keep the brush the user last chose.
    if fresh || !matches!(state.brush_mode, PaintMode::Lower | PaintMode::Raise) {
        state.brush_mode = PaintMode::Lower;
        state.mask_white = false;
    }
    state.mask_stroke = None;
    state.canvas_drag = None;
    state.show_height = false;
    state.vector_draft = None;
    state.node_selection.clear();
    state.node_hover = None;
    state.status = format!("Editing mask · {name}");
    true
}

/// Leaves mask editing and refreshes the normal map.
pub(crate) fn end_mask_edit(state: &mut EditorState) {
    state.mask_edit_layer = None;
    state.mask_stroke = None;
    state.canvas_tool = CanvasTool::Select;
    state.canvas_drag = None;
    state.status = "Mask editing closed".into();
    state.request_eval_now();
}

/// Validates the mask target against the current selection and layer stack.
/// Selecting another layer or removing the mask leaves mask-editing mode.
pub(crate) fn mask_edit_active(state: &mut EditorState, doc: &DocumentRes) -> bool {
    let Some(id) = state.mask_edit_layer else {
        return false;
    };
    let selected = state
        .selected
        .as_deref()
        .and_then(|path| doc.0.layer_by_path(path))
        .map(|layer| layer.id);
    let has_mask = doc
        .0
        .find_layer(id)
        .is_some_and(|layer| layer.mask.is_some());
    if !has_mask || selected != Some(id) {
        state.mask_edit_layer = None;
        state.mask_stroke = None;
        state.canvas_drag = None;
        false
    } else {
        true
    }
}

/// RGBA for the red quick-mask tint: alpha is the hidden amount, so the draw
/// tint can scale the whole overlay's opacity without rebuilding the texture.
pub(crate) fn mask_overlay_rgba(mask: &Grid<f32>) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(mask.data.len() * 4);
    for value in &mask.data {
        let alpha = ((1.0 - value.clamp(0.0, 1.0)) * 255.0).round() as u8;
        rgba.extend_from_slice(&[232, 52, 64, alpha]);
    }
    rgba
}

/// Overlay tint at the user's opacity.
pub(crate) fn mask_overlay_tint(opacity: f32) -> egui::Color32 {
    egui::Color32::from_white_alpha((opacity.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Uploads the painted mask as an egui texture when its revision, size or the
/// view mode changed. `Mask` is an opaque grayscale image; `Overlay` is a red
/// quick-mask tint whose alpha is the hidden amount, drawn over the normals.
pub(crate) fn mask_texture(
    ui: &egui::Ui,
    state: &mut EditorState,
    layer_id: LayerId,
    revision: u64,
    mask: &Grid<f32>,
    view: MaskView,
) -> Option<egui::TextureId> {
    let key = (layer_id, revision, mask.w, mask.h, view);
    // Painting must update live; mask-setting sliders must not rebuild the
    // whole texture every frame. Defer those until the pointer is released.
    let painting = state.mask_stroke.is_some()
        || matches!(state.canvas_drag, Some(CanvasDrag::MaskPaint { .. }));
    let pointer_down = ui.input(|input| input.pointer.any_down());
    if state.mask_tex_key != Some(key) && (!pointer_down || painting) {
        let rgba = match view {
            MaskView::Overlay => mask_overlay_rgba(mask),
            _ => {
                let mut rgba = Vec::with_capacity(mask.data.len() * 4);
                for value in &mask.data {
                    let gray = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                    rgba.extend_from_slice(&[gray, gray, gray, 255]);
                }
                rgba
            }
        };
        let image =
            egui::ColorImage::from_rgba_unmultiplied([mask.w as usize, mask.h as usize], &rgba);
        if let Some(texture) = &mut state.mask_tex {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            state.mask_tex = Some(ui.ctx().load_texture(
                "n-shade_mask_preview",
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        state.mask_tex_key = Some(key);
    }
    state.mask_tex.as_ref().map(|texture| texture.id())
}

enum MaskOp {
    Invert,
    Fill(f32),
    Soften,
}

const MASK_TOOLS: [(PaintMode, &str, &str); 4] = [
    (
        PaintMode::Raise,
        "Reveal",
        "Paint white: show the layer here (X swaps with Hide)",
    ),
    (
        PaintMode::Lower,
        "Hide",
        "Paint black: hide the layer here (X swaps with Reveal)",
    ),
    (
        PaintMode::Smooth,
        "Blur",
        "Soften mask edges under the brush",
    ),
    (
        PaintMode::Flatten,
        "Value",
        "Paint a partial value, for a softer blend of the detail",
    ),
];

/// Brush strip shown above the 2D canvas while a mask is open.
pub(crate) fn mask_toolbar(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
) -> bool {
    let Some(layer_id) = state.mask_edit_layer else {
        return false;
    };
    let Some(layer) = doc.0.find_layer(layer_id) else {
        state.mask_edit_layer = None;
        return false;
    };
    if layer.mask.is_none() {
        state.mask_edit_layer = None;
        return false;
    }
    let name = layer.name.clone();
    let smart_count = layer
        .mask_settings
        .generators
        .iter()
        .filter(|generator| generator.enabled)
        .count();

    handle_mask_shortcuts(ui, state);

    let mut op = None;
    let mut exit = false;
    egui::Frame::new()
        .fill(CONTAINER_HIGH)
        .stroke(egui::Stroke::NONE)
        .corner_radius(egui::CornerRadius::same(RADIUS_SM))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);

            // Row 1: target, view mode, exit.
            ui.horizontal(|ui| {
                let (icon_rect, _) =
                    ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                draw_icon(ui.painter(), icon_rect, Icon::Mask, ACCENT);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let done = egui::Button::new(
                        egui::RichText::new("Done")
                            .size(TEXT_SMALL)
                            .strong()
                            .color(ON_PRIMARY),
                    )
                    .fill(ACCENT)
                    .corner_radius(4);
                    if ui
                        .add(done)
                        .on_hover_text("Leave mask editing (Esc)")
                        .clicked()
                    {
                        exit = true;
                    }
                    for view in MaskView::ALL.iter().rev() {
                        if segment(ui, view.label(), state.mask_view == *view)
                            .on_hover_text(match view {
                                MaskView::Overlay => "Live normal map, hidden areas tinted red",
                                MaskView::Mask => "Painted mask in black and white",
                                MaskView::Result => "Live normal map only",
                            })
                            .clicked()
                        {
                            state.mask_view = *view;
                            if view.shows_result() {
                                state.request_eval_now();
                            }
                        }
                    }
                    let title = if smart_count > 0 {
                        format!("Mask · {name}  (+{smart_count} smart)")
                    } else {
                        format!("Mask · {name}")
                    };
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(title)
                                .size(TEXT_LABEL)
                                .strong()
                                .color(TEXT),
                        )
                        .truncate(),
                    );
                });
            });

            // Row 2: brush tool and settings.
            ui.horizontal_wrapped(|ui| {
                for (mode, label, tip) in MASK_TOOLS {
                    if segment(ui, label, state.brush_mode == mode)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        state.brush_mode = mode;
                        if matches!(mode, PaintMode::Raise | PaintMode::Lower) {
                            state.mask_white = mode == PaintMode::Raise;
                        }
                    }
                }
                if state.brush_mode == PaintMode::Flatten {
                    percent_drag(ui, &mut state.brush_flatten_target, "Value ", 0.0)
                        .on_hover_text("Mask value painted by the Value brush");
                }
                ui.separator();
                ui.add(
                    egui::DragValue::new(&mut state.brush_radius)
                        .range(1.0..=512.0)
                        .speed(0.5)
                        .fixed_decimals(0)
                        .prefix("Size "),
                )
                .on_hover_text("Brush radius in document pixels ([ and ])");
                if state.mask_view == MaskView::Overlay {
                    percent_drag(ui, &mut state.mask_overlay_opacity, "Overlay ", 0.05)
                        .on_hover_text("Mask overlay opacity");
                }
                percent_drag(ui, &mut state.brush_hardness, "Hard ", 0.0)
                    .on_hover_text("Edge hardness");
                percent_drag(ui, &mut state.brush_flow, "Flow ", 0.01)
                    .on_hover_text("How fast each dab builds up");
                percent_drag(ui, &mut state.brush_opacity, "Opacity ", 0.01).on_hover_text(
                    "Most a single stroke can change the mask. Overlapping dabs never \
                     exceed it; start a new stroke to build further.",
                );
                ui.menu_button(egui::RichText::new("Pen").size(TEXT_SMALL), |ui| {
                    pen_controls(ui, state);
                })
                .response
                .on_hover_text("Pen tablet pressure for brush size and flow");
                if state.pen_detected && state.pen_pressure_active {
                    ui.label(
                        egui::RichText::new(format!(
                            "{:.0}%",
                            state.pen_pressure.clamp(0.0, 1.0) * 100.0
                        ))
                        .size(TEXT_SMALL)
                        .color(ACCENT_TEXT),
                    )
                    .on_hover_text("Live pen pressure");
                }
                ui.separator();
                ui.menu_button(egui::RichText::new("Mask ops").size(TEXT_SMALL), |ui| {
                    if ui.button("Invert").clicked() {
                        op = Some(MaskOp::Invert);
                        ui.close();
                    }
                    if ui.button("Fill white (reveal all)").clicked() {
                        op = Some(MaskOp::Fill(1.0));
                        ui.close();
                    }
                    if ui.button("Fill black (hide all)").clicked() {
                        op = Some(MaskOp::Fill(0.0));
                        ui.close();
                    }
                    if ui
                        .button(format!(
                            "Fill with value ({:.0}%)",
                            state.brush_flatten_target * 100.0
                        ))
                        .clicked()
                    {
                        op = Some(MaskOp::Fill(state.brush_flatten_target));
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button("Soften whole mask")
                        .on_hover_text("Blur every edge by a quarter of the brush size")
                        .clicked()
                    {
                        op = Some(MaskOp::Soften);
                        ui.close();
                    }
                });
            });
        });

    if let Some(op) = op {
        state.ensure_undo_gesture(&doc.0);
        let soften_sigma = (state.brush_radius * 0.25).max(0.5);
        let wrap = doc.0.wrap;
        if let Some(layer) = doc.0.find_layer_mut(layer_id) {
            if let Some(mask) = layer.mask.as_mut() {
                match op {
                    MaskOp::Invert => {
                        for value in &mut mask.data {
                            *value = 1.0 - value.clamp(0.0, 1.0);
                        }
                    }
                    MaskOp::Fill(value) => mask.data.fill(value),
                    MaskOp::Soften => {
                        *mask = ns_core::filter::gaussian_blur(mask, soften_sigma, wrap);
                    }
                }
                layer.touch_mask();
                state.mark_dirty();
            }
        }
    }
    if exit {
        end_mask_edit(state);
    }
    true
}

fn handle_mask_shortcuts(ui: &egui::Ui, state: &mut EditorState) {
    if ui.ctx().text_edit_focused() {
        return;
    }
    let (swap, smaller, bigger) = ui.input(|input| {
        (
            input.key_pressed(egui::Key::X),
            input.key_pressed(egui::Key::OpenBracket),
            input.key_pressed(egui::Key::CloseBracket),
        )
    });
    if swap {
        state.mask_white = !state.mask_white;
        state.brush_mode = if state.mask_white {
            PaintMode::Raise
        } else {
            PaintMode::Lower
        };
    }
    if smaller {
        state.brush_radius = (state.brush_radius / 1.2).max(1.0);
    }
    if bigger {
        state.brush_radius = (state.brush_radius * 1.2).min(512.0);
    }
}

/// Compact toggle used for the tool and view groups.
fn segment(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .size(TEXT_SMALL)
                .color(if active { ACCENT_TEXT } else { TEXT }),
        )
        .fill(if active { SELECTED_BG } else { INPUT_BG })
        .stroke(egui::Stroke::new(
            1.0_f32,
            if active { ACCENT } else { BORDER },
        ))
        .corner_radius(4)
        .min_size(egui::vec2(0.0, 22.0)),
    )
}

/// 0..=1 value shown and edited as a percentage.
fn percent_drag(ui: &mut egui::Ui, value: &mut f32, prefix: &str, min: f32) -> egui::Response {
    ui.add(
        egui::DragValue::new(value)
            .range(min..=1.0)
            .speed(0.005)
            .prefix(prefix)
            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
            .custom_parser(|text| {
                text.trim()
                    .trim_end_matches('%')
                    .parse::<f64>()
                    .ok()
                    .map(|v| v / 100.0)
            }),
    )
}
