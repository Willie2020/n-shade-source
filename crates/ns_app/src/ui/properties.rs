//! Properties dock content.

use super::*;
use ns_core::brush::Symmetry;

/// Edits requested from the boolean section. They are applied after the
/// mutable borrow of the selected layer ends.
enum CompoundUiAction {
    Flatten,
    SplitIslands,
    Release,
}

/// Distinct hues handed out by "Assign color", chosen so neighbouring slots
/// stay easy to tell apart in a Substance material list.
const ID_PALETTE: [[f32; 3]; 12] = [
    [0.90, 0.20, 0.20], // red
    [0.20, 0.55, 0.95], // blue
    [0.25, 0.80, 0.35], // green
    [0.95, 0.70, 0.15], // amber
    [0.70, 0.35, 0.90], // violet
    [0.15, 0.80, 0.80], // cyan
    [0.95, 0.45, 0.70], // pink
    [0.55, 0.75, 0.20], // lime
    [0.95, 0.55, 0.20], // orange
    [0.40, 0.45, 0.90], // indigo
    [0.65, 0.50, 0.30], // brown
    [0.75, 0.75, 0.75], // grey
];

/// Two colours count as the same material when their 8-bit export is equal.
fn same_color(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter()
        .zip(b.iter())
        .all(|(a, b)| (a - b).abs() <= 0.5 / 255.0)
}

/// Every ID colour already used anywhere in the document, groups included.
fn collect_id_colors(doc: &Document) -> Vec<[f32; 3]> {
    fn visit(layers: &[Layer], out: &mut Vec<[f32; 3]>) {
        for layer in layers {
            if let Some(color) = layer.id_color {
                if !out.iter().any(|used| same_color(*used, color)) {
                    out.push(color);
                }
            }
            if let Some(children) = layer.children() {
                visit(children, out);
            }
        }
    }
    let mut out = Vec::new();
    visit(&doc.layers, &mut out);
    out
}

/// First unused palette entry, then golden-angle hues once it runs out.
fn next_id_color(used: &[[f32; 3]]) -> [f32; 3] {
    if let Some(color) = ID_PALETTE
        .iter()
        .find(|candidate| !used.iter().any(|used| same_color(*used, **candidate)))
    {
        return *color;
    }
    let hue = (used.len() as f32 * 0.618_034).fract();
    hsv_color(hue, 0.65, 0.95)
}

/// Writes a rectangle's corner rounding, clamped so the radius can never
/// exceed half the shorter side, and remembers it as the default for the next
/// rectangle drawn. Returns whether the stored radius actually changed.
fn apply_rect_corner(
    short_side: f32,
    corner_radius: &mut f32,
    requested: f32,
    state: &mut EditorState,
) -> bool {
    let limit = (short_side * 0.5).max(0.0);
    let corner = if requested.is_finite() {
        requested.clamp(0.0, limit)
    } else {
        *corner_radius
    };
    if (corner - *corner_radius).abs() <= 1e-4 {
        return false;
    }
    *corner_radius = corner;
    if short_side > 0.0 {
        state.rect_corner = corner / short_side;
    }
    true
}

/// HSV to sRGB with hue in 0..1, saturation and value in 0..1.
fn hsv_color(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    let h = hue.rem_euclid(1.0) * 6.0;
    let c = value * saturation;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    [r + m, g + m, b + m]
}

/// One-line explanation used by the boolean buttons and their tooltips.
fn operation_hint(op: VectorOp) -> &'static str {
    match op {
        VectorOp::Add => "Merges the outlines into one shape; internal overlaps leave no seam.",
        VectorOp::Subtract => "Cuts the shapes above the bottom one out of it.",
        VectorOp::Intersect => "Keeps only the area covered by every visible shape.",
        VectorOp::Xor => "Removes the overlapping areas and keeps the outer regions.",
        VectorOp::Divide => "Splits the overlaps into separate regions so every cut is beveled.",
        VectorOp::PartialCut => {
            "Cuts the upper shapes' height profiles into the bottom shape and stops at its base, \
             leaving a pocket floor."
        }
        VectorOp::PartialSlash => {
            "Cuts tapered grooves into the bottom shape along each upper shape's long axis."
        }
        VectorOp::ClipDivide => {
            "Splits the bottom shape along the upper shapes' bands; each band's bevel becomes \
             the two seam walls."
        }
    }
}

pub(crate) struct PropertiesPanel;

impl Panel for PropertiesPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        // The projection tool owns the top of the dock while it is active; its
        // Apply/Cancel run after every card has been drawn.
        let mut projection_actions = ProjectionActions::default();
        if frame.projection.enabled {
            projection_actions = projection_card(ui, frame.projection);
        }
        // The layer's own settings come first; operations act on the
        // selection and follow below them.
        properties_card(ui, frame.state, frame.doc);
        // The two sections share the panel tone, so a hairline marks the seam.
        ui.add_space(2.0);
        ui.separator();
        let apply = operations_card(ui, frame.state, frame.doc);
        if projection_actions.cancel {
            frame.projection.disable();
            frame.state.status = "3D projection closed".into();
        } else if projection_actions.apply {
            crate::projection::apply_projection_now(frame.state, frame.doc, frame.projection);
        }
        if apply {
            if let Err(reason) = apply_selected_operation(frame.state, frame.doc) {
                frame.state.status = reason;
            }
        }
    }
}

