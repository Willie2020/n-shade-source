//! Layers dock panel and footer.

use super::*;

/// Gap between layer rows (`.list { gap: 6px }`). The drop strips between rows
/// are exactly this tall, so reordering targets and the visual gap share space.
const LIST_GAP: f32 = 6.0;

pub(crate) struct LayersPanel;

impl Panel for LayersPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        layers_card(ui, frame.state, frame.doc);
    }
}

pub(crate) struct LayerFooterPanel;

impl Panel for LayerFooterPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        layer_footer(ui, frame.state, frame.doc);
    }
}
pub(crate) fn layers_card(ui: &mut egui::Ui, state: &mut EditorState, doc: &mut DocumentRes) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height((ui.available_height() - 2.0).max(180.0));
        ui.spacing_mut().item_spacing.y = 6.0;
        let mut action = None;
        ui.horizontal(|ui| {
            dock_title(ui, Icon::Grid, "Layers");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.scope(|ui| {
                    let widgets = &mut ui.visuals_mut().widgets;
                    let accent_border = egui::Stroke::new(1.0_f32, PRIMARY.gamma_multiply(0.35));
                    widgets.inactive.weak_bg_fill = ACCENT_WASH;
                    widgets.inactive.bg_fill = ACCENT_WASH;
                    widgets.inactive.bg_stroke = accent_border;
                    widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, PRIMARY);
                    widgets.hovered.weak_bg_fill = state_layer(ACCENT_WASH, PRIMARY, 0.18);
                    widgets.hovered.bg_fill = widgets.hovered.weak_bg_fill;
                    widgets.hovered.bg_stroke = accent_border;
                    widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, PRIMARY);
                    widgets.open.weak_bg_fill = widgets.hovered.weak_bg_fill;
                    widgets.open.bg_fill = widgets.hovered.weak_bg_fill;
                    widgets.open.bg_stroke = accent_border;
                    widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, PRIMARY);
                    ui.menu_button(egui::RichText::new("+ Add").size(TEXT_SMALL), |ui| {
                    if ui.button("Rectangle").clicked() {
                        action = Some(LayerAction::AddRect);
                        ui.close();
                    }
                    if ui.button("Ellipse").clicked() {
                        action = Some(LayerAction::AddEllipse);
                        ui.close();
                    }
                    if ui.button("Star").clicked() {
                        action = Some(LayerAction::AddStar);
                        ui.close();
                    }
                    if ui.button("Import SVG...").clicked() {
                        open_svg_dialog(state);
                        ui.close();
                    }
                    if ui.button("Import photo...").clicked() {
                        open_photo_dialog(state);
                        ui.close();
                    }
                    if ui.button("Import alpha...").clicked() {
                        open_alpha_dialog(state);
                        ui.close();
                    }
                    if ui.button("Import normal map...").clicked() {
                        open_normal_dialog(state);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("New group").clicked() {
                        action = Some(LayerAction::NewGroup);
                        ui.close();
                    }
                    if ui
                        .button("Group selected")
                        .on_hover_text(
                            "Wrap the selected layer(s) in a group. Ctrl/Cmd-click rows to add or remove one, Shift-click for a range.",
                        )
                        .clicked()
                    {
                        action = Some(LayerAction::GroupSelected);
                        ui.close();
                    }
                    if ui.button("Ungroup selected").clicked() {
                        action = Some(LayerAction::UngroupSelected);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button("Mask layer below selection")
                        .on_hover_text("Insert a paintable mask layer below and bind it")
                        .clicked()
                    {
                        action = Some(LayerAction::NewMaskLayer);
                        ui.close();
                    }
                    ui.separator();
                    let selected_count = state.multi_selected.len()
                        + usize::from(state.selected.is_some());
                    let can_boolean = selected_count >= 2;
                    ui.menu_button("Boolean", |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "Combine {selected_count} selected shape{}.",
                                if selected_count == 1 { "" } else { "s" }
                            ))
                            .size(TEXT_CAPTION)
                            .color(MUTED),
                        );
                        ui.label(
                            egui::RichText::new("Shapes stay editable inside the group.")
                                .size(TEXT_CAPTION)
                                .color(MUTED),
                        );
                        for op in VectorOp::ALL {
                            let enabled = can_boolean
                                && (op != VectorOp::Divide || selected_count <= 8);
                            let response = ui
                                .add_enabled(enabled, egui::Button::new(op.label()))
                                .on_hover_text(match op {
                                    VectorOp::Add => "Merge the outlines into one silhouette",
                                    VectorOp::Subtract => {
                                        "Cut the shapes above the bottom one out of it"
                                    }
                                    VectorOp::Intersect => "Keep only the overlapping area",
                                    VectorOp::Xor => "Remove the overlapping area",
                                    VectorOp::Divide => {
                                        "Split overlaps into separately beveled regions"
                                    }
                                    VectorOp::PartialCut => {
                                        "Cut the cutters' pockets into the bottom shape, leaving a floor"
                                    }
                                    VectorOp::PartialSlash => {
                                        "Cut tapered grooves along the cutters' long axes"
                                    }
                                    VectorOp::ClipDivide => {
                                        "Split the bottom shape along the cutters' bands"
                                    }
                                });
                            if response.clicked() {
                                action = Some(LayerAction::VectorCompound(op));
                                ui.close();
                            }
                        }
                    });
                    if ui.button("Outline stroke").clicked() {
                        action = Some(LayerAction::OutlineStroke);
                        ui.close();
                    }
                    if ui
                        .button("Bake to pixels")
                        .on_hover_text(
                            "Rasterize the selected shapes and merge each selected group into one baked layer in one undo step; the normal result stays the same",
                        )
                        .clicked()
                    {
                        action = Some(LayerAction::BakeToRaster);
                        ui.close();
                    }
                    if ui
                        .button("Cut shapes below")
                        .on_hover_text(
                            "Use the selected stroked line as a knife: the shapes behind it are cut along the band and split into separate layers",
                        )
                        .clicked()
                    {
                        action = Some(LayerAction::CutBelow);
                        ui.close();
                    }
                    })
                    .response
                    .on_hover_text("Add a shape, a group, a mask layer or an import");
                });
                // Multi-selection feedback: the set that booleans and grouping
                // act on, so Ctrl/Shift picks are never invisible.
                let multi = state.multi_selected.len();
                if multi > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} selected",
                            multi + usize::from(state.selected.is_some())
                        ))
                        .size(TEXT_CAPTION)
                        .color(ACCENT),
                    );
                }
            });
        });
        ui.add_space(6.0);
        layer_header(ui, state, doc);
        ui.add_space(6.0);

        let rows = cached_layer_rows(ui.ctx(), state, doc);
        let selection_mode =
            RowSelection::from_modifiers(ui.input(|input| input.modifiers));
        let mut drop = None;
        // `.list`: PANEL fill, 1 px LINE, r10, 10 px padding, 6 px gap. The top
        // margin is 4 because the first drop strip contributes the other 6 px
        // when idle (strips are one gap tall, before every row).
        egui::Frame::new()
            .fill(CONTAINER_LOW)
            .stroke(egui::Stroke::new(1.0_f32, OUTLINE_VARIANT))
            .corner_radius(egui::CornerRadius::same(RADIUS_MD))
            .inner_margin(egui::Margin {
                left: 10,
                right: 10,
                top: 4,
                bottom: 10,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let list_height = ui.available_height().max(60.0);
                ui.set_min_height(list_height);
                // Clicking the empty part of the stack drops the selection.
                // Registered before the rows and their controls so their
                // widgets stay on top and win the click hit-test.
                let background = ui.interact(
                    ui.max_rect(),
                    ui.id().with("layer_list_background"),
                    egui::Sense::click(),
                );
                egui::ScrollArea::vertical()
                    .id_salt("layer_stack")
                    .max_height(list_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if rows.is_empty() {
                            ui.add_space(18.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No layers yet")
                                        .size(TEXT_LABEL)
                                        .color(MUTED),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Use + Add, or draw a shape on the canvas.",
                                    )
                                    .size(TEXT_CAPTION)
                                    .color(MUTED),
                                );
                            });
                            ui.add_space(10.0);
                        }
                        for row in rows.iter() {
                            let parent = path_parent_id(&doc.0, &row.path);
                            let index = row.path.last().copied().unwrap_or(0) + 1;
                            layer_drop_strip(
                                ui,
                                doc,
                                parent,
                                index,
                                row.depth,
                                LIST_GAP,
                                &mut drop,
                            );
                            layer_row(
                                ui,
                                row,
                                state,
                                doc,
                                &mut action,
                                selection_mode,
                                &mut drop,
                            );
                        }
                        // Fill the rest of the stack with a root drop target
                        // so moving a layer out of a group does not require
                        // hitting a thin strip.
                        let remaining = ui.available_height();
                        let root_zone = if remaining.is_finite() {
                            remaining.clamp(20.0, 480.0)
                        } else {
                            20.0
                        };
                        layer_drop_strip(ui, doc, None, 0, 0, root_zone, &mut drop);
                    });
                if background.clicked() {
                    clear_canvas_selection(state);
                }
            });

        if let Some(drop) = drop {
            apply_layer_drop(state, doc, drop);
        }
        if let Some(action) = action {
            apply_layer_action(action, state, doc);
        }
    });
}

