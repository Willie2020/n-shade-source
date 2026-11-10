//! Keyboard shortcut reference window.
//!
//! Only shortcuts that are actually wired up are listed. The open state lives
//! in egui's temporary data, so the reference is presentation-only.

use super::*;

fn sheet_id() -> egui::Id {
    egui::Id::new("nshade_shortcut_sheet")
}

pub(crate) fn toggle_shortcuts(ctx: &egui::Context) {
    let open = ctx
        .data(|data| data.get_temp::<bool>(sheet_id()))
        .unwrap_or(false);
    ctx.data_mut(|data| data.insert_temp(sheet_id(), !open));
}

/// Shortcut groups: `(group title, [(keys, action)])`.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Tools",
        &[
            ("V", "Select and transform"),
            ("R", "Rectangle"),
            ("O", "Ellipse"),
            ("S", "Star"),
            ("T", "Text: click the canvas to place a live text layer"),
            ("P", "Pen / last vector tool"),
            ("L", "Lasso / last cleanup tool"),
            ("B", "Paint (height or mask)"),
            ("H", "Pan the 2D canvas"),
            ("Space (hold)", "Temporary pan"),
        ],
    ),
    (
        "Edit",
        &[
            ("Ctrl+Z", "Undo"),
            ("Ctrl+Shift+Z", "Redo"),
            ("Ctrl+C / Ctrl+V", "Copy / paste layers"),
            ("Ctrl+G", "Group selected layers"),
            ("Delete", "Delete selected layer"),
            ("Arrow keys", "Nudge 1 px"),
            ("Shift+Arrows", "Nudge 10 px"),
        ],
    ),
    (
        "View",
        &[
            ("Ctrl+1 … Ctrl+5", "Properties, Mesh, Bake, Lightmap, PBR"),
            ("M", "Toggle the mask overlay"),
            ("Esc", "Cancel the drag or draft and return to Select"),
            ("Shift+click", "Add or remove a layer from the selection"),
        ],
    ),
    (
        "Canvas",
        &[
            ("Shift", "Constrain angles and proportions"),
            ("Alt+drag", "Duplicate while dragging"),
            ("Alt+Shift+drag", "Duplicate on a locked axis"),
            ("Ctrl (during drag)", "Temporarily disable snapping"),
            ("?", "Show or hide this list"),
        ],
    ),
    (
        "Touchpad & pen",
        &[
            ("Pinch / Ctrl+wheel", "Zoom the 2D canvas and 3D view"),
            (
                "Two-finger scroll",
                "Pan the 2D canvas; dolly or orbit in 3D",
            ),
            ("Shift+scroll", "Pan the canvas sideways"),
            ("Pen pressure", "Brush size and flow (Settings toggles)"),
        ],
    ),
    (
        "File",
        &[
            ("Ctrl+N", "New project"),
            ("Ctrl+O", "Open a project"),
            ("Ctrl+S / Ctrl+Shift+S", "Save / save as"),
            ("Ctrl+E", "Export the selected maps"),
        ],
    ),
];

pub(crate) fn shortcuts_ui(ctx: &egui::Context) {
    if !ctx
        .data(|data| data.get_temp::<bool>(sheet_id()))
        .unwrap_or(false)
    {
        return;
    }
    let mut open = true;
    egui::Window::new("Keyboard shortcuts")
        .id(egui::Id::new("shortcuts_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(430.0)
        .max_height(560.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            egui::ScrollArea::vertical()
                .id_salt("shortcuts_scroll")
                .max_height(520.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (group, items) in SHORTCUTS {
                        subheading(ui, group);
                        for (keys, action) in *items {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [148.0, 20.0],
                                    egui::Label::new(
                                        egui::RichText::new(*keys)
                                            .size(TEXT_SMALL)
                                            .monospace()
                                            .color(ACCENT_TEXT),
                                    ),
                                );
                                ui.label(egui::RichText::new(*action).size(TEXT_SMALL).color(TEXT));
                            });
                        }
                        ui.add_space(4.0);
                    }
                    ui.separator();
                    ui.label(
                        egui::RichText::new(
                            "Press ? or close this window to return to the canvas.",
                        )
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                    );
                });
        });
    if !open {
        ctx.data_mut(|data| data.insert_temp(sheet_id(), false));
    }
}
