//! Cable tool: turn a custom vector path into a smooth round tube.
//!
//! The Path comes from the Pen tool (or any editable vector path). Clicking it
//! with the Cable tool converts the layer in place: the layer name, opacity,
//! mask and ID colour are kept, its shape becomes the baked cable group, and
//! the original path is stored as the centreline so width and terminal changes
//! can rebuild it.

use super::*;

pub(crate) fn cable_edit(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
) {
    let hovered_path = response.hover_pos().and_then(|pointer| {
        let point = view.screen_to_doc(pointer);
        top_item_at(&doc.0, state, point)
            .filter(|path| doc.0.layer_by_path(path).is_some_and(is_vector_layer))
    });
    ui.ctx().set_cursor_icon(if hovered_path.is_some() {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::Default
    });

    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let point = view.screen_to_doc(pointer);
            if let Some(path) = top_item_at(&doc.0, state, point) {
                if doc.0.layer_by_path(&path).is_some_and(is_vector_layer) {
                    convert_path_layer_to_cable(state, doc, &path);
                }
            }
        }
    }
}

/// Convert one editable vector path layer into a cable in place.
///
/// Returns `false` when the layer is not a shape with a path source or the
/// path cannot make a tube. The layer identity is preserved, so masks, IDs and
/// the row name survive the conversion.
pub(crate) fn convert_path_layer_to_cable(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    path: &[usize],
) -> bool {
    // A cable body is itself a path; converting it again would wrap the tube
    // in a second cable.
    if path.len() > 1
        && doc
            .0
            .layer_by_path(&path[..path.len() - 1])
            .is_some_and(|parent| parent.cable.is_some())
    {
        return false;
    }
    let width = state.cable_width;
    let height = state.cable_height;
    let cap = state.cable_cap;
    let cap_scale = state.cable_cap_scale;
    let Some(layer) = doc.0.layer_by_path(path) else {
        return false;
    };
    let LayerKind::Shape(shape) = &layer.kind else {
        return false;
    };
    let ShapeSource::Path(centerline) = &shape.source else {
        return false;
    };
    let Some(meta) =
        ns_core::cable::CableMeta::from_path(centerline, width, height, cap, cap_scale)
    else {
        return false;
    };
    let Some(cable) = ns_core::cable_layer(&meta) else {
        return false;
    };
    state.ensure_undo_gesture(&doc.0);
    let Some(layer) = doc.0.layer_by_path_mut(path) else {
        return false;
    };
    layer.kind = cable.kind;
    layer.cable = Some(meta);
    layer.touch();
    state.mark_dirty();
    state.selected = Some(path.to_vec());
    state.multi_selected.clear();
    state.node_selection.clear();
    state.edge_selection = None;
    state.vector_tool = CanvasTool::Cable;
    state.status = format!(
        "Cable created from this path · {:.0} px tube · {:.0} px high · {} ends · tweak in Properties",
        width,
        height,
        cap.label()
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_path() -> Path {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(30.0, 40.0)));
        subpath.nodes.push(Node::smooth(
            Vec2::new(100.0, 50.0),
            Vec2::new(-20.0, 0.0),
            Vec2::new(20.0, 0.0),
        ));
        subpath.nodes.push(Node::corner(Vec2::new(160.0, 90.0)));
        path.subpaths.push(subpath);
        path
    }

    #[test]
    fn converting_a_path_turns_it_into_a_cable_in_place() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(256)));
        doc.0
            .layers
            .push(Layer::shape("Router line", ShapeSource::Path(open_path())));
        let mut state = EditorState {
            selected: Some(vec![0]),
            canvas_tool: CanvasTool::Cable,
            cable_width: 24.0,
            cable_height: 10.0,
            cable_cap: CableCap::Round,
            ..Default::default()
        };
        assert!(convert_path_layer_to_cable(&mut state, &mut doc, &[0]));
        let layer = &doc.0.layers[0];
        assert_eq!(layer.name, "Router line", "the row keeps its identity");
        assert!(matches!(layer.kind, LayerKind::Group { .. }));
        let meta = layer.cable.as_ref().expect("cable authoring data");
        assert!(meta.centerline.is_some(), "the path is the centreline");
        assert!((meta.width - 24.0).abs() < 1e-4);
        assert_eq!(state.selected.as_deref(), Some(&[0usize][..]));
    }

    #[test]
    fn converting_a_primitive_is_refused() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(128)));
        doc.0.layers.push(Layer::shape(
            "Rectangle",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(64.0),
                size: Vec2::splat(40.0),
                corner_radius: 2.0,
            }),
        ));
        let mut state = EditorState::default();
        assert!(!convert_path_layer_to_cable(&mut state, &mut doc, &[0]));
        assert!(matches!(doc.0.layers[0].kind, LayerKind::Shape(_)));
    }
}