fn layer_header(ui: &mut egui::Ui, state: &mut EditorState, doc: &mut DocumentRes) {
    let selected = state
        .selected
        .as_deref()
        .and_then(|path| doc.0.layer_by_path(path));
    let has_selection = selected.is_some();
    // A live boolean group advertises its vector operation here; every other
    // layer advertises how it blends into the stack.
    let boolean_op = selected.and_then(|layer| layer.vector_op);
    let mut blend = selected
        .map(|layer| layer.blend)
        .unwrap_or(BlendMode::Derivative);
    let mut opacity = selected.map(|layer| layer.opacity).unwrap_or(1.0);
    let mut blend_changed = false;
    let mut opacity_changed = false;
    let mut boolean_changed = None;

    let label = if boolean_op.is_some() {
        "Boolean"
    } else {
        "Blend"
    };
    let tooltip = if boolean_op.is_some() {
        "Boolean operation of the selected group"
    } else {
        "Blend mode of the selected layer(s)"
    };

    ui.add_enabled_ui(has_selection, |ui| {
        let wide = ui.available_width() >= 260.0;
        if wide {
            // Lay the row out right-to-left and add the combo last: it then takes
            // exactly the space left over. The old left-to-right version guessed
            // the labels' and field's widths, and the small overflow ratcheted the
            // resizable dock wider every frame until it hit its maximum. The outer
            // `horizontal` keeps the row one line tall.
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let opacity_width = 54.0;
                    opacity_changed |= opacity_field(ui, &mut opacity, opacity_width);
                    ui.label(egui::RichText::new("Opacity").size(TEXT_SMALL).color(MUTED));
                    let label_width = ui
                        .painter()
                        .layout_no_wrap(
                            label.to_owned(),
                            egui::FontId::proportional(TEXT_SMALL),
                            MUTED,
                        )
                        .size()
                        .x;
                    let combo_width = (ui.available_width() - label_width - 12.0).max(56.0);
                    match boolean_op {
                        Some(op) => {
                            let (chosen, _) = vector_op_combo(
                                ui,
                                "header_boolean",
                                op,
                                combo_width,
                                26.0,
                                TEXT,
                                true,
                            );
                            if let Some(op) = chosen {
                                boolean_changed = Some(op);
                            }
                        }
                        None => {
                            let (mode, _) = blend_mode_combo(
                                ui,
                                "header_blend",
                                blend,
                                combo_width,
                                26.0,
                                TEXT,
                                true,
                            );
                            if let Some(mode) = mode {
                                blend = mode;
                                blend_changed = true;
                            }
                        }
                    }
                    ui.label(egui::RichText::new(label).size(TEXT_SMALL).color(MUTED))
                        .on_hover_text(tooltip);
                });
            });
        } else {
            let width = ui.available_width();
            match boolean_op {
                Some(op) => {
                    let (chosen, _) =
                        vector_op_combo(ui, "header_boolean", op, width, 26.0, TEXT, true);
                    if let Some(op) = chosen {
                        boolean_changed = Some(op);
                    }
                }
                None => {
                    let (mode, _) =
                        blend_mode_combo(ui, "header_blend", blend, width, 26.0, TEXT, true);
                    if let Some(mode) = mode {
                        blend = mode;
                        blend_changed = true;
                    }
                }
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(egui::RichText::new("Opacity").size(TEXT_SMALL).color(MUTED));
                let field_width = (ui.available_width() - 4.0).max(48.0);
                opacity_changed |= opacity_field(ui, &mut opacity, field_width);
            });
        }
    });

    if let Some(op) = boolean_changed {
        apply_layer_action(LayerAction::SetBoolean(op), state, doc);
    }
    if blend_changed || opacity_changed {
        apply_blend_to_selection(state, doc, blend, opacity);
    }
}

