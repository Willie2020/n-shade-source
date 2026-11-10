//! Ruler tool: a persistent angle guide that the drawing tools snap to.
//!
//! The guide is an infinite line through its anchor. Drawing with Shift
//! projects new nodes onto it, so a cut line can be laid down at a custom
//! angle without reading numbers off the screen.

use super::*;

/// Guide colour, deliberately different from the pen's warm draft.
pub(crate) const RULER_COLOR: egui::Color32 = PRIMARY_COOL;
/// Distance between ruler ticks, in screen pixels.
const TICK_SPACING: f32 = 40.0;

/// Drag to set the ruler's anchor and angle; click to move the anchor; a
/// right-click clears it.
pub(crate) fn ruler_edit(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut EditorState,
    view: &CanvasView,
) {
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let shift = ui.input(|input| input.modifiers.shift);

    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
            let angle = state.ruler.map_or(0.0, |ruler| ruler.angle);
            state.ruler = Some(RulerState {
                origin: view.screen_to_doc(origin),
                angle,
            });
            state.ruler_dragging = true;
        }
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        if let (Some(pointer), Some(ruler)) = (response.interact_pointer_pos(), state.ruler) {
            let delta = view.screen_to_doc(pointer) - ruler.origin;
            // A couple of pixels of jitter should not spin the guide.
            if delta.length() > 2.0 {
                let angle = if shift {
                    step_angle(delta, RULER_STEP)
                } else {
                    delta.y.atan2(delta.x)
                };
                state.ruler = Some(RulerState { angle, ..ruler });
            }
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        state.ruler_dragging = false;
        if let Some(ruler) = state.ruler {
            state.status = format!(
                "Ruler at {} · hold Shift while drawing to snap to it",
                angle_label(ruler.angle)
            );
        }
    }
    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let angle = state.ruler.map_or(0.0, |ruler| ruler.angle);
            state.ruler = Some(RulerState {
                origin: view.screen_to_doc(pointer),
                angle,
            });
            state.status = format!(
                "Ruler at {} · hold Shift while drawing to snap to it",
                angle_label(angle)
            );
        }
    }
    if response.secondary_clicked() {
        dismiss_ruler(state);
    }
}

/// Remove the guide.
pub(crate) fn dismiss_ruler(state: &mut EditorState) {
    state.ruler = None;
    state.ruler_dragging = false;
    state.status = "Ruler cleared · Shift snaps drawing to 45°".into();
}

/// Point the guide at `angle`, placing it in the middle of the document when
/// there is none yet.
fn set_ruler_angle(state: &mut EditorState, doc: &DocumentRes, degrees: f32) {
    let center = Vec2::new(doc.0.size.x as f32, doc.0.size.y as f32) * 0.5;
    let angle = degrees.to_radians();
    let ruler = state.ruler.unwrap_or(RulerState {
        origin: center,
        angle,
    });
    state.ruler = Some(RulerState { angle, ..ruler });
    state.status = format!(
        "Ruler at {} · hold Shift while drawing to snap to it",
        angle_label(angle)
    );
}

/// Draw the guide across the canvas.
pub(crate) fn draw_ruler(painter: &egui::Painter, view: &CanvasView, state: &EditorState) {
    let Some(ruler) = state.ruler else {
        return;
    };
    let origin = view.doc_to_screen(ruler.origin);
    let direction = view.doc_to_screen(ruler.origin + ruler.direction()) - origin;
    if direction.length_sq() < 1e-6 {
        return;
    }
    let direction = direction.normalized();
    let Some((start, end)) = clip_line(origin, direction, painter.clip_rect()) else {
        return;
    };
    // Full strength while drawing, a quiet reference the rest of the time.
    let drawing = matches!(
        state.canvas_tool,
        CanvasTool::Ruler | CanvasTool::Pen | CanvasTool::Polygon | CanvasTool::Node
    );
    let alpha = if drawing { 235 } else { 130 };
    let color = egui::Color32::from_rgba_unmultiplied(
        RULER_COLOR.r(),
        RULER_COLOR.g(),
        RULER_COLOR.b(),
        alpha,
    );
    painter.add(egui::Shape::dashed_line(
        &[start, end],
        egui::Stroke::new(1.0_f32, color),
        7.0,
        5.0,
    ));

    // Ticks make the guide readable as a ruler rather than a stray line.
    let perpendicular = egui::vec2(-direction.y, direction.x);
    let t0 = (start - origin).dot(direction);
    let t1 = (end - origin).dot(direction);
    let mut index = (t0 / TICK_SPACING).ceil() as i32;
    while (index as f32) * TICK_SPACING <= t1 {
        if index != 0 {
            let base = origin + direction * (index as f32 * TICK_SPACING);
            let long = index % 4 == 0;
            let reach = if long { 6.0 } else { 3.0 };
            painter.line_segment(
                [base - perpendicular * reach, base + perpendicular * reach],
                egui::Stroke::new(1.0_f32, color),
            );
        }
        index += 1;
    }

    painter.circle_stroke(origin, 6.5, egui::Stroke::new(1.0_f32, color));
    painter.circle_filled(origin, 3.0, color);

    // Angle chip beside the anchor, on the side away from the centre.
    let text = angle_label(ruler.angle);
    let galley =
        painter.layout_no_wrap(text, egui::FontId::proportional(11.0), egui::Color32::WHITE);
    let anchor = origin + perpendicular * 14.0;
    let pos = egui::pos2(anchor.x + 4.0, anchor.y - galley.size().y * 0.5);
    let chip = egui::Rect::from_min_size(pos, galley.size()).expand2(egui::vec2(4.0, 2.0));
    painter.rect_filled(chip, 3.0, egui::Color32::from_black_alpha(165));
    painter.rect_stroke(
        chip,
        3.0,
        egui::Stroke::new(1.0_f32, color),
        egui::StrokeKind::Inside,
    );
    painter.galley(pos, galley, color);
}

