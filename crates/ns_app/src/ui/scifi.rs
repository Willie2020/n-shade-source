//! Optional procedural sci-fi texture generator window.
//!
//! Deliberately parked outside the workflow: it has its own window, its own
//! state, and it only reads its parameters and writes PNGs. It never touches
//! the document, so opening it cannot disturb the normal-map work.

use super::*;
use crate::scifi::{self, ScifiView, PALETTES, SIZE_CHOICES};

const PREVIEW_MAX: f32 = 420.0;

/// Draw the generator window. Missing maps or dirty parameters regenerate once
/// at the top of the frame; the preview texture follows the active view.
pub(crate) fn scifi_window(ctx: &egui::Context, panel: &mut crate::scifi::ScifiPanel) {
    if !panel.open {
        return;
    }
    if panel.texture.is_none()
        || panel
            .pending
            .is_some_and(|since| since.elapsed().as_millis() >= 150)
    {
        panel.regenerate();
    }
    if panel.texture_handle.is_none() || panel.texture_view != Some(panel.view) {
        if let Some(texture) = &panel.texture {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [texture.size as usize, texture.size as usize],
                texture.view(panel.view),
            );
            panel.texture_handle =
                Some(ctx.load_texture("scifi_preview", image, egui::TextureOptions::LINEAR));
            panel.texture_view = Some(panel.view);
        }
    }

    let mut open = panel.open;
    egui::Window::new("Sci-Fi texture generator")
        .open(&mut open)
        .default_width(820.0)
        .default_pos(egui::pos2(300.0, 60.0))
        .resizable(true)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(
                    "Optional tool · procedural panels, greebles and wear. Generates images only; the document and its layers are never touched.",
                )
                .size(TEXT_SMALL)
                .color(MUTED),
            );
            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                preview_column(ui, panel);
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_min_width(310.0);
                    controls(ui, panel);
                });
            });
        });
    panel.open = open;
}

fn preview_column(ui: &mut egui::Ui, panel: &mut crate::scifi::ScifiPanel) {
    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            for view in ScifiView::ALL {
                let active = panel.view == view;
                if tab_button(ui, view.label(), active).clicked() && !active {
                    panel.view = view;
                }
            }
        });
        ui.add_space(4.0);
        let side = ui.available_width().min(PREVIEW_MAX);
        if let Some(handle) = &panel.texture_handle {
            ui.add(egui::Image::new((handle.id(), egui::vec2(side, side))).bg_fill(APP_BG));
        } else {
            ui.allocate_space(egui::vec2(side, side));
        }
        ui.add_space(6.0);
        if let Some(texture) = &panel.texture {
            ui.label(
                egui::RichText::new(texture.summary())
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(format!("Save {} view...", panel.view.label()))
                .clicked()
            {
                save_views(panel, false);
            }
            if ui
                .button("Save all views...")
                .on_hover_text(
                    "Writes final, albedo, normal, height and emissive next to each other",
                )
                .clicked()
            {
                save_views(panel, true);
            }
        });
        if !panel.status.is_empty() {
            ui.label(
                egui::RichText::new(&panel.status)
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }
    });
}

