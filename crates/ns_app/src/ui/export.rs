//! Compact export card pinned to the bottom of the Layers dock.

use super::*;

/// Starts collapsed to the header, summary and primary action so the layer
/// stack above keeps its height; the chevron expands the options and the
/// choice is pinned for the rest of the session.
pub(crate) fn export_card(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    cache: &mut EvalCache,
    bridge: &mut BridgeRuntime,
) {
    let document_size = doc.0.size.x.max(doc.0.size.y);
    // Presses in this card can change the normal-map convention, so this is an
    // edit site. Other shell clicks stay free of undo snapshots.
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
    let open_id = egui::Id::new("export_card_open");
    let pinned: Option<bool> = ui.ctx().data(|data| data.get_temp(open_id));
    let mut open = pinned.unwrap_or(false);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 5.0;
        ui.horizontal(|ui| {
            dock_title(ui, Icon::Download, "Export");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = chevron_button(ui, open).on_hover_text(if open {
                    "Hide export options"
                } else {
                    "Show export options"
                });
                if response.clicked() {
                    open = !open;
                    ui.ctx().data_mut(|data| data.insert_temp(open_id, open));
                }
                if !open {
                    let summary = export_summary(state);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(summary).size(TEXT_CAPTION).color(MUTED),
                        )
                        .truncate(),
                    );
                }
            });
        });

        if open {
            export_options(ui, state, doc, document_size);
        }

        let map_count = export_map_count(state);
        let tile_count = export_tile_count(state);
        let can_export = map_count > 0 || state.export_project;
        let button_label = if map_count == 0 {
            if state.export_project {
                "Export project".to_string()
            } else {
                "Export maps".to_string()
            }
        } else {
            let tiles = if tile_count > 1 {
                format!(" × {tile_count} tiles")
            } else {
                String::new()
            };
            match map_count {
                1 => format!("Export 1 map{tiles}"),
                n => format!("Export {n} maps{tiles}"),
            }
        };
        let response = ui
            .add_enabled_ui(can_export, |ui| filled_button(ui, &button_label, 50.0))
            .inner;
        if response.clicked() {
            export_project(state, doc, cache);
            if state.status.starts_with("Exported") {
                queue_toast(ui.ctx(), format!("{button_label} · done"), false);
            }
        }
        send_to_blender_row(ui, state, bridge);
        if can_export {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(export_summary_line(state, document_size, doc.0.green))
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                )
                .truncate(),
            )
            .on_hover_text("What the next export writes, in order");
        } else {
            ui.label(
                egui::RichText::new("Choose at least one map or the project file")
                    .size(TEXT_CAPTION)
                    .color(WARNING),
            );
        }
    });
}

/// Pushes maps into the running Blender session.
///
/// The main button sends exactly what the export card has ticked, so "export"
/// and "send" never disagree; the small button sends every document map the
/// bridge knows about, which is the one-click path when the add-on is set to
/// manual.
fn send_to_blender_row(ui: &mut egui::Ui, state: &mut EditorState, bridge: &mut BridgeRuntime) {
    let selected = crate::bridge::document_selection(state);
    let live = bridge.enabled && bridge.listening();
    ui.horizontal(|ui| {
        let label = match selected.len() {
            0 => "Send to Blender".to_string(),
            count => format!("Send {count} to Blender"),
        };
        let send = ui.add_enabled(
            live && !selected.is_empty(),
            egui::Button::new(egui::RichText::new(label).size(TEXT_SMALL)),
        );
        if send
            .on_hover_text(
                "Push the maps ticked above into the running Blender session. The \
                 add-on fetches them even when realtime streaming is off.",
            )
            .clicked()
        {
            let count = bridge.publish(selected);
            state.status = format!("Queued {count} map(s) for Blender");
            queue_toast(ui.ctx(), format!("Sent {count} map(s) to Blender"), false);
        }
        let all = ui.add_enabled(
            live,
            egui::Button::new(egui::RichText::new("All").size(TEXT_SMALL)),
        );
        if all
            .on_hover_text("Send every document map the bridge can serve")
            .clicked()
        {
            let count = bridge.publish(crate::bridge::DOCUMENT_MAPS);
            state.status = format!("Queued {count} map(s) for Blender");
            queue_toast(ui.ctx(), format!("Sent {count} map(s) to Blender"), false);
        }
        if !live {
            ui.label(
                egui::RichText::new("bridge off")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            )
            .on_hover_text("Enable the Blender bridge in Settings");
        }
    });
}

