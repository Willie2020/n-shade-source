//! Existing-path node editing: anchors, handles, and node types.

use super::*;

use crate::snapping::Subject;

const NODE_HIT_RADIUS: f32 = 8.0;
const HANDLE_HIT_RADIUS: f32 = 7.0;
const HANDLE_MIN_LENGTH: f32 = 0.5;

pub(crate) const NODE_HINT: &str = "Node: drag anchors/handles (smart guides snap) · arrows nudge (Shift 10 px, Alt/Ctrl 0.1 px) · Shift-click multi-select · Alt-drag to pull handles · double-click a segment to add · click a segment for its edge bevel · Del to delete";

pub(crate) fn node_edit_input(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
) {
    let additive =
        ui.input(|input| input.modifiers.shift || input.modifiers.ctrl || input.modifiers.command);
    let alt = ui.input(|input| input.modifiers.alt);
    let snap_enabled = ui.input(|input| !(input.modifiers.ctrl || input.modifiers.command));
    let constrain = ui.input(|input| input.modifiers.shift);
    let tool = state.canvas_tool;

    // A locked layer keeps its outline read-only; the nodes are not even hovered.
    if selection_locked(&doc.0, state) {
        state.node_hover = None;
        state.node_selection.clear();
        return;
    }

    if let Some(pointer) = response.hover_pos() {
        node_hover(state, doc, view, pointer);
    } else {
        state.node_hover = None;
    }

    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
            node_drag_start(state, doc, view, origin, additive, alt);
        }
    }
    if response.dragged_by(egui::PointerButton::Primary) && state.canvas_drag.is_some() {
        if let Some(pointer) = response.interact_pointer_pos() {
            if matches!(state.canvas_drag, Some(CanvasDrag::NodeEdit { .. })) {
                node_drag_update(state, doc, view, pointer, alt, snap_enabled);
            } else {
                // Dragging a line/segment rather than an anchor falls back to
                // the normal layer move, which already has smart guides.
                canvas_edit_drag_with_constraint(
                    state,
                    doc,
                    view,
                    pointer,
                    snap_enabled,
                    constrain,
                );
            }
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        canvas_edit_stop(state, doc);
    }
    if response.double_clicked() {
        if tool == CanvasTool::Node {
            if let Some(pointer) = response.interact_pointer_pos() {
                insert_node_at(state, doc, view, pointer);
            }
        }
        return;
    }
    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            match tool {
                CanvasTool::AddAnchor => insert_node_at(state, doc, view, pointer),
                CanvasTool::DeleteAnchor => delete_anchor_at(state, doc, view, pointer),
                CanvasTool::ConvertAnchor => convert_anchor_at(state, doc, view, pointer),
                _ => node_click(state, doc, view, pointer, additive),
            }
        }
    }
    let focus_ok = !ui.ctx().text_edit_focused()
        && (!ui.ctx().memory(|memory| memory.focused().is_some()) || state.canvas_has_focus);
    if tool == CanvasTool::Node
        && focus_ok
        && ui.input(|input| {
            input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
        })
    {
        delete_selected_nodes(doc, state);
    }
}

fn delete_anchor_at(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
) {
    let Some(node) = selected_path(&doc.0, state).and_then(|path| node_at(path, view, pointer))
    else {
        return;
    };
    state.node_selection.clear();
    state.node_selection.push(node);
    delete_selected_nodes(doc, state);
}

fn convert_anchor_at(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
) {
    let Some(node_ref) = selected_path(&doc.0, state).and_then(|path| node_at(path, view, pointer))
    else {
        return;
    };
    let Some(path) = selected_path(&doc.0, state) else {
        return;
    };
    let current = selected_node_kind(path, &node_ref).unwrap_or(NodeKind::Corner);
    let neighbors = neighbor_positions(path, node_ref);
    let target = if current == NodeKind::Corner {
        NodeKind::Smooth
    } else {
        NodeKind::Corner
    };
    modify_selected_path(doc, state, |path| {
        let Some(node) = node_mut(path, node_ref) else {
            return;
        };
        match target {
            NodeKind::Corner => {
                node.kind = NodeKind::Corner;
                node.handle_in = Vec2::ZERO;
                node.handle_out = Vec2::ZERO;
            }
            NodeKind::Smooth => {
                if node.handle_in.length_squared() < 1e-3 && node.handle_out.length_squared() < 1e-3
                {
                    if let Some((previous, next)) = neighbors {
                        let tangent = next - previous;
                        let length = tangent.length() * 0.15;
                        if let Some(direction) = tangent.try_normalize() {
                            node.handle_out = direction * length;
                            node.handle_in = -direction * length;
                        }
                    }
                }
                apply_node_kind(node, NodeKind::Smooth);
            }
            NodeKind::Symmetric => apply_node_kind(node, NodeKind::Symmetric),
        }
    });
}

fn neighbor_positions(path: &Path, node_ref: NodeRef) -> Option<(Vec2, Vec2)> {
    let subpath = path.subpaths.get(node_ref.subpath)?;
    let count = subpath.nodes.len();
    if count < 2 {
        return None;
    }
    let previous = if node_ref.node == 0 {
        if subpath.closed {
            count - 1
        } else {
            1
        }
    } else {
        node_ref.node - 1
    };
    let next = if node_ref.node + 1 >= count {
        if subpath.closed {
            0
        } else {
            node_ref.node.saturating_sub(1)
        }
    } else {
        node_ref.node + 1
    };
    Some((subpath.nodes[previous].pos, subpath.nodes[next].pos))
}

