//! Floating custom bevel-profile curve editor.
//!
//! The curve is a monotone cubic spline through the control points, the same
//! interpolation used to build the bevel LUT. The graph draws that exact curve,
//! so what you see is the profile that gets evaluated.

use super::*;

const CONTROL_POINTS: usize = 7;
const SAMPLE_COUNT: usize = 160;
const POINT_HIT_RADIUS: f32 = 13.0;
const MIN_POINT_GAP: f32 = 0.01;

/// Opens the editor for one or more shape layers, seeding the curve from the
/// first target's current profile.
pub(crate) fn open_curve_editor(state: &mut EditorState, doc: &DocumentRes, targets: Vec<LayerId>) {
    let mut ids = Vec::new();
    let mut original = Vec::new();
    let mut base = None;
    for id in targets {
        let Some(layer) = doc.0.find_layer(id) else {
            continue;
        };
        let LayerKind::Shape(shape) = &layer.kind else {
            continue;
        };
        ids.push(id);
        original.push((id, shape.profile.clone()));
        if base.is_none() {
            base = Some(shape.profile.clone());
        }
    }
    if ids.is_empty() {
        return;
    }
    let points: Vec<egui::Pos2> = base
        .unwrap_or(Profile::Round)
        .control_points(CONTROL_POINTS)
        .into_iter()
        .map(|(x, y)| egui::pos2(x, y))
        .collect();
    state.curve_editor = Some(CurveEditor {
        targets: ids,
        edge: None,
        original_edge: None,
        initial: points.clone(),
        points,
        drag: None,
        selected: None,
        original,
    });
    state.status = "Editing a custom bevel curve".into();
}

/// Opens the editor for one edge of a path, seeded from its current override or
/// the shape profile. Changes apply only to that edge.
pub(crate) fn open_edge_curve_editor(
    state: &mut EditorState,
    doc: &DocumentRes,
    layer_id: LayerId,
    edge: EdgeRef,
) {
    let Some(layer) = doc.0.find_layer(layer_id) else {
        return;
    };
    let LayerKind::Shape(shape) = &layer.kind else {
        return;
    };
    let ShapeSource::Path(path) = &shape.source else {
        return;
    };
    if edge.edge >= path.edge_count(edge.subpath) {
        return;
    }
    let current = path
        .edge_profile(edge.subpath, edge.edge)
        .cloned()
        .unwrap_or_else(|| shape.profile.clone());
    let points: Vec<egui::Pos2> = current
        .control_points(CONTROL_POINTS)
        .into_iter()
        .map(|(x, y)| egui::pos2(x, y))
        .collect();
    state.curve_editor = Some(CurveEditor {
        targets: vec![layer_id],
        edge: Some(edge),
        original_edge: Some((
            layer_id,
            edge,
            path.edge_profile(edge.subpath, edge.edge).cloned(),
        )),
        initial: points.clone(),
        points,
        drag: None,
        selected: None,
        original: Vec::new(),
    });
    state.status = "Editing this edge's custom bevel curve".into();
}