/// Text-specific controls: content, font, size, tracking, line height and
/// alignment. The section only appears for live text sources; the Shape
/// section below still supplies the bevel, outline style and masks.
fn text_section(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    text: &mut TextSource,
    changed: &mut bool,
) {
    collapsible_section(
        ui,
        "text_section",
        Icon::Sliders,
        "Text",
        None,
        true,
        |ui| {
            ui.label(egui::RichText::new("Content").size(TEXT_LABEL).color(MUTED));
            let response = ui.add(
                egui::TextEdit::multiline(&mut text.content)
                    .desired_width(f32::INFINITY)
                    .desired_rows(2)
                    .margin(egui::Margin::symmetric(8, 6))
                    .hint_text("Type your text"),
            );
            if state.focus_text_content {
                response.request_focus();
                state.focus_text_content = false;
            }
            *changed |= response.changed();
            ui.label(
                egui::RichText::new("Enter starts a new line. The layer name follows the content until you rename it.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                field_label(ui, "Font");
                egui::ComboBox::from_id_salt("text_font")
                    .width(ui.available_width())
                    .selected_text(text.font.label())
                    .show_ui(ui, |ui| {
                        for font in BuiltinFont::ALL {
                            let source = FontSource::Builtin(font);
                            if ui
                                .selectable_label(text.font == source, font.label())
                                .clicked()
                            {
                                text.font = source;
                                *changed = true;
                            }
                        }
                        if matches!(text.font, FontSource::File { .. }) {
                            ui.separator();
                            let file_label = format!("File · {}", text.font.label());
                            ui.label(
                                egui::RichText::new(file_label)
                                    .size(TEXT_SMALL)
                                    .color(MUTED),
                            );
                        }
                    });
            });
            ui.horizontal(|ui| {
                if ui
                    .button("Load font file…")
                    .on_hover_text("Load a .ttf or .otf file. The path is stored in the project; bundled Inter is used if the file is missing.")
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Font", &["ttf", "otf", "ttc"])
                        .pick_file()
                    {
                        match FontSource::from_file(&path) {
                            Ok(source) => {
                                state.status = format!("Font loaded · {}", source.label());
                                text.font = source;
                                *changed = true;
                            }
                            Err(err) => state.status = format!("Font load failed · {err}"),
                        }
                    }
                }
                if !text.font_available() {
                    ui.label(
                        egui::RichText::new("Missing · using Inter")
                            .size(TEXT_CAPTION)
                            .color(WARNING),
                    )
                    .on_hover_text(
                        "The font file is no longer at its saved path. Pick a new file or choose a bundled face; the project still opens.",
                    );
                }
            });
            if let FontSource::File { path, .. } = &text.font {
                ui.label(
                    egui::RichText::new(path.as_str())
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                )
                .on_hover_text(path.as_str());
            }

            ui.add_space(8.0);
            *changed |= slider_row(ui, "Size (px)", &mut text.size_px, 4.0..=512.0, 0.5, 1);
            *changed |= slider_row(ui, "Tracking", &mut text.tracking_px, -20.0..=80.0, 0.25, 1);
            *changed |= slider_row(ui, "Line height", &mut text.line_height, 0.6..=3.0, 0.01, 2);
            ui.label(
                egui::RichText::new("Tracking is extra space between glyphs; line height is a multiple of the size.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                field_label(ui, "Align");
                for align in TextAlign::ALL {
                    *changed |= ui
                        .selectable_value(&mut text.align, align, align.label())
                        .changed();
                }
            });
            ui.label(
                egui::RichText::new(
                    "Text is live vector geometry: bevels, outlines, booleans, knife cuts, masks and Material ID all apply.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        },
    );
}

pub(crate) fn properties_card(ui: &mut egui::Ui, state: &mut EditorState, doc: &mut DocumentRes) {
    // The Text tool auto-focuses the Content field, so the usual pointer-press
    // checkpoint never runs. Start one baseline on the next frame: the first
    // keystroke then restores the default content, not the whole placement.
    if state.text_edit_baseline {
        state.finish_undo_gesture();
        state.begin_undo_gesture(&doc.0);
        state.text_edit_baseline = false;
    }
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        // Name the edited layer in the header: the dock is contextual and the
        // old title gave no clue which row it belonged to.
        let title = state
            .selected
            .as_deref()
            .and_then(|path| doc.0.layer_by_path(path))
            .map(|layer| format!("Properties · {}", layer.name))
            .unwrap_or_else(|| "Properties".to_string());
        section_title(ui, Icon::Settings, &title);
        ui.add_space(5.0);

        let shows_cable_tool_card = shows_cable_card(state) && !selected_is_cable(doc, state);
        if shows_cable_tool_card {
            cable_editor(ui, state);
            let path_ready = state
                .selected
                .as_deref()
                .and_then(|path| doc.0.layer_by_path(path))
                .is_some_and(is_vector_layer);
            ui.add_space(6.0);
            let create = ui
                .add_enabled(
                    path_ready,
                    egui::Button::new(
                        egui::RichText::new("Create cable from selected path").size(13.0),
                    ),
                )
                .on_hover_text(
                    "Draw the centreline with the Pen, select it, then convert it into a tube. \
                     The layer keeps its name, mask and ID colour.",
                )
                .clicked();
            if create {
                if let Some(path) = state.selected.clone() {
                    convert_path_layer_to_cable(state, doc, &path);
                }
            }
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(5.0);
        }

        if shows_brush_card(state) {
            brush_editor(ui, state, &doc.0);
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(5.0);
        }

        let Some(path) = state.selected.clone() else {
            ui.label(
                egui::RichText::new("No layer selected")
                    .size(TEXT_LABEL)
                    .color(MUTED),
            );
            ui.label(
                egui::RichText::new("Pick a layer in the Layers panel to edit it here.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            return;
        };
        let Some(layer_id) = doc.0.layer_by_path(&path).map(|layer| layer.id) else {
            state.selected = None;
            ui.label(
                egui::RichText::new("No layer selected")
                    .size(TEXT_LABEL)
                    .color(MUTED),
            );
            return;
        };
        // Captured before the mutable borrow: editing text content renames the
        // layer while its name still matches the old content.
        let layer_name = doc
            .0
            .layer_by_path(&path)
            .map(|layer| layer.name.clone())
            .unwrap_or_default();
        let mask_candidates = doc.0.mask_candidates(layer_id);

        let compound_operands = doc
            .0
            .layer_by_path(&path)
            .map(Layer::visible_vector_operand_count)
            .unwrap_or(0);
        let compound_style_source = doc
            .0
            .layer_by_path(&path)
            .and_then(Layer::children)
            .and_then(|children| {
                children
                    .iter()
                    .find(|child| child.paints() && child.has_shape_geometry())
                    .map(|child| {
                        if child.name.is_empty() {
                            "the bottom shape".to_string()
                        } else {
                            child.name.clone()
                        }
                    })
            });
        // Collected before the mutable borrow: "Assign color" needs to know
        // which palette entries every other layer already uses.
        let used_id_colors = collect_id_colors(&doc.0);
        // Collected before the mutable borrow: the tile picker offers every
        // tile detected on the imported mesh.
        let udim_tiles = if state.udim_enabled {
            state.udim_tiles.clone()
        } else {
            Vec::new()
        };
        // Presses in the properties card can edit the selected layer. Start the
        // gesture before the mutable borrow below so undo captures the pre-edit
        // value. Clicks in other panels no longer snapshot the document.
        if ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary))
            && ui.input(|input| {
                input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| ui.max_rect().contains(pos))
            })
        {
            state.ensure_undo_gesture(&doc.0);
        }
        // A cutter's parent boolean group tells this shape how it is used.
        let parent_cut_op = path.split_last().and_then(|(_, parent)| {
            if parent.is_empty() {
                None
            } else {
                doc.0
                    .layer_by_path(parent)
                    .and_then(|layer| layer.vector_op)
                    .filter(|op| op.is_cut())
            }
        });
        let Some(layer) = doc.0.layer_by_path_mut(&path) else {
            return;
        };
        let mut changed = false;
        let mut mask_changed = false;
        let mut outline_stroke_request = None;
        let mut cut_request = false;
        let mut curve_request: Option<Vec<LayerId>> = None;
        let mut edge_curve_request: Option<(LayerId, EdgeRef)> = None;
        let mut mask_action: Option<LayerAction> = None;
        let mut auto_rename: Option<String> = None;
        let initial_compound = layer.vector_op.is_some();
        let mut vector_op = layer.vector_op;
        let mut compound_action: Option<CompoundUiAction> = None;

        ui.horizontal(|ui| {
            field_label(ui, "Name");
            changed |= ui
                .add_sized(
                    [ui.available_width(), 26.0],
                    egui::TextEdit::singleline(&mut layer.name)
                        .margin(egui::Margin::symmetric(8, 5)),
                )
                .changed();
        });

        if !udim_tiles.is_empty() {
            ui.horizontal(|ui| {
                field_label(ui, "UDIM tile");
                let label = match layer.tile {
                    Some(tile) => format!("Tile {tile}"),
                    None => "All tiles".to_string(),
                };
                egui::ComboBox::from_id_salt("layer_udim_tile")
                    .width(ui.available_width())
                    .selected_text(egui::RichText::new(label).size(TEXT_LABEL))
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(&mut layer.tile, None, "All tiles")
                            .changed();
                        for tile in &udim_tiles {
                            changed |= ui
                                .selectable_value(&mut layer.tile, Some(*tile), format!("Tile {tile}"))
                                .changed();
                        }
                    });
            })
            .response
            .on_hover_text(
                "Which UDIM tile this layer contributes to. New layers follow the tile \
                 selected above the canvas; All tiles shares one layer across the mesh.",
            );
        }

        ui.add_space(2.0);
        // Stack the two controls on a minimum-width dock: keeping them on one
        // row used to set a wider minimum than the Bake tab and stretched the
        // whole side panel when switching tabs.
        let narrow = ui.available_width() < 240.0;
        if narrow {
            ui.horizontal(|ui| {
                changed |= toggle_switch(ui, &mut layer.visible).changed();
                ui.label(egui::RichText::new("Visible").size(TEXT_LABEL).color(TEXT));
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Opacity").size(TEXT_LABEL).color(MUTED));
                let mut percent = layer.opacity * 100.0;
                if ui
                    .add_sized(
                        [62.0, 26.0],
                        egui::DragValue::new(&mut percent)
                            .range(0.0..=100.0)
                            .speed(0.5)
                            .suffix("%")
                            .fixed_decimals(0),
                    )
                    .on_hover_text("Layer opacity in percent, matching the Layers dock")
                    .changed()
                {
                    layer.opacity = (percent / 100.0).clamp(0.0, 1.0);
                    changed = true;
                }
            });
        } else {
            ui.horizontal(|ui| {
                changed |= toggle_switch(ui, &mut layer.visible).changed();
                ui.label(egui::RichText::new("Visible").size(TEXT_LABEL).color(TEXT));
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Opacity").size(TEXT_LABEL).color(MUTED));
                let mut percent = layer.opacity * 100.0;
                if ui
                    .add_sized(
                        [62.0, 26.0],
                        egui::DragValue::new(&mut percent)
                            .range(0.0..=100.0)
                            .speed(0.5)
                            .suffix("%")
                            .fixed_decimals(0),
                    )
                    .on_hover_text("Layer opacity in percent, matching the Layers dock")
                    .changed()
                {
                    layer.opacity = (percent / 100.0).clamp(0.0, 1.0);
                    changed = true;
                }
            });
        }

        ui.add_space(2.0);
        // Text is the layer's defining content, so its editor sits above the
        // blend and mask controls instead of inside the bevel section.
        if let LayerKind::Shape(shape) = &mut layer.kind {
            if let ShapeSource::Text(text) = &mut shape.source {
                let old_content = text.content.clone();
                text_section(ui, state, text, &mut changed);
                if text.content != old_content && layer_name == old_content {
                    auto_rename = Some(text.content.clone());
                }
                ui.add_space(12.0);
            }
        }
        ui.horizontal(|ui| {
            field_label(ui, "Blend mode");
            let width = ui.available_width();
            egui::ComboBox::from_id_salt("blend_mode")
                .width(width)
                .selected_text(blend_label(layer.blend))
                .show_ui(ui, |ui| {
                    for mode in BLEND_MODES {
                        let choice =
                            ui.selectable_value(&mut layer.blend, mode, blend_label(mode));
                        if choice.changed() {
                            changed = true;
                        }
                        choice.on_hover_text(blend_hint(mode));
                    }
                });
        });
        if layer.blend.is_boolean() {
            ui.label(
                egui::RichText::new(
                    "Legacy surface boolean. Boolean work now happens on vector outlines through a boolean layer group.",
                )
                .size(TEXT_SMALL)
                .color(WARNING),
            );
        }

        // Material ID: one color per layer or group for the exported id.png.
        ui.add_space(2.0);
        // The hex readout goes on its own line so the row still fits when the
        // dock is at its minimum width.
        let mut id_hex = None;
        ui.horizontal(|ui| {
            field_label(ui, "Material ID");
            if let Some(color) = layer.id_color {
                let mut rgb = [
                    (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                ];
                if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
                    layer.id_color = Some([
                        rgb[0] as f32 / 255.0,
                        rgb[1] as f32 / 255.0,
                        rgb[2] as f32 / 255.0,
                    ]);
                    changed = true;
                }
                let hex = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
                if ui
                    .small_button("Clear")
                    .on_hover_text(format!("Remove {hex} from the exported ID map"))
                    .clicked()
                {
                    layer.id_color = None;
                    changed = true;
                }
                id_hex = Some(hex);
            } else if ui
                .button("Assign color")
                .on_hover_text("Give this layer or group its own color in the exported ID map")
                .clicked()
            {
                layer.id_color = Some(next_id_color(&used_id_colors));
                changed = true;
            }
        });
        if let Some(hex) = id_hex {
            ui.label(egui::RichText::new(hex).size(TEXT_CAPTION).color(MUTED));
        }
        if layer.id_color.is_some() {
            ui.label(
                egui::RichText::new(
                    "Exported as id.png · follows this layer's masks and clipping",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        }

        // The boolean operation is the layer's defining behaviour, so it sits
        // directly under the blend row. The shapes themselves stay internal:
        // only the combined outline reaches the normal map, so there is no
        // operand list to manage here.
        if initial_compound {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let icon = vector_op.map(vector_op_icon).unwrap_or(Icon::Grid);
                let (icon_rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                draw_icon(ui.painter(), icon_rect, icon, ACCENT);
                ui.label(
                    egui::RichText::new(match vector_op {
                        Some(op) => format!("Boolean · {}", op.label()),
                        None => "Group".into(),
                    })
                    .size(TEXT_LABEL)
                    .strong()
                    .color(TEXT),
                );
            });
            compound_action = boolean_editor(
                ui,
                &mut vector_op,
                compound_style_source.as_deref(),
                compound_operands,
                &mut changed,
            );
        }

        // A committed cable is one shape layer with authoring data attached;
        // its card edits the parameters and rebuilds the baked outline.
        changed |= cable_layer_properties(ui, layer, state);

        // Cuts are boolean-group operations; the group's Boolean section owns
        // the operation, and each cutter's Shape section owns its parameters.
        // The parent's cut mode widens the Radius range and adds Taper/Step.
        let cut_mode = parent_cut_op.is_some() || layer.blend.is_cut();
        match &mut layer.kind {
            LayerKind::Group { children } => {
                ui.add_space(12.0);
                if initial_compound {
                    if vector_op.is_some() {
                        ui.label(
                            egui::RichText::new("Boolean bevel style")
                                .size(TEXT_LABEL)
                                .color(TEXT),
                        );
                        ui.label(
                            egui::RichText::new(
                                "The bottom-most visible shape supplies these bevel settings; editing here applies them to the whole group.",
                            )
                            .size(TEXT_CAPTION)
                            .color(MUTED),
                        );
                    }
                } else {
                    let title = format!("Group · {} layers", children.len());
                    ui.horizontal(|ui| {
                        let (icon_rect, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                        draw_icon(ui.painter(), icon_rect, Icon::Grid, MUTED);
                        ui.label(egui::RichText::new(title).size(TEXT_LABEL).color(TEXT));
                    });
                    compound_action = boolean_editor(
                        ui,
                        &mut vector_op,
                        compound_style_source.as_deref(),
                        compound_operands,
                        &mut changed,
                    );
                    ui.label(
                        egui::RichText::new(
                            "Shapes keep their own outlines; the boolean is resolved before the bevel, so only the combined result reaches the normal map.",
                        )
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                    );
                }
                let (group_changed, custom_targets) = group_shape_properties(ui, children);
                changed |= group_changed;
                if custom_targets.is_some() {
                    curve_request = custom_targets;
                }
            }
            LayerKind::Shape(shape) => {
                ui.add_space(12.0);
                let raster_size = match &shape.source {
                    ShapeSource::Raster(raster) => Some((raster.w, raster.h)),
                    ShapeSource::RasterRegion(region) => Some((region.coverage.w, region.coverage.h)),
                    _ => None,
                };
                collapsible_section(
                    ui,
                    "shape_section",
                    if raster_size.is_some() {
                        Icon::Grid
                    } else {
                        Icon::Rectangle
                    },
                    if raster_size.is_some() { "Pixels" } else { "Shape" },
                    None,
                    true,
                    |ui| {
                        if shape.baked_normal.is_some() {
                            ui.label(
                                egui::RichText::new(
                                    "Baked group · drag this layer on the canvas to move it",
                                )
                                .size(TEXT_SMALL)
                                .color(MUTED),
                            )
                            .on_hover_text(
                                "The group's exact normals are stored as pixels. Its bevel and \
                                 profile are fixed; the layer still selects, snaps and moves like \
                                 a shape.",
                            );
                            return;
                        }
                        if let Some((w, h)) = raster_size {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Baked pixels · {w}×{h} px · not vector-editable"
                                ))
                                .size(TEXT_SMALL)
                                .color(MUTED),
                            )
                            .on_hover_text(
                                "This shape's geometry is stored as coverage. Bevel, \
                                 profile and blend still apply; node editing and \
                                 fill rules do not.",
                            );
                            ui.add_space(8.0);
                        } else {
                            ui.horizontal(|ui| {
                                field_label(ui, "Style");
                                egui::ComboBox::from_id_salt("shape_style")
                                    .width(ui.available_width())
                                    .selected_text(shape.style.label())
                                    .show_ui(ui, |ui| {
                                        changed |= ui
                                            .selectable_value(
                                                &mut shape.style,
                                                ShapeStyle::Fill,
                                                "Fill",
                                            )
                                            .changed();
                                        changed |= ui
                                            .selectable_value(
                                                &mut shape.style,
                                                ShapeStyle::Stroke,
                                                "Outline stroke",
                                            )
                                            .changed();
                                    });
                            });
                            if shape.style.is_stroke() {
                                changed |= slider_row(
                                    ui,
                                    "Stroke (px)",
                                    &mut shape.stroke_width_px,
                                    0.5..=256.0,
                                    0.5,
                                    1,
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "The band is centred on the shape boundary; Radius bevels both edges.",
                                    )
                                    .size(TEXT_SMALL)
                                    .color(MUTED),
                                );
                            }
                            ui.add_space(12.0);
                        }
                        // The rectangle's own corner rounding is geometry, not
                        // bevel: 0 px gives pointy corners, half the short side
                        // turns the rectangle into a pill. The last value picked
                        // here becomes the default for the next rectangle.
                        if let ShapeSource::Primitive(Primitive::Rect {
                            size,
                            corner_radius,
                            ..
                        }) = &mut shape.source
                        {
                            let short = size.abs().min_element();
                            let limit = short * 0.5;
                            if limit >= 1.0 {
                                let mut corner = corner_radius.clamp(0.0, limit);
                                changed |=
                                    slider_row(ui, "Corner (px)", &mut corner, 0.0..=limit, 0.25, 1);
                                changed |= apply_rect_corner(short, corner_radius, corner, state);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "0 px is a pointy corner; {limit:.0} px fills the short \
                                         side with the round."
                                    ))
                                    .size(TEXT_CAPTION)
                                    .color(MUTED),
                                )
                                .on_hover_text(
                                    "Rounds the rectangle's outline. The bevel Radius below is the \
                                     normal-map edge and does not change the silhouette.",
                                );
                            }
                        }
                        ui.add_space(2.0);
                        // Keep the per-layer bevel range aligned with the
                        // group control: 1–10 px keeps shared and individual
                        // bevels in the same predictable band. Cut layers use
                        // the same value as their wall width, which can be
                        // wider.
                        let radius_range = if cut_mode {
                            1.0..=64.0
                        } else {
                            1.0..=10.0
                        };
                        changed |= slider_row_soft_range(
                            ui,
                            "Radius (px)",
                            &mut shape.radius_px,
                            radius_range,
                            0.1,
                            1,
                        );
                        // Depth is signed: positive raises the bevel, negative
                        // carves it. The sign is stored in `invert`, so old
                        // projects keep loading unchanged and the GPU path sees
                        // the same flags it always did.
                        let mut depth = if shape.invert {
                            -shape.height_px
                        } else {
                            shape.height_px
                        };
                        if slider_row(
                            ui,
                            "Depth (px)",
                            &mut depth,
                            -128.0..=128.0,
                            0.2,
                            1,
                        ) {
                            shape.height_px = depth.abs();
                            shape.invert = depth < 0.0;
                            changed = true;
                        }
                        ui.label(
                            egui::RichText::new(
                                "Positive raises the bevel, negative carves it. Depth over Radius is \
                                 the slope, so deeper edges come from a larger Depth or a smaller \
                                 Radius.",
                            )
                            .size(TEXT_CAPTION)
                            .color(MUTED),
                        )
                        .on_hover_text(
                            "Height used to shade every Round and Chamfer bevel as if it were one \
                             radius tall, so changing it looked identical. The normal now follows the \
                             real rise-over-run ratio, which is what makes plates read at different \
                             depths in the map.",
                        );
                        changed |= slider_row(
                            ui,
                            "Softness (px)",
                            &mut shape.softness_px,
                            0.0..=32.0,
                            0.1,
                            1,
                        );
                        changed |= slider_row(
                            ui,
                            "Smooth (px)",
                            &mut shape.smooth_px,
                            0.0..=8.0,
                            0.1,
                            1,
                        );
                        ui.label(
                            egui::RichText::new("Smooths jagged edges and mask borders.")
                                .size(TEXT_CAPTION)
                                .color(MUTED),
                        )
                        .on_hover_text(
                            "Averages the outline and its shading: removes jagged edges, noisy \
                             mask borders and faceted radii while the profile keeps its shape. \
                             Softness softens the slope instead.",
                        );
                        changed |= slider_row(
                            ui,
                            "Anti-alias",
                            &mut shape.edge_aa_px,
                            0.0..=8.0,
                            0.1,
                            1,
                        );
                        ui.label(
                            egui::RichText::new(
                                "Fades the bevel to flat at the edge; raise it when small shapes stair-step.",
                            )
                            .size(TEXT_CAPTION)
                            .color(MUTED),
                        )
                        .on_hover_text(
                            "Softens the silhouette without changing the interior slope. Unlike \
                             Smooth, which averages the outline, this ramps the normal back to \
                             flat over the last pixels of the bevel.",
                        );
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            changed |= toggle_switch(ui, &mut shape.invert).changed();
                            ui.label(
                                egui::RichText::new("Recessed (carve)")
                                    .size(12.0),
                            );
                        });
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            field_label(ui, "Profile");
                            let show_curve = ui.available_width() > 170.0;
                            let combo_width = (ui.available_width()
                                - if show_curve { 48.0 } else { 4.0 })
                            .max(80.0);
                            let mut choice = None;
                            egui::ComboBox::from_id_salt("profile")
                                .width(combo_width)
                                .height(260.0)
                                .selected_text(profile_label(&shape.profile))
                                .show_ui(ui, |ui| {
                                    for (profile, label) in profile_choices() {
                                        if ui
                                            .selectable_label(shape.profile == profile, label)
                                            .clicked()
                                        {
                                            choice = Some(profile);
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    if ui
                                        .selectable_label(
                                            matches!(shape.profile, Profile::Custom(_)),
                                            "Custom curve...",
                                        )
                                        .clicked()
                                    {
                                        curve_request = Some(vec![layer_id]);
                                        ui.close();
                                    }
                                });
                            if let Some(profile) = choice {
                                shape.profile = profile;
                                changed = true;
                            }
                            if show_curve {
                                let (curve_rect, curve_response) = ui.allocate_exact_size(
                                    egui::vec2(38.0, 24.0),
                                    egui::Sense::click(),
                                );
                                draw_profile_curve(ui.painter(), curve_rect, &shape.profile);
                                if curve_response
                                    .on_hover_text("Edit the bevel curve")
                                    .clicked()
                                {
                                    curve_request = Some(vec![layer_id]);
                                }
                            }
                        });
                        if let Profile::Chamfer { degrees } = &mut shape.profile {
                            changed |= slider_row(ui, "Angle", degrees, 5.0..=85.0, 0.5, 1);
                        }
                        if matches!(shape.profile, Profile::Custom(_))
                            && ui.button("Edit custom curve...").clicked()
                        {
                            curve_request = Some(vec![layer_id]);
                        }
                        // A cutter child gets its cut-specific parameters here;
                        // the operation itself lives on the parent boolean group.
                        if let Some(cut_op) = parent_cut_op {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("Cutter")
                                    .size(TEXT_LABEL)
                                    .strong()
                                    .color(TEXT),
                            );
                            match cut_op {
                                VectorOp::PartialSlash => {
                                    changed |= slider_row(
                                        ui,
                                        "Taper in",
                                        &mut shape.cut_taper_in,
                                        0.0..=0.5,
                                        0.005,
                                        3,
                                    );
                                    changed |= slider_row(
                                        ui,
                                        "Taper out",
                                        &mut shape.cut_taper_out,
                                        0.0..=0.5,
                                        0.005,
                                        3,
                                    );
                                    ui.label(
                                        egui::RichText::new(
                                            "Depth, Radius and Profile above are the groove's \
                                             depth, wall width and cross-section. Taper fades it \
                                             at either end of the long axis.",
                                        )
                                        .size(TEXT_CAPTION)
                                        .color(MUTED),
                                    );
                                }
                                VectorOp::ClipDivide => {
                                    changed |= slider_row(
                                        ui,
                                        "Step (px)",
                                        &mut shape.cut_step_px,
                                        -96.0..=96.0,
                                        0.1,
                                        1,
                                    );
                                    ui.label(
                                        egui::RichText::new(
                                            "The band's Radius and Profile above become the two \
                                             seam walls; Step raises the far side so the split \
                                             reads as two plates.",
                                        )
                                        .size(TEXT_CAPTION)
                                        .color(MUTED),
                                    );
                                }
                                VectorOp::PartialCut => {
                                    ui.label(
                                        egui::RichText::new(
                                            "Depth, Radius and Profile above are the pocket's \
                                             depth, wall width and wall curve. The cut stops at \
                                             the bottom shape's base.",
                                        )
                                        .size(TEXT_CAPTION)
                                        .color(MUTED),
                                    );
                                }
                                _ => {}
                            }
                        }
                        ui.add_space(3.0);
                        if let ShapeSource::Path(path) = &mut shape.source {
                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                field_label(ui, "Use as");
                                let previous = shape.vector_role;
                                egui::ComboBox::from_id_salt("vector_role")
                                    .width(ui.available_width())
                                    .selected_text(shape.vector_role.label())
                                    .show_ui(ui, |ui| {
                                        for role in VectorRole::ALL {
                                            changed |= ui
                                                .selectable_value(
                                                    &mut shape.vector_role,
                                                    role,
                                                    role.label(),
                                                )
                                                .changed();
                                        }
                                    });
                                if previous != shape.vector_role {
                                    state.status = match shape.vector_role {
                                        VectorRole::Surface => {
                                            "Vector · normal shape".into()
                                        }
                                        VectorRole::Path => {
                                            "Vector · path guide · add a Sweep operation to put \
                                             copies on it"
                                                .into()
                                        }
                                    };
                                }
                            });
                            if shape.vector_role == VectorRole::Path {
                                ui.label(
                                    egui::RichText::new(
                                        "A path guide is not painted. Its spine drives Sweep \
                                         operations; the shape keeps its nodes and stays editable.",
                                    )
                                    .size(TEXT_SMALL)
                                    .color(MUTED),
                                );
                            }
                            ui.horizontal(|ui| {
                                field_label(ui, "Fill rule");
                                egui::ComboBox::from_id_salt("vector_fill_rule")
                                    .width(ui.available_width())
                                    .selected_text(match path.fill_rule {
                                        FillRule::NonZero => "Non-zero",
                                        FillRule::EvenOdd => "Even-odd",
                                    })
                                    .show_ui(ui, |ui| {
                                        changed |= ui
                                            .selectable_value(
                                                &mut path.fill_rule,
                                                FillRule::NonZero,
                                                "Non-zero",
                                            )
                                            .changed();
                                        changed |= ui
                                            .selectable_value(
                                                &mut path.fill_rule,
                                                FillRule::EvenOdd,
                                                "Even-odd",
                                            )
                                            .changed();
                                    });
                            });
                            ui.label(
                                egui::RichText::new(format!(
                                    "Vector path · {} node{}",
                                    path.node_count(),
                                    if path.node_count() == 1 { "" } else { "s" }
                                ))
                                .size(TEXT_SMALL)
                                .color(MUTED),
                            );
                        } else if matches!(shape.source, ShapeSource::Primitive(_)) {
                            ui.add_space(4.0);
                            if ui.button("Convert to editable path").clicked() {
                                if let Some(path) = shape.source.to_path() {
                                    shape.source = ShapeSource::Path(path);
                                    state.vector_tool = CanvasTool::Node;
                                    state.canvas_tool = CanvasTool::Node;
                                    state.viewport_tab = ViewportTab::TwoD;
                                    state.node_selection.clear();
                                    state.node_hover = None;
                                    state.status = NODE_HINT.into();
                                    changed = true;
                                }
                            }
                        }

                        if raster_size.is_none() {
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new("Extract outline")
                                    .size(TEXT_LABEL)
                                    .color(TEXT),
                            );
                            let _ = slider_row(
                                ui,
                                "Width (px)",
                                &mut state.stroke_width,
                                1.0..=128.0,
                                0.5,
                                1,
                            );
                            if ui.button("Create outline layer").clicked() {
                                outline_stroke_request = Some(state.stroke_width);
                            }
                            ui.label(
                                egui::RichText::new(
                                    "Creates a separate round-joined stroke shape above this layer.",
                                )
                                .size(TEXT_SMALL)
                                .color(MUTED),
                            );
                        }

                        if raster_size.is_none() {
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new("Cut shapes").size(TEXT_LABEL).color(TEXT),
                            );
                            if ui
                                .button("Cut shapes below")
                                .on_hover_text(
                                    "Subtract this shape from the shapes behind it and keep every cut piece as its own layer",
                                )
                                .clicked()
                            {
                                cut_request = true;
                            }
                            ui.label(
                                egui::RichText::new(
                                    "Runs on this outline: with Style set to Outline stroke the line's band cuts along the path, and every piece keeps this bevel style as its own layer.",
                                )
                                .size(TEXT_SMALL)
                                .color(MUTED),
                            );
                        }
                    });

                // Per-edge bevels are a vector-edit feature: they only appear
                // while the Node tool is active and an edge is selected, so the
                // normal Shape section stays unchanged for regular layers.
                if state.canvas_tool.is_node_edit_tool() {
                    if let (Some(edge), ShapeSource::Path(path)) =
                        (state.edge_selection, &mut shape.source)
                    {
                        let default_profile = shape.profile.clone();
                        changed |= advanced_edge_properties(
                            ui,
                            path,
                            &default_profile,
                            edge,
                            layer_id,
                            &mut edge_curve_request,
                        );
                    }
                }
            }
            LayerKind::Fill { height } => {
                ui.add_space(12.0);
                changed |= slider_row(ui, "Fill height", height, 0.0..=1.0, 0.01, 3);
                ui.label(
                    egui::RichText::new("Fill layers only contribute to height exports.")
                        .size(TEXT_SMALL)
                        .color(MUTED),
                );
            }
            LayerKind::HeightPaint { height, strength } => {
                ui.add_space(12.0);
                changed |= slider_row(ui, "Strength", strength, 0.1..=64.0, 0.2, 1);
                ui.add_space(2.0);
                ui.label(egui::RichText::new("Brush").size(TEXT_LABEL).color(TEXT));
                ui.horizontal(|ui| {
                    field_label(ui, "Mode");
                    egui::ComboBox::from_id_salt("paint_mode")
                        .width(ui.available_width())
                        .selected_text(state.brush_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in PaintMode::ALL {
                                ui.selectable_value(&mut state.brush_mode, mode, mode.label());
                            }
                        });
                });
                let _ = slider_row(
                    ui,
                    "Radius (px)",
                    &mut state.brush_radius,
                    1.0..=256.0,
                    0.5,
                    1,
                );
                let _ = slider_row(
                    ui,
                    "Hardness",
                    &mut state.brush_hardness,
                    0.0..=1.0,
                    0.01,
                    2,
                );
                let _ = slider_row(ui, "Flow", &mut state.brush_flow, 0.01..=1.0, 0.01, 2);
                ui.add_space(4.0);
                pen_controls(ui, state);
                if state.brush_mode == PaintMode::Flatten {
                    let _ = slider_row(
                        ui,
                        "Target height",
                        &mut state.brush_flatten_target,
                        0.0..=1.0,
                        0.01,
                        2,
                    );
                }
                if ui.button("Clear paint").clicked() {
                    height.data.fill(0.0);
                    changed = true;
                }
                ui.label(
                    egui::RichText::new(
                        "Select the Paint tool and drag on the 2D canvas. Painting creates \
                         a height-paint layer if the selected layer is not one.",
                    )
                    .size(TEXT_SMALL)
                    .color(MUTED),
                );
            }
            LayerKind::NormalPaint { normal } => {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("Normal paint")
                        .size(TEXT_LABEL)
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new(
                        "Normal-map brush strokes are stored directly. Painting adds their \
                         slopes to the surface below; no height field is derived.",
                    )
                    .size(TEXT_SMALL)
                    .color(MUTED),
                );
                if ui.button("Clear paint").clicked() {
                    normal.data.fill(glam::Vec3::Z);
                    changed = true;
                }
            }
            LayerKind::PhotoHeight(photo) => {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {} × {}",
                            if photo.alpha { "Alpha" } else { "Photo" },
                            photo.height.w,
                            photo.height.h
                        ))
                        .size(TEXT_SMALL)
                        .color(MUTED),
                    );
                    if !photo.alpha {
                        ui.menu_button("Presets...", |ui| {
                            for preset in PhotoPreset::ALL {
                                if ui.button(preset.label()).clicked() {
                                    let scale = photo.height.w as f32 / 1024.0;
                                    let (strength, radii, weights) = preset.params(scale);
                                    photo.strength = strength;
                                    photo.blur_radii = radii;
                                    photo.weights = weights;
                                    changed = true;
                                    ui.close();
                                }
                            }
                        });
                    }
                });
                if photo.alpha {
                    ui.label(
                        egui::RichText::new(
                            "Imported from the image alpha channel (or luminance for opaque \
                             images). Bright pixels become raised detail and bake straight into \
                             the normal map.",
                        )
                        .size(TEXT_SMALL)
                        .color(MUTED),
                    );
                }
                changed |= slider_row(ui, "Strength", &mut photo.strength, 0.1..=64.0, 0.2, 1);
                for i in 0..3 {
                    changed |= slider_row(
                        ui,
                        &format!("Blur {}", i + 1),
                        &mut photo.blur_radii[i],
                        0.0..=64.0,
                        0.2,
                        1,
                    );
                    changed |= slider_row(
                        ui,
                        &format!("Weight {}", i + 1),
                        &mut photo.weights[i],
                        -2.0..=2.0,
                        0.01,
                        2,
                    );
                }
            }
            LayerKind::ImportedNormal { .. } => {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("Imported normal map")
                        .size(TEXT_LABEL)
                        .color(MUTED),
                );
            }
            LayerKind::Flat => {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("Flat base layer")
                        .size(TEXT_LABEL)
                        .color(MUTED),
                );
            }
        }

        // The mask follows the layer-kind section: Shape, Group or another
        // kind's settings are the primary content, masking refines them.
        ui.add_space(12.0);
        let mask_result = mask_section(ui, state, layer, &mask_candidates);
        changed |= mask_result.changed;
        mask_changed |= mask_result.mask_changed;
        let begin_mask = mask_result.begin_edit;
        if mask_result.bind_below || mask_result.unbind {
            mask_action = Some(LayerAction::ToggleMask);
        }
        if mask_result.new_mask_layer {
            mask_action = Some(LayerAction::NewMaskLayer);
        }

        if vector_op != layer.vector_op {
            if let Some(new_op) = vector_op {
                if layer.vector_op.is_none() && (layer.name.is_empty() || layer.name == "Group") {
                    layer.name = format!("{} boolean", new_op.short_label());
                } else if let Some(old_op) = layer.vector_op {
                    let old_name = format!("{} boolean", old_op.short_label());
                    if layer.name == old_name {
                        layer.name = format!("{} boolean", new_op.short_label());
                    }
                }
            } else if let Some(old_op) = layer.vector_op {
                if layer.name == format!("{} boolean", old_op.short_label()) {
                    layer.name = "Group".into();
                }
            }
            layer.vector_op = vector_op;
            changed = true;
            state.status = match vector_op {
                Some(op) => format!("Boolean · {}", op.label()),
                None => "Boolean off · ordinary group".into(),
            };
        }

        if changed {
            layer.touch();
            state.mark_dirty();
        }
        if let Some(name) = auto_rename {
            layer.name = if name.is_empty() { "Text".into() } else { name };
        }
        if mask_changed {
            layer.touch_mask();
            state.mark_dirty();
        }
        if begin_mask {
            begin_mask_edit(state, doc, layer_id);
        }
        if let Some(action) = mask_action {
            apply_layer_action(action, state, doc);
        }
        if let Some(width) = outline_stroke_request {
            outline_selected_stroke(state, doc, width);
        }
        if cut_request {
            state.selected = Some(path.clone());
            apply_layer_action(LayerAction::CutBelow, state, doc);
        }
        if let Some(targets) = curve_request {
            open_curve_editor(state, doc, targets);
        }
        if let Some((layer_id, edge)) = edge_curve_request {
            open_edge_curve_editor(state, doc, layer_id, edge);
        }
        if let Some(action) = compound_action {
            match action {
                CompoundUiAction::Flatten => {
                    state.selected = Some(path.clone());
                    state.multi_selected.clear();
                    apply_layer_action(LayerAction::FlattenCompound, state, doc);
                }
                CompoundUiAction::SplitIslands => {
                    state.selected = Some(path.clone());
                    state.multi_selected.clear();
                    apply_layer_action(LayerAction::ReleaseDivide, state, doc);
                }
                CompoundUiAction::Release => {
                    state.selected = Some(path.clone());
                    state.multi_selected.clear();
                    apply_layer_action(LayerAction::UngroupSelected, state, doc);
                }
            }
        }
    });
}