/// Number of texture maps selected for export. The project file is not a map.
fn export_map_count(state: &EditorState) -> usize {
    [
        state.export_normal,
        state.export_height,
        state.export_edge_wear,
        state.export_curvature,
        state.export_edge_detect,
        state.export_id_map,
    ]
    .into_iter()
    .filter(|enabled| *enabled)
    .count()
}

/// How many image sets the next export writes. UDIM batches produce one set
/// per tile, so the action and summary can say how many files that means.
fn export_tile_count(state: &EditorState) -> usize {
    if state.udim_enabled && state.export_all_tiles && state.udim_tiles.len() > 1 {
        state.udim_tiles.len()
    } else {
        1
    }
}

/// One-line confirmation of the settings, shown under the primary action.
fn export_summary_line(state: &EditorState, document_size: u32, green: GreenChannel) -> String {
    let mut maps = Vec::new();
    for (enabled, label) in [
        (state.export_normal, "Normal"),
        (state.export_height, "Height"),
        (state.export_edge_wear, "Edge wear"),
        (state.export_curvature, "Curvature"),
        (state.export_edge_detect, "Edge detect"),
        (state.export_id_map, "ID map"),
        (state.export_project, "Project"),
    ] {
        if enabled {
            maps.push(label);
        }
    }
    if maps.is_empty() {
        return "Nothing selected".into();
    }
    let size = if state.export_resolution == 0 {
        document_size
    } else {
        state.export_resolution
    };
    let mut parts = vec![maps.join(", ")];
    if export_map_count(state) > 0 {
        parts.push(if state.export_sixteen_bit {
            "16-bit".into()
        } else {
            "8-bit".into()
        });
    }
    parts.push(format!("{size} × {size}"));
    if export_tile_count(state) > 1 {
        parts.push(format!("{} UDIM tiles", state.udim_tiles.len()));
    }
    if state.export_normal {
        parts.push(normal_convention_label(green).to_string());
    }
    parts.retain(|part| !part.is_empty());
    parts.join(" · ")
}