/// Draws the floating editor and applies edits live to all target shapes.
pub(crate) fn curve_editor_window(
    ctx: &egui::Context,
    state: &mut EditorState,
    doc: &mut DocumentRes,
) {
    let Some(mut editor) = state.curve_editor.take() else {
        return;
    };

    let mut open = true;
    let mut cancel = false;
    let mut reset = false;
    let mut close = false;
    let mut changed = false;

    egui::Window::new("Custom bevel profile")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(440.0)
        .min_width(340.0)
        .default_pos(ctx.viewport_rect().center() - egui::vec2(230.0, 220.0))
        .show(ctx, |ui| {
            let shape_word = if editor.edge.is_some() {
                "edge".to_string()
            } else if editor.targets.len() == 1 {
                "shape".to_string()
            } else {
                "shapes".to_string()
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("Editing {} {shape_word}", editor.targets.len()))
                        .size(TEXT_LABEL)
                        .color(TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} point{}",
                            editor.points.len(),
                            if editor.points.len() == 1 { "" } else { "s" }
                        ))
                        .size(TEXT_SMALL)
                        .color(MUTED),
                    );
                });
            });
            ui.add_space(5.0);

            let width = ui.available_width().clamp(300.0, 560.0);
            let height = (width * 0.72).clamp(210.0, 400.0);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
            let inner = graph_rect(rect);

            let pointer = response.interact_pointer_pos();
            let hover = response
                .hover_pos()
                .and_then(|pos| nearest_point(inner, &editor.points, pos));
            draw_graph(ui, rect, inner, &editor, hover);

            if response.drag_started() {
                if let Some(pos) = pointer {
                    editor.drag = nearest_point(inner, &editor.points, pos);
                    if let Some(index) = editor.drag {
                        editor.selected = Some(index);
                    }
                }
            }
            if response.dragged() {
                if let (Some(index), Some(pos)) = (editor.drag, pointer) {
                    let snapped = drag_point(inner, &editor.points, index, pos, snap_enabled(ui));
                    if editor.points[index] != snapped {
                        editor.points[index] = snapped;
                        changed = true;
                    }
                }
            }
            if response.drag_stopped() {
                editor.drag = None;
            }
            if response.clicked() {
                editor.selected = hover;
            }
            if response.double_clicked() {
                if let Some(pos) = pointer {
                    if nearest_point(inner, &editor.points, pos).is_none() {
                        let norm = to_normalized(inner, pos);
                        if norm.x > MIN_POINT_GAP
                            && norm.x < 1.0 - MIN_POINT_GAP
                            && editor
                                .points
                                .iter()
                                .all(|point| (point.x - norm.x).abs() > MIN_POINT_GAP)
                        {
                            let y = snap_value(norm.y, snap_enabled(ui));
                            let index = editor
                                .points
                                .iter()
                                .position(|point| point.x > norm.x)
                                .unwrap_or(editor.points.len().saturating_sub(1))
                                .max(1);
                            editor.points.insert(index, egui::pos2(norm.x, y));
                            editor.selected = Some(index);
                            changed = true;
                        }
                    }
                }
            }
            if response.secondary_clicked() {
                if let Some(pos) = pointer {
                    if let Some(index) = nearest_point(inner, &editor.points, pos) {
                        if index > 0 && index + 1 < editor.points.len() {
                            editor.points.remove(index);
                            editor.drag = None;
                            editor.selected = None;
                            changed = true;
                        }
                    }
                }
            }

            if response.hovered() {
                let cursor = if editor.drag.is_some() {
                    egui::CursorIcon::Grabbing
                } else if hover.is_some() {
                    egui::CursorIcon::Grab
                } else {
                    egui::CursorIcon::Crosshair
                };
                ui.ctx().set_cursor_icon(cursor);
            }

            ui.add_space(4.0);
            let readout = editor
                .selected
                .or(hover)
                .filter(|index| *index < editor.points.len());
            if let Some(index) = readout {
                let point = editor.points[index];
                ui.label(
                    egui::RichText::new(format!("t {:.2}   height {:.2}", point.x, point.y))
                        .size(TEXT_SMALL)
                        .color(ACCENT),
                );
            } else {
                ui.label(
                    egui::RichText::new("Drag points")
                        .size(TEXT_SMALL)
                        .color(MUTED),
                );
            }
            ui.label(
                egui::RichText::new(
                    "Double-click to add · right-click to remove · Shift snaps to 0.05",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                if ui
                    .button(egui::RichText::new("Reset").size(TEXT_SMALL))
                    .on_hover_text("Restore the curve this window opened with")
                    .clicked()
                {
                    reset = true;
                }
                if ui
                    .button(egui::RichText::new("Cancel").size(TEXT_SMALL))
                    .on_hover_text("Restore the profiles from before editing")
                    .clicked()
                {
                    cancel = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(egui::RichText::new("Apply").size(TEXT_SMALL))
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        });

    if reset {
        editor.points = editor.initial.clone();
        editor.selected = None;
        changed = true;
    }
    if changed {
        apply_curve(doc, state, &editor);
    }
    if cancel {
        restore_profiles(doc, state, &editor);
        state.curve_editor = None;
        state.status = "Custom bevel curve cancelled".into();
        return;
    }
    if !open || close {
        state.curve_editor = None;
        if changed || close {
            state.status = "Custom bevel curve applied".into();
        }
        return;
    }
    state.curve_editor = Some(editor);
}

