//! Bézier pen tool state machine.

use super::*;
pub(crate) fn pen_edit(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
) {
    // Shift first straightens the pending segment: onto the ruler when one is
    // placed, otherwise onto the nearest 45 degree step. Smart guides then
    // pull that constrained point onto nearby vertices, axes and edges; the
    // preview snaps too, so the node lands where the line already points.
    let shift = ui.input(|input| input.modifiers.shift);
    // Closing is the only way to make a shape now, so the first node's ring
    // lights up as soon as a click there would close the loop.
    let close_armed = response
        .hover_pos()
        .is_some_and(|pointer| pen_close_candidate(state, view, pointer));
    if let Some(pointer) = response.hover_pos() {
        let (hover, guides) = pen_place(state, doc, view, view.screen_to_doc(pointer), shift);
        let draft = state.vector_draft.get_or_insert_with(VectorDraft::default);
        draft.hover = Some(hover);
        draft.close_armed = close_armed;
        draft.guides = guides;
    } else if let Some(draft) = &mut state.vector_draft {
        draft.hover = None;
        draft.close_armed = false;
        draft.guides.clear();
    }

    if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
        // Enter ends the path as it was drawn: an open line. Closing a loop is
        // a deliberate gesture (click the first node), never a side effect of
        // confirming the path, so Enter does not add a closing segment the
        // user has not placed.
        finish_pen_path(state, doc, false);
        return;
    }

    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
            if !pen_close_candidate(state, view, origin) {
                pen_add_node(state, doc, view, origin, true, shift);
            }
        }
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        if let Some(pointer) = response.interact_pointer_pos() {
            pen_drag_handle(state, view, pointer, shift);
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        if let Some(draft) = &mut state.vector_draft {
            draft.drag_index = None;
        }
    }

    if response.double_clicked() {
        // Same as Enter: end the open path without closing it.
        finish_pen_path(state, doc, false);
        return;
    }
    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            if pen_close_candidate(state, view, pointer) {
                finish_pen_path(state, doc, true);
            } else {
                pen_add_node(state, doc, view, pointer, false, shift);
            }
        }
    }
}

pub(crate) fn pen_close_candidate(
    state: &EditorState,
    view: &CanvasView,
    pointer: egui::Pos2,
) -> bool {
    let Some(draft) = &state.vector_draft else {
        return false;
    };
    draft.nodes.len() >= 2
        && draft
            .nodes
            .first()
            .is_some_and(|node| view.doc_to_screen(node.pos).distance(pointer) <= 10.0)
}

pub(crate) fn pen_add_node(
    state: &mut EditorState,
    doc: &DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    dragging: bool,
    shift: bool,
) {
    let pos = pen_place(state, doc, view, view.screen_to_doc(pointer), shift).0;
    let draft = state.vector_draft.get_or_insert_with(VectorDraft::default);
    draft.nodes.push(Node::corner(pos));
    draft.hover = Some(pos);
    // The guides described the segment just placed; the next hover rebuilds
    // them for the new pending segment.
    draft.guides.clear();
    draft.drag_index = dragging.then_some(draft.nodes.len() - 1);
}

pub(crate) fn pen_drag_handle(
    state: &mut EditorState,
    view: &CanvasView,
    pointer: egui::Pos2,
    shift: bool,
) {
    let ruler = state.ruler;
    let pos = view.screen_to_doc(pointer);
    let Some(draft) = &mut state.vector_draft else {
        return;
    };
    let Some(index) = draft.drag_index else {
        return;
    };
    let Some(node) = draft.nodes.get_mut(index) else {
        return;
    };
    // Shift aligns the handle direction like a segment; the length still
    // follows the pointer.
    let handle = snapped_pen_handle(node.pos, pos - node.pos, ruler, shift);
    if handle.length_squared() > 4.0 {
        node.kind = NodeKind::Symmetric;
        node.handle_out = handle;
        node.handle_in = -handle;
    }
}

/// Commit the pen draft as a layer.
///
/// `closed` is only true when the user clicked the first node on purpose.
/// Enter and double-click end the path open, so a loop is never closed behind
/// the user's back.
pub(crate) fn finish_pen_path(state: &mut EditorState, doc: &mut DocumentRes, closed: bool) {
    if state
        .vector_draft
        .as_ref()
        .is_none_or(|draft| draft.nodes.len() < 2)
    {
        return;
    }
    let Some(draft) = state.vector_draft.take() else {
        return;
    };
    let path = Path {
        subpaths: vec![SubPath {
            nodes: draft.nodes,
            closed,
        }],
        fill_rule: FillRule::NonZero,
    };
    if state.mask_edit_layer.is_some() {
        stamp_mask_shape(doc, state, ShapeSource::Path(path));
        state.viewport_tab = ViewportTab::TwoD;
        state.status = "Vector shape stamped into mask".into();
        return;
    }
    let scale = doc.0.size.x as f32 / 1024.0;
    let mut layer = Layer::shape(
        if closed { "Vector shape" } else { "Cut line" },
        ShapeSource::Path(path),
    );
    if let LayerKind::Shape(shape) = &mut layer.kind {
        if closed {
            shape.radius_px = 18.0 * scale;
            shape.height_px = 14.0 * scale;
        } else {
            // Filling an open path would close it along the last chord, so a
            // line follows the outline style instead and is ready to cut.
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 10.0 * scale;
            shape.radius_px = 5.0 * scale;
            shape.height_px = 6.0 * scale;
        }
    }
    add_layer(doc, state, layer);
    state.viewport_tab = ViewportTab::TwoD;
    state.status = if closed {
        "Vector shape created".into()
    } else {
        "Cut line created · set Stroke (px), then Cut shapes below".into()
    };
}