fn export_options(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    document_size: u32,
) {
    // Maps, then file format, then the options that only apply to one map.
    // The old card mixed map toggles with "16-bit" and "Project", so an
    // outlined chip could mean any of three things.
    subheading(ui, "Maps");
    chip_row(
        ui,
        &mut [
            (&mut state.export_normal, "Normal"),
            (&mut state.export_height, "Height"),
            (&mut state.export_curvature, "Curvature"),
            (&mut state.export_edge_wear, "Edge wear"),
            (&mut state.export_edge_detect, "Edge detect"),
            (&mut state.export_id_map, "ID map"),
        ],
    );

    subheading(ui, "Format");
    let format_selected = usize::from(state.export_sixteen_bit);
    if let Some(index) = segmented(
        ui,
        format_selected,
        &["8-bit", "16-bit"],
        ui.available_width(),
    ) {
        state.export_sixteen_bit = index == 1;
    }

    // UDIM meshes split the same map set across tiles. Exports use the tile
    // number as a suffix (`normal_1001.png`), so the tile list is also a file
    // count rather than a display-only preference.
    if state.udim_enabled && state.udim_tiles.len() > 1 {
        subheading(ui, "UDIM tiles");
        let labels = [
            format!("Current ({})", doc.0.active_tile),
            format!("All ({})", state.udim_tiles.len()),
        ];
        let selected = usize::from(state.export_all_tiles);
        if let Some(index) = segmented(
            ui,
            selected,
            &[labels[0].as_str(), labels[1].as_str()],
            ui.available_width(),
        ) {
            state.export_all_tiles = index == 1;
        }
        let map_count = export_map_count(state);
        let caption = if map_count == 0 {
            "The project file is written once, not per tile".to_string()
        } else if state.export_all_tiles {
            format!(
                "{map_count} maps × {} tiles · normal_1001.png, normal_{}.png, …",
                state.udim_tiles.len(),
                state.udim_tiles.last().copied().unwrap_or(1001)
            )
        } else {
            format!("One set · normal_{}.png …", doc.0.active_tile)
        };
        ui.label(egui::RichText::new(caption).size(TEXT_CAPTION).color(MUTED));
    }

    // Normal-map convention: the document stores canonical OpenGL internals,
    // so this only selects the green channel written to normal.png (and how a
    // normal map is decoded on import). The preview always shows the surface.
    if state.export_normal {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(
                egui::RichText::new("Normal Y")
                    .size(TEXT_SMALL)
                    .color(MUTED),
            )
            .on_hover_text("Green-channel convention written to normal.png");
            let width = ui.available_width();
            let current = usize::from(doc.0.green == GreenChannel::DirectX);
            let mut changed = false;
            if let Some(index) = segmented(ui, current, &["OpenGL (Y+)", "DirectX (Y−)"], width) {
                doc.0.green = if index == 0 {
                    GreenChannel::OpenGl
                } else {
                    GreenChannel::DirectX
                };
                changed = true;
            }
            if changed {
                state.mark_dirty();
                state.status = format!(
                    "Normal map exports as {}",
                    normal_convention_label(doc.0.green)
                );
            }
        });
        ui.label(
            egui::RichText::new("OpenGL: Blender, Unity, Godot · DirectX: Unreal Engine")
                .size(TEXT_CAPTION)
                .color(MUTED),
        );
    }

    // Parameter rows sit directly under the chips, one compact line each until
    // opened, so the card stays short unless a map is actually being tuned.
    if state.export_edge_wear {
        let summary = format!(
            "thr {:.2} · soft {:.2} · str {:.2}",
            state.edge_wear_params.threshold,
            state.edge_wear_params.softness,
            state.edge_wear_params.strength
        );
        let mut open = state.export_edge_wear_open;
        params_toggle(ui, &mut open, "Edge wear", &summary);
        state.export_edge_wear_open = open;
        if open {
            ui.indent("edge_wear_params", |ui| {
                let params = &mut state.edge_wear_params;
                ui.horizontal(|ui| {
                    param(ui, "Threshold", &mut params.threshold, 0.0..=1.0);
                    param(ui, "Softness", &mut params.softness, 0.01..=1.0);
                });
                ui.horizontal(|ui| {
                    param(ui, "Strength", &mut params.strength, 0.0..=1.0);
                    if ui.small_button("Reset").clicked() {
                        *params = ns_core::derived::EdgeWearParams::default();
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Curvature is normalized to the strongest edge. Raise the threshold to keep wear off soft creases.",
                    )
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            });
        }
    }
    if state.export_edge_detect {
        let summary = format!(
            "thr {:.2} · soft {:.2} · str {:.2}",
            state.edge_detect_params.threshold,
            state.edge_detect_params.softness,
            state.edge_detect_params.strength
        );
        let mut open = state.export_edge_detect_open;
        params_toggle(ui, &mut open, "Edge detect", &summary);
        state.export_edge_detect_open = open;
        if open {
            ui.indent("edge_detect_params", |ui| {
                let params = &mut state.edge_detect_params;
                ui.horizontal(|ui| {
                    param(ui, "Threshold", &mut params.threshold, 0.0..=1.0);
                    param(ui, "Softness", &mut params.softness, 0.01..=1.0);
                });
                ui.horizontal(|ui| {
                    param(ui, "Strength", &mut params.strength, 0.0..=1.0);
                    if ui.small_button("Reset").clicked() {
                        *params = ns_core::derived::EdgeDetectParams::default();
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Absolute curvature: convex ridges and concave creases are treated the same.",
                    )
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            });
        }
    }
    if state.export_id_map {
        ui.label(
            egui::RichText::new(
                "Assign ID colors per layer or group in Properties → Material ID. Exported as 8-bit id.png.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
    }

    if state.export_height {
        let has_imported = doc.0.has_imported_normals();
        let resolved = state.export_height_source.integrates_height(has_imported);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Height source")
                    .size(TEXT_SMALL)
                    .color(MUTED),
            )
            .on_hover_text("How the exported height map is produced");
            let width = ui.available_width();
            egui::ComboBox::from_id_salt("export_height_source")
                .width(width)
                .selected_text(
                    egui::RichText::new(height_source_label(state.export_height_source, resolved))
                        .size(TEXT_LABEL),
                )
                .show_ui(ui, |ui| {
                    for source in HeightSource::ALL {
                        let label =
                            height_source_label(source, source.integrates_height(has_imported));
                        ui.selectable_value(&mut state.export_height_source, source, label)
                            .on_hover_text(source.hint());
                    }
                });
        });
        if state.export_height_source == HeightSource::Direct && has_imported {
            ui.label(
                egui::RichText::new("Imported normals have no direct height and will be omitted.")
                    .size(TEXT_CAPTION)
                    .color(WARNING),
            );
        }
    } else {
        ui.label(
            egui::RichText::new("Height source")
                .size(TEXT_SMALL)
                .color(TEXT_DISABLED),
        )
        .on_hover_text("Turn on the Height map to choose a source");
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new("Size").size(TEXT_SMALL).color(MUTED))
            .on_hover_text("Export resolution");
        let resolution_label = if state.export_resolution == 0 {
            format!("Document ({document_size})")
        } else {
            format!("{} px", state.export_resolution)
        };
        egui::ComboBox::from_id_salt("export_resolution")
            .width(ui.available_width())
            .selected_text(egui::RichText::new(resolution_label).size(TEXT_LABEL))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.export_resolution,
                    0,
                    format!("Document ({document_size})"),
                );
                for size in [512u32, 1024, 2048, 4096] {
                    ui.selectable_value(&mut state.export_resolution, size, format!("{size} px"));
                }
            });
    });

    let path_text = state
        .export_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "output (default)".to_string());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new("Save to").size(TEXT_SMALL).color(MUTED))
            .on_hover_text("Destination folder for the exported files");
        let button_width = 108.0;
        let path_width = (ui.available_width() - button_width - 6.0).max(40.0);
        ui.add_sized(
            [path_width, 24.0],
            egui::Label::new(
                egui::RichText::new(&path_text)
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            )
            .truncate(),
        )
        .on_hover_text(&path_text);
        if ui
            .add_sized(
                [button_width, 26.0],
                egui::Button::new(egui::RichText::new("Choose folder...").size(TEXT_SMALL)),
            )
            .on_hover_text("Choose the export output folder")
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                state.export_dir = Some(path);
            }
        }
    });

    m3_checkbox(ui, &mut state.export_project, "Also save the project file")
        .on_hover_text("Write a .nsproj alongside the maps so the layer stack can be reopened");
}

