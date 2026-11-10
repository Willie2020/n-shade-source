//! Mask section of the Properties dock: painted mask adjustments, clipping,
//! and the smart (procedural) mask stack used to mix detail normals.

use super::*;

/// Outcome flags for the caller, which owns the document borrow.
#[derive(Default)]
pub(crate) struct MaskSectionResult {
    /// Layer content changed (e.g. clip source); call `Layer::touch`.
    pub changed: bool,
    /// Only the mask changed; call `Layer::touch_mask`.
    pub mask_changed: bool,
    /// Open the 2D mask editor for this layer.
    pub begin_edit: bool,
    /// Bind the sibling below as this layer's mask.
    pub bind_below: bool,
    /// Insert a new paintable mask layer below and bind it.
    pub new_mask_layer: bool,
    /// Remove the clip binding.
    pub unbind: bool,
}

/// One-click starting points for common normal-mixing masks.
pub(crate) const SMART_PRESETS: [(&str, &str); 5] = [
    (
        "Flat areas only",
        "Keep this detail on plateaus; hide it on bevels and slopes below",
    ),
    (
        "Bevels and slopes",
        "Keep this detail on the slopes of the shapes below",
    ),
    (
        "Convex edges",
        "Edge wear: reveal along ridges and outer bevels",
    ),
    (
        "Cavities",
        "Reveal in creases, inner corners and the foot of shapes",
    ),
    (
        "Noise breakup",
        "Tileable noise to break up a uniform detail",
    ),
];

pub(crate) fn preset_generator(index: usize) -> MaskGenerator {
    MaskGenerator::new(match index {
        0 => GeneratorKind::slope(),
        1 => GeneratorKind::Slope {
            min_deg: 15.0,
            max_deg: 90.0,
            softness_deg: 6.0,
        },
        2 => GeneratorKind::curvature(),
        3 => GeneratorKind::Curvature {
            mode: CurvatureMode::Cavities,
            spread_px: 3.0,
            contrast: 1.5,
        },
        _ => GeneratorKind::noise(),
    })
}

pub(crate) fn mask_section(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    layer: &mut Layer,
    mask_candidates: &[(LayerId, String, usize)],
) -> MaskSectionResult {
    let mut result = MaskSectionResult::default();
    let summary = mask_summary(layer);
    collapsible_section(
        ui,
        "mask_section",
        Icon::Mask,
        &format!("Mask{summary}"),
        None,
        true,
        |ui| {
            ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
            let has_any = layer.mask.is_some()
                || layer.mask_layer.is_some()
                || !layer.mask_settings.generators.is_empty();
            if has_any {
                ui.horizontal(|ui| {
                    result.mask_changed |=
                        toggle_switch(ui, &mut layer.mask_settings.enabled).changed();
                    ui.label(
                        egui::RichText::new(if layer.mask_settings.enabled {
                            "Mask enabled"
                        } else {
                            "Mask disabled (layer fully visible)"
                        })
                        .size(TEXT_LABEL)
                        .color(if layer.mask_settings.enabled {
                            TEXT
                        } else {
                            WARNING
                        }),
                    );
                })
                .response
                .on_hover_text("Shift-click the mask thumbnail in the layer list to toggle");
            }
            if has_any {
                ui.horizontal(|ui| {
                    let changed = toggle_switch(ui, &mut state.show_mask_overlay).changed();
                    ui.label(
                        egui::RichText::new("Canvas overlay (M)")
                            .size(TEXT_LABEL)
                            .color(TEXT),
                    );
                    if changed {
                        state.status = if state.show_mask_overlay {
                            "Mask overlay on".into()
                        } else {
                            "Mask overlay off".into()
                        };
                    }
                })
                .response
                .on_hover_text(
                    "Show this mask over the normal map; toggle off to edit the normals alone",
                );
                if state.show_mask_overlay {
                    slider_row(
                        ui,
                        "Overlay",
                        &mut state.mask_overlay_opacity,
                        0.05..=1.0,
                        0.005,
                        2,
                    );
                }
            }

            mask_subheading(ui, "Painted");
            painted_controls(ui, state, layer, &mut result);

            mask_subheading(ui, "Clip to layer");
            clip_controls(ui, layer, mask_candidates, &mut result);

            mask_subheading(ui, "Smart masks");
            smart_controls(ui, layer, &mut result);
        },
    );
    result
}