/// Click-to-place polygon: corners only, no Bézier handles.
///
/// The draft is shared with the pen so snapping, smart guides, hover preview
/// and Esc cancellation behave the same. The difference is that a polygon is
/// always closed when it is committed: Enter, a double-click or a click on the
/// first corner finishes a filled shape.
pub(crate) fn polygon_edit(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
) {
    let shift = ui.input(|input| input.modifiers.shift);
    let close_armed = response
        .hover_pos()
        .is_some_and(|pointer| polygon_close_candidate(state, view, pointer));
    if let Some(pointer) = response.hover_pos() {
        let (hover, guides) = pen_place(state, doc, view, view.screen_to_doc(pointer), shift);
        let draft = state.vector_draft.get_or_insert_with(VectorDraft::default);
        draft.hover = Some(hover);
        draft.close_armed = close_armed;
        draft.guides = guides;
    } else if let Some(draft) = &mut state.vector_draft {
        draft.hover = None;
        draft.close_armed = false;
        draft.guides.clear();
    }

    if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
        finish_polygon(state, doc);
        return;
    }

    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
            if !polygon_close_candidate(state, view, origin) {
                polygon_add_node(state, doc, view, origin, shift);
            }
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        if let Some(draft) = &mut state.vector_draft {
            draft.drag_index = None;
        }
    }

    if response.double_clicked() {
        finish_polygon(state, doc);
        return;
    }
    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            if polygon_close_candidate(state, view, pointer) {
                finish_polygon(state, doc);
            } else {
                polygon_add_node(state, doc, view, pointer, shift);
            }
        }
    }
}

/// Closing needs three corners: two points are a line, not a polygon.
pub(crate) fn polygon_close_candidate(
    state: &EditorState,
    view: &CanvasView,
    pointer: egui::Pos2,
) -> bool {
    let Some(draft) = &state.vector_draft else {
        return false;
    };
    draft.nodes.len() >= 3
        && draft
            .nodes
            .first()
            .is_some_and(|node| view.doc_to_screen(node.pos).distance(pointer) <= 10.0)
}

pub(crate) fn polygon_add_node(
    state: &mut EditorState,
    doc: &DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    shift: bool,
) {
    let pos = pen_place(state, doc, view, view.screen_to_doc(pointer), shift).0;
    let draft = state.vector_draft.get_or_insert_with(VectorDraft::default);
    draft.nodes.push(Node::corner(pos));
    draft.hover = Some(pos);
    draft.guides.clear();
    draft.drag_index = None;
}