/// Opacity drag field shared by both header layouts; true when edited.
fn opacity_field(ui: &mut egui::Ui, opacity: &mut f32, width: f32) -> bool {
    let mut percent = *opacity * 100.0;
    let changed = ui
        .add_sized(
            [width, 24.0],
            egui::DragValue::new(&mut percent)
                .range(0.0..=100.0)
                .suffix("%")
                .fixed_decimals(0),
        )
        .changed();
    if changed {
        *opacity = (percent / 100.0).clamp(0.0, 1.0);
    }
    changed
}

fn apply_blend_to_selection(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    blend: BlendMode,
    opacity: f32,
) {
    let mut ids = Vec::new();
    if let Some(path) = state.selected.as_deref() {
        if let Some(layer) = doc.0.layer_by_path(path) {
            ids.push(layer.id);
        }
    }
    for path in &state.multi_selected {
        if let Some(layer) = doc.0.layer_by_path(path) {
            ids.push(layer.id);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    state.ensure_undo_gesture(&doc.0);
    for id in ids {
        if let Some(layer) = doc.0.find_layer_mut(id) {
            layer.blend = blend;
            layer.opacity = opacity;
            layer.touch();
        }
    }
    state.mark_dirty();
}

pub(crate) fn layer_footer(ui: &mut egui::Ui, state: &mut EditorState, doc: &mut DocumentRes) {
    let mut action = None;
    ui.add_space(1.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let button = egui::vec2(26.0, 26.0);
        let (can_up, can_down) = move_flags(&doc.0, state);
        // Disabled rather than hidden, so the footer buttons never shift.
        if icon_button_svg(
            ui,
            PACK_ARROW_UP,
            false,
            can_up,
            if can_up {
                "Move layer up"
            } else {
                "Already at the top"
            },
            button,
            false,
        )
        .clicked()
            && can_up
        {
            action = Some(LayerAction::Up);
        }
        if icon_button_svg(
            ui,
            PACK_ARROW_DOWN,
            false,
            can_down,
            if can_down {
                "Move layer down"
            } else {
                "Already at the bottom"
            },
            button,
            false,
        )
        .clicked()
            && can_down
        {
            action = Some(LayerAction::Down);
        }
        let selected_masked = state
            .selected
            .as_deref()
            .and_then(|path| doc.0.layer_by_path(path))
            .is_some_and(|layer| layer.mask_layer.is_some());
        if icon_button_sized(
            ui,
            Icon::Mask,
            selected_masked,
            state.selected.is_some(),
            if selected_masked {
                "Unbind the layer below as a mask"
            } else {
                "Clip to the layer below"
            },
            button,
        )
        .clicked()
        {
            action = Some(LayerAction::ToggleMask);
        }
        let editing_selected_mask = state
            .selected
            .as_deref()
            .and_then(|path| doc.0.layer_by_path(path))
            .is_some_and(|layer| state.mask_edit_layer == Some(layer.id));
        if icon_button_svg(
            ui,
            PACK_BRUSH,
            editing_selected_mask,
            state.selected.is_some(),
            "Paint the selected layer's mask on the 2D canvas",
            button,
            false,
        )
        .clicked()
        {
            action = Some(LayerAction::EditMask);
        }
        let has_selection = state.selected.is_some();
        if icon_button_svg(
            ui,
            PACK_TRASH,
            false,
            has_selection,
            if has_selection {
                "Delete the selected layer (Del). Undo is offered in the toast"
            } else {
                "Nothing selected"
            },
            button,
            true,
        )
        .clicked()
            && has_selection
        {
            action = Some(LayerAction::Delete);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let count = match doc.0.layer_count() {
                1 => "1 layer".to_string(),
                n => format!("{n} layers"),
            };
            let count_width = ui
                .painter()
                .layout_no_wrap(count.clone(), egui::FontId::proportional(TEXT_SMALL), MUTED)
                .size()
                .x;
            if ui.available_width() >= count_width + 4.0 {
                ui.label(egui::RichText::new(count).size(TEXT_SMALL).color(MUTED));
            }
            if ui.available_width() >= 62.0
                && text_button(ui, "Reset", 56.0, 28.0, false)
                    .on_hover_text(
                        "Replace the document with the demo scene. Undo is offered in the toast",
                    )
                    .clicked()
            {
                action = Some(LayerAction::ResetDemo);
            }
        });
    });
    if let Some(action) = action {
        // Destructive actions offer an Undo toast instead of a blocking modal.
        let toast = match &action {
            LayerAction::Delete => Some("Layer deleted"),
            LayerAction::ResetDemo => Some("Demo scene restored"),
            LayerAction::Duplicate => Some("Layer duplicated"),
            LayerAction::BakeToRaster => Some("Baked to pixels"),
            _ => None,
        };
        apply_layer_action(action, state, doc);
        if let Some(text) = toast {
            queue_toast(ui.ctx(), text, true);
        }
    }
}