/// Material 3 segmented control: one container, selected cell tonal. Returns
/// the newly selected index when the click changes it.
fn segmented(ui: &mut egui::Ui, selected: usize, labels: &[&str], width: f32) -> Option<usize> {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width.max(80.0), 32.0), egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(RADIUS_SM), CONTAINER_HIGH);
    let count = labels.len().max(1) as f32;
    let cell_width = rect.width() / count;
    let hover = response.hover_pos();
    for (index, label) in labels.iter().enumerate() {
        let cell = egui::Rect::from_min_size(
            egui::pos2(rect.left() + index as f32 * cell_width, rect.top()),
            egui::vec2(cell_width, rect.height()),
        );
        let hovered = hover.is_some_and(|pos| cell.contains(pos));
        if index == selected {
            painter.rect_filled(
                cell.shrink(3.0),
                egui::CornerRadius::same(6),
                SECONDARY_CONTAINER,
            );
        } else if hovered {
            painter.rect_filled(
                cell.shrink(3.0),
                egui::CornerRadius::same(6),
                hover_layer(CONTAINER_HIGH),
            );
        }
        painter.text(
            cell.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            egui::FontId::proportional(TEXT_SMALL),
            if index == selected {
                ON_SECONDARY_CONTAINER
            } else {
                ON_SURFACE_VAR
            },
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            egui::CornerRadius::same(10),
            egui::Stroke::new(2.0_f32, PRIMARY),
            egui::StrokeKind::Outside,
        );
    }
    let mut chosen = None;
    if response.clicked() {
        if let Some(pos) = hover {
            let index = (((pos.x - rect.left()) / cell_width).floor() as usize)
                .min(labels.len().saturating_sub(1));
            if index != selected {
                chosen = Some(index);
            }
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
    chosen
}

fn normal_convention_label(convention: GreenChannel) -> &'static str {
    match convention {
        GreenChannel::OpenGl => "OpenGL (Y+)",
        GreenChannel::DirectX => "DirectX (Y−)",
    }
}