/// Clip the infinite line through `origin` along `direction` to `rect`.
pub(crate) fn clip_line(
    origin: egui::Pos2,
    direction: egui::Vec2,
    rect: egui::Rect,
) -> Option<(egui::Pos2, egui::Pos2)> {
    let mut enter = f32::NEG_INFINITY;
    let mut exit = f32::INFINITY;
    for (p, q) in [
        (-direction.x, origin.x - rect.left()),
        (direction.x, rect.right() - origin.x),
        (-direction.y, origin.y - rect.top()),
        (direction.y, rect.bottom() - origin.y),
    ] {
        if p.abs() < 1e-6 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let t = q / p;
        if p < 0.0 {
            if t > exit {
                return None;
            }
            enter = enter.max(t);
        } else {
            if t < enter {
                return None;
            }
            exit = exit.min(t);
        }
    }
    (enter <= exit).then(|| (origin + direction * enter, origin + direction * exit))
}

/// Ruler readout and quick angles, shown while the ruler or pen is active.
pub(crate) fn ruler_options_bar(ui: &mut egui::Ui, state: &mut EditorState, doc: &DocumentRes) {
    let relevant = state.canvas_tool == CanvasTool::Ruler || state.ruler.is_some();
    if !relevant {
        return;
    }
    let mut angle_edit = None;
    let mut clear = false;
    egui::Frame::new()
        .fill(CONTAINER_HIGH)
        .stroke(egui::Stroke::NONE)
        .corner_radius(egui::CornerRadius::same(RADIUS_SM))
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                let (icon_rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                draw_icon(ui.painter(), icon_rect, Icon::Ruler, ACCENT);
                ui.label(egui::RichText::new("Ruler").size(TEXT_LABEL).strong());
                ui.separator();

                let mut degrees = state.ruler.map_or(0.0, |ruler| ruler.degrees());
                ui.label(egui::RichText::new("Angle").size(TEXT_LABEL).color(MUTED));
                let response = ui.add(
                    egui::DragValue::new(&mut degrees)
                        .speed(0.5)
                        .range(0.0..=360.0)
                        .max_decimals(1)
                        .suffix("°"),
                );
                if response.changed() {
                    angle_edit = Some(degrees);
                }
                ui.separator();
                for degrees in [0.0_f32, 45.0, 90.0, 135.0] {
                    let selected = state
                        .ruler
                        .is_some_and(|ruler| (ruler.degrees() - degrees).abs() < 0.05);
                    if ui
                        .selectable_label(selected, format!("{degrees:.0}°"))
                        .clicked()
                    {
                        angle_edit = Some(degrees);
                    }
                }
                if state.ruler.is_some() {
                    ui.separator();
                    ui.checkbox(&mut state.ruler_locks_duplicates, "Lock dupes")
                        .on_hover_text(
                            "Alt-dragged copies move exactly along this ruler angle. \
                             Hold Ctrl/Cmd during the drag to place one freely, or turn \
                             this off to use smart-guide dragging by default.",
                        );
                }
                ui.separator();
                if ui
                    .button("Center")
                    .on_hover_text("Move the ruler to the middle of the canvas")
                    .clicked()
                {
                    if let Some(ruler) = state.ruler {
                        let center = Vec2::new(doc.0.size.x as f32, doc.0.size.y as f32) * 0.5;
                        state.ruler = Some(RulerState {
                            origin: center,
                            ..ruler
                        });
                    }
                }
                if ui
                    .button("Clear")
                    .on_hover_text(
                        "Remove the ruler, or right-click the canvas with the ruler tool",
                    )
                    .clicked()
                {
                    clear = true;
                }
                let hint = if state.ruler.is_some() {
                    if state.ruler_locks_duplicates {
                        "Shift snaps drawing · Alt-drag duplicates follow this angle"
                    } else {
                        "Shift snaps drawing to this angle"
                    }
                } else {
                    "Drag on the canvas to place the ruler"
                };
                ui.label(egui::RichText::new(hint).size(TEXT_CAPTION).color(MUTED));
            });
        });
    if let Some(degrees) = angle_edit {
        set_ruler_angle(state, doc, degrees);
    }
    if clear {
        dismiss_ruler(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 100.0))
    }

    #[test]
    fn a_horizontal_guide_spans_the_clip_rect() {
        let (start, end) =
            clip_line(egui::pos2(50.0, 50.0), egui::vec2(1.0, 0.0), rect()).expect("line");
        assert_eq!((start.x, start.y), (0.0, 50.0));
        assert_eq!((end.x, end.y), (200.0, 50.0));
    }

    #[test]
    fn a_diagonal_guide_is_clipped_at_the_edges() {
        let direction = egui::vec2(1.0, 1.0).normalized();
        let (start, end) = clip_line(egui::pos2(50.0, 50.0), direction, rect()).expect("line");
        // Down-right at 45 degrees from the middle leaves through the bottom
        // edge at (100, 100); the other way it reaches the corner.
        assert!((start.x - 0.0).abs() < 1e-3 && (start.y - 0.0).abs() < 1e-3);
        assert!((end.x - 100.0).abs() < 1e-3);
        assert!((end.y - 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_guide_outside_the_rect_has_no_span() {
        let direction = egui::vec2(1.0, 0.0);
        assert!(clip_line(egui::pos2(50.0, 300.0), direction, rect()).is_none());
        assert!(clip_line(egui::pos2(-50.0, 300.0), direction, rect()).is_none());
    }
}