/// Commit the polygon draft as a closed, filled layer. Fewer than three corners
/// keep the draft so the user can finish the shape.
pub(crate) fn finish_polygon(state: &mut EditorState, doc: &mut DocumentRes) {
    if state
        .vector_draft
        .as_ref()
        .is_none_or(|draft| draft.nodes.len() < 3)
    {
        return;
    }
    let Some(draft) = state.vector_draft.take() else {
        return;
    };
    let path = Path {
        subpaths: vec![SubPath {
            nodes: draft.nodes,
            closed: true,
        }],
        fill_rule: FillRule::NonZero,
    };
    if state.mask_edit_layer.is_some() {
        stamp_mask_shape(doc, state, ShapeSource::Path(path));
        state.viewport_tab = ViewportTab::TwoD;
        state.status = "Polygon stamped into mask".into();
        return;
    }
    let scale = doc.0.size.x as f32 / 1024.0;
    let mut layer = Layer::shape("Polygon", ShapeSource::Path(path));
    if let LayerKind::Shape(shape) = &mut layer.kind {
        shape.radius_px = 18.0 * scale;
        shape.height_px = 14.0 * scale;
    }
    add_layer(doc, state, layer);
    state.viewport_tab = ViewportTab::TwoD;
    state.status = "Polygon created · click to start the next one".into();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> CanvasView {
        CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0)),
            Vec2::splat(100.0),
        )
    }

    /// Two clicked nodes at (20, 20) and (60, 20); the canvas maps 1:1.
    fn drafted() -> EditorState {
        EditorState {
            canvas_tool: CanvasTool::Pen,
            vector_draft: Some(VectorDraft {
                nodes: vec![
                    Node::corner(Vec2::new(20.0, 20.0)),
                    Node::corner(Vec2::new(60.0, 20.0)),
                ],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn finish(closed: bool) -> (EditorState, DocumentRes) {
        let mut state = drafted();
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(100)));
        finish_pen_path(&mut state, &mut doc, closed);
        (state, doc)
    }

    #[test]
    fn only_a_click_on_the_first_node_closes_the_loop() {
        let state = drafted();
        let view = view();
        // Within the ring around the first node closes; anything else adds a
        // node. Enter and double-click never take this path.
        assert!(pen_close_candidate(&state, &view, egui::pos2(24.0, 22.0)));
        assert!(!pen_close_candidate(&state, &view, egui::pos2(60.0, 20.0)));
        assert!(!pen_close_candidate(&state, &view, egui::pos2(45.0, 45.0)));
    }

    #[test]
    fn a_single_node_draft_never_closes() {
        let mut state = drafted();
        let view = view();
        if let Some(draft) = &mut state.vector_draft {
            draft.nodes.truncate(1);
        }
        assert!(!pen_close_candidate(&state, &view, egui::pos2(20.0, 20.0)));
    }

    #[test]
    fn an_open_path_becomes_a_stroked_cut_line() {
        let (state, doc) = finish(false);
        assert!(state.vector_draft.is_none(), "the draft is consumed");
        assert_eq!(
            state.canvas_tool,
            CanvasTool::Pen,
            "the pen stays active for the next path"
        );
        let layer = doc.0.layers.last().expect("a layer is created");
        assert_eq!(layer.name, "Cut line");
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("expected a shape layer")
        };
        assert!(shape.style.is_stroke(), "open lines are ready to cut");
        assert!(matches!(&shape.source, ShapeSource::Path(path) if !path.subpaths[0].closed));
    }

    #[test]
    fn a_closed_path_becomes_a_filled_shape() {
        let (state, doc) = finish(true);
        assert_eq!(state.canvas_tool, CanvasTool::Pen);
        let layer = doc.0.layers.last().expect("a layer is created");
        assert_eq!(layer.name, "Vector shape");
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("expected a shape layer")
        };
        assert!(!shape.style.is_stroke());
        assert!(matches!(&shape.source, ShapeSource::Path(path) if path.subpaths[0].closed));
    }

    #[test]
    fn one_node_is_not_enough_to_finish() {
        let mut state = drafted();
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(100)));
        if let Some(draft) = &mut state.vector_draft {
            draft.nodes.truncate(1);
        }
        finish_pen_path(&mut state, &mut doc, false);
        assert!(doc.0.layers.is_empty(), "nothing is committed");
        assert!(state.vector_draft.is_some(), "the draft is kept");
    }

    fn polygon_drafted(corners: usize) -> EditorState {
        let nodes = (0..corners)
            .map(|index| Node::corner(Vec2::new(20.0 + index as f32 * 15.0, 20.0)))
            .collect();
        EditorState {
            canvas_tool: CanvasTool::Polygon,
            shape_tool: CanvasTool::Polygon,
            vector_draft: Some(VectorDraft {
                nodes,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_polygon_closes_into_a_filled_shape() {
        let mut state = polygon_drafted(4);
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(100)));
        finish_polygon(&mut state, &mut doc);

        assert!(state.vector_draft.is_none(), "the draft is consumed");
        assert_eq!(
            state.canvas_tool,
            CanvasTool::Polygon,
            "the polygon tool stays active for the next shape"
        );
        let layer = doc.0.layers.last().expect("a layer is created");
        assert_eq!(layer.name, "Polygon");
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("expected a shape layer")
        };
        assert!(!shape.style.is_stroke(), "polygons are filled");
        let ShapeSource::Path(path) = &shape.source else {
            panic!("expected a path")
        };
        assert!(path.subpaths[0].closed);
        assert_eq!(path.subpaths[0].nodes.len(), 4);
    }

    #[test]
    fn a_polygon_needs_three_corners() {
        let mut state = polygon_drafted(2);
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(100)));
        finish_polygon(&mut state, &mut doc);
        assert!(doc.0.layers.is_empty(), "two corners are a line");
        assert!(state.vector_draft.is_some(), "the draft is kept");
    }

    #[test]
    fn closing_a_polygon_needs_three_corners() {
        let view = view();
        let mut state = polygon_drafted(2);
        assert!(!polygon_close_candidate(
            &state,
            &view,
            egui::pos2(20.0, 20.0)
        ));
        if let Some(draft) = &mut state.vector_draft {
            draft.nodes.push(Node::corner(Vec2::new(65.0, 40.0)));
        }
        assert!(polygon_close_candidate(
            &state,
            &view,
            egui::pos2(22.0, 21.0)
        ));
    }
}
