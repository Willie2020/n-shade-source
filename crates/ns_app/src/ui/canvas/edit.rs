//! Direct shape editing: selection, move, resize, and creation.

use super::*;

use glam::Affine2;
use ns_core::brush::{stamp_normal_tip, stamp_tip, symmetry_placements, BrushMode, DabTransform};
use std::collections::HashSet;

use crate::snapping::{
    self, MovingEdges, SnapKinds, SnapOptions, SnapResult, SnapSession, Subject,
};
pub(crate) fn canvas_edit_start(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    duplicate: bool,
) {
    canvas_edit_start_impl(state, doc, view, pointer, duplicate, false);
}

/// Start a normal Select-tool move even though a node-edit tool is active.
/// Used when the press misses every anchor and lands on a path segment: the
/// line itself should still drag and snap like a shape.
pub(super) fn canvas_edit_start_for_node_tool(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    duplicate: bool,
) {
    canvas_edit_start_impl(state, doc, view, pointer, duplicate, true);
}

fn canvas_edit_start_impl(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    duplicate: bool,
    allow_node_tool: bool,
) {
    let tool = if allow_node_tool {
        CanvasTool::Select
    } else {
        state.canvas_tool
    };
    let p = view.screen_to_doc(pointer);
    if state.mask_edit_layer.is_some() {
        match tool {
            CanvasTool::Paint => start_mask_paint(state, doc, p),
            CanvasTool::Rectangle | CanvasTool::Ellipse | CanvasTool::Star => {
                state.canvas_drag = Some(CanvasDrag::Create {
                    tool,
                    start: p,
                    current: p,
                });
            }
            _ => state.canvas_drag = None,
        }
        return;
    }
    if tool.is_node_edit_tool() {
        return;
    }
    if tool == CanvasTool::Paint {
        start_paint(state, doc, p);
        return;
    }
    if tool == CanvasTool::Text {
        place_text(state, doc, p);
        return;
    }
    if tool != CanvasTool::Select {
        state.canvas_drag = Some(CanvasDrag::Create {
            tool,
            start: p,
            current: p,
        });
        return;
    }

    // Alt-drag is always a duplicate gesture, even when the press lands inside
    // a transform handle's hit radius. Without this guard a multi-selection
    // whose union edge is close to the pointer would resize instead of copy.
    // Node tools have no visible transform handles, so a line press must not
    // hit an invisible one either.
    if !duplicate && !allow_node_tool {
        // Locked content cannot be transformed; clicking elsewhere still selects.
        if selection_locked(&doc.0, state)
            && (selected_rotation_hit(doc, state, view, pointer)
                || selected_handle_hit(doc, state, view, pointer).is_some())
        {
            state.status = "Layer is locked · unlock it to transform".into();
            return;
        }
        if selected_rotation_hit(doc, state, view, pointer) {
            start_rotation(state, doc, p);
            return;
        }

        if let Some(handle) = selected_handle_hit(doc, state, view, pointer) {
            let Some(bounds) = selection_bounds(&doc.0, state) else {
                return;
            };
            let sources = collect_selection_sources(doc, state);
            if sources.is_empty() {
                state.status = "This selection has no scalable vector content".into();
                return;
            }
            state.canvas_drag = Some(CanvasDrag::Resize {
                handle,
                sources,
                bounds,
            });
            return;
        }
    }

    // A selected collapsed group behaves as one canvas object: dragging any of
    // its visible content moves the whole group instead of selecting a child.
    if let Some(path) = state.selected.clone() {
        let collapsed_hit = doc.0.layer_by_path(&path).is_some_and(|layer| {
            layer.is_group() && state.collapsed.contains(&layer.id) && layer_contains(layer, p)
        });
        if collapsed_hit {
            if duplicate {
                start_duplicate_move(state, doc, p);
            } else if let Some(layer) = doc.0.layer_by_path(&path) {
                start_group_move(state, doc, layer, p);
            }
            return;
        }
    }

    if let Some(path) = top_item_at(&doc.0, state, p) {
        if duplicate {
            // Alt-drag copies the selection when the hit layer is part of it,
            // so a multi-selection duplicates as one set; otherwise only the
            // hit layer is copied.
            let in_selection = state.selected.as_deref() == Some(path.as_slice())
                || state.multi_selected.contains(&path);
            if !in_selection {
                state.selected = Some(path.clone());
                state.multi_selected.clear();
            }
            state.node_selection.clear();
            state.node_hover = None;
            start_duplicate_move(state, doc, p);
            return;
        }
        // Dragging a member of a multi-selection moves the whole set. Keep
        // the primary and multi-selection intact for the drag.
        let in_selection = state.selected.as_deref() == Some(path.as_slice())
            || state.multi_selected.contains(&path);
        if !state.multi_selected.is_empty() && in_selection {
            state.node_selection.clear();
            state.node_hover = None;
            start_selection_move(state, doc, p);
            return;
        }

        state.selected = Some(path.clone());
        state.multi_selected.clear();
        state.node_selection.clear();
        state.node_hover = None;
        if let Some(layer) = doc.0.layer_by_path(&path) {
            if layer.locked {
                state.status = "Layer is locked · unlock it to move".into();
                return;
            }
            match &layer.kind {
                LayerKind::Shape(shape) => {
                    state.canvas_drag = Some(CanvasDrag::Move {
                        layer_id: layer.id,
                        start: p,
                        source: shape.source.clone(),
                    });
                    begin_snap_session(state, doc, [layer.id]);
                }
                LayerKind::Group { .. } => start_group_move(state, doc, layer, p),
                _ => {}
            }
        }
    } else {
        state.canvas_drag = None;
    }
}

/// Drag any member of a multi-selection: the whole set moves together.
fn start_selection_move(state: &mut EditorState, doc: &mut DocumentRes, start: Vec2) {
    if selection_locked(&doc.0, state) {
        state.status = "Layer is locked · unlock it to move".into();
        return;
    }
    let sources = collect_selection_sources(doc, state);
    if sources.is_empty() {
        state.status = "This selection has no translatable shape layers".into();
        return;
    }
    let excluded: Vec<LayerId> = sources.iter().map(|(id, _)| *id).collect();
    state.canvas_drag = Some(CanvasDrag::MoveGroup {
        start,
        sources,
        axis: None,
    });
    begin_snap_session(state, doc, excluded);
}

/// Alt-drag: copy the selection, select the copies and move them under the
/// pointer. The originals stay exactly where they were, which is what makes
/// the gesture a duplicate rather than a move.
fn start_duplicate_move(state: &mut EditorState, doc: &mut DocumentRes, start: Vec2) {
    if selection_locked(&doc.0, state) {
        state.status = "Layer is locked · unlock it to duplicate".into();
        return;
    }
    // The originals are valid geometry but must not act as snap targets for
    // their own copies: at a small drag the copy would be pulled back onto
    // the original and hysteresis would keep it stuck there.
    let mut original_ids = Vec::new();
    let paths = state.selected.iter().chain(state.multi_selected.iter());
    for path in paths {
        if let Some(layer) = doc.0.layer_by_path(path) {
            original_ids.push(layer.id);
        }
    }

    let copies = duplicate_selected_layers(state, doc);
    if copies.is_empty() {
        state.canvas_drag = None;
        return;
    }
    let mut sources = Vec::new();
    for path in &copies {
        if let Some(layer) = doc.0.layer_by_path(path) {
            collect_group_sources(layer, &mut sources);
        }
    }
    let mut seen = HashSet::new();
    sources.retain(|(id, _)| seen.insert(*id));
    if sources.is_empty() {
        // Flat, photo and painted layers have no outline to translate; the
        // copy exists but stays where it landed in the stack.
        state.canvas_drag = None;
        state.status = "Layer duplicated · this kind has no outline to drag".into();
        return;
    }
    let mut excluded: Vec<LayerId> = sources.iter().map(|(id, _)| *id).collect();
    excluded.extend(original_ids);
    let axis = if state.ruler_locks_duplicates {
        state.ruler.map(|ruler| ruler.angle)
    } else {
        None
    };
    state.canvas_drag = Some(CanvasDrag::MoveGroup {
        start,
        sources,
        axis,
    });
    if let Some(angle) = axis {
        state.clear_snap_guides();
        state.status = format!(
            "Duplicated · drag along the ruler at {}",
            angle_label(angle)
        );
    } else {
        begin_snap_session(state, doc, excluded);
    }
}

pub(crate) fn canvas_edit_drag(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    snap_enabled: bool,
) {
    canvas_edit_drag_with_constraint(state, doc, view, pointer, snap_enabled, false);
}