/// Selected label for the height-source combo. Auto spells out what it resolves
/// to for the current document.
fn height_source_label(source: HeightSource, resolved_integrated: bool) -> String {
    match source {
        HeightSource::Auto => format!(
            "Auto ({})",
            if resolved_integrated {
                "integrated"
            } else {
                "direct"
            }
        ),
        _ => source.label().to_string(),
    }
}

/// One row of equally sized toggle chips that always fills the card width.
/// Rows wrap onto two lines only when a label would not fit.
fn chip_row(ui: &mut egui::Ui, chips: &mut [(&mut bool, &str)]) {
    let spacing = 4.0;
    let count = chips.len() as f32;
    let width = ((ui.available_width() - spacing * (count - 1.0)) / count).floor();
    let font = egui::FontId::proportional(TEXT_SMALL);
    let widest = chips
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), TEXT)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    let per_row = if widest + 24.0 <= width {
        chips.len()
    } else {
        chips.len().div_ceil(2)
    };
    for row in chips.chunks_mut(per_row.max(1)) {
        let n = row.len() as f32;
        let chip_width = ((ui.available_width() - spacing * (n - 1.0)) / n).floor();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = spacing;
            for (value, label) in row.iter_mut() {
                export_chip(ui, value, label, chip_width);
            }
        });
    }
}

/// A compact disclosure row for a derived-map parameter block.
fn params_toggle(ui: &mut egui::Ui, open: &mut bool, title: &str, summary: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        if chevron_button(ui, *open).clicked() {
            *open = !*open;
        }
        let text = ui.add(
            egui::Label::new(egui::RichText::new(title).size(TEXT_SMALL).color(TEXT))
                .sense(egui::Sense::click()),
        );
        if text.clicked() {
            *open = !*open;
        }
        ui.label(egui::RichText::new(summary).size(TEXT_CAPTION).color(MUTED));
    });
}

/// Label + compact drag value, used by the derived-map parameter rows.
fn param(ui: &mut egui::Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>) {
    ui.label(egui::RichText::new(label).size(TEXT_CAPTION).color(MUTED));
    ui.add(
        egui::DragValue::new(value)
            .speed(0.01)
            .range(range)
            .max_decimals(2),
    );
}