/// Choose the boolean operation for a group of vector shapes and commit the
/// result when the user asks for it.
///
/// The shapes never need to be inspected: their outlines are combined before
/// the bevel, so the group behaves like one shape whose silhouette is the
/// boolean result.
/// Advanced vector-edit properties for the selected path edge.
///
/// Only the shape's Node tool shows this section. The override lives on the
/// edge's start node, so the rest of the path keeps the shape profile and the
/// layer keeps evaluating through the same bevel pipeline.
fn advanced_edge_properties(
    ui: &mut egui::Ui,
    path: &mut Path,
    default_profile: &Profile,
    edge: EdgeRef,
    layer_id: LayerId,
    edge_curve_request: &mut Option<(LayerId, EdgeRef)>,
) -> bool {
    let count = path.edge_count(edge.subpath);
    if edge.edge >= count {
        return false;
    }
    let node_count = path.subpaths[edge.subpath].nodes.len();
    let from = edge.edge + 1;
    let to = if edge.edge + 1 < node_count {
        edge.edge + 2
    } else {
        1
    };
    let mut changed = false;
    ui.add_space(12.0);
    collapsible_section(
        ui,
        "advanced_edge_properties",
        Icon::Sliders,
        "Advanced properties",
        None,
        true,
        |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Edge {}/{} · node {from} → {to}",
                    edge.edge + 1,
                    count
                ))
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
            let current = path.edge_profile(edge.subpath, edge.edge).cloned();
            let default_label = format!("Shape default ({})", profile_label(default_profile));
            let selected_text = current
                .as_ref()
                .map(profile_label)
                .unwrap_or_else(|| default_label.clone());
            let mut choice: Option<Option<Profile>> = None;
            ui.horizontal(|ui| {
                field_label(ui, "Bevel");
                egui::ComboBox::from_id_salt("edge_bevel_profile")
                    .width(ui.available_width())
                    .height(260.0)
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(current.is_none(), default_label.clone())
                            .clicked()
                        {
                            choice = Some(None);
                            ui.close();
                        }
                        for (profile, label) in profile_choices() {
                            if ui
                                .selectable_label(current.as_ref() == Some(&profile), label)
                                .clicked()
                            {
                                choice = Some(Some(profile));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui
                            .selectable_label(
                                matches!(current, Some(Profile::Custom(_))),
                                "Custom curve...",
                            )
                            .clicked()
                        {
                            *edge_curve_request = Some((layer_id, edge));
                            ui.close();
                        }
                    });
            });
            if let Some(profile) = choice {
                path.set_edge_profile(edge.subpath, edge.edge, profile);
                changed = true;
            }
            if let Some(Profile::Chamfer { degrees }) = path
                .subpaths
                .get_mut(edge.subpath)
                .and_then(|subpath| subpath.nodes.get_mut(edge.edge))
                .and_then(|node| node.bevel.as_mut())
            {
                changed |= slider_row(ui, "Angle", degrees, 5.0..=85.0, 0.5, 1);
            }
            if matches!(current, Some(Profile::Custom(_)))
                && ui.button("Edit custom curve...").clicked()
            {
                *edge_curve_request = Some((layer_id, edge));
            }
            if current.is_some() && ui.button("Reset to shape default").clicked() {
                path.set_edge_profile(edge.subpath, edge.edge, None);
                changed = true;
            }
            ui.label(
                egui::RichText::new(
                    "Applies to this edge only. Profiles blend across the corner so the normal \
                     map stays continuous.",
                )
                .size(TEXT_SMALL)
                .color(MUTED),
            );
        },
    );
    changed
}