/// Drag with an optional Shift axis lock. `constrain` rounds the movement to
/// the nearest 45° ray and **bypasses smart guides**, so a duplicate cannot be
/// pulled a few pixels off the line it is meant to follow. The flag is read
/// every frame, so Shift can be pressed or released mid-drag.
pub(crate) fn canvas_edit_drag_with_constraint(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    snap_enabled: bool,
    constrain: bool,
) {
    let p = view.screen_to_doc(pointer);
    // Move the drag out of the state for the duration of this update. This
    // avoids cloning the cached geometry snapshots on every pointer move;
    // none of the helpers below touch `canvas_drag`, so it is restored intact.
    let Some(mut drag) = state.canvas_drag.take() else {
        return;
    };
    match &mut drag {
        CanvasDrag::Create { current, .. } => {
            *current = p;
        }
        CanvasDrag::Move {
            layer_id,
            start,
            source,
        } => {
            let raw = p - *start;
            let delta = if constrain {
                constrained_delta(state, raw)
            } else {
                match source.bounds() {
                    Some(bounds) => {
                        let rect = Bounds {
                            min: bounds.min + raw,
                            max: bounds.max + raw,
                        };
                        let snap = snap_translation(
                            state,
                            view,
                            Subject::Rect {
                                rect,
                                moving: MovingEdges::ALL,
                            },
                            snap_enabled,
                        );
                        raw + snap.delta
                    }
                    None => raw,
                }
            };
            if let Some(new_source) = source.transformed(Vec2::ONE, delta) {
                apply_shape_source(doc, state, *layer_id, new_source);
            }
        }
        CanvasDrag::Resize {
            handle,
            sources,
            bounds,
        } => {
            if let Some((scale, offset)) = resize_transform(*bounds, *handle, p) {
                apply_source_transform(doc, state, sources, scale, offset);
            }
        }
        CanvasDrag::MoveGroup {
            start,
            sources,
            axis,
        } => {
            let raw = p - *start;
            // An explicit Shift axis lock outranks the ruler's Lock dupes
            // setting. Otherwise a ruler at, say, 85° would still introduce a
            // small sideways drift while the user is asking for a straight
            // vertical duplicate.
            let delta = if constrain {
                constrained_delta(state, raw)
            } else {
                match axis {
                    Some(angle) if snap_enabled => {
                        // Ruler lock: keep only the component along the locked
                        // angle, so the copy cannot drift sideways.
                        let direction = Vec2::from_angle(*angle);
                        direction * raw.dot(direction)
                    }
                    Some(_) => raw,
                    None => match union_source_bounds(sources) {
                        Some(bounds) => {
                            let rect = Bounds {
                                min: bounds.min + raw,
                                max: bounds.max + raw,
                            };
                            let snap = snap_translation(
                                state,
                                view,
                                Subject::Rect {
                                    rect,
                                    moving: MovingEdges::ALL,
                                },
                                snap_enabled,
                            );
                            raw + snap.delta
                        }
                        None => raw,
                    },
                }
            };
            apply_group_translation(doc, state, sources, delta);
            if axis.is_some() || constrain {
                state.active_guides.clear();
            }
        }
        CanvasDrag::Rotate {
            center,
            start_angle,
            sources,
        } => {
            let angle = (p - *center).y.atan2((p - *center).x);
            let delta = normalize_angle(angle - *start_angle);
            apply_rotation(doc, state, sources, *center, delta);
        }
        CanvasDrag::Paint { layer_id, last } => {
            let from = *last;
            paint_segment(doc, state, *layer_id, from, p);
            *last = p;
        }
        CanvasDrag::MaskPaint { layer_id, last } => {
            let from = *last;
            mask_segment(doc, state, *layer_id, from, p);
            *last = p;
        }
        CanvasDrag::NodeEdit { .. } => {}
    }
    state.canvas_drag = Some(drag);
}

pub(crate) fn canvas_edit_stop(state: &mut EditorState, doc: &mut DocumentRes) {
    state.clear_snap_guides();
    if matches!(
        state.canvas_drag,
        Some(CanvasDrag::Paint { .. }) | Some(CanvasDrag::MaskPaint { .. })
    ) {
        state.canvas_drag = None;
        state.mask_stroke = None;
        return;
    }
    if let Some(CanvasDrag::Create {
        tool,
        start,
        current,
        ..
    }) = state.canvas_drag.take()
    {
        let bounds = Bounds {
            min: start.min(current),
            max: start.max(current),
        };
        if state.mask_edit_layer.is_some() {
            if let Some(layer) = make_created_layer(tool, bounds, state.rect_corner) {
                if let LayerKind::Shape(shape) = &layer.kind {
                    stamp_mask_shape(doc, state, shape.source.clone());
                }
            }
            // Keep the stamping tool active so the mask colour can be swapped
            // and another shape stamped without re-selecting the tool.
        } else if let Some(layer) = make_created_layer(tool, bounds, state.rect_corner) {
            add_layer(doc, state, layer);
            state.mark_dirty();
            // The drawing tool stays active so another shape can be placed
            // immediately; Esc is the way back to Select.
        }
    }
}
pub(crate) fn apply_shape_source(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    layer_id: LayerId,
    source: ShapeSource,
) {
    if let Some(layer) = doc.0.find_layer_mut(layer_id) {
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.source = source;
            layer.touch();
            state.mark_dirty();
        }
    }
}

pub(crate) fn rotation_handle_pos(view: &CanvasView, bounds: Bounds) -> egui::Pos2 {
    let top = view.doc_to_screen(Vec2::new(bounds.center().x, bounds.min.y));
    top + egui::vec2(0.0, -26.0)
}

pub(crate) fn selected_rotation_hit(
    doc: &DocumentRes,
    state: &EditorState,
    view: &CanvasView,
    pointer: egui::Pos2,
) -> bool {
    let Some(bounds) = selection_bounds(&doc.0, state) else {
        return false;
    };
    let handle = rotation_handle_pos(view, bounds);
    handle.distance(pointer) <= 18.0
}

fn normalize_angle(angle: f32) -> f32 {
    let wrapped = angle.rem_euclid(std::f32::consts::TAU);
    if wrapped > std::f32::consts::PI {
        wrapped - std::f32::consts::TAU
    } else {
        wrapped
    }
}

fn start_rotation(state: &mut EditorState, doc: &mut DocumentRes, pointer: Vec2) {
    let Some(bounds) = selection_bounds(&doc.0, state) else {
        state.status = "This selection cannot be rotated".into();
        return;
    };
    let sources = collect_selection_sources(doc, state);
    if sources.is_empty() {
        state.status = "This selection has no rotatable vector content".into();
        return;
    }
    let center = bounds.center();
    state.canvas_drag = Some(CanvasDrag::Rotate {
        center,
        start_angle: (pointer - center).y.atan2((pointer - center).x),
        sources,
    });
}

fn rotated_source(source: &ShapeSource, transform: Affine2) -> Option<ShapeSource> {
    match source {
        ShapeSource::Path(path) => {
            let mut path = path.clone();
            path.apply_transform(transform);
            Some(ShapeSource::Path(path))
        }
        ShapeSource::Primitive(_) => {
            // Primitives have no rotation field, so bake the rotation into an
            // editable path. Node editing remains available afterwards.
            let polygon = source.as_polygon()?;
            let mut path = Path::from_polygon(&polygon);
            path.apply_transform(transform);
            Some(ShapeSource::Path(path))
        }
        // Text has no rotation field either: rotate the glyph outlines and
        // keep the result editable as a path.
        ShapeSource::Text(_) => {
            let mut path = source.to_path()?;
            path.apply_transform(transform);
            Some(ShapeSource::Path(path))
        }
        ShapeSource::RasterRegion(region) => {
            region.transformed_matrix(transform.matrix2, transform.translation)
        }
        ShapeSource::Raster(_) => None,
    }
}

fn apply_rotation(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    sources: &[(LayerId, ShapeSource)],
    center: Vec2,
    delta: f32,
) {
    if delta.abs() < 1e-5 {
        return;
    }
    let transform = Affine2::from_translation(center)
        * Affine2::from_angle(delta)
        * Affine2::from_translation(-center);
    let mut rotated = false;
    for (layer_id, source) in sources {
        let Some(new_source) = rotated_source(source, transform) else {
            continue;
        };
        let Some(layer) = doc.0.find_layer_mut(*layer_id) else {
            continue;
        };
        let LayerKind::Shape(shape) = &mut layer.kind else {
            continue;
        };
        shape.source = new_source;
        layer.touch();
        rotated = true;
    }
    if rotated {
        state.mark_dirty();
    }
}

fn brush_mode(state: &EditorState) -> BrushMode {
    match state.brush_mode {
        PaintMode::Raise => BrushMode::Raise,
        PaintMode::Lower => BrushMode::Lower,
        PaintMode::Smooth => BrushMode::Smooth,
        PaintMode::Flatten => BrushMode::Flatten(state.brush_flatten_target),
    }
}

fn ensure_paint_layer(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    normal: bool,
) -> Option<LayerId> {
    if let Some(path) = state.selected.clone() {
        if let Some(layer) = doc.0.layer_by_path(&path) {
            if layer.locked {
                state.status = "Layer is locked · unlock it to paint".into();
                return None;
            }
            let matches_kind = if normal {
                matches!(layer.kind, LayerKind::NormalPaint { .. })
            } else {
                matches!(layer.kind, LayerKind::HeightPaint { .. })
            };
            if matches_kind {
                return Some(layer.id);
            }
        }
    }
    let layer = if normal {
        Layer::normal_paint(
            "Normal paint",
            Grid::new(doc.0.size.x, doc.0.size.y, glam::Vec3::Z),
        )
    } else {
        Layer::height_paint("Paint", Grid::new(doc.0.size.x, doc.0.size.y, 0.0), 12.0)
    };
    let path = add_layer(doc, state, layer);
    doc.0.layer_by_path(&path).map(|layer| layer.id)
}