fn controls(ui: &mut egui::Ui, panel: &mut crate::scifi::ScifiPanel) {
    // Edit a copy so the buttons can regenerate immediately without fighting
    // the borrow of `panel.params`.
    let mut params = panel.params.clone();
    let mut changed = false;
    let mut regenerate_now = false;
    ui.horizontal(|ui| {
        field_label(ui, "Seed");
        changed |= ui
            .add(
                egui::DragValue::new(&mut params.seed)
                    .speed(1.0)
                    .range(0..=999_999),
            )
            .changed();
        if ui.button("Randomise").clicked() {
            params.randomise();
            regenerate_now = true;
        }
        if ui
            .button("Generate")
            .on_hover_text("Regenerate with the current parameters")
            .clicked()
        {
            regenerate_now = true;
        }
    });
    ui.horizontal(|ui| {
        field_label(ui, "Palette");
        egui::ComboBox::from_id_salt("scifi_palette")
            .width(ui.available_width())
            .selected_text(PALETTES[params.palette.min(PALETTES.len() - 1)].name)
            .show_ui(ui, |ui| {
                for (index, palette) in PALETTES.iter().enumerate() {
                    changed |= ui
                        .selectable_value(&mut params.palette, index, palette.name)
                        .changed();
                }
            });
    });
    ui.horizontal(|ui| {
        field_label(ui, "Layout");
        egui::ComboBox::from_id_salt("scifi_layout")
            .width(ui.available_width())
            .selected_text(params.layout.label())
            .show_ui(ui, |ui| {
                for layout in scifi::Layout::ALL {
                    changed |= ui
                        .selectable_value(&mut params.layout, layout, layout.label())
                        .changed();
                }
            });
    });
    ui.horizontal(|ui| {
        field_label(ui, "Resolution");
        egui::ComboBox::from_id_salt("scifi_size")
            .width(ui.available_width())
            .selected_text(format!("{} px", params.size))
            .show_ui(ui, |ui| {
                for size in SIZE_CHOICES {
                    changed |= ui
                        .selectable_value(&mut params.size, size, format!("{size} px"))
                        .changed();
                }
            });
    });
    ui.horizontal(|ui| {
        field_label(ui, "Symmetry");
        egui::ComboBox::from_id_salt("scifi_symmetry")
            .width(ui.available_width())
            .selected_text(params.symmetry.label())
            .show_ui(ui, |ui| {
                for symmetry in scifi::Symmetry::ALL {
                    changed |= ui
                        .selectable_value(&mut params.symmetry, symmetry, symmetry.label())
                        .changed();
                }
            });
    });
    ui.add_space(4.0);
    changed |= slider(ui, "Chamfer size", &mut params.chamfer, 0..=10, 1.0);
    changed |= slider(ui, "Subdivision depth", &mut params.depth, 2..=9, 1.0);
    changed |= slider(ui, "Seam width", &mut params.seam, 1..=8, 1.0);
    changed |= slider(ui, "Detail density", &mut params.density, 1..=10, 1.0);
    changed |= slider(ui, "Wear & grime", &mut params.wear, 0..=10, 1.0);
    changed |= slider(ui, "Emissive glow", &mut params.glow, 0..=10, 1.0);
    changed |= slider(ui, "Light angle", &mut params.light_deg, 0..=360, 1.0);
    changed |= slider(
        ui,
        "Normal strength",
        &mut params.normal_strength,
        1..=10,
        1.0,
    );
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        changed |= toggle_switch(ui, &mut params.balance).changed();
        ui.label(egui::RichText::new("Compositional balance").size(12.0));
    });
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "Rule-driven panels first, then greebles from a size/aspect vocabulary. \
             Height drives the normal, AO and wear, so the maps stay consistent. \
             Output is not wired into the layer stack yet.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
    if changed {
        panel.mark_dirty();
    }
    panel.params = params;
    if regenerate_now {
        panel.regenerate();
    }
}

fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    speed: f32,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        field_label(ui, label);
        let width = (ui.available_width() - 56.0).max(60.0);
        let mut float = *value as f32;
        let mut drag = *value as f32;
        changed |= ui
            .add_sized(
                [width, 18.0],
                egui::Slider::new(&mut float, *range.start() as f32..=*range.end() as f32)
                    .show_value(false)
                    .trailing_fill(true),
            )
            .changed();
        changed |= ui
            .add_sized(
                [50.0, 24.0],
                egui::DragValue::new(&mut drag)
                    .speed(speed)
                    .range(*range.start() as f32..=*range.end() as f32)
                    .fixed_decimals(0),
            )
            .changed();
        if changed {
            *value = float.round() as u32;
        }
    });
    changed
}

/// Save the active view, or every view, through a file dialog.
fn save_views(panel: &mut crate::scifi::ScifiPanel, all: bool) {
    let Some(texture) = &panel.texture else {
        return;
    };
    let palette = &PALETTES[panel.params.palette.min(PALETTES.len() - 1)];
    let stem = format!(
        "scifi_{}_seed{}",
        palette.name.to_ascii_lowercase().replace(' ', "_"),
        panel.params.seed
    );
    let default_name = format!("{stem}_{}.png", panel.view.label().to_ascii_lowercase());
    let Some(path) = rfd::FileDialog::new()
        .set_file_name(&default_name)
        .add_filter("PNG image", &["png"])
        .save_file()
    else {
        return;
    };
    if all {
        let Some(folder) = path.parent() else {
            panel.status = "Pick a destination file inside a folder".into();
            return;
        };
        let mut written = 0;
        let mut error = None;
        for view in ScifiView::ALL {
            let file = folder.join(format!("{stem}_{}.png", view.label().to_ascii_lowercase()));
            match scifi::save_png(&file, texture.size, texture.view(view)) {
                Ok(()) => written += 1,
                Err(message) => error = Some(message),
            }
        }
        panel.status = match error {
            Some(message) => format!("Saved {written} views · {message}"),
            None => format!("Saved {written} views to {}", folder.display()),
        };
    } else {
        panel.status = match scifi::save_png(&path, texture.size, texture.view(panel.view)) {
            Ok(()) => format!("Saved {}", path.display()),
            Err(message) => format!("Save failed · {message}"),
        };
    }
}