fn shows_brush_card(state: &EditorState) -> bool {
    matches!(state.canvas_tool, CanvasTool::Paint | CanvasTool::Pen)
        || state.mask_edit_layer.is_some()
}

/// Cable settings live in Properties while the Cable tool is active, so the
/// terminal shape can be chosen before the first waypoint is placed.
fn shows_cable_card(state: &EditorState) -> bool {
    state.canvas_tool == CanvasTool::Cable
}

/// True when the selected layer is a cable, or a child of one, in which case
/// the layer's own card replaces the tool defaults.
fn selected_is_cable(doc: &DocumentRes, state: &EditorState) -> bool {
    let Some(path) = state.selected.as_deref() else {
        return false;
    };
    if doc
        .0
        .layer_by_path(path)
        .is_some_and(|layer| layer.cable.is_some())
    {
        return true;
    }
    path.len() > 1
        && doc
            .0
            .layer_by_path(&path[..path.len() - 1])
            .is_some_and(|layer| layer.cable.is_some())
}

/// Editable cable parameters after creation.
///
/// Width and terminal changes rebuild the baked outline; changing only Height
/// updates the existing shape's bevel depth, which is cheaper while dragging.
fn cable_layer_properties(ui: &mut egui::Ui, layer: &mut Layer, state: &mut EditorState) -> bool {
    let Some(original) = layer.cable.clone() else {
        return false;
    };
    let mut meta = original;
    let mut geometry_changed = false;
    let mut height_changed = false;
    ui.add_space(12.0);
    collapsible_section(
        ui,
        "cable_layer_properties",
        Icon::Cable,
        "Cable",
        None,
        true,
        |ui| {
            geometry_changed |= slider_row(ui, "Width (px)", &mut meta.width, 2.0..=256.0, 0.5, 1);
            height_changed |= slider_row(ui, "Height (px)", &mut meta.height, 0.5..=64.0, 0.1, 1);
            labeled_combo(ui, "Ends", "cable_layer_cap", meta.cap.label(), |ui| {
                for cap in CableCap::ALL {
                    if ui
                        .selectable_value(&mut meta.cap, cap, cap.label())
                        .changed()
                    {
                        geometry_changed = true;
                    }
                }
            });
            geometry_changed |= slider_row(ui, "End size", &mut meta.cap_scale, 0.5..=3.0, 0.01, 2);
            ui.label(
                egui::RichText::new(
                    "Changing Width or Ends rebuilds the tube from its centreline path. Height only \
                     changes the bevel depth.",
                )
                .size(TEXT_SMALL)
                .color(MUTED),
            );
        },
    );
    if !geometry_changed && !height_changed {
        return false;
    }
    let height = meta.height.clamp(0.5, 256.0);
    meta.height = height;
    layer.cable = Some(meta.clone());
    if geometry_changed {
        if let Some(rebuilt) = ns_core::cable_layer(&meta) {
            layer.kind = rebuilt.kind;
        }
    } else if let LayerKind::Group { children } = &mut layer.kind {
        // Height alone does not change the silhouette: update the body and the
        // terminals in place instead of rebuilding the whole tube.
        for child in children {
            if let LayerKind::Shape(shape) = &mut child.kind {
                shape.height_px = if child.name == "Cable body" {
                    height
                } else {
                    height * 1.05
                };
            }
            child.touch();
        }
    }
    layer.touch();
    state.node_selection.clear();
    state.edge_selection = None;
    true
}