fn modify_selected_path(
    doc: &mut DocumentRes,
    state: &mut EditorState,
    edit: impl FnOnce(&mut Path),
) {
    let Some(selected) = state.selected.clone() else {
        return;
    };
    let Some(layer) = doc.0.layer_by_path_mut(&selected) else {
        return;
    };
    let LayerKind::Shape(shape) = &mut layer.kind else {
        return;
    };
    let ShapeSource::Path(path) = &mut shape.source else {
        return;
    };
    edit(path);
    layer.touch();
    state.mark_dirty();
}

fn node_hover(state: &mut EditorState, doc: &DocumentRes, view: &CanvasView, pointer: egui::Pos2) {
    state.node_hover = selected_path(&doc.0, state).and_then(|path| {
        node_handle_hit(path, state, view, pointer)
            .map(|(node, _)| node)
            .or_else(|| node_at(path, view, pointer))
    });
}

fn node_drag_start(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    additive: bool,
    alt: bool,
) {
    let Some(layer) = selected_layer(doc, state) else {
        node_click(state, doc, view, pointer, additive);
        return;
    };
    let layer_id = layer.id;
    let Some(path) = layer_path(layer) else {
        return;
    };

    if let Some((node, kind)) = node_handle_hit(path, state, view, pointer) {
        let original = path.clone();
        state.canvas_drag = Some(CanvasDrag::NodeEdit {
            layer_id,
            original,
            anchor: view.screen_to_doc(pointer),
            nodes: vec![node],
            kind,
        });
        // Handle tips align to the same guides as anchors do.
        begin_snap_session(state, doc, [layer_id]);
        return;
    }

    let Some(node) = node_at(path, view, pointer) else {
        // A line/segment press has no anchor. In the Node tool, fall back to a
        // normal snapped shape move instead of doing nothing.
        if state.canvas_tool == CanvasTool::Node {
            canvas_edit_start_for_node_tool(state, doc, view, pointer, alt);
        }
        return;
    };
    if additive {
        if !state.node_selection.contains(&node) {
            state.node_selection.push(node);
        }
    } else if !state.node_selection.contains(&node) {
        state.node_selection.clear();
        state.node_selection.push(node);
    }

    let original = path.clone();
    let kind = if alt {
        NodeDragKind::PullHandles
    } else {
        NodeDragKind::Move
    };
    let nodes = if alt {
        vec![node]
    } else {
        state.node_selection.clone()
    };
    state.canvas_drag = Some(CanvasDrag::NodeEdit {
        layer_id,
        original,
        anchor: view.screen_to_doc(pointer),
        nodes,
        kind,
    });
    if kind == NodeDragKind::Move {
        begin_snap_session(state, doc, [layer_id]);
    } else {
        state.clear_snap_guides();
    }
}

fn insert_node_at(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
) {
    let Some((layer_id, mut path)) = selected_layer(doc, state)
        .and_then(|layer| layer_path(layer).map(|path| (layer.id, path.clone())))
    else {
        return;
    };
    if node_at(&path, view, pointer).is_some() {
        return;
    }
    let Some((subpath_index, segment, t, point)) = nearest_segment(&path, view, pointer) else {
        return;
    };

    let Some(subpath) = path.subpaths.get_mut(subpath_index) else {
        return;
    };
    let count = subpath.nodes.len();
    if count < 2 {
        return;
    }
    let next = (segment + 1) % count;
    let a = subpath.nodes[segment].clone();
    let b = subpath.nodes[next].clone();
    // The new node splits one edge in two: both halves keep the edge's
    // profile so adding an anchor never changes the shading.
    let edge_bevel = subpath.nodes[segment].bevel.clone();
    let p0 = a.pos;
    let p1 = a.pos + a.handle_out;
    let p2 = b.pos + b.handle_in;
    let p3 = b.pos;
    let p01 = lerp(p0, p1, t);
    let p12 = lerp(p1, p2, t);
    let p23 = lerp(p2, p3, t);
    let p012 = lerp(p01, p12, t);
    let p123 = lerp(p12, p23, t);

    subpath.nodes[segment].handle_out = p01 - a.pos;
    subpath.nodes[next].handle_in = p23 - b.pos;
    subpath.nodes.insert(
        segment + 1,
        Node {
            pos: point,
            handle_in: p012 - point,
            handle_out: p123 - point,
            kind: NodeKind::Smooth,
            bevel: edge_bevel,
        },
    );
    state.node_selection.clear();
    state.node_selection.push(NodeRef {
        subpath: subpath_index,
        node: segment + 1,
    });
    apply_shape_source(doc, state, layer_id, ShapeSource::Path(path));
}