fn apply_curve(doc: &mut DocumentRes, state: &mut EditorState, editor: &CurveEditor) {
    let points: Vec<(f32, f32)> = editor
        .points
        .iter()
        .map(|point| (point.x, point.y))
        .collect();
    let lut = Profile::lut_from_points(&points);
    state.ensure_undo_gesture(&doc.0);
    if let Some(edge) = editor.edge {
        let Some(id) = editor.targets.first().copied() else {
            return;
        };
        let Some(layer) = doc.0.find_layer_mut(id) else {
            return;
        };
        if let LayerKind::Shape(shape) = &mut layer.kind {
            if let ShapeSource::Path(path) = &mut shape.source {
                path.set_edge_profile(edge.subpath, edge.edge, Some(Profile::Custom(lut)));
                layer.touch();
            }
        }
        state.mark_dirty();
        return;
    }
    for id in &editor.targets {
        let Some(layer) = doc.0.find_layer_mut(*id) else {
            continue;
        };
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.profile = Profile::Custom(lut.clone());
            layer.touch();
        }
    }
    state.mark_dirty();
}

fn restore_profiles(doc: &mut DocumentRes, state: &mut EditorState, editor: &CurveEditor) {
    state.ensure_undo_gesture(&doc.0);
    if let Some((id, edge, profile)) = &editor.original_edge {
        if let Some(layer) = doc.0.find_layer_mut(*id) {
            if let LayerKind::Shape(shape) = &mut layer.kind {
                if let ShapeSource::Path(path) = &mut shape.source {
                    path.set_edge_profile(edge.subpath, edge.edge, profile.clone());
                    layer.touch();
                }
            }
        }
        state.mark_dirty();
        return;
    }
    for (id, profile) in &editor.original {
        let Some(layer) = doc.0.find_layer_mut(*id) else {
            continue;
        };
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.profile = profile.clone();
            layer.touch();
        }
    }
    state.mark_dirty();
}

fn graph_rect(rect: egui::Rect) -> egui::Rect {
    rect.shrink2(egui::vec2(10.0, 8.0))
}

fn draw_graph(
    ui: &egui::Ui,
    rect: egui::Rect,
    inner: egui::Rect,
    editor: &CurveEditor,
    hover: Option<usize>,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 5.0, INPUT_BG);
    painter.rect_stroke(
        rect,
        5.0,
        egui::Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Inside,
    );

    for step in 1..4 {
        let t = step as f32 / 4.0;
        let x = inner.left() + t * inner.width();
        let y = inner.bottom() - t * inner.height();
        let stroke = egui::Stroke::new(1.0_f32, BORDER.gamma_multiply(0.45));
        painter.line_segment(
            [egui::pos2(x, inner.top()), egui::pos2(x, inner.bottom())],
            stroke,
        );
        painter.line_segment(
            [egui::pos2(inner.left(), y), egui::pos2(inner.right(), y)],
            stroke,
        );
    }
    painter.line_segment(
        [inner.left_bottom(), inner.right_top()],
        egui::Stroke::new(1.0_f32, BORDER.gamma_multiply(0.35)),
    );

    if editor.points != editor.initial {
        let initial = smooth_curve(&editor.initial);
        let screen: Vec<egui::Pos2> = initial
            .iter()
            .map(|point| to_screen(inner, *point))
            .collect();
        painter.add(egui::Shape::line(
            screen,
            egui::Stroke::new(1.0_f32, MUTED.gamma_multiply(0.55)),
        ));
    }

    let curve = smooth_curve(&editor.points);
    let screen_points: Vec<egui::Pos2> =
        curve.iter().map(|point| to_screen(inner, *point)).collect();
    let mut fill = Vec::with_capacity(screen_points.len() + 2);
    fill.push(inner.left_bottom());
    fill.extend(screen_points.iter().copied());
    fill.push(inner.right_bottom());
    painter.add(egui::Shape::Path(egui::epaint::PathShape {
        points: fill,
        closed: true,
        fill: ACCENT.gamma_multiply(0.10),
        stroke: egui::Stroke::NONE.into(),
    }));
    painter.add(egui::Shape::line(
        screen_points,
        egui::Stroke::new(2.0_f32, ACCENT),
    ));

    if let Some(index) = hover {
        let screen = to_screen(inner, editor.points[index]);
        painter.line_segment(
            [
                egui::pos2(screen.x, inner.top()),
                egui::pos2(screen.x, inner.bottom()),
            ],
            egui::Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.25)),
        );
    }

    for (index, point) in editor.points.iter().enumerate() {
        let screen = to_screen(inner, *point);
        let active = editor.drag == Some(index);
        let highlight = active || editor.selected == Some(index);
        let radius = if active {
            6.0
        } else if highlight {
            5.5
        } else {
            4.5
        };
        if hover == Some(index) && !active {
            painter.circle_filled(screen, radius + 3.0, ACCENT.gamma_multiply(0.18));
        }
        painter.circle_filled(
            screen,
            radius,
            if active {
                egui::Color32::WHITE
            } else if highlight {
                ACCENT
            } else {
                TEXT
            },
        );
        painter.circle_stroke(
            screen,
            radius,
            egui::Stroke::new(1.2_f32, if highlight { TEXT } else { ACCENT }),
        );
    }

    painter.text(
        inner.left_bottom() + egui::vec2(4.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        "0",
        egui::FontId::proportional(TEXT_CAPTION),
        MUTED,
    );
    painter.text(
        inner.right_top() + egui::vec2(-4.0, 4.0),
        egui::Align2::RIGHT_TOP,
        "1",
        egui::FontId::proportional(TEXT_CAPTION),
        MUTED,
    );
}