fn mask_summary(layer: &Layer) -> String {
    let settings = &layer.mask_settings;
    let mut parts = Vec::new();
    if layer.mask.is_some() {
        parts.push("painted".to_string());
    }
    if layer.mask_layer.is_some() {
        parts.push("clipped".to_string());
    }
    let smart = settings.generators.iter().filter(|g| g.enabled).count();
    if smart > 0 {
        parts.push(format!("{smart} smart"));
    }
    if parts.is_empty() {
        String::new()
    } else if !settings.enabled && (layer.mask.is_some() || smart > 0) {
        " · off".to_string()
    } else {
        format!(" · {}", parts.join(" + "))
    }
}

/// Sentence-case group label. No rule: spacing and tonal cards carry the
/// structure, per the Material border budget.
fn mask_subheading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(text)
            .size(TEXT_CAPTION)
            .strong()
            .color(MUTED),
    );
}

fn painted_controls(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    layer: &mut Layer,
    result: &mut MaskSectionResult,
) {
    let editing = state.mask_edit_layer == Some(layer.id);
    let label = if editing {
        "Editing on the 2D canvas"
    } else if layer.mask.is_some() {
        "Edit painted mask"
    } else {
        "Paint a mask"
    };
    let button = if editing {
        accent_button(egui::RichText::new(label).size(TEXT_LABEL).color(PRIMARY))
    } else {
        ghost_button(egui::RichText::new(label).size(TEXT_LABEL))
    };
    let width = ui.available_width();
    if ui
        .add_sized([width, 26.0], button)
        .on_hover_text("Paint with Reveal/Hide/Blur/Value brushes or stamp shapes")
        .clicked()
        && !editing
    {
        result.begin_edit = true;
    }

    if layer.mask.is_none() {
        return;
    }
    let settings = &mut layer.mask_settings;
    ui.horizontal(|ui| {
        result.mask_changed |= toggle_switch(ui, &mut settings.invert).changed();
        ui.label(egui::RichText::new("Invert").size(TEXT_LABEL));
        if text_button(ui, "Remove", 68.0, 26.0, true)
            .on_hover_text("Delete the painted bitmap")
            .clicked()
        {
            result.changed = true;
            result.mask_changed = true;
            result.begin_edit = false;
            if state.mask_edit_layer == Some(layer.id) {
                end_mask_edit(state);
            }
            layer.mask = None;
        }
    });
    if layer.mask.is_none() {
        return;
    }
    let settings = &mut layer.mask_settings;
    result.mask_changed |= slider_row(
        ui,
        "Feather px",
        &mut settings.feather_px,
        0.0..=64.0,
        0.1,
        1,
    );
    result.mask_changed |= slider_row(ui, "Density", &mut settings.density, 0.0..=1.0, 0.005, 2);
    let mut black = settings.black_point;
    let mut white = settings.white_point;
    if slider_row(ui, "Black point", &mut black, 0.0..=0.99, 0.005, 2) {
        settings.black_point = black.min(settings.white_point - 0.01);
        result.mask_changed = true;
    }
    if slider_row(ui, "White point", &mut white, 0.01..=1.0, 0.005, 2) {
        settings.white_point = white.max(settings.black_point + 0.01);
        result.mask_changed = true;
    }
    ui.label(
        egui::RichText::new(
            "Black/white points harden soft brush edges; density fades the whole \
             painted mask without repainting.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
}

fn clip_controls(
    ui: &mut egui::Ui,
    layer: &mut Layer,
    mask_candidates: &[(LayerId, String, usize)],
    result: &mut MaskSectionResult,
) {
    // Binding a sibling below is the usual workflow: the mask layer sits under
    // the shape it cuts, as in a clipping mask.
    let width = ui.available_width();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if layer.mask_layer.is_some() {
            if ui
                .add_sized(
                    [(width - 4.0) * 0.5, 24.0],
                    ghost_button(egui::RichText::new("Unbind").size(TEXT_SMALL)),
                )
                .on_hover_text("Stop using the bound layer as a mask")
                .clicked()
            {
                result.unbind = true;
            }
        } else {
            if ui
                .add_sized(
                    [(width - 4.0) * 0.5, 24.0],
                    ghost_button(egui::RichText::new("Bind below").size(TEXT_SMALL)),
                )
                .on_hover_text("Use the layer directly below as this layer's mask")
                .clicked()
            {
                result.bind_below = true;
            }
        }
        if ui
            .add_sized(
                [(width - 4.0) * 0.5, 24.0],
                ghost_button(egui::RichText::new("+ Mask layer").size(TEXT_SMALL)),
            )
            .on_hover_text("Insert a paintable mask layer directly below and bind it")
            .clicked()
        {
            result.new_mask_layer = true;
        }
    });
    let current = layer
        .mask_layer
        .and_then(|id| {
            mask_candidates
                .iter()
                .find(|(candidate, _, _)| *candidate == id)
                .map(|(_, name, _)| name.clone())
        })
        .unwrap_or_else(|| "None".to_string());
    ui.horizontal(|ui| {
        field_label(ui, "Source");
        egui::ComboBox::from_id_salt("mask_source")
            .width(ui.available_width())
            .selected_text(current)
            .show_ui(ui, |ui| {
                result.changed |= ui
                    .selectable_value(&mut layer.mask_layer, None, "None")
                    .changed();
                for (id, name, depth) in mask_candidates {
                    let label = format!("{}{}", "  ".repeat(*depth), name);
                    result.changed |= ui
                        .selectable_value(&mut layer.mask_layer, Some(*id), label)
                        .changed();
                }
            });
    });
    if layer.mask_layer.is_some() {
        ui.horizontal(|ui| {
            result.mask_changed |=
                toggle_switch(ui, &mut layer.mask_settings.invert_clip).changed();
            ui.label(egui::RichText::new("Use outside of source").size(TEXT_LABEL));
        });
        // Offsetting the mask past the shape below is what gives a clean rim
        // before the next shape up.
        let settings = &mut layer.mask_settings;
        result.mask_changed |= slider_row(
            ui,
            "Offset px",
            &mut settings.clip_offset_px,
            -64.0..=64.0,
            0.1,
            1,
        );
        result.mask_changed |= slider_row(
            ui,
            "Edge softness",
            &mut settings.clip_feather_px,
            0.0..=32.0,
            0.05,
            2,
        );
        ui.label(
            egui::RichText::new(
                "Positive offset grows the mask beyond the source (a rim); \
                 negative shrinks it inside.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
    }
    ui.label(
        egui::RichText::new("Clips this layer to the source's coverage.")
            .size(TEXT_CAPTION)
            .color(MUTED),
    )
    .on_hover_text(
        "Binding hides the source so it only acts as a mask; unbinding reveals it again.",
    );
}

fn smart_controls(ui: &mut egui::Ui, layer: &mut Layer, result: &mut MaskSectionResult) {
    ui.label(
        egui::RichText::new(
            "Built from the surface of the layers below, so details follow the \
             shapes underneath. Multiplied with the painted mask.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
    let mut remove = None;
    let mut move_up = None;
    let count = layer.mask_settings.generators.len();
    for (index, generator) in layer.mask_settings.generators.iter_mut().enumerate() {
        ui.push_id(("smart_mask", index), |ui| {
            egui::Frame::new()
                .fill(CONTAINER_HIGH)
                .stroke(egui::Stroke::NONE)
                .corner_radius(egui::CornerRadius::same(RADIUS_MD))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.horizontal(|ui| {
                        result.mask_changed |= toggle_switch(ui, &mut generator.enabled).changed();
                        ui.label(
                            egui::RichText::new(generator.kind.label())
                                .size(TEXT_LABEL)
                                .strong()
                                .color(if generator.enabled { TEXT } else { MUTED }),
                        );
                        result.mask_changed |= ui
                            .checkbox(
                                &mut generator.invert,
                                egui::RichText::new("Invert").size(TEXT_SMALL),
                            )
                            .changed();
                        if index > 0
                            && ui
                                .add(egui::Button::new("Up").frame(false).small())
                                .on_hover_text("Move up (order only affects readability)")
                                .clicked()
                        {
                            move_up = Some(index);
                        }
                        if ui
                            .add(egui::Button::new("×").frame(false))
                            .on_hover_text("Remove this smart mask")
                            .clicked()
                        {
                            remove = Some(index);
                        }
                    });
                    if generator.enabled {
                        result.mask_changed |= generator_params(ui, &mut generator.kind);
                        result.mask_changed |= slider_row(
                            ui,
                            "Strength",
                            &mut generator.strength,
                            0.0..=1.0,
                            0.005,
                            2,
                        );
                    }
                });
        });
    }
    if let Some(index) = remove {
        layer.mask_settings.generators.remove(index);
        result.mask_changed = true;
    }
    if let Some(index) = move_up {
        if index > 0 && index < count {
            layer.mask_settings.generators.swap(index, index - 1);
            result.mask_changed = true;
        }
    }

    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(width, 26.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.menu_button(
                egui::RichText::new("+ Add smart mask").size(TEXT_LABEL),
                |ui| {
                    ui.set_min_width(230.0);
                    for (index, (label, tip)) in SMART_PRESETS.iter().enumerate() {
                        if ui.button(*label).on_hover_text(*tip).clicked() {
                            layer.mask_settings.generators.push(preset_generator(index));
                            layer.mask_settings.enabled = true;
                            result.mask_changed = true;
                            ui.close();
                        }
                    }
                },
            );
        },
    );
}

fn generator_params(ui: &mut egui::Ui, kind: &mut GeneratorKind) -> bool {
    let mut changed = false;
    match kind {
        GeneratorKind::Slope {
            min_deg,
            max_deg,
            softness_deg,
        } => {
            changed |= slider_row(ui, "Min angle", min_deg, 0.0..=90.0, 0.25, 1);
            changed |= slider_row(ui, "Max angle", max_deg, 0.0..=90.0, 0.25, 1);
            changed |= slider_row(ui, "Softness", softness_deg, 0.0..=30.0, 0.1, 1);
            if *min_deg > *max_deg {
                std::mem::swap(min_deg, max_deg);
            }
        }
        GeneratorKind::Curvature {
            mode,
            spread_px,
            contrast,
        } => {
            ui.horizontal(|ui| {
                field_label(ui, "Select");
                egui::ComboBox::from_id_salt("curvature_mode")
                    .width(ui.available_width())
                    .selected_text(mode.label())
                    .show_ui(ui, |ui| {
                        for option in CurvatureMode::ALL {
                            changed |= ui.selectable_value(mode, option, option.label()).changed();
                        }
                    });
            });
            changed |= slider_row(ui, "Spread px", spread_px, 0.0..=32.0, 0.1, 1);
            changed |= slider_row(ui, "Contrast", contrast, 0.1..=8.0, 0.02, 2);
        }
        GeneratorKind::Noise {
            scale_px,
            octaves,
            seed,
            threshold,
            softness,
        } => {
            changed |= slider_row(ui, "Scale px", scale_px, 4.0..=512.0, 0.5, 0);
            changed |= slider_row(ui, "Threshold", threshold, 0.0..=1.0, 0.005, 2);
            changed |= slider_row(ui, "Softness", softness, 0.0..=1.0, 0.005, 2);
            ui.horizontal(|ui| {
                field_label(ui, "Octaves");
                changed |= ui
                    .add(egui::DragValue::new(octaves).range(1..=8).speed(0.05))
                    .changed();
                ui.label(egui::RichText::new("Seed").size(TEXT_SMALL).color(MUTED));
                changed |= ui
                    .add(egui::DragValue::new(seed).range(0..=9999).speed(0.2))
                    .changed();
                if ui
                    .small_button("New")
                    .on_hover_text("Pick a different random pattern")
                    .clicked()
                {
                    *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345) % 10_000;
                    changed = true;
                }
            });
        }
    }
    changed
}