pub(crate) fn move_flags(doc: &ns_core::Document, state: &EditorState) -> (bool, bool) {
    state
        .selected
        .as_deref()
        .map(|path| move_flags_for(doc, path))
        .unwrap_or((false, false))
}

pub(crate) fn move_flags_for(doc: &ns_core::Document, path: &[usize]) -> (bool, bool) {
    let (last, parents) = match path.split_last() {
        Some(parts) => parts,
        None => return (false, false),
    };
    let count = if parents.is_empty() {
        doc.layers.len()
    } else {
        doc.layer_by_path(parents)
            .and_then(Layer::children)
            .map(|children| children.len())
            .unwrap_or(0)
    };
    (*last + 1 < count, *last > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header row wider than the dock gives it becomes the panel's new min
    /// width, so the resizable Layers dock grows a few pixels every frame until
    /// it hits its maximum and ignores the width the user dragged to.
    #[test]
    fn the_blend_row_never_widens_the_dock() {
        let ctx = egui::Context::default();
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0.layers.push(Layer::flat("base"));
        let mut state = EditorState::default();
        let mut available = 0.0;
        let mut used = 0.0;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_width(420.0);
            available = ui.available_width();
            used = ui
                .scope(|ui| layer_header(ui, &mut state, &mut doc))
                .response
                .rect
                .width();
        });
        output.drop_without_applying_deltas();
        assert!(
            used <= available + 0.01,
            "the blend row is {used} px wide in a {available} px dock"
        );
    }

    fn frame(
        ctx: &egui::Context,
        state: &mut EditorState,
        doc: &mut DocumentRes,
        events: Vec<egui::Event>,
    ) {
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(320.0, 400.0),
        ));
        raw.events = events;
        let output = ctx.run_ui(raw, |ui| {
            ui.set_width(320.0);
            layers_card(ui, state, doc);
        });
        output.drop_without_applying_deltas();
    }

    fn click(ctx: &egui::Context, state: &mut EditorState, doc: &mut DocumentRes, at: egui::Pos2) {
        frame(ctx, state, doc, vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false] {
            frame(
                ctx,
                state,
                doc,
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
    }

    /// Clicking the list's empty area drops the whole selection; the rows and
    /// their controls are registered on top, so their clicks still select.
    #[test]
    fn clicking_the_empty_layer_list_clears_the_selection() {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| {
            options.max_passes = std::num::NonZeroUsize::new(1).unwrap();
        });
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(128)));
        doc.0.layers.push(Layer::flat("base"));
        doc.0.layers.push(Layer::flat("top"));
        let mut state = EditorState::default();
        state.selected = Some(vec![1]);
        state.multi_selected.push(vec![0]);
        // Lay out once so the panel has a measured height.
        frame(&ctx, &mut state, &mut doc, Vec::new());
        // Two 66 px rows end well above this point; the rest of the list is
        // filled by the root drop strip, which does not sense clicks.
        click(&ctx, &mut state, &mut doc, egui::pos2(160.0, 340.0));
        assert!(state.selected.is_none(), "the primary selection must clear");
        assert!(
            state.multi_selected.is_empty(),
            "the multi-selection must clear"
        );
    }
}