fn nearest_segment(
    path: &Path,
    view: &CanvasView,
    pointer: egui::Pos2,
) -> Option<(usize, usize, f32, Vec2)> {
    let mut best: Option<(usize, usize, f32, Vec2, f32)> = None;
    for (subpath_index, subpath) in path.subpaths.iter().enumerate() {
        let count = subpath.nodes.len();
        if count < 2 {
            continue;
        }
        let segments = if subpath.closed { count } else { count - 1 };
        for segment in 0..segments {
            let a = &subpath.nodes[segment];
            let b = &subpath.nodes[(segment + 1) % count];
            let p0 = a.pos;
            let p1 = a.pos + a.handle_out;
            let p2 = b.pos + b.handle_in;
            let p3 = b.pos;
            for step in 0..=32 {
                let t = step as f32 / 32.0;
                let point = cubic_at(p0, p1, p2, p3, t);
                let distance = view.doc_to_screen(point).distance(pointer);
                if best.is_none_or(|(_, _, _, _, current)| distance < current) {
                    best = Some((subpath_index, segment, t, point, distance));
                }
            }
        }
    }
    best.filter(|(_, _, _, _, distance)| *distance <= 8.0)
        .map(|(subpath, segment, t, point, _)| (subpath, segment, t, point))
}

fn lerp(a: Vec2, b: Vec2, t: f32) -> Vec2 {
    a + (b - a) * t
}

fn cubic_at(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let u = 1.0 - t;
    p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t)
}

fn node_click(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    additive: bool,
) {
    if let Some(path) = selected_path(&doc.0, state) {
        if let Some((node, _)) = node_handle_hit(path, state, view, pointer) {
            state.edge_selection = None;
            toggle_node(state, node, additive);
            return;
        }
        if let Some(node) = node_at(path, view, pointer) {
            state.edge_selection = None;
            toggle_node(state, node, additive);
            return;
        }
        // A click on a segment selects the edge for per-edge bevel editing.
        // Dragging that segment still moves the shape; this only runs when the
        // press/ release stayed a click.
        if let Some((subpath, edge, _, _)) = nearest_segment(path, view, pointer) {
            state.edge_selection = Some(EdgeRef { subpath, edge });
            state.node_selection.clear();
            state.node_hover = None;
            return;
        }
    }

    let p = view.screen_to_doc(pointer);
    if let Some(shape_path) = vector_shape_at(&doc.0, state, p) {
        if state.selected.as_deref() != Some(shape_path.as_slice()) {
            state.selected = Some(shape_path);
            state.multi_selected.clear();
            state.node_selection.clear();
            state.node_hover = None;
            state.edge_selection = None;
            return;
        }
    }
    state.node_selection.clear();
    state.node_hover = None;
    state.edge_selection = None;
}

fn toggle_node(state: &mut EditorState, node: NodeRef, additive: bool) {
    if additive {
        if let Some(index) = state.node_selection.iter().position(|entry| *entry == node) {
            state.node_selection.remove(index);
        } else {
            state.node_selection.push(node);
        }
    } else {
        state.node_selection.clear();
        state.node_selection.push(node);
    }
}

fn node_drag_update(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    break_handles: bool,
    snap_enabled: bool,
) {
    let Some(CanvasDrag::NodeEdit {
        layer_id,
        original,
        anchor,
        nodes,
        kind,
    }) = state.canvas_drag.clone()
    else {
        return;
    };
    let pointer = view.screen_to_doc(pointer);
    let mut path = original;
    match kind {
        NodeDragKind::Move => {
            let mut delta = pointer - anchor;
            // Snap the centre of the selected anchors. The edited layer is
            // excluded from the guide targets, so a node cannot snap to itself
            // or to another node that is moving with it.
            let positions: Vec<Vec2> = nodes
                .iter()
                .filter_map(|node| node_ref_node(&path, *node).map(|node| node.pos))
                .collect();
            if !positions.is_empty() {
                let center = positions.iter().copied().sum::<Vec2>() / positions.len() as f32;
                let snapped =
                    snap_translation(state, view, Subject::Point(center + delta), snap_enabled);
                delta += snapped.delta;
            }
            for node in &nodes {
                if let Some(node) = node_mut(&mut path, *node) {
                    node.pos += delta;
                }
            }
        }
        NodeDragKind::PullHandles => {
            let mut handle = pointer - anchor;
            if handle.length_squared() > 4.0 {
                // The mirrored handle is aligned by its tip, which is the point
                // the artist is actually steering onto a guide.
                let origin = nodes
                    .first()
                    .and_then(|node| node_ref_node(&path, *node))
                    .map_or(anchor, |node| node.pos);
                handle +=
                    snap_translation(state, view, Subject::Point(origin + handle), snap_enabled)
                        .delta;
                for node in &nodes {
                    if let Some(node) = node_mut(&mut path, *node) {
                        node.kind = NodeKind::Symmetric;
                        node.handle_out = handle;
                        node.handle_in = -handle;
                    }
                }
            }
        }
        NodeDragKind::HandleIn | NodeDragKind::HandleOut => {
            for node in &nodes {
                if let Some(node) = node_mut(&mut path, *node) {
                    let mut handle = pointer - node.pos;
                    if handle.length_squared() > 4.0 {
                        // Snap the tip rather than the anchor: the tip is what
                        // the artist is lining up with the guide.
                        handle += snap_translation(
                            state,
                            view,
                            Subject::Point(node.pos + handle),
                            snap_enabled,
                        )
                        .delta;
                        set_handle(node, kind, handle, break_handles);
                    }
                }
            }
        }
    }
    apply_shape_source(doc, state, layer_id, ShapeSource::Path(path));
}