/// Every transformed dab for a pointer position: the brush angle plus the
/// selected mirror or radial symmetry.
fn brush_dabs(state: &EditorState, doc_size: Vec2, centre: Vec2) -> Vec<DabTransform> {
    let pivot = state.symmetry_pivot * doc_size;
    let radius = state.brush_radius_now();
    symmetry_placements(
        centre,
        state.brush_angle_deg.to_radians(),
        state.symmetry,
        state.symmetry_radial_count,
        pivot,
    )
    .into_iter()
    .map(|placement| DabTransform {
        center: placement.center,
        radius,
        matrix: placement.matrix,
    })
    .collect()
}

fn paint_dab(doc: &mut DocumentRes, state: &mut EditorState, layer_id: LayerId, p: Vec2) {
    let dabs = brush_dabs(state, doc.0.size.as_vec2(), p);
    let flow = state.brush_flow_now() * state.brush_opacity_now();
    let tip_index = state.brush_library.selected_index();
    let mode = brush_mode(state);
    let hardness = state.brush_hardness;
    let Some(layer) = doc.0.find_layer_mut(layer_id) else {
        return;
    };
    let dirty = {
        let tip = state.brush_library.tips.get(tip_index);
        match &mut layer.kind {
            LayerKind::NormalPaint { normal } => {
                let Some(tip) = tip.filter(|tip| tip.is_normal()) else {
                    return;
                };
                let mut dirty = false;
                for dab in &dabs {
                    dirty |= stamp_normal_tip(normal, tip, dab, flow).is_some();
                }
                dirty
            }
            LayerKind::HeightPaint { height, .. } => {
                let mut dirty = false;
                for dab in &dabs {
                    dirty |= stamp_tip(height, tip, dab, hardness, flow, mode).is_some();
                }
                dirty
            }
            _ => false,
        }
    };
    if dirty {
        layer.touch();
        state.mark_dirty();
    }
}

fn paint_segment(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    layer_id: LayerId,
    from: Vec2,
    to: Vec2,
) {
    let doc_size = doc.0.size.as_vec2();
    let flow = state.brush_flow_now() * state.brush_opacity_now();
    let tip_index = state.brush_library.selected_index();
    let mode = brush_mode(state);
    let hardness = state.brush_hardness;
    let radius = state.brush_radius_now();
    let step = (radius * 0.2).max(1.0);
    let steps = (((to - from).length() / step).ceil() as usize).max(1);
    let Some(layer) = doc.0.find_layer_mut(layer_id) else {
        return;
    };
    let dirty = {
        let tip = state.brush_library.tips.get(tip_index);
        let mut dirty = false;
        for i in 1..=steps {
            let p = from.lerp(to, i as f32 / steps as f32);
            for dab in brush_dabs(state, doc_size, p) {
                match &mut layer.kind {
                    LayerKind::NormalPaint { normal } => {
                        if let Some(tip) = tip.filter(|tip| tip.is_normal()) {
                            dirty |= stamp_normal_tip(normal, tip, &dab, flow).is_some();
                        }
                    }
                    LayerKind::HeightPaint { height, .. } => {
                        dirty |= stamp_tip(height, tip, &dab, hardness, flow, mode).is_some();
                    }
                    _ => {}
                }
            }
        }
        dirty
    };
    if dirty {
        layer.touch();
        state.mark_dirty();
    }
}

fn start_paint(state: &mut EditorState, doc: &mut DocumentRes, p: Vec2) {
    let normal = state.brush_library.is_normal();
    let Some(layer_id) = ensure_paint_layer(state, doc, normal) else {
        return;
    };
    paint_dab(doc, state, layer_id, p);
    state.canvas_drag = Some(CanvasDrag::Paint { layer_id, last: p });
}

pub(crate) fn paint_click(state: &mut EditorState, doc: &mut DocumentRes, p: Vec2) {
    if let Some(layer_id) = state.mask_edit_layer {
        state.mask_stroke = None;
        mask_dab(doc, state, layer_id, p);
        state.canvas_drag = None;
        return;
    }
    let normal = state.brush_library.is_normal();
    let Some(layer_id) = ensure_paint_layer(state, doc, normal) else {
        return;
    };
    paint_dab(doc, state, layer_id, p);
    state.canvas_drag = None;
}

fn start_mask_paint(state: &mut EditorState, doc: &mut DocumentRes, p: Vec2) {
    let Some(layer_id) = state.mask_edit_layer else {
        return;
    };
    // A drag is one stroke: its opacity cap holds until the button is released.
    state.mask_stroke = None;
    if let Some((mask, scale)) = mask_and_scale(doc, layer_id) {
        state.mask_stroke = Some(new_mask_stroke(state, mask, scale));
    }
    mask_dab(doc, state, layer_id, p);
    state.canvas_drag = Some(CanvasDrag::MaskPaint { layer_id, last: p });
}

/// Mask pixels per document pixel. Masks normally match the document, but
/// older projects or imports may carry a different resolution.
fn mask_and_scale(doc: &DocumentRes, layer_id: LayerId) -> Option<(&Grid<f32>, f32)> {
    let mask = doc.0.find_layer(layer_id)?.mask.as_ref()?;
    let scale = mask.w as f32 / doc.0.size.x.max(1) as f32;
    Some((mask, scale))
}

/// The value a mask brush moves towards.
fn mask_target_value(state: &EditorState) -> f32 {
    match state.brush_mode {
        PaintMode::Raise => 1.0,
        PaintMode::Lower => 0.0,
        PaintMode::Flatten => state.brush_flatten_target.clamp(0.0, 1.0),
        // Blur strokes use their own target; this is never painted.
        PaintMode::Smooth => 0.5,
    }
}

fn new_mask_stroke(
    state: &EditorState,
    mask: &Grid<f32>,
    scale: f32,
) -> ns_core::brush::MaskStroke {
    use ns_core::brush::{MaskStroke, StrokeTarget};
    let target = if state.brush_mode == PaintMode::Smooth {
        StrokeTarget::Blur
    } else {
        StrokeTarget::Value(mask_target_value(state))
    };
    MaskStroke::new(
        mask,
        target,
        state.brush_opacity_now(),
        state.brush_radius_now() * scale * 0.35,
    )
}

/// One dab. Outside a drag the dab is its own single-dab stroke.
fn mask_dab(doc: &mut DocumentRes, state: &mut EditorState, layer_id: LayerId, p: Vec2) {
    mask_stroke_points(doc, state, layer_id, &[p]);
}

fn mask_segment(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    layer_id: LayerId,
    from: Vec2,
    to: Vec2,
) {
    let step = (state.brush_radius_now() * 0.15).max(0.75);
    let steps = (((to - from).length() / step).ceil() as usize).max(1);
    let points: Vec<Vec2> = (1..=steps)
        .map(|i| from.lerp(to, i as f32 / steps as f32))
        .collect();
    mask_stroke_points(doc, state, layer_id, &points);
}

fn mask_stroke_points(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    layer_id: LayerId,
    points: &[Vec2],
) {
    let Some((mask, scale)) = mask_and_scale(doc, layer_id) else {
        return;
    };
    let one_shot = state.mask_stroke.is_none();
    let mut stroke = match state.mask_stroke.take() {
        Some(stroke) => stroke,
        None => new_mask_stroke(state, mask, scale),
    };
    let pivot = state.symmetry_pivot * doc.0.size.as_vec2() * scale;
    let Some(layer) = doc.0.find_layer_mut(layer_id) else {
        return;
    };
    let Some(mask) = layer.mask.as_mut() else {
        return;
    };
    let radius = state.brush_radius_now() * scale;
    let flow = state.brush_flow_now();
    let hardness = state.brush_hardness;
    let tip_index = state.brush_library.selected_index();
    let angle = state.brush_angle_deg.to_radians();
    let dirty = {
        let tip = state.brush_library.tips.get(tip_index);
        let mut dirty = false;
        for p in points {
            for placement in symmetry_placements(
                *p * scale,
                angle,
                state.symmetry,
                state.symmetry_radial_count,
                pivot,
            ) {
                let dab = DabTransform {
                    center: placement.center,
                    radius,
                    matrix: placement.matrix,
                };
                dirty |= stroke
                    .dab_with_tip(mask, tip, &dab, hardness, flow)
                    .is_some();
            }
        }
        dirty
    };
    if !one_shot {
        state.mask_stroke = Some(stroke);
    }
    if dirty {
        layer.touch_mask();
        state.mark_dirty();
    }
}

/// Rasterizes a primitive or pen path into the active mask, moving covered
/// pixels towards the brush value (white, black or the Value target) by the
/// brush opacity.
pub(crate) fn stamp_mask_shape(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    source: ShapeSource,
) {
    let Some(layer_id) = state.mask_edit_layer else {
        return;
    };
    let doc_size = doc.0.size;
    // Shapes stamp the last chosen colour (or the Value target).
    let value = match state.brush_mode {
        PaintMode::Flatten => state.brush_flatten_target.clamp(0.0, 1.0),
        _ if state.mask_white => 1.0,
        _ => 0.0,
    };
    let opacity = state.brush_opacity.clamp(0.0, 1.0);
    let Some(layer) = doc.0.find_layer_mut(layer_id) else {
        return;
    };
    let Some(mask) = layer.mask.as_mut() else {
        return;
    };
    let mut coverage = source.rasterize(doc_size.x, doc_size.y);
    if coverage.w != mask.w || coverage.h != mask.h {
        coverage = coverage.resized(mask.w, mask.h);
    }
    for (dst, c) in mask.data.iter_mut().zip(coverage.data.iter()) {
        let t = c.clamp(0.0, 1.0) * opacity;
        *dst += (value - *dst) * t;
    }
    layer.touch_mask();
    state.mark_dirty();
}