/// Tube size and terminal shape for the Cable tool. The tube is a single
/// smooth stroke and each end gets an ordinary shape layer, so both stay
/// editable after the cable is committed.
fn cable_editor(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        draw_icon(ui.painter(), rect, Icon::Cable, MUTED);
        ui.label(
            egui::RichText::new("Cable tool")
                .size(TEXT_LABEL)
                .color(TEXT),
        );
    });
    ui.label(
        egui::RichText::new(
            "Defaults for the next cable. Draw the centreline with the Pen, then click that \
             path with the Cable tool (or use the button below).",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
    let _ = slider_row(
        ui,
        "Width (px)",
        &mut state.cable_width,
        2.0..=128.0,
        0.5,
        1,
    );
    let _ = slider_row(
        ui,
        "Height (px)",
        &mut state.cable_height,
        0.5..=64.0,
        0.1,
        1,
    );
    labeled_combo(ui, "Ends", "cable_cap", state.cable_cap.label(), |ui| {
        for cap in CableCap::ALL {
            ui.selectable_value(&mut state.cable_cap, cap, cap.label());
        }
    });
    let _ = slider_row(
        ui,
        "End size",
        &mut state.cable_cap_scale,
        0.5..=3.0,
        0.01,
        2,
    );
    ui.label(
        egui::RichText::new("End size is a multiple of the tube width.")
            .size(TEXT_SMALL)
            .color(MUTED),
    );
}

/// Brush library, pressure, symmetry and radial controls. Shown while the
/// painting tool (or the Pen tool) is active, so a tip can be imported before
/// the first stroke and without selecting a paint layer first.
fn brush_editor(ui: &mut egui::Ui, state: &mut EditorState, doc: &Document) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        draw_icon(ui.painter(), rect, Icon::Brush, MUTED);
        ui.label(egui::RichText::new("Brush").size(TEXT_LABEL).color(TEXT));
        if state.canvas_tool == CanvasTool::Pen {
            ui.label(
                egui::RichText::new("used by Paint (B)")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }
    });

    ui.horizontal(|ui| {
        field_label(ui, "Tip");
        let mut chosen = state.brush_library.selected_index();
        let selected_name = state
            .brush_library
            .selected()
            .map(|tip| tip.name.clone())
            .unwrap_or_else(|| "Soft round".to_string());
        egui::ComboBox::from_id_salt("brush_tip")
            .width(ui.available_width())
            .selected_text(selected_name)
            .show_ui(ui, |ui| {
                for (index, tip) in state.brush_library.tips.iter().enumerate() {
                    let kind = if tip.is_normal() {
                        "normal"
                    } else if tip.is_image() {
                        "alpha"
                    } else {
                        "round"
                    };
                    ui.selectable_value(&mut chosen, index, format!("{} · {kind}", tip.name));
                }
            });
        state.brush_library.select(chosen);
    });

    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Import alpha...")
            .on_hover_text(
                "Import a grayscale or transparent PNG as a height/mask brush tip. \
                 Transparent pixels are skipped.",
            )
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "Brush images",
                    &[
                        "png", "jpg", "jpeg", "tga", "exr", "bmp", "tif", "tiff", "webp",
                    ],
                )
                .pick_file()
            {
                match crate::brushes::load_alpha_tip(&path) {
                    Ok(tip) => {
                        let name = tip.name.clone();
                        state.brush_library.push(tip);
                        state.status = format!("Alpha brush added · {name}");
                    }
                    Err(error) => state.status = error,
                }
            }
        }
        if ui
            .button("Import normal...")
            .on_hover_text(
                "Import a tangent-space normal map as a stamp. Its slopes are added \
                 to the surface instead of being converted to height.",
            )
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "Normal maps",
                    &[
                        "png", "jpg", "jpeg", "tga", "exr", "bmp", "tif", "tiff", "webp",
                    ],
                )
                .pick_file()
            {
                let flip_green = doc.green == GreenChannel::DirectX;
                match crate::brushes::load_normal_tip(&path, flip_green) {
                    Ok(tip) => {
                        let name = tip.name.clone();
                        state.brush_library.push(tip);
                        state.status = format!("Normal brush added · {name}");
                    }
                    Err(error) => state.status = error,
                }
            }
        }
        if ui.button("Remove").clicked() && !state.brush_library.remove_selected() {
            state.status = "Keep at least one brush tip".into();
        }
    });

    let normal_tip = state.brush_library.is_normal();
    let image_tip = state
        .brush_library
        .selected()
        .is_some_and(|tip| tip.is_image());
    ui.label(
        egui::RichText::new(if normal_tip {
            "Normal tips paint into a Normal paint layer. Painting creates one if the \
             selected layer is not a normal paint layer."
        } else {
            "Alpha tips paint height. The alpha or luminance of the image is the dab \
             coverage; hardness applies to the round tip only."
        })
        .size(TEXT_SMALL)
        .color(MUTED),
    );

    let _ = slider_row(
        ui,
        "Radius (px)",
        &mut state.brush_radius,
        1.0..=256.0,
        0.5,
        1,
    );
    let _ = slider_row(
        ui,
        "Angle",
        &mut state.brush_angle_deg,
        -180.0..=180.0,
        1.0,
        0,
    );
    ui.add_enabled_ui(!image_tip, |ui| {
        let _ = slider_row(
            ui,
            "Hardness",
            &mut state.brush_hardness,
            0.0..=1.0,
            0.01,
            2,
        );
    });
    let _ = slider_row(ui, "Flow", &mut state.brush_flow, 0.01..=1.0, 0.01, 2);
    let _ = slider_row(ui, "Opacity", &mut state.brush_opacity, 0.0..=1.0, 0.01, 2);
    ui.add_space(4.0);
    pen_controls(ui, state);

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        field_label(ui, "Symmetry");
        egui::ComboBox::from_id_salt("brush_symmetry")
            .width(ui.available_width())
            .selected_text(state.symmetry.label())
            .show_ui(ui, |ui| {
                for mode in Symmetry::ALL {
                    ui.selectable_value(&mut state.symmetry, mode, mode.label());
                }
            });
    });
    if state.symmetry.is_radial() {
        let mut copies = state.symmetry_radial_count as f32;
        if slider_row(ui, "Copies", &mut copies, 2.0..=32.0, 1.0, 0) {
            state.symmetry_radial_count = copies.round().clamp(2.0, 32.0) as u32;
        }
    }
    if state.symmetry != Symmetry::Off {
        let mut px = state.symmetry_pivot.x * 100.0;
        let mut py = state.symmetry_pivot.y * 100.0;
        ui.horizontal(|ui| {
            field_label(ui, "Pivot %");
            ui.add(egui::DragValue::new(&mut px).speed(0.5).range(0.0..=100.0));
            ui.add(egui::DragValue::new(&mut py).speed(0.5).range(0.0..=100.0));
            if ui.button("Center").clicked() {
                px = 50.0;
                py = 50.0;
            }
        });
        state.symmetry_pivot = Vec2::new(px / 100.0, py / 100.0);
        ui.label(
            egui::RichText::new("Mirror and radial dabs also rotate or mirror the brush tip.")
                .size(TEXT_SMALL)
                .color(MUTED),
        );
    }
}