pub(crate) fn draw_node_overlay(
    painter: &egui::Painter,
    view: &CanvasView,
    doc: &ns_core::Document,
    state: &EditorState,
) {
    let Some(path) = selected_path(doc, state) else {
        return;
    };
    let accent = SELECTION;
    let edge = SURFACE_LOWEST;

    // Selected edge gets a soft halo under the node markers.
    if let Some(selected_edge) = state.edge_selection {
        if let Some(subpath) = path.subpaths.get(selected_edge.subpath) {
            let count = subpath.nodes.len();
            let segments = if subpath.closed {
                count
            } else {
                count.saturating_sub(1)
            };
            if count >= 2 && selected_edge.edge < segments {
                let a = &subpath.nodes[selected_edge.edge];
                let b = &subpath.nodes[(selected_edge.edge + 1) % count];
                let mut points = Vec::with_capacity(25);
                for step in 0..=24 {
                    let t = step as f32 / 24.0;
                    let point =
                        cubic_at(a.pos, a.pos + a.handle_out, b.pos + b.handle_in, b.pos, t);
                    points.push(view.doc_to_screen(point));
                }
                painter.add(egui::Shape::line(
                    points.clone(),
                    egui::Stroke::new(5.0_f32, accent.gamma_multiply(0.25)),
                ));
                painter.add(egui::Shape::line(
                    points,
                    egui::Stroke::new(2.0_f32, accent),
                ));
            }
        }
    }

    for (subpath_index, subpath) in path.subpaths.iter().enumerate() {
        for (node_index, node) in subpath.nodes.iter().enumerate() {
            let node_ref = NodeRef {
                subpath: subpath_index,
                node: node_index,
            };
            let selected = state.node_selection.contains(&node_ref);
            let hovered = state.node_hover == Some(node_ref);
            let center = view.doc_to_screen(node.pos);
            let fill = if selected {
                accent
            } else if hovered {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_gray(224)
            };
            let highlight = egui::Stroke::new(1.0_f32, edge);
            match node.kind {
                NodeKind::Corner => {
                    let size = if selected { 9.0 } else { 7.0 };
                    let rect = egui::Rect::from_center_size(center, egui::vec2(size, size));
                    painter.rect_filled(rect, 1.0, fill);
                    painter.rect_stroke(rect, 1.0, highlight, egui::StrokeKind::Inside);
                }
                NodeKind::Smooth => {
                    let radius = if selected { 5.0 } else { 4.0 };
                    painter.circle_filled(center, radius, fill);
                    painter.circle_stroke(center, radius, highlight);
                }
                NodeKind::Symmetric => {
                    let radius = if selected { 5.5 } else { 4.5 };
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(center.x, center.y - radius),
                            egui::pos2(center.x + radius, center.y),
                            egui::pos2(center.x, center.y + radius),
                            egui::pos2(center.x - radius, center.y),
                        ],
                        fill,
                        highlight,
                    ));
                }
            }

            if selected {
                for handle in [node.handle_in, node.handle_out] {
                    if handle.length_squared() > 1.0 {
                        let handle_pos = view.doc_to_screen(node.pos + handle);
                        painter.line_segment(
                            [center, handle_pos],
                            egui::Stroke::new(1.0_f32, accent.gamma_multiply(0.8)),
                        );
                        painter.circle_filled(handle_pos, 3.5, accent);
                        painter.circle_stroke(handle_pos, 3.5, highlight);
                    }
                }
            }
        }
    }
}

pub(crate) fn selected_node_kind(path: &Path, node: &NodeRef) -> Option<NodeKind> {
    node_ref_node(path, *node).map(|node| node.kind)
}

pub(crate) fn set_kind_on_path(path: &mut Path, selection: &[NodeRef], kind: NodeKind) {
    for node in selection {
        if let Some(node) = node_mut(path, *node) {
            apply_node_kind(node, kind);
        }
    }
}

pub(crate) fn reset_handles_on_path(path: &mut Path, selection: &[NodeRef]) {
    for node in selection {
        if let Some(node) = node_mut(path, *node) {
            node.kind = NodeKind::Corner;
            node.handle_in = Vec2::ZERO;
            node.handle_out = Vec2::ZERO;
        }
    }
}

pub(crate) fn delete_nodes_on_path(path: &mut Path, selection: &mut Vec<NodeRef>) {
    for (subpath_index, subpath) in path.subpaths.iter_mut().enumerate() {
        let mut remove: Vec<usize> = selection
            .iter()
            .filter(|node| node.subpath == subpath_index)
            .map(|node| node.node)
            .collect();
        remove.sort_unstable();
        remove.dedup();
        remove.truncate(subpath.nodes.len().saturating_sub(2));
        if remove.is_empty() {
            continue;
        }
        let mut index = 0;
        subpath.nodes.retain(|_| {
            let keep = !remove.contains(&index);
            index += 1;
            keep
        });
    }
    selection.clear();
}

pub(crate) fn delete_selected_nodes(doc: &mut DocumentRes, state: &mut EditorState) {
    state.ensure_undo_gesture(&doc.0);
    let Some(path) = state.selected.clone() else {
        return;
    };
    let Some(layer) = doc.0.layer_by_path_mut(&path) else {
        return;
    };
    let LayerKind::Shape(shape) = &mut layer.kind else {
        return;
    };
    let ShapeSource::Path(path) = &mut shape.source else {
        return;
    };
    let before = path.node_count();
    delete_nodes_on_path(path, &mut state.node_selection);
    if path.node_count() != before {
        // Node indices shift, so an edge selection would point at another edge.
        state.edge_selection = None;
        layer.touch();
        state.mark_dirty();
    }
}