/// Every selected layer path, primary first. The primary is filtered out of
/// the additions so it is never transformed twice.
fn selected_layer_paths(state: &EditorState) -> Vec<&[usize]> {
    let primary = state.selected.as_deref();
    let mut paths = Vec::new();
    if let Some(primary) = primary {
        paths.push(primary);
    }
    paths.extend(
        state
            .multi_selected
            .iter()
            .map(Vec::as_slice)
            .filter(|path| Some(*path) != primary),
    );
    paths
}

/// Union bounds of every selected visible layer. Groups contribute the union
/// of their visible descendants, so one box drives a group or multi-selection
/// exactly like a single shape.
pub(crate) fn selection_bounds(doc: &ns_core::Document, state: &EditorState) -> Option<Bounds> {
    selected_layer_paths(state)
        .into_iter()
        .filter_map(|path| doc.layer_by_path(path))
        .filter(|layer| layer.visible)
        .filter_map(layer_bounds)
        .reduce(|a, b| Bounds {
            min: a.min.min(b.min),
            max: a.max.max(b.max),
        })
}

pub(crate) fn selected_handle_hit(
    doc: &DocumentRes,
    state: &EditorState,
    view: &CanvasView,
    pointer: egui::Pos2,
) -> Option<ResizeHandle> {
    let bounds = selection_bounds(&doc.0, state)?;
    for (handle, p) in handle_positions(bounds) {
        if view.doc_to_screen(p).distance(pointer) <= 8.0 {
            return Some(handle);
        }
    }
    None
}