fn boolean_editor(
    ui: &mut egui::Ui,
    op: &mut Option<VectorOp>,
    style_source: Option<&str>,
    shapes: usize,
    changed: &mut bool,
) -> Option<CompoundUiAction> {
    let mut request = None;

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        for candidate in VectorOp::ALL {
            let active = *op == Some(candidate);
            let button = egui::Button::new(
                egui::RichText::new(candidate.short_label())
                    .size(TEXT_SMALL)
                    .color(if active {
                        ON_SECONDARY_CONTAINER
                    } else {
                        ON_SURFACE_VAR
                    }),
            )
            .fill(if active {
                SECONDARY_CONTAINER
            } else {
                CONTAINER_HIGH
            })
            .stroke(egui::Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(RADIUS_SM))
            .min_size(egui::vec2(0.0, 28.0));
            if ui
                .add(button)
                .on_hover_text(format!(
                    "{}\n{}",
                    candidate.label(),
                    operation_hint(candidate)
                ))
                .clicked()
            {
                *op = Some(candidate);
                *changed = true;
            }
        }
        if op.is_some()
            && ui
                .add(
                    egui::Button::new(egui::RichText::new("Off").size(TEXT_SMALL).color(MUTED))
                        .fill(CONTAINER_HIGH)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(egui::CornerRadius::same(RADIUS_SM))
                        .min_size(egui::vec2(0.0, 28.0)),
                )
                .on_hover_text("Return this group to ordinary stacking")
                .clicked()
        {
            *op = None;
            *changed = true;
        }
    });

    let Some(operation) = *op else {
        ui.label(
            egui::RichText::new(
                "Choose an operation to combine this group's shapes into one outline.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        return None;
    };

    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "{shapes} shape{}",
                if shapes == 1 { "" } else { "s" }
            ))
            .size(TEXT_CAPTION)
            .color(if shapes < 2 { WARNING } else { MUTED }),
        );
        ui.label(
            egui::RichText::new(if operation.is_cut() {
                "· bottom shape is the base"
            } else {
                "· combined as vectors, then beveled"
            })
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
    });
    if operation.is_cut() {
        ui.label(
            egui::RichText::new(
                "The bottom shape is the base; the shapes above it are cutters. Each cutter's \
                 Shape section controls its depth, wall and profile.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
    }
    ui.label(
        egui::RichText::new(operation_hint(operation))
            .size(TEXT_CAPTION)
            .color(MUTED),
    );
    if let Some(name) = style_source {
        ui.label(
            egui::RichText::new(format!("Bevel style from {name}"))
                .size(TEXT_CAPTION)
                .color(MUTED),
        );
    }
    if shapes < 2 {
        ui.label(
            egui::RichText::new("Add a second visible shape to combine.")
                .size(TEXT_CAPTION)
                .color(WARNING),
        );
    }
    if operation == VectorOp::Divide && shapes > 8 {
        ui.label(
            egui::RichText::new(
                "Divide supports up to eight visible shapes; the extras merge into one outline.",
            )
            .size(TEXT_CAPTION)
            .color(WARNING),
        );
    }

    ui.add_space(3.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        // A cut cannot collapse to one outline without losing its height
        // surface, so it bakes to editable layers using the cut blends.
        let (label, tip, action) = if operation == VectorOp::Divide {
            (
                "Split into shapes",
                "Replace this Divide group with one editable shape per region",
                CompoundUiAction::SplitIslands,
            )
        } else if operation.is_cut() {
            (
                "Bake cut to shapes",
                "Remove the live cut and keep the base and cutters as editable layers; the \
                 pocket, groove or seam keeps its surface through the cut blends",
                CompoundUiAction::Flatten,
            )
        } else {
            (
                "Bake to shape",
                "Replace this boolean group with one editable shape holding the combined outline",
                CompoundUiAction::Flatten,
            )
        };
        if ui
            .add(
                egui::Button::new(egui::RichText::new(label).size(TEXT_SMALL))
                    .min_size(egui::vec2(0.0, 22.0))
                    .corner_radius(4),
            )
            .on_hover_text(tip)
            .clicked()
        {
            request = Some(action);
        }
        if ui
            .add(
                egui::Button::new(egui::RichText::new("Release shapes").size(TEXT_SMALL))
                    .min_size(egui::vec2(0.0, 22.0))
                    .corner_radius(4),
            )
            .on_hover_text("Ungroup and keep every shape as an editable layer")
            .clicked()
        {
            request = Some(CompoundUiAction::Release);
        }
    });

    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_radius_clamps_to_the_pill_limit() {
        let mut state = EditorState::default();
        let mut radius = 6.0;
        // A 120 x 40 rectangle can round at most 20 px.
        assert!(apply_rect_corner(40.0, &mut radius, 80.0, &mut state));
        assert!((radius - 20.0).abs() < 1e-4);
        assert!((state.rect_corner - 0.5).abs() < 1e-4);
    }

    #[test]
    fn pointy_corners_are_remembered_for_the_next_rectangle() {
        let mut state = EditorState::default();
        let mut radius = 12.0;
        assert!(apply_rect_corner(100.0, &mut radius, 0.0, &mut state));
        assert_eq!(radius, 0.0);
        assert_eq!(state.rect_corner, 0.0);
        // Repeating the same value is not a change, so it does not checkpoint.
        assert!(!apply_rect_corner(100.0, &mut radius, 0.0, &mut state));
    }

    /// Top-left corner of the first text shape that contains `needle`.
    fn text_origin(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(text) if text.galley.text().contains(needle) => Some(text.pos),
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| walk(shape, needle)),
                _ => None,
            }
        }
        shapes
            .iter()
            .find_map(|clipped| walk(&clipped.shape, needle))
    }

    fn rect_document() -> (DocumentRes, EditorState) {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(200)));
        let layer = Layer::shape(
            "rect",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(100.0),
                // 120 x 40, so the pill limit is 20 px.
                size: Vec2::new(120.0, 40.0),
                corner_radius: 12.0,
            }),
        );
        doc.0.layers.push(layer);
        let state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        (doc, state)
    }

    #[test]
    fn the_properties_corner_slider_remembers_a_pointy_rectangle() {
        let ctx = egui::Context::default();
        let (mut doc, mut state) = rect_document();

        // First pass lays the panel out so the pointer can hit the slider.
        let mut panel_width = 0.0;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_width(276.0);
            ui.set_height(1600.0);
            properties_card(ui, &mut state, &mut doc);
            panel_width = ui.min_rect().width();
        });
        let label = text_origin(&output.shapes, "Corner (px)");
        output.drop_without_applying_deltas();
        assert!(
            panel_width <= 276.01,
            "the corner row widened the dock to {panel_width:.2} px"
        );
        let label = label.expect("the rectangle must expose a corner row");

        // A press near the left end of the rail asks for pointy corners.
        let press = egui::pos2(label.x + FIELD_LABEL_WIDTH + 10.0, label.y + 12.0);
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::PointerMoved(press));
        raw.events.push(egui::Event::PointerButton {
            pos: press,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        let output = ctx.run_ui(raw, |ui| {
            ui.set_width(276.0);
            ui.set_height(1600.0);
            properties_card(ui, &mut state, &mut doc);
        });
        output.drop_without_applying_deltas();

        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("still a shape");
        };
        let ShapeSource::Primitive(Primitive::Rect { corner_radius, .. }) = &shape.source else {
            panic!("still a rectangle");
        };
        assert!(
            *corner_radius < 2.0,
            "expected pointy corners, got {corner_radius}"
        );
        assert!(
            state.rect_corner < 0.1,
            "new rectangles keep it: {}",
            state.rect_corner
        );
    }

    #[test]
    fn a_non_finite_corner_request_is_ignored() {
        let mut state = EditorState::default();
        let mut radius = 5.0;
        assert!(!apply_rect_corner(100.0, &mut radius, f32::NAN, &mut state));
        assert_eq!(radius, 5.0);
    }

    #[test]
    fn the_path_guide_properties_show_the_use_as_control() {
        let ctx = egui::Context::default();
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(200)));
        let mut spine = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(20.0, 100.0)));
        subpath.nodes.push(Node::corner(Vec2::new(180.0, 100.0)));
        spine.subpaths.push(subpath);
        let mut layer = Layer::shape("Spine", ShapeSource::Path(spine));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.vector_role = VectorRole::Path;
        }
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        let mut panel_width = 0.0;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_width(276.0);
            ui.set_height(1600.0);
            properties_card(ui, &mut state, &mut doc);
            panel_width = ui.min_rect().width();
        });
        text_origin(&output.shapes, "Use as").expect("the vector exposes a Use as row");
        text_origin(&output.shapes, "Path guide").expect("the selected role is shown");
        output.drop_without_applying_deltas();
        assert!(
            panel_width <= 276.01,
            "the role row widened the dock to {panel_width:.2} px"
        );
        assert!(
            doc.0.layers[0].is_path_guide(),
            "drawing the card must not reset the role"
        );
    }
}