/// Move the selected anchors by `delta` document pixels. Arrow keys drive
/// this while a node tool is active. `checkpoint` takes the undo snapshot for
/// the first press of a burst.
pub(crate) fn nudge_selected_nodes(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    delta: Vec2,
    checkpoint: bool,
) -> bool {
    if state.node_selection.is_empty() {
        return false;
    }
    let Some(path) = state.selected.clone() else {
        return false;
    };
    let Some(layer) = doc.0.layer_by_path(&path) else {
        return false;
    };
    let layer_id = layer.id;
    let Some(mut shape_path) = layer_path(layer).cloned() else {
        return false;
    };
    if checkpoint {
        state.checkpoint_document(&doc.0);
    }
    let selection = state.node_selection.clone();
    for node_ref in &selection {
        if let Some(node) = node_mut(&mut shape_path, *node_ref) {
            node.pos += delta;
        }
    }
    apply_shape_source(doc, state, layer_id, ShapeSource::Path(shape_path));
    true
}

fn selected_layer<'a>(doc: &'a DocumentRes, state: &EditorState) -> Option<&'a Layer> {
    doc.0.layer_by_path(state.selected.as_deref()?)
}

fn selected_path<'a>(doc: &'a ns_core::Document, state: &EditorState) -> Option<&'a Path> {
    layer_path(doc.layer_by_path(state.selected.as_deref()?)?)
}

