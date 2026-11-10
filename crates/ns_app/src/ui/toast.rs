//! Transient feedback above the status bar.
//!
//! Toasts are presentation-only: the text lives in egui's temporary data, so
//! nothing is added to the document or the undo history. Destructive actions
//! (delete, reset) queue an undoable toast instead of a modal confirmation.

use super::*;

#[derive(Clone)]
struct ToastState {
    text: String,
    undo: bool,
    deadline: f64,
}

fn toast_id() -> egui::Id {
    egui::Id::new("nshade_toast")
}

/// Show `text` for six seconds. `undo` adds an Undo button that reverses the
/// last document edit when pressed.
pub(crate) fn queue_toast(ctx: &egui::Context, text: impl Into<String>, undo: bool) {
    let now = ctx.input(|input| input.time);
    ctx.data_mut(|data| {
        data.insert_temp(
            toast_id(),
            ToastState {
                text: text.into(),
                undo,
                deadline: now + 6.0,
            },
        );
    });
    ctx.request_repaint();
}

/// Draw the active toast. Returns `true` when its Undo button was pressed; the
/// caller owns the undo call because it needs the document resource.
pub(crate) fn toast_ui(ctx: &egui::Context) -> bool {
    let Some(mut toast) = ctx.data(|data| data.get_temp::<ToastState>(toast_id())) else {
        return false;
    };
    let now = ctx.input(|input| input.time);
    if now > toast.deadline {
        ctx.data_mut(|data| data.remove::<ToastState>(toast_id()));
        return false;
    }

    let mut undo_clicked = false;
    let response = egui::Area::new(egui::Id::new("nshade_toast_area"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -40.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                // M3 snackbar: inverse surface, no stroke, elevation 2.
                .fill(ON_SURFACE)
                .stroke(egui::Stroke::NONE)
                .corner_radius(egui::CornerRadius::same(RADIUS_SM))
                .inner_margin(egui::Margin::symmetric(16, 10))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 14,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(130),
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        ui.label(
                            egui::RichText::new(&toast.text)
                                .size(TEXT_LABEL)
                                .color(SURFACE),
                        );
                        if toast.undo {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(58.0, 28.0), egui::Sense::click());
                            if response.hovered() {
                                ui.painter().rect_filled(
                                    rect,
                                    egui::CornerRadius::same(RADIUS_XS),
                                    state_layer(ON_SURFACE, SURFACE, HOVER_ALPHA),
                                );
                            }
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Undo",
                                egui::FontId::proportional(TEXT_LABEL),
                                SURFACE,
                            );
                            let response = response
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("Undo this action (Ctrl+Z)");
                            if response.clicked() {
                                undo_clicked = true;
                            }
                        }
                    });
                });
        });

    // Hovering pauses the countdown, so the Undo button never disappears under
    // the pointer.
    if response.response.hovered() {
        toast.deadline = now + 1.5;
        ctx.data_mut(|data| data.insert_temp(toast_id(), toast));
        ctx.request_repaint();
    }
    if undo_clicked {
        ctx.data_mut(|data| data.remove::<ToastState>(toast_id()));
    }
    undo_clicked
}