/// Samples the smooth curve that [`Profile::lut_from_points`] evaluates.
fn smooth_curve(points: &[egui::Pos2]) -> Vec<egui::Pos2> {
    let pairs: Vec<(f32, f32)> = points.iter().map(|point| (point.x, point.y)).collect();
    let profile = Profile::Custom(Profile::lut_from_points(&pairs));
    (0..=SAMPLE_COUNT)
        .map(|index| {
            let t = index as f32 / SAMPLE_COUNT as f32;
            egui::pos2(t, profile.sample(t))
        })
        .collect()
}

fn drag_point(
    rect: egui::Rect,
    points: &[egui::Pos2],
    index: usize,
    pos: egui::Pos2,
    snap: bool,
) -> egui::Pos2 {
    let norm = to_normalized(rect, pos);
    let count = points.len();
    let x = if index == 0 || count < 2 {
        0.0
    } else if index + 1 == count {
        1.0
    } else {
        snap_value(norm.x, snap).clamp(
            points[index - 1].x + MIN_POINT_GAP,
            points[index + 1].x - MIN_POINT_GAP,
        )
    };
    egui::pos2(x, snap_value(norm.y, snap).clamp(0.0, 1.0))
}

fn snap_enabled(ui: &egui::Ui) -> bool {
    ui.input(|input| input.modifiers.shift)
}

fn snap_value(value: f32, snap: bool) -> f32 {
    if snap {
        (value * 20.0).round() / 20.0
    } else {
        value
    }
}

fn nearest_point(rect: egui::Rect, points: &[egui::Pos2], pos: egui::Pos2) -> Option<usize> {
    let mut best = None;
    let mut best_distance = POINT_HIT_RADIUS;
    for (index, point) in points.iter().enumerate() {
        let distance = to_screen(rect, *point).distance(pos);
        if distance < best_distance {
            best_distance = distance;
            best = Some(index);
        }
    }
    best
}

fn to_screen(rect: egui::Rect, point: egui::Pos2) -> egui::Pos2 {
    egui::pos2(
        rect.left() + point.x * rect.width(),
        rect.bottom() - point.y * rect.height(),
    )
}

fn to_normalized(rect: egui::Rect, pos: egui::Pos2) -> egui::Pos2 {
    egui::pos2(
        ((pos.x - rect.left()) / rect.width().max(1e-5)).clamp(0.0, 1.0),
        ((rect.bottom() - pos.y) / rect.height().max(1e-5)).clamp(0.0, 1.0),
    )
}