fn export_summary(state: &EditorState) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (enabled, label) in [
        (state.export_normal, "Normal"),
        (state.export_height, "Height"),
        (state.export_project, "Project"),
        (state.export_edge_wear, "Wear"),
        (state.export_curvature, "Curv."),
        (state.export_edge_detect, "Edges"),
        (state.export_id_map, "IDs"),
    ] {
        if enabled {
            parts.push(label.to_string());
        }
    }
    if parts.is_empty() {
        "nothing selected".into()
    } else {
        if export_tile_count(state) > 1 {
            parts.push(format!("{} tiles", state.udim_tiles.len()));
        }
        parts.join(" · ")
    }
}

/// Material 3 filter chip: neutral fill when off, tonal fill + check when on.
/// No outline; the check and the text keep the state independent of colour.
fn export_chip(ui: &mut egui::Ui, value: &mut bool, label: &str, width: f32) {
    let active = *value;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width.max(40.0), 32.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
    }
    let fill = if active {
        SECONDARY_CONTAINER
    } else {
        CONTAINER_HIGH
    };
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(RADIUS_SM), fill);
    if response.hovered() {
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_SM),
            state_layer(fill, ON_SURFACE, HOVER_ALPHA),
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            egui::CornerRadius::same(10),
            egui::Stroke::new(2.0_f32, PRIMARY),
            egui::StrokeKind::Outside,
        );
    }
    let text_color = if active {
        ON_SECONDARY_CONTAINER
    } else {
        ON_SURFACE_VAR
    };
    let galley = painter.layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(TEXT_SMALL),
        text_color,
    );
    // Shift the label right when the check occupies the leading edge.
    let check_space = if active { 16.0 } else { 0.0 };
    let text_left = rect.center().x - galley.size().x * 0.5 + check_space * 0.5;
    painter.galley(
        egui::pos2(text_left, rect.center().y - galley.size().y * 0.5),
        galley,
        text_color,
    );
    if active {
        let c = egui::pos2(rect.left() + 14.0, rect.center().y);
        let stroke = egui::Stroke::new(1.8_f32, ON_SECONDARY_CONTAINER);
        painter.line_segment(
            [c + egui::vec2(-3.5, 0.0), c + egui::vec2(-1.0, 3.0)],
            stroke,
        );
        painter.line_segment(
            [c + egui::vec2(-1.0, 3.0), c + egui::vec2(3.5, -3.0)],
            stroke,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, active, label)
    });
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if active {
            "Included in the export. Click to skip."
        } else {
            "Skipped. Click to include in the export."
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(normal: bool, height: bool, project: bool, sixteen: bool) -> EditorState {
        EditorState {
            export_normal: normal,
            export_height: height,
            export_project: project,
            export_sixteen_bit: sixteen,
            ..Default::default()
        }
    }

    #[test]
    fn the_project_file_is_not_counted_as_a_map() {
        assert_eq!(export_map_count(&state(true, true, true, true)), 2);
        assert_eq!(export_map_count(&state(false, false, true, true)), 0);
    }

    #[test]
    fn the_tile_count_multiplies_only_for_all_tiles() {
        let mut state = state(true, true, true, true);
        state.udim_enabled = true;
        state.udim_tiles = vec![1001, 1002, 1011];
        state.export_all_tiles = true;
        assert_eq!(export_tile_count(&state), 3);
        state.export_all_tiles = false;
        assert_eq!(export_tile_count(&state), 1);
    }

    #[test]
    fn the_summary_names_maps_format_size_and_convention() {
        let state = state(true, true, true, true);
        assert_eq!(
            export_summary_line(&state, 1024, GreenChannel::DirectX),
            "Normal, Height, Project · 16-bit · 1024 × 1024 · DirectX (Y−)"
        );
    }

    #[test]
    fn the_summary_omits_format_when_only_the_project_is_selected() {
        let state = state(false, false, true, true);
        assert_eq!(
            export_summary_line(&state, 512, GreenChannel::OpenGl),
            "Project · 512 × 512"
        );
    }
}