pub(crate) fn top_item_at(
    doc: &ns_core::Document,
    state: &EditorState,
    p: Vec2,
) -> Option<Vec<usize>> {
    fn visit(
        layers: &[Layer],
        collapsed: &HashSet<LayerId>,
        p: Vec2,
        prefix: &[usize],
    ) -> Option<Vec<usize>> {
        for (index, layer) in layers.iter().enumerate().rev() {
            if !layer.visible {
                continue;
            }
            match &layer.kind {
                LayerKind::Shape(shape) if shape.source.contains(p) => {
                    let mut path = prefix.to_vec();
                    path.push(index);
                    return Some(path);
                }
                LayerKind::Group { children } => {
                    let mut path = prefix.to_vec();
                    path.push(index);
                    if collapsed.contains(&layer.id) {
                        if layer_contains(layer, p) {
                            return Some(path);
                        }
                    } else if let Some(found) = visit(children, collapsed, p, &path) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }
    visit(&doc.layers, &state.collapsed, p, &[])
}

fn layer_contains(layer: &Layer, p: Vec2) -> bool {
    if !layer.visible {
        return false;
    }
    match &layer.kind {
        LayerKind::Shape(shape) => shape.source.contains(p),
        LayerKind::Group { children } => children.iter().any(|child| layer_contains(child, p)),
        _ => false,
    }
}

fn collect_group_sources(layer: &Layer, sources: &mut Vec<(LayerId, ShapeSource)>) {
    match &layer.kind {
        LayerKind::Shape(shape) => {
            if matches!(
                &shape.source,
                ShapeSource::Path(_)
                    | ShapeSource::Primitive(_)
                    | ShapeSource::Text(_)
                    | ShapeSource::RasterRegion(_)
            ) {
                sources.push((layer.id, shape.source.clone()));
            }
        }
        LayerKind::Group { children } => {
            for child in children {
                collect_group_sources(child, sources);
            }
        }
        _ => {}
    }
}

/// Shape sources for every selected layer, with nested groups expanded and
/// duplicates removed. Scaling and rotation share this snapshot so a group and
/// one of its children never receive the same transform twice.
fn collect_selection_sources(
    doc: &DocumentRes,
    state: &EditorState,
) -> Vec<(LayerId, ShapeSource)> {
    let mut sources = Vec::new();
    for path in selected_layer_paths(state) {
        if let Some(layer) = doc.0.layer_by_path(path) {
            collect_group_sources(layer, &mut sources);
        }
    }
    let mut seen = HashSet::new();
    sources.retain(|(id, _)| seen.insert(*id));
    sources
}

fn start_group_move(state: &mut EditorState, doc: &DocumentRes, layer: &Layer, start: Vec2) {
    if layer.locked {
        state.status = "Layer is locked · unlock it to move".into();
        return;
    }
    let mut sources = Vec::new();
    collect_group_sources(layer, &mut sources);
    if sources.is_empty() {
        state.status = "This group has no translatable shape layers".into();
        return;
    }
    let excluded: Vec<LayerId> = sources.iter().map(|(id, _)| *id).collect();
    state.canvas_drag = Some(CanvasDrag::MoveGroup {
        start,
        sources,
        axis: None,
    });
    begin_snap_session(state, doc, excluded);
}

/// Capture the document's reference geometry once for a move gesture.
///
/// The moved layers (and their descendants through the source ids) are
/// excluded so the subject cannot snap to itself.
pub(super) fn begin_snap_session(
    state: &mut EditorState,
    doc: &DocumentRes,
    excluded: impl IntoIterator<Item = LayerId>,
) {
    state.active_guides.clear();
    if !state.smart_guides {
        state.snap_session = None;
        return;
    }
    let excluded: HashSet<LayerId> = excluded.into_iter().collect();
    if excluded.is_empty() {
        state.snap_session = None;
        return;
    }
    let targets = snapping::collect_targets(&doc.0, &excluded);
    state.snap_session = Some(SnapSession {
        targets,
        hints: Default::default(),
    });
}

/// Snap a raw rectangular subject and publish the guides for the overlay.
pub(super) fn snap_translation(
    state: &mut EditorState,
    view: &CanvasView,
    subject: Subject,
    enabled: bool,
) -> SnapResult {
    if !enabled || !state.smart_guides {
        if let Some(session) = state.snap_session.as_mut() {
            session.hints.reset();
        }
        state.active_guides.clear();
        return SnapResult::default();
    }
    let options = SnapOptions {
        tolerance: GUIDE_TOLERANCE_PX / view.scale(),
        kinds: SnapKinds::default(),
    };
    let result = match state.snap_session.as_mut() {
        Some(session) => snapping::snap(subject, &session.targets, &options, &mut session.hints),
        None => return SnapResult::default(),
    };
    state.active_guides = result.guides.clone();
    result
}

fn union_source_bounds(sources: &[(LayerId, ShapeSource)]) -> Option<Bounds> {
    sources
        .iter()
        .filter_map(|(_, source)| source.bounds())
        .reduce(|a, b| Bounds {
            min: a.min.min(b.min),
            max: a.max.max(b.max),
        })
}

fn apply_group_translation(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    sources: &[(LayerId, ShapeSource)],
    delta: Vec2,
) {
    let mut moved = false;
    for (layer_id, source) in sources {
        let Some(new_source) = source.transformed(Vec2::ONE, delta) else {
            continue;
        };
        let Some(layer) = doc.0.find_layer_mut(*layer_id) else {
            continue;
        };
        let LayerKind::Shape(shape) = &mut layer.kind else {
            continue;
        };
        shape.source = new_source;
        layer.touch();
        moved = true;
    }
    if moved {
        state.mark_dirty();
    }
}

/// Nudge every selected layer (and every shape inside a selected group) by
/// `delta` document pixels. Arrow keys drive this. `checkpoint` takes the undo
/// snapshot for the first press of a burst, so holding a key leaves one entry.
pub(crate) fn nudge_selection(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    delta: Vec2,
    checkpoint: bool,
) -> bool {
    if selection_locked(&doc.0, state) {
        state.status = "Layer is locked · unlock it to move".into();
        return false;
    }
    let sources = collect_selection_sources(doc, state);
    if sources.is_empty() {
        return false;
    }
    if checkpoint {
        state.checkpoint_document(&doc.0);
    }
    apply_group_translation(doc, state, &sources, delta);
    true
}

pub(crate) fn layer_bounds(layer: &Layer) -> Option<Bounds> {
    match &layer.kind {
        LayerKind::Shape(shape) => shape.source.bounds(),
        LayerKind::Group { children } => children
            .iter()
            .filter(|child| child.visible)
            .filter_map(layer_bounds)
            .reduce(|a, b| Bounds {
                min: a.min.min(b.min),
                max: a.max.max(b.max),
            }),
        _ => None,
    }
}

pub(crate) fn handle_positions(bounds: Bounds) -> [(ResizeHandle, Vec2); 8] {
    let c = bounds.center();
    [
        (ResizeHandle::N, Vec2::new(c.x, bounds.min.y)),
        (ResizeHandle::S, Vec2::new(c.x, bounds.max.y)),
        (ResizeHandle::W, Vec2::new(bounds.min.x, c.y)),
        (ResizeHandle::E, Vec2::new(bounds.max.x, c.y)),
        (ResizeHandle::Nw, bounds.min),
        (ResizeHandle::Ne, Vec2::new(bounds.max.x, bounds.min.y)),
        (ResizeHandle::Sw, Vec2::new(bounds.min.x, bounds.max.y)),
        (ResizeHandle::Se, bounds.max),
    ]
}

/// Round a drag to the nearest 45° ray. Horizontal and vertical moves keep the
/// other centre coordinate exactly fixed, which is what makes an Alt+Shift
/// duplicate line up perfectly instead of drifting with a smart guide.
fn constrained_delta(state: &mut EditorState, raw: Vec2) -> Vec2 {
    state.active_guides.clear();
    if let Some(session) = state.snap_session.as_mut() {
        session.hints.reset();
    }
    if raw.length_squared() < 1.0e-8 {
        return raw;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let angle = (raw.y.atan2(raw.x) / step).round() * step;
    let direction = Vec2::from_angle(angle);
    direction * raw.dot(direction)
}

/// Resolve a handle drag into one scale/offset transform. Applying the same
/// transform to every source keeps a group or multi-selection rigid: shared
/// bounds, relative spacing and proportions all scale together.
fn resize_transform(bounds: Bounds, handle: ResizeHandle, p: Vec2) -> Option<(Vec2, Vec2)> {
    let mut min = bounds.min;
    let mut max = bounds.max;
    if handle.moves_left() {
        min.x = p.x;
    }
    if handle.moves_right() {
        max.x = p.x;
    }
    if handle.moves_top() {
        min.y = p.y;
    }
    if handle.moves_bottom() {
        max.y = p.y;
    }

    let min_size = 4.0;
    if max.x - min.x < min_size {
        if handle.moves_left() {
            min.x = max.x - min_size;
        } else {
            max.x = min.x + min_size;
        }
    }
    if max.y - min.y < min_size {
        if handle.moves_top() {
            min.y = max.y - min_size;
        } else {
            max.y = min.y + min_size;
        }
    }

    let old_size = bounds.size();
    let new_size = max - min;
    if old_size.x.abs() < 1e-4 || old_size.y.abs() < 1e-4 {
        return None;
    }
    let scale = Vec2::new(new_size.x / old_size.x, new_size.y / old_size.y);
    let offset = min - bounds.min * scale;
    Some((scale, offset))
}

fn apply_source_transform(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    sources: &[(LayerId, ShapeSource)],
    scale: Vec2,
    offset: Vec2,
) {
    let mut transformed = false;
    for (layer_id, source) in sources {
        let Some(new_source) = source.transformed(scale, offset) else {
            continue;
        };
        let Some(layer) = doc.0.find_layer_mut(*layer_id) else {
            continue;
        };
        let LayerKind::Shape(shape) = &mut layer.kind else {
            continue;
        };
        shape.source = new_source;
        layer.touch();
        transformed = true;
    }
    if transformed {
        state.mark_dirty();
    }
}

pub(crate) fn make_created_layer(
    tool: CanvasTool,
    bounds: Bounds,
    rect_corner: f32,
) -> Option<Layer> {
    let size = bounds.size();
    if size.min_element() < 2.0 {
        return None;
    }
    let center = bounds.center();
    let primitive = match tool {
        CanvasTool::Rectangle => Primitive::Rect {
            center,
            size,
            corner_radius: size.min_element() * rect_corner.clamp(0.0, 0.5),
        },
        CanvasTool::Ellipse => Primitive::Ellipse {
            center,
            radii: size * 0.5,
        },
        CanvasTool::Star => Primitive::Star {
            center,
            inner_radius: size.min_element() * 0.22,
            outer_radius: size.min_element() * 0.5,
            points: 5,
            rotation: -std::f32::consts::FRAC_PI_2,
        },
        CanvasTool::Select
        | CanvasTool::Text
        | CanvasTool::Ruler
        | CanvasTool::LassoRect
        | CanvasTool::LassoEllipse
        | CanvasTool::LassoPolygon
        | CanvasTool::LassoFree
        | CanvasTool::LassoLine
        | CanvasTool::Polygon
        | CanvasTool::Cable
        | CanvasTool::Pen
        | CanvasTool::Node
        | CanvasTool::AddAnchor
        | CanvasTool::DeleteAnchor
        | CanvasTool::ConvertAnchor
        | CanvasTool::Paint => return None,
    };

    let mut layer = Layer::shape(tool.label(), ShapeSource::Primitive(primitive));
    if let LayerKind::Shape(shape) = &mut layer.kind {
        // Layer radius shares the 1–10 px range used by the Properties and
        // group controls. Depth equals the radius so a fresh shape keeps the
        // 45-degree edge it had before Depth started scaling the normal; the
        // Properties slider can raise or carve it from there.
        let radius = (size.min_element() * 0.18).clamp(1.0, 10.0);
        shape.radius_px = radius;
        shape.height_px = radius;
    }
    Some(layer)
}

/// Place a live text layer at the clicked point and hand the keyboard to the
/// Properties content field. The tool drops back to Select so the new block
/// can be moved immediately.
fn place_text(state: &mut EditorState, doc: &mut DocumentRes, p: Vec2) {
    let scale = (doc.0.size.x as f32 / 1024.0).max(0.25);
    let mut layer = Layer::shape(
        "Text",
        ShapeSource::Text(TextSource::new("Text", p, 96.0 * scale)),
    );
    if let LayerKind::Shape(shape) = &mut layer.kind {
        shape.radius_px = 6.0 * scale;
        shape.height_px = 10.0 * scale;
        shape.edge_aa_px = 0.4 * scale;
    }
    let path = add_layer(doc, state, layer);
    state.focus_text_content = true;
    state.text_edit_baseline = true;
    state.right_tab = RightTab::Properties;
    state.show_right_dock = true;
    activate_tool(state, CanvasTool::Select);
    state.selected = Some(path);
    state.status = "Text placed · type the content, then choose a font in Properties (T)".into();
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;

    fn rect_at(name: &str, x: f32) -> Layer {
        Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(x, 30.0),
                size: Vec2::splat(16.0),
                corner_radius: 2.0,
            }),
        )
    }

    fn rect_named(name: &str, center: Vec2, size: f32) -> Layer {
        Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size: Vec2::splat(size),
                corner_radius: 0.0,
            }),
        )
    }

    fn layer_centre(layer: &Layer) -> Option<Vec2> {
        match &layer.kind {
            LayerKind::Shape(shape) => shape.source.bounds().map(|bounds| bounds.center()),
            _ => None,
        }
    }

    fn centre(doc: &DocumentRes, index: usize) -> Option<Vec2> {
        let LayerKind::Shape(shape) = &doc.0.layers[index].kind else {
            return None;
        };
        shape.source.bounds().map(|bounds| bounds.center())
    }

    fn canvas_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(96.0, 96.0))
    }

    #[test]
    fn text_tool_places_a_live_text_layer() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        let mut state = EditorState {
            canvas_tool: CanvasTool::Text,
            ..Default::default()
        };
        let view = CanvasView::for_test(canvas_rect(), Vec2::splat(96.0));

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(24.0, 30.0), false);

        assert_eq!(doc.0.layers.len(), 1);
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("text tool must create a shape layer");
        };
        let ShapeSource::Text(text) = &shape.source else {
            panic!("text tool must create a live text source");
        };
        assert_eq!(text.anchor, Vec2::new(24.0, 30.0));
        assert_eq!(state.canvas_tool, CanvasTool::Select);
        assert_eq!(state.selected, Some(vec![0]));
        assert!(state.focus_text_content);
        assert!(state.text_edit_baseline);
    }

    #[test]
    fn alt_drag_on_a_shape_duplicates_and_moves_the_copy() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("a", 40.0));
        let mut state = EditorState::default();
        let view = CanvasView::for_test(canvas_rect(), Vec2::splat(96.0));

        // Press inside the rect (centre 40,30; size 16).
        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(40.0, 30.0), true);
        assert_eq!(doc.0.layers.len(), 2, "the drag duplicated the layer");
        assert_eq!(doc.0.layers[1].name, "a copy");
        assert!(matches!(
            state.canvas_drag,
            Some(CanvasDrag::MoveGroup { .. })
        ));

        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(52.0, 30.0), false);
        assert_eq!(
            centre(&doc, 0),
            Some(Vec2::new(40.0, 30.0)),
            "the original stays put"
        );
        assert_eq!(
            centre(&doc, 1),
            Some(Vec2::new(52.0, 30.0)),
            "the copy follows the pointer"
        );
        canvas_edit_stop(&mut state, &mut doc);
        assert!(state.canvas_drag.is_none());
    }

    #[test]
    fn alt_shift_duplicate_stays_exactly_on_the_axis() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("a", 30.0));
        let mut state = EditorState::default();
        let view = CanvasView::for_test(canvas_rect(), Vec2::splat(96.0));

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(30.0, 30.0), true);
        assert_eq!(doc.0.layers.len(), 2);
        // The pointer is 4 px below the horizontal centre line; Shift must
        // discard that component instead of letting a guide drift the copy.
        canvas_edit_drag_with_constraint(
            &mut state,
            &mut doc,
            &view,
            egui::pos2(80.0, 34.0),
            true,
            true,
        );

        assert_eq!(centre(&doc, 0), Some(Vec2::new(30.0, 30.0)));
        assert_eq!(centre(&doc, 1), Some(Vec2::new(80.0, 30.0)));
    }

    #[test]
    fn shift_duplicate_overrides_a_near_vertical_ruler_lock() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        doc.0
            .layers
            .push(rect_named("a", Vec2::new(50.0, 50.0), 16.0));
        let mut state = EditorState {
            ruler: Some(RulerState {
                origin: Vec2::ZERO,
                angle: 85.0_f32.to_radians(),
            }),
            ruler_locks_duplicates: true,
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(50.0, 50.0), true);
        assert!(matches!(
            state.canvas_drag,
            Some(CanvasDrag::MoveGroup { axis: Some(_), .. })
        ));
        canvas_edit_drag_with_constraint(
            &mut state,
            &mut doc,
            &view,
            egui::pos2(54.0, 120.0),
            true,
            true,
        );

        // Shift must win over the 85° ruler: the copy stays exactly on the
        // original's vertical centre line instead of drifting ~6 px right.
        assert!((centre(&doc, 0).unwrap() - Vec2::new(50.0, 50.0)).length() < 1.0e-4);
        assert!((centre(&doc, 1).unwrap() - Vec2::new(50.0, 120.0)).length() < 1.0e-4);
    }

    #[test]
    fn constrained_delta_snaps_to_45_degree_rays() {
        let mut state = EditorState::default();
        assert_eq!(
            constrained_delta(&mut state, Vec2::new(50.0, 4.0)),
            Vec2::new(50.0, 0.0)
        );
        let vertical = constrained_delta(&mut state, Vec2::new(4.0, 50.0));
        assert!(vertical.x.abs() < 1.0e-4);
        assert!((vertical.y - 50.0).abs() < 1.0e-4);
        let diagonal = constrained_delta(&mut state, Vec2::new(50.0, 48.0));
        assert!((diagonal.x - diagonal.y).abs() < 1.0e-4);
        assert!(diagonal.x > 45.0 && diagonal.y > 45.0);
        assert!(state.active_guides.is_empty());
    }

    #[test]
    fn alt_drag_copies_every_selected_layer() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("a", 20.0));
        doc.0.layers.push(rect_at("b", 50.0));
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };
        let view = CanvasView::for_test(canvas_rect(), Vec2::splat(96.0));
        // Pressing one member copies the whole set.
        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(50.0, 30.0), true);
        assert_eq!(doc.0.layers.len(), 4);
        let names: Vec<&str> = doc
            .0
            .layers
            .iter()
            .map(|layer| layer.name.as_str())
            .collect();
        assert_eq!(names, ["a", "a copy", "b", "b copy"]);

        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(60.0, 30.0), false);
        assert_eq!(centre(&doc, 0), Some(Vec2::new(20.0, 30.0)));
        assert_eq!(centre(&doc, 1), Some(Vec2::new(30.0, 30.0)));
        assert_eq!(centre(&doc, 2), Some(Vec2::new(50.0, 30.0)));
        assert_eq!(centre(&doc, 3), Some(Vec2::new(60.0, 30.0)));
    }

    #[test]
    fn node_tool_line_drag_falls_back_to_a_snapped_move() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("a", 40.0));
        let mut state = EditorState {
            canvas_tool: CanvasTool::Node,
            ..Default::default()
        };
        let view = CanvasView::for_test(canvas_rect(), Vec2::splat(96.0));

        canvas_edit_start_for_node_tool(&mut state, &mut doc, &view, egui::pos2(40.0, 30.0), false);
        assert!(matches!(state.canvas_drag, Some(CanvasDrag::Move { .. })));
        assert!(state.snap_session.is_some(), "the line move must snap");
    }

    #[test]
    fn alt_drag_on_empty_canvas_duplicates_nothing() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("a", 40.0));
        let mut state = EditorState::default();
        let view = CanvasView::for_test(canvas_rect(), Vec2::splat(96.0));
        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(4.0, 80.0), true);
        assert_eq!(doc.0.layers.len(), 1, "nothing was under the pointer");
        assert!(state.canvas_drag.is_none());
    }

    #[test]
    fn duplicate_drag_follows_a_locked_ruler_angle() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(400)));
        doc.0.layers.push(Layer::shape(
            "a",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(100.0),
                size: Vec2::splat(40.0),
                corner_radius: 0.0,
            }),
        ));
        let mut state = EditorState {
            ruler: Some(RulerState {
                origin: Vec2::ZERO,
                angle: std::f32::consts::FRAC_PI_4,
            }),
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0)),
            Vec2::splat(400.0),
        );

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(100.0, 100.0), true);
        assert!(matches!(
            state.canvas_drag,
            Some(CanvasDrag::MoveGroup { axis: Some(_), .. })
        ));
        // Raw drag is 40 px to the right; the 45° lock keeps only its
        // along-line component, so the copy moves 20 px right and 20 px down.
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(140.0, 100.0), true);
        assert!((centre(&doc, 0).unwrap() - Vec2::splat(100.0)).length() < 1e-3);
        assert!((centre(&doc, 1).unwrap() - Vec2::new(120.0, 120.0)).length() < 1e-3);
        assert!(state.active_guides.is_empty());
    }

    #[test]
    fn duplicate_drag_is_free_when_the_ruler_lock_is_off() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(400)));
        doc.0.layers.push(Layer::shape(
            "a",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(100.0),
                size: Vec2::splat(40.0),
                corner_radius: 0.0,
            }),
        ));
        let mut state = EditorState {
            ruler: Some(RulerState {
                origin: Vec2::ZERO,
                angle: std::f32::consts::FRAC_PI_4,
            }),
            ruler_locks_duplicates: false,
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0)),
            Vec2::splat(400.0),
        );

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(100.0, 100.0), true);
        assert!(matches!(
            state.canvas_drag,
            Some(CanvasDrag::MoveGroup { axis: None, .. })
        ));
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(140.0, 100.0), true);
        assert!((centre(&doc, 1).unwrap() - Vec2::new(140.0, 100.0)).length() < 1e-3);
    }

    #[test]
    fn ctrl_drag_bypasses_the_ruler_lock() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(400)));
        doc.0.layers.push(Layer::shape(
            "a",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(100.0),
                size: Vec2::splat(40.0),
                corner_radius: 0.0,
            }),
        ));
        let mut state = EditorState {
            ruler: Some(RulerState {
                origin: Vec2::ZERO,
                angle: std::f32::consts::FRAC_PI_4,
            }),
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0)),
            Vec2::splat(400.0),
        );

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(100.0, 100.0), true);
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(140.0, 100.0), false);
        assert!((centre(&doc, 1).unwrap() - Vec2::new(140.0, 100.0)).length() < 1e-3);
        assert!(state.active_guides.is_empty());
    }

    #[test]
    fn dragging_a_member_moves_the_whole_selection() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(400)));
        for (name, x) in [("a", 50.0), ("b", 150.0)] {
            doc.0.layers.push(Layer::shape(
                name,
                ShapeSource::Primitive(Primitive::Rect {
                    center: Vec2::new(x, 50.0),
                    size: Vec2::splat(40.0),
                    corner_radius: 0.0,
                }),
            ));
        }
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0)),
            Vec2::splat(400.0),
        );

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(50.0, 50.0), false);
        assert!(matches!(
            state.canvas_drag,
            Some(CanvasDrag::MoveGroup { .. })
        ));
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(60.0, 50.0), true);
        assert_eq!(centre(&doc, 0), Some(Vec2::new(60.0, 50.0)));
        assert_eq!(centre(&doc, 1), Some(Vec2::new(160.0, 50.0)));
        assert_eq!(state.selected, Some(vec![0]));
        assert_eq!(state.multi_selected, vec![vec![1]]);
    }

    #[test]
    fn alt_drag_snaps_the_copy_to_a_neighbour() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(400)));
        doc.0.layers.push(rect_at("a", 100.0));
        // Square corners keep the target edge exact; the copy starts 5 px
        // left of this shape's left edge.
        doc.0.layers.push(Layer::shape(
            "b",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(300.0, 200.0),
                size: Vec2::splat(16.0),
                corner_radius: 0.0,
            }),
        ));
        let mut state = EditorState::default();
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0)),
            Vec2::splat(400.0),
        );

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(100.0, 30.0), true);
        assert!(state.snap_session.is_some());
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(279.0, 30.0), true);
        assert_eq!(centre(&doc, 0), Some(Vec2::new(100.0, 30.0)));
        assert_eq!(centre(&doc, 1), Some(Vec2::new(284.0, 30.0)));
        assert!(!state.active_guides.is_empty(), "a guide must be visible");

        canvas_edit_stop(&mut state, &mut doc);
        assert!(state.snap_session.is_none());
        assert!(state.active_guides.is_empty());
    }

    #[test]
    fn alt_drag_does_not_snap_back_to_the_original() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(400)));
        doc.0.layers.push(rect_at("a", 100.0));
        let mut state = EditorState::default();
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0)),
            Vec2::splat(400.0),
        );

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(100.0, 30.0), true);
        // Six pixels is inside the guide tolerance; the original must still
        // not pull its own copy back to the start position.
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(106.0, 30.0), true);
        let centre = centre(&doc, 1).expect("the copy exists");
        assert!((centre - Vec2::new(106.0, 30.0)).length() < 1e-3);
    }

    #[test]
    fn ctrl_drag_skips_the_snap_and_the_guides() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(400)));
        doc.0.layers.push(rect_at("a", 100.0));
        doc.0.layers.push(rect_at("b", 300.0));
        let mut state = EditorState::default();
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0)),
            Vec2::splat(400.0),
        );

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(100.0, 30.0), true);
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(279.0, 30.0), false);
        assert_eq!(centre(&doc, 1), Some(Vec2::new(279.0, 30.0)));
        assert!(state.active_guides.is_empty());
    }

    #[test]
    fn nudging_moves_every_selected_layer() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("a", 20.0));
        doc.0.layers.push(rect_at("b", 50.0));
        doc.0.layers.push(rect_at("c", 80.0));
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![2]],
            ..Default::default()
        };

        assert!(nudge_selection(
            &mut state,
            &mut doc,
            Vec2::new(1.0, 2.0),
            true
        ));
        assert_eq!(centre(&doc, 0), Some(Vec2::new(21.0, 32.0)));
        assert_eq!(centre(&doc, 1), Some(Vec2::new(50.0, 30.0)), "not selected");
        assert_eq!(centre(&doc, 2), Some(Vec2::new(81.0, 32.0)));
    }

    #[test]
    fn a_nudge_burst_undoes_in_one_step() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("a", 20.0));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        // The first press of a burst takes the checkpoint, the repeats reuse it.
        assert!(nudge_selection(
            &mut state,
            &mut doc,
            Vec2::new(10.0, 0.0),
            true
        ));
        assert!(nudge_selection(
            &mut state,
            &mut doc,
            Vec2::new(10.0, 0.0),
            false
        ));
        assert_eq!(centre(&doc, 0), Some(Vec2::new(40.0, 30.0)));
        assert!(state.undo(&mut doc));
        assert_eq!(
            centre(&doc, 0),
            Some(Vec2::new(20.0, 30.0)),
            "the whole burst is one undo step"
        );
    }

    #[test]
    fn nudging_a_group_and_its_child_moves_the_child_once() {
        let child = Layer::shape(
            "child",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(40.0, 40.0),
                size: Vec2::splat(10.0),
                corner_radius: 1.0,
            }),
        );
        let group = Layer::group("group", vec![child]);
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(group);
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![0, 0]],
            ..Default::default()
        };
        assert!(nudge_selection(
            &mut state,
            &mut doc,
            Vec2::new(5.0, 0.0),
            true
        ));
        let LayerKind::Group { children } = &doc.0.layers[0].kind else {
            panic!("still a group")
        };
        let LayerKind::Shape(shape) = &children[0].kind else {
            panic!("still a shape")
        };
        let bounds = shape.source.bounds().expect("source bounds");
        assert_eq!(bounds.center(), Vec2::new(45.0, 40.0), "moved exactly once");
    }

    #[test]
    fn nudging_nothing_is_a_no_op() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(Layer::flat("flat"));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        assert!(!nudge_selection(&mut state, &mut doc, Vec2::ONE, true));
    }

    #[test]
    fn rotating_a_primitive_keeps_its_center_and_becomes_a_path() {
        let source = ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::new(20.0, 30.0),
            size: Vec2::new(16.0, 8.0),
            corner_radius: 2.0,
        });
        let center = source.bounds().unwrap().center();
        let transform = Affine2::from_translation(center)
            * Affine2::from_angle(std::f32::consts::FRAC_PI_4)
            * Affine2::from_translation(-center);
        let rotated = rotated_source(&source, transform).expect("primitive should rotate");
        assert!(matches!(rotated, ShapeSource::Path(_)));
        let after = rotated.bounds().unwrap();
        assert!((after.center() - center).length() < 0.01);
    }

    #[test]
    fn mask_brush_paints_black_and_white() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(32)));
        let mut layer = Layer::flat("masked");
        layer.mask = Some(Grid::new(32, 32, 1.0));
        let id = layer.id;
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            mask_edit_layer: Some(id),
            brush_radius: 8.0,
            brush_hardness: 1.0,
            brush_flow: 1.0,
            brush_mode: PaintMode::Lower,
            ..Default::default()
        };

        mask_dab(&mut doc, &mut state, id, Vec2::splat(16.0));
        let mask = doc.0.layers[0].mask.as_ref().unwrap();
        assert!(*mask.get(16, 16) < 0.01);
        // The canvas preview keys off mask_revision; a content-only touch would
        // leave the mask texture stale while painting.
        assert_eq!(doc.0.layers[0].mask_revision, 1);

        state.brush_mode = PaintMode::Raise;
        mask_dab(&mut doc, &mut state, id, Vec2::splat(16.0));
        let mask = doc.0.layers[0].mask.as_ref().unwrap();
        assert!(*mask.get(16, 16) > 0.99);
        assert_eq!(doc.0.layers[0].mask_revision, 2);
        assert!(state.edit_serial > 0);
    }

    #[test]
    fn mask_shapes_add_white_and_cut_with_lower() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(32)));
        let mut layer = Layer::flat("masked");
        layer.mask = Some(Grid::new(32, 32, 0.0));
        let id = layer.id;
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            mask_edit_layer: Some(id),
            mask_white: true,
            ..Default::default()
        };
        let source = ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::splat(16.0),
            size: Vec2::splat(12.0),
            corner_radius: 2.0,
        });

        stamp_mask_shape(&mut doc, &mut state, source.clone());
        let mask = doc.0.layers[0].mask.as_ref().unwrap();
        assert!(*mask.get(16, 16) > 0.99);
        assert!(*mask.get(1, 1) < 0.01);
        assert_eq!(doc.0.layers[0].mask_revision, 1);

        state.mask_white = false;
        stamp_mask_shape(&mut doc, &mut state, source);
        let mask = doc.0.layers[0].mask.as_ref().unwrap();
        assert!(*mask.get(16, 16) < 0.01);
        assert_eq!(doc.0.layers[0].mask_revision, 2);
    }

    #[test]
    fn mask_strokes_respect_opacity_and_mask_resolution() {
        // A half-resolution mask: document pixel 32 maps to mask pixel 16.
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(64)));
        let mut layer = Layer::flat("masked");
        layer.mask = Some(Grid::new(32, 32, 1.0));
        let id = layer.id;
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            mask_edit_layer: Some(id),
            canvas_tool: CanvasTool::Paint,
            brush_radius: 8.0,
            brush_hardness: 0.9,
            brush_flow: 0.5,
            brush_opacity: 0.5,
            brush_mode: PaintMode::Lower,
            ..Default::default()
        };
        start_mask_paint(&mut state, &mut doc, Vec2::splat(32.0));
        for _ in 0..20 {
            mask_segment(
                &mut doc,
                &mut state,
                id,
                Vec2::splat(32.0),
                Vec2::new(33.0, 32.0),
            );
        }
        canvas_edit_stop(&mut state, &mut doc);
        assert!(state.mask_stroke.is_none());
        let mask = doc.0.layers[0].mask.as_ref().unwrap();
        assert!(
            (mask.get(16, 16) - 0.5).abs() < 0.02,
            "{}",
            mask.get(16, 16)
        );
        assert_eq!(*mask.get(2, 2), 1.0);

        // A second stroke builds further, as in other paint tools.
        start_mask_paint(&mut state, &mut doc, Vec2::splat(32.0));
        for _ in 0..20 {
            mask_segment(
                &mut doc,
                &mut state,
                id,
                Vec2::splat(32.0),
                Vec2::new(33.0, 32.0),
            );
        }
        canvas_edit_stop(&mut state, &mut doc);
        let mask = doc.0.layers[0].mask.as_ref().unwrap();
        assert!(
            (mask.get(16, 16) - 0.25).abs() < 0.02,
            "{}",
            mask.get(16, 16)
        );
    }

    #[test]
    fn group_handles_scale_every_child_around_the_group_box() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        doc.0.layers.push(Layer::group(
            "group",
            vec![
                rect_named("a", Vec2::new(50.0, 50.0), 20.0),
                rect_named("b", Vec2::new(150.0, 150.0), 20.0),
            ],
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );

        let bounds = selection_bounds(&doc.0, &state).expect("group bounds");
        assert_eq!(bounds.min, Vec2::new(40.0, 40.0));
        assert_eq!(bounds.max, Vec2::new(160.0, 160.0));
        let handle = view.doc_to_screen(Vec2::new(160.0, 160.0));
        assert_eq!(
            selected_handle_hit(&doc, &state, &view, handle),
            Some(ResizeHandle::Se)
        );

        canvas_edit_start(&mut state, &mut doc, &view, handle, false);
        assert!(matches!(state.canvas_drag, Some(CanvasDrag::Resize { .. })));
        canvas_edit_drag(
            &mut state,
            &mut doc,
            &view,
            view.doc_to_screen(Vec2::new(200.0, 200.0)),
            false,
        );

        let children = doc.0.layers[0].children().expect("group children");
        assert!(
            (layer_centre(&children[0]).unwrap() - Vec2::new(53.3333, 53.3333)).length() < 0.01
        );
        assert!(
            (layer_centre(&children[1]).unwrap() - Vec2::new(186.6667, 186.6667)).length() < 0.01
        );
    }

    #[test]
    fn multi_selection_handles_use_the_union_box() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        doc.0
            .layers
            .push(rect_named("a", Vec2::new(50.0, 50.0), 20.0));
        doc.0
            .layers
            .push(rect_named("b", Vec2::new(150.0, 50.0), 20.0));
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );

        let bounds = selection_bounds(&doc.0, &state).expect("multi bounds");
        assert_eq!(bounds.min, Vec2::new(40.0, 40.0));
        assert_eq!(bounds.max, Vec2::new(160.0, 60.0));
        let handle = view.doc_to_screen(Vec2::new(160.0, 60.0));
        assert_eq!(
            selected_handle_hit(&doc, &state, &view, handle),
            Some(ResizeHandle::Se)
        );

        canvas_edit_start(&mut state, &mut doc, &view, handle, false);
        canvas_edit_drag(
            &mut state,
            &mut doc,
            &view,
            view.doc_to_screen(Vec2::new(200.0, 80.0)),
            false,
        );

        assert!(
            (layer_centre(&doc.0.layers[0]).unwrap() - Vec2::new(53.3333, 60.0)).length() < 0.01
        );
        assert!(
            (layer_centre(&doc.0.layers[1]).unwrap() - Vec2::new(186.6667, 60.0)).length() < 0.01
        );
        assert_eq!(state.selected, Some(vec![0]));
        assert_eq!(state.multi_selected, vec![vec![1]]);
    }

    #[test]
    fn multi_selection_rotation_handle_rotates_every_selected_shape() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        doc.0
            .layers
            .push(rect_named("a", Vec2::new(50.0, 100.0), 20.0));
        doc.0
            .layers
            .push(rect_named("b", Vec2::new(150.0, 100.0), 20.0));
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );
        let bounds = selection_bounds(&doc.0, &state).expect("multi bounds");
        let handle = rotation_handle_pos(&view, bounds);

        canvas_edit_start(&mut state, &mut doc, &view, handle, false);
        assert!(matches!(state.canvas_drag, Some(CanvasDrag::Rotate { .. })));
        canvas_edit_drag(
            &mut state,
            &mut doc,
            &view,
            view.doc_to_screen(Vec2::new(200.0, 100.0)),
            false,
        );

        assert!((layer_centre(&doc.0.layers[0]).unwrap() - Vec2::new(100.0, 50.0)).length() < 0.01);
        assert!(
            (layer_centre(&doc.0.layers[1]).unwrap() - Vec2::new(100.0, 150.0)).length() < 0.01
        );
    }

    #[test]
    fn group_rotation_handle_rotates_nested_shapes_around_the_group_centre() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        doc.0.layers.push(Layer::group(
            "group",
            vec![
                rect_named("a", Vec2::new(50.0, 50.0), 20.0),
                rect_named("b", Vec2::new(150.0, 150.0), 20.0),
            ],
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );
        let bounds = selection_bounds(&doc.0, &state).expect("group bounds");
        let handle = rotation_handle_pos(&view, bounds);

        canvas_edit_start(&mut state, &mut doc, &view, handle, false);
        assert!(matches!(state.canvas_drag, Some(CanvasDrag::Rotate { .. })));
        canvas_edit_drag(
            &mut state,
            &mut doc,
            &view,
            view.doc_to_screen(Vec2::new(200.0, 100.0)),
            false,
        );

        let children = doc.0.layers[0].children().expect("group children");
        assert!((layer_centre(&children[0]).unwrap() - Vec2::new(150.0, 50.0)).length() < 0.01);
        assert!((layer_centre(&children[1]).unwrap() - Vec2::new(50.0, 150.0)).length() < 0.01);
    }

    #[test]
    fn painting_updates_a_height_paint_layer() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(32)));
        let layer = Layer::height_paint("paint", Grid::new(32, 32, 0.0), 12.0);
        let id = layer.id;
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            brush_radius: 8.0,
            brush_hardness: 1.0,
            brush_flow: 1.0,
            ..Default::default()
        };
        paint_dab(&mut doc, &mut state, id, Vec2::splat(16.0));
        let LayerKind::HeightPaint { height, .. } = &doc.0.layers[0].kind else {
            panic!("expected a paint layer");
        };
        assert!(*height.get(16, 16) > 0.0);
        assert!(state.edit_serial > 0);
    }

    #[test]
    fn a_normal_brush_creates_and_paints_a_normal_paint_layer() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(32)));
        let tip = ns_core::brush::BrushTip::from_normal(
            "bevel",
            Grid::new(4, 4, glam::Vec3::new(0.4, 0.0, 0.9)),
            None,
        );
        let mut state = EditorState {
            brush_radius: 8.0,
            brush_flow: 1.0,
            ..Default::default()
        };
        state.brush_library.push(tip);
        paint_click(&mut state, &mut doc, Vec2::splat(16.0));
        assert_eq!(doc.0.layers.len(), 1);
        let LayerKind::NormalPaint { normal } = &doc.0.layers[0].kind else {
            panic!("expected a normal paint layer");
        };
        assert!(*normal.get(16, 16) != glam::Vec3::Z);
    }

    #[test]
    fn mirror_symmetry_paints_both_sides() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(32)));
        let layer = Layer::height_paint("paint", Grid::new(32, 32, 0.0), 12.0);
        let id = layer.id;
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            brush_radius: 6.0,
            brush_hardness: 1.0,
            brush_flow: 1.0,
            symmetry: ns_core::brush::Symmetry::MirrorX,
            symmetry_pivot: Vec2::splat(0.5),
            ..Default::default()
        };
        paint_dab(&mut doc, &mut state, id, Vec2::new(8.0, 16.0));
        let LayerKind::HeightPaint { height, .. } = &doc.0.layers[0].kind else {
            panic!("expected a paint layer");
        };
        assert!(*height.get(8, 16) > 0.0, "original side painted");
        assert!(*height.get(24, 16) > 0.0, "mirrored side painted");
        assert_eq!(*height.get(16, 2), 0.0);
    }

    #[test]
    fn new_rectangles_take_the_chosen_corner_rounding() {
        let bounds = Bounds {
            min: Vec2::new(10.0, 20.0),
            max: Vec2::new(110.0, 60.0),
        };
        // The short side is 40 px, so the pill limit is 20 px.
        for (fraction, expected) in [(0.0, 0.0), (0.12, 4.8), (0.5, 20.0)] {
            let layer = make_created_layer(CanvasTool::Rectangle, bounds, fraction)
                .expect("a 100 x 40 rectangle is large enough");
            let LayerKind::Shape(shape) = &layer.kind else {
                panic!("expected a shape");
            };
            let ShapeSource::Primitive(Primitive::Rect { corner_radius, .. }) = &shape.source
            else {
                panic!("expected a rectangle primitive");
            };
            assert!(
                (corner_radius - expected).abs() < 1e-4,
                "fraction {fraction} gave {corner_radius}"
            );
        }
    }

    #[test]
    fn a_baked_shape_can_still_be_dragged_on_the_canvas() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("baked", 40.0));
        let LayerKind::Shape(shape) = &mut doc.0.layers[0].kind else {
            panic!("expected a shape");
        };
        shape.bake_to_raster(UVec2::splat(96)).expect("shape bakes");
        let mut state = EditorState::default();
        let view = CanvasView::for_test(canvas_rect(), Vec2::splat(96.0));

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(40.0, 30.0), false);
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(52.0, 30.0), false);

        assert_eq!(
            centre(&doc, 0),
            Some(Vec2::new(52.0, 30.0)),
            "a baked layer must move like the shape it replaced"
        );
    }

    #[test]
    fn baked_shapes_move_and_nudge_in_a_multi_selection() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(96)));
        doc.0.layers.push(rect_at("a", 30.0));
        doc.0.layers.push(rect_at("b", 62.0));
        for layer in &mut doc.0.layers {
            let LayerKind::Shape(shape) = &mut layer.kind else {
                panic!("expected a shape");
            };
            shape.bake_to_raster(UVec2::splat(96)).expect("shape bakes");
        }
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            smart_guides: false,
            ..Default::default()
        };
        let view = CanvasView::for_test(canvas_rect(), Vec2::splat(96.0));

        canvas_edit_start(&mut state, &mut doc, &view, egui::pos2(34.0, 30.0), false);
        assert!(matches!(
            state.canvas_drag,
            Some(CanvasDrag::MoveGroup { .. })
        ));
        canvas_edit_drag(&mut state, &mut doc, &view, egui::pos2(44.0, 30.0), false);
        assert_eq!(centre(&doc, 0), Some(Vec2::new(40.0, 30.0)));
        assert_eq!(centre(&doc, 1), Some(Vec2::new(72.0, 30.0)));

        assert!(nudge_selection(
            &mut state,
            &mut doc,
            Vec2::new(4.0, -2.0),
            true
        ));
        assert_eq!(centre(&doc, 0), Some(Vec2::new(44.0, 28.0)));
        assert_eq!(centre(&doc, 1), Some(Vec2::new(76.0, 28.0)));
    }

    #[test]
    fn creating_a_shape_keeps_the_drawing_tool_active() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        let mut state = EditorState {
            canvas_tool: CanvasTool::Rectangle,
            canvas_drag: Some(CanvasDrag::Create {
                tool: CanvasTool::Rectangle,
                start: Vec2::new(20.0, 20.0),
                current: Vec2::new(80.0, 60.0),
            }),
            ..Default::default()
        };
        canvas_edit_stop(&mut state, &mut doc);
        assert_eq!(doc.0.layers.len(), 1, "the rectangle is created");
        assert_eq!(
            state.canvas_tool,
            CanvasTool::Rectangle,
            "the tool must stay active until Esc"
        );
    }
}