fn layer_path(layer: &Layer) -> Option<&Path> {
    match &layer.kind {
        LayerKind::Shape(shape) => match &shape.source {
            ShapeSource::Path(path) => Some(path),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn is_vector_layer(layer: &Layer) -> bool {
    matches!(&layer.kind, LayerKind::Shape(shape) if matches!(&shape.source, ShapeSource::Path(_)))
}

pub(crate) fn vector_shape_at(
    doc: &ns_core::Document,
    state: &EditorState,
    point: Vec2,
) -> Option<Vec<usize>> {
    let path = top_item_at(doc, state, point)?;
    let layer = doc.layer_by_path(&path)?;
    is_vector_layer(layer).then_some(path)
}

fn node_ref_node(path: &Path, node: NodeRef) -> Option<&Node> {
    path.subpaths.get(node.subpath)?.nodes.get(node.node)
}

fn node_mut(path: &mut Path, node: NodeRef) -> Option<&mut Node> {
    path.subpaths
        .get_mut(node.subpath)?
        .nodes
        .get_mut(node.node)
}

fn node_at(path: &Path, view: &CanvasView, pointer: egui::Pos2) -> Option<NodeRef> {
    for (subpath_index, subpath) in path.subpaths.iter().enumerate() {
        for (node_index, node) in subpath.nodes.iter().enumerate() {
            if view.doc_to_screen(node.pos).distance(pointer) <= NODE_HIT_RADIUS {
                return Some(NodeRef {
                    subpath: subpath_index,
                    node: node_index,
                });
            }
        }
    }
    None
}

fn node_handle_hit(
    path: &Path,
    state: &EditorState,
    view: &CanvasView,
    pointer: egui::Pos2,
) -> Option<(NodeRef, NodeDragKind)> {
    for node_ref in &state.node_selection {
        let Some(node) = node_ref_node(path, *node_ref) else {
            continue;
        };
        for (handle, kind) in [
            (node.handle_in, NodeDragKind::HandleIn),
            (node.handle_out, NodeDragKind::HandleOut),
        ] {
            if handle.length_squared() > 1.0
                && view.doc_to_screen(node.pos + handle).distance(pointer) <= HANDLE_HIT_RADIUS
            {
                return Some((*node_ref, kind));
            }
        }
    }
    None
}

pub(crate) fn set_handle(node: &mut Node, kind: NodeDragKind, handle: Vec2, break_handles: bool) {
    let direction = handle.normalize();
    let out_len = node.handle_out.length();
    let in_len = node.handle_in.length();

    if break_handles {
        node.kind = NodeKind::Corner;
        match kind {
            NodeDragKind::HandleIn => node.handle_in = handle,
            NodeDragKind::HandleOut => node.handle_out = handle,
            _ => {}
        }
        return;
    }

    match node.kind {
        NodeKind::Corner => match kind {
            NodeDragKind::HandleIn => node.handle_in = handle,
            NodeDragKind::HandleOut => node.handle_out = handle,
            _ => {}
        },
        NodeKind::Symmetric => match kind {
            NodeDragKind::HandleIn => {
                node.handle_in = handle;
                node.handle_out = -direction * handle.length();
            }
            NodeDragKind::HandleOut => {
                node.handle_out = handle;
                node.handle_in = -direction * handle.length();
            }
            _ => {}
        },
        NodeKind::Smooth => match kind {
            NodeDragKind::HandleIn => {
                node.handle_in = handle;
                node.handle_out = if out_len > HANDLE_MIN_LENGTH {
                    -direction * out_len
                } else {
                    -handle
                };
            }
            NodeDragKind::HandleOut => {
                node.handle_out = handle;
                node.handle_in = if in_len > HANDLE_MIN_LENGTH {
                    -direction * in_len
                } else {
                    -handle
                };
            }
            _ => {}
        },
    }
}

pub(crate) fn apply_node_kind(node: &mut Node, kind: NodeKind) {
    node.kind = kind;
    if matches!(kind, NodeKind::Corner) {
        return;
    }

    let out_len = node.handle_out.length();
    let in_len = node.handle_in.length();
    if out_len < 1e-3 && in_len < 1e-3 {
        return;
    }
    let direction = if out_len >= in_len && out_len > 1e-3 {
        node.handle_out / out_len
    } else {
        -node.handle_in / in_len
    };

    match kind {
        NodeKind::Smooth => {
            node.handle_out = direction * out_len;
            node.handle_in = -direction * in_len;
        }
        NodeKind::Symmetric => {
            let length = if out_len < 1e-3 {
                in_len
            } else if in_len < 1e-3 {
                out_len
            } else {
                (out_len + in_len) * 0.5
            };
            node.handle_out = direction * length;
            node.handle_in = -direction * length;
        }
        NodeKind::Corner => {}
    }
}

/// Anchor options bar shown above the 2D canvas while a vector tool is active.
/// Replaces the old text buttons in Properties.
pub(crate) fn node_options_bar(ui: &mut egui::Ui, state: &mut EditorState, doc: &mut DocumentRes) {
    if state.mask_edit_layer.is_some() || !state.canvas_tool.is_node_edit_tool() {
        return;
    }
    let Some(path) = selected_path(&doc.0, state) else {
        return;
    };
    let active_kind = state
        .node_selection
        .first()
        .and_then(|node| selected_node_kind(path, node))
        .filter(|kind| {
            state
                .node_selection
                .iter()
                .all(|node| selected_node_kind(path, node) == Some(*kind))
        });
    let count = state.node_selection.len();
    let selection = state.node_selection.clone();

    let mut set_kind = None;
    let mut reset = false;
    let mut delete = false;
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
                draw_icon(
                    ui.painter(),
                    icon_rect,
                    vector_tool_icon(state.canvas_tool),
                    ACCENT,
                );
                ui.label(
                    egui::RichText::new(state.canvas_tool.label())
                        .size(TEXT_LABEL)
                        .strong(),
                );
                ui.separator();
                for (kind, icon, label) in [
                    (NodeKind::Corner, Icon::CornerNode, "Corner"),
                    (NodeKind::Smooth, Icon::SmoothNode, "Smooth"),
                    (NodeKind::Symmetric, Icon::SymmetricNode, "Symmetric"),
                ] {
                    if icon_button_sized(
                        ui,
                        icon,
                        active_kind == Some(kind),
                        count > 0,
                        label,
                        egui::vec2(22.0, 22.0),
                    )
                    .clicked()
                    {
                        set_kind = Some(kind);
                    }
                }
                ui.separator();
                if icon_button_sized(
                    ui,
                    Icon::ResetHandles,
                    false,
                    count > 0,
                    "Reset handles",
                    egui::vec2(22.0, 22.0),
                )
                .clicked()
                {
                    reset = true;
                }
                if icon_button_sized(
                    ui,
                    Icon::Trash,
                    false,
                    count > 0,
                    "Delete selected nodes",
                    egui::vec2(22.0, 22.0),
                )
                .clicked()
                {
                    delete = true;
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{count} node{}",
                        if count == 1 { "" } else { "s" }
                    ))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
                let hint = match state.canvas_tool {
                    CanvasTool::Node => "Drag anchors/handles · Shift-click multi-select",
                    CanvasTool::AddAnchor => "Click a path segment to add an anchor",
                    CanvasTool::DeleteAnchor => "Click an anchor to delete it",
                    CanvasTool::ConvertAnchor => "Click an anchor to toggle corner/smooth",
                    _ => "",
                };
                if !hint.is_empty() {
                    ui.label(egui::RichText::new(hint).size(TEXT_CAPTION).color(MUTED));
                }
            });
        });

    if let Some(kind) = set_kind {
        modify_selected_path(doc, state, |path| {
            set_kind_on_path(path, &selection, kind);
        });
    }
    if reset {
        modify_selected_path(doc, state, |path| {
            reset_handles_on_path(path, &selection);
        });
    }
    if delete {
        delete_selected_nodes(doc, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smooth_node(handle_in: Vec2, handle_out: Vec2) -> Node {
        Node::smooth(Vec2::ZERO, handle_in, handle_out)
    }

    #[test]
    fn nudging_moves_only_the_selected_anchors() {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        for x in [10.0, 40.0, 70.0] {
            subpath.nodes.push(Node::corner(Vec2::new(x, 20.0)));
        }
        path.subpaths.push(subpath);
        let mut layer = Layer::shape("line", ShapeSource::Path(path));
        layer.touch();
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(96)));
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            canvas_tool: CanvasTool::Node,
            node_selection: vec![NodeRef {
                subpath: 0,
                node: 1,
            }],
            ..Default::default()
        };

        assert!(nudge_selected_nodes(
            &mut state,
            &mut doc,
            Vec2::new(2.0, -3.0),
            true
        ));
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("still a shape")
        };
        let ShapeSource::Path(path) = &shape.source else {
            panic!("still a path")
        };
        let nodes = &path.subpaths[0].nodes;
        assert_eq!(nodes[0].pos, Vec2::new(10.0, 20.0));
        assert_eq!(nodes[1].pos, Vec2::new(42.0, 17.0));
        assert_eq!(nodes[2].pos, Vec2::new(70.0, 20.0));
    }

    #[test]
    fn nudging_a_node_accepts_subpixel_steps() {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(10.0, 20.0)));
        subpath.nodes.push(Node::corner(Vec2::new(40.0, 20.0)));
        path.subpaths.push(subpath);
        let mut layer = Layer::shape("line", ShapeSource::Path(path));
        layer.touch();
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(96)));
        doc.0.layers.push(layer);
        let mut state = EditorState {
            selected: Some(vec![0]),
            canvas_tool: CanvasTool::Node,
            node_selection: vec![NodeRef {
                subpath: 0,
                node: 1,
            }],
            ..Default::default()
        };

        assert!(nudge_selected_nodes(
            &mut state,
            &mut doc,
            Vec2::new(0.1, -0.2),
            true
        ));
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("still a shape")
        };
        let ShapeSource::Path(path) = &shape.source else {
            panic!("still a path")
        };
        let node = &path.subpaths[0].nodes[1];
        assert!((node.pos.x - 40.1).abs() < 1e-4);
        assert!((node.pos.y - 19.8).abs() < 1e-4);
    }

    #[test]
    fn moving_a_node_snaps_to_a_smart_guide() {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        for x in [10.0, 40.0, 70.0] {
            subpath.nodes.push(Node::corner(Vec2::new(x, 50.0)));
        }
        path.subpaths.push(subpath);
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(200)));
        doc.0
            .layers
            .push(Layer::shape("line", ShapeSource::Path(path.clone())));
        let layer_id = doc.0.layers[0].id;
        doc.0.layers.push(Layer::shape(
            "target",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(92.0, 50.0),
                size: Vec2::new(20.0, 60.0),
                corner_radius: 0.0,
            }),
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            canvas_tool: CanvasTool::Node,
            node_selection: vec![NodeRef {
                subpath: 0,
                node: 1,
            }],
            ..Default::default()
        };
        state.canvas_drag = Some(CanvasDrag::NodeEdit {
            layer_id,
            original: path,
            anchor: Vec2::new(40.0, 50.0),
            nodes: state.node_selection.clone(),
            kind: NodeDragKind::Move,
        });
        begin_snap_session(&mut state, &doc, [layer_id]);
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );

        // Raw node position is 80; the target's left edge is 82, inside the
        // 8 px guide tolerance.
        node_drag_update(
            &mut state,
            &mut doc,
            &view,
            egui::pos2(80.0, 50.0),
            false,
            true,
        );
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("still a shape")
        };
        let ShapeSource::Path(path) = &shape.source else {
            panic!("still a path")
        };
        let node = &path.subpaths[0].nodes[1];
        assert!((node.pos.x - 82.0).abs() < 1e-3, "node at {}", node.pos.x);
        assert!(!state.active_guides.is_empty(), "the guide must be visible");
    }

    #[test]
    fn moving_a_node_snaps_to_a_horizontal_guide() {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(10.0, 50.0)));
        subpath.nodes.push(Node::corner(Vec2::new(40.0, 50.0)));
        path.subpaths.push(subpath);
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(200)));
        doc.0
            .layers
            .push(Layer::shape("line", ShapeSource::Path(path.clone())));
        let layer_id = doc.0.layers[0].id;
        doc.0.layers.push(Layer::shape(
            "target",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(92.0, 50.0),
                size: Vec2::new(20.0, 60.0),
                corner_radius: 0.0,
            }),
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            canvas_tool: CanvasTool::Node,
            node_selection: vec![NodeRef {
                subpath: 0,
                node: 1,
            }],
            ..Default::default()
        };
        state.canvas_drag = Some(CanvasDrag::NodeEdit {
            layer_id,
            original: path,
            anchor: Vec2::new(40.0, 50.0),
            nodes: state.node_selection.clone(),
            kind: NodeDragKind::Move,
        });
        begin_snap_session(&mut state, &doc, [layer_id]);
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );

        // The target's bottom edge is y=80; the raw node Y is 74.
        node_drag_update(
            &mut state,
            &mut doc,
            &view,
            egui::pos2(60.0, 74.0),
            false,
            true,
        );
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("still a shape")
        };
        let ShapeSource::Path(path) = &shape.source else {
            panic!("still a path")
        };
        let node = &path.subpaths[0].nodes[1];
        assert!((node.pos.y - 80.0).abs() < 1e-3, "node at {}", node.pos.y);
        assert!(
            state
                .active_guides
                .iter()
                .any(|guide| guide.kind == GuideKind::Horizontal),
            "the horizontal guide must be visible"
        );
    }

    #[test]
    fn dragging_a_handle_snaps_its_tip_to_a_smart_guide() {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(10.0, 50.0)));
        let mut node = Node::corner(Vec2::new(40.0, 50.0));
        node.kind = ns_core::path::NodeKind::Smooth;
        subpath.nodes.push(node);
        path.subpaths.push(subpath);
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(200)));
        doc.0
            .layers
            .push(Layer::shape("line", ShapeSource::Path(path.clone())));
        let layer_id = doc.0.layers[0].id;
        doc.0.layers.push(Layer::shape(
            "target",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(92.0, 50.0),
                size: Vec2::new(20.0, 60.0),
                corner_radius: 0.0,
            }),
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            canvas_tool: CanvasTool::Node,
            node_selection: vec![NodeRef {
                subpath: 0,
                node: 1,
            }],
            ..Default::default()
        };
        state.canvas_drag = Some(CanvasDrag::NodeEdit {
            layer_id,
            original: path,
            anchor: Vec2::new(40.0, 50.0),
            nodes: state.node_selection.clone(),
            kind: NodeDragKind::HandleOut,
        });
        begin_snap_session(&mut state, &doc, [layer_id]);
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );

        // The raw handle tip is 80; the target's left edge is 82, inside the
        // 8 px guide tolerance.
        node_drag_update(
            &mut state,
            &mut doc,
            &view,
            egui::pos2(80.0, 50.0),
            false,
            true,
        );
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("still a shape")
        };
        let ShapeSource::Path(path) = &shape.source else {
            panic!("still a path")
        };
        let handle = path.subpaths[0].nodes[1].handle_out;
        assert!((handle.x - 42.0).abs() < 1e-3, "handle at {}", handle.x);
        assert!(!state.active_guides.is_empty(), "the guide must be visible");
    }

    #[test]
    fn nudging_without_a_node_selection_does_nothing() {
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(96)));
        doc.0.layers.push(Layer::shape(
            "shape",
            ShapeSource::Path(Path::new(FillRule::NonZero)),
        ));
        let mut state = EditorState {
            selected: Some(vec![0]),
            canvas_tool: CanvasTool::Node,
            ..Default::default()
        };
        assert!(!nudge_selected_nodes(
            &mut state,
            &mut doc,
            Vec2::new(1.0, 0.0),
            true
        ));
    }

    #[test]
    fn symmetric_conversion_equalizes_and_opposes_handles() {
        let mut node = smooth_node(Vec2::new(-1.0, 0.0), Vec2::new(2.0, 0.0));
        apply_node_kind(&mut node, NodeKind::Symmetric);
        assert_eq!(node.kind, NodeKind::Symmetric);
        assert!((node.handle_out.length() - 1.5).abs() < 1e-4);
        assert!((node.handle_in.length() - 1.5).abs() < 1e-4);
        assert!(node.handle_in.dot(node.handle_out) < 0.0);
    }

    #[test]
    fn smooth_handle_drag_preserves_opposite_length() {
        let mut node = smooth_node(Vec2::new(-3.0, 0.0), Vec2::new(1.0, 0.0));
        set_handle(
            &mut node,
            NodeDragKind::HandleOut,
            Vec2::new(0.0, 2.0),
            false,
        );
        assert_eq!(node.handle_out, Vec2::new(0.0, 2.0));
        assert_eq!(node.handle_in, Vec2::new(0.0, -3.0));
        assert_eq!(node.kind, NodeKind::Smooth);
    }

    #[test]
    fn corner_handle_drag_leaves_other_handle_alone() {
        let mut node = Node {
            pos: Vec2::ZERO,
            handle_in: Vec2::new(-1.0, 0.0),
            handle_out: Vec2::new(1.0, 0.0),
            kind: NodeKind::Corner,
            bevel: None,
        };
        set_handle(
            &mut node,
            NodeDragKind::HandleOut,
            Vec2::new(0.0, 2.0),
            false,
        );
        assert_eq!(node.handle_in, Vec2::new(-1.0, 0.0));
        assert_eq!(node.handle_out, Vec2::new(0.0, 2.0));
    }

    #[test]
    fn deletion_keeps_at_least_two_nodes_in_a_subpath() {
        let mut path = Path::from_polygon(&[
            Vec2::ZERO,
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ]);
        let mut selection = vec![
            NodeRef {
                subpath: 0,
                node: 0,
            },
            NodeRef {
                subpath: 0,
                node: 1,
            },
            NodeRef {
                subpath: 0,
                node: 2,
            },
        ];
        delete_nodes_on_path(&mut path, &mut selection);
        assert_eq!(path.subpaths[0].nodes.len(), 2);
        assert!(selection.is_empty());
    }

    #[test]
    fn neighbor_positions_wrap_closed_paths_and_clamp_open_paths() {
        let mut path = Path::from_polygon(&[
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
        ]);
        let first = NodeRef {
            subpath: 0,
            node: 0,
        };
        let (previous, next) = neighbor_positions(&path, first).unwrap();
        assert_eq!(previous, Vec2::new(10.0, 10.0));
        assert_eq!(next, Vec2::new(10.0, 0.0));

        path.subpaths[0].closed = false;
        let (previous, next) = neighbor_positions(&path, first).unwrap();
        assert_eq!(previous, Vec2::new(10.0, 0.0));
        assert_eq!(next, Vec2::new(10.0, 0.0));

        let last = NodeRef {
            subpath: 0,
            node: 2,
        };
        let (previous, next) = neighbor_positions(&path, last).unwrap();
        assert_eq!(previous, Vec2::new(10.0, 0.0));
        assert_eq!(next, Vec2::new(10.0, 0.0));
    }
}
