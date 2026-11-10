//! Layer manipulation from the 3D viewport.
//!
//! Layers are 2D content projected onto the preview mesh through its UVs, so a
//! drag in the 3D view is converted into a document-space move or stretch of
//! the selected shape/vector layer. Both views stay in sync because the same
//! document is edited, and undo works through the normal gesture snapshot.

use super::*;
use glam::Vec3;

/// Vertical field of view of the preview camera, in radians.
pub(crate) const PREVIEW_FOV: f32 = 38.0 * std::f32::consts::PI / 180.0;

impl PreviewShape {
    /// World size of one UV tile on the preview mesh, used to convert world
    /// movement into texture movement.
    pub(crate) fn uv_extent(self) -> f32 {
        match self {
            Self::Plane => 1.1,
            Self::Cube => 1.05,
            Self::RoundedCube => 1.04,
            Self::Cylinder => 1.0,
            Self::Imported => 1.0,
        }
    }
}

/// Camera basis for the preview: forward, right and up in world space.
pub(crate) fn camera_basis(yaw: f32, pitch: f32) -> (Vec3, Vec3, Vec3) {
    let position = Vec3::new(
        pitch.cos() * yaw.sin(),
        pitch.sin(),
        pitch.cos() * yaw.cos(),
    );
    let forward = (-position).normalize_or_zero();
    let right = forward.cross(Vec3::Y).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    (forward, right, up)
}

/// Convert a screen-space drag in the 3D view into a document-space delta.
///
/// The mapping goes screen px -> world units at the mesh centre -> UV on the
/// preview mesh -> document pixels. Document Y grows downward, world Y up.
#[allow(clippy::too_many_arguments)]
pub(crate) fn screen_delta_to_doc(
    drag: egui::Vec2,
    viewport_height: f32,
    yaw: f32,
    pitch: f32,
    distance: f32,
    shape: PreviewShape,
    doc_size: Vec2,
) -> Vec2 {
    if viewport_height <= 1.0 {
        return Vec2::ZERO;
    }
    let world_per_px =
        2.0 * distance.max(0.1) * (PREVIEW_FOV * 0.5).tan() / viewport_height.max(1.0);
    let (_, right, up) = camera_basis(yaw, pitch);
    // Screen Y points down, so dragging down is a negative move along `up`.
    let world = right * (drag.x * world_per_px) - up * (drag.y * world_per_px);
    let extent = shape.uv_extent().max(1e-3);
    let uv = Vec2::new(world.dot(Vec3::X), world.dot(Vec3::Y)) / extent;
    Vec2::new(uv.x * doc_size.x, -uv.y * doc_size.y)
}

/// Move or stretch `layer` by a document-space delta. Returns true when the
/// layer changed.
pub(crate) fn apply_layer_delta(
    layer: &mut Layer,
    mode: PreviewGizmo,
    doc_delta: Vec2,
) -> std::result::Result<(), &'static str> {
    if doc_delta.length_squared() < 1e-8 || mode == PreviewGizmo::Off {
        return Ok(());
    }
    let LayerKind::Shape(shape) = &mut layer.kind else {
        return Err("3D move/stretch works on shape and vector layers");
    };
    let Some(bounds) = shape.source.bounds() else {
        return Err("This layer has no editable outline");
    };
    let (scale, offset) = match mode {
        PreviewGizmo::Move => (Vec2::ONE, doc_delta),
        PreviewGizmo::Stretch => {
            // Stretch along the dominant axis of the drag so a horizontal drag
            // widens the layer and a vertical drag lengthens it.
            let size = bounds.size().max(Vec2::splat(1.0));
            let (axis, amount) = if doc_delta.x.abs() >= doc_delta.y.abs() {
                (0, doc_delta.x)
            } else {
                (1, doc_delta.y)
            };
            let mut scale = Vec2::ONE;
            scale[axis] = 1.0 + 2.0 * amount / size[axis];
            scale[axis] = scale[axis].clamp(0.05, 20.0);
            // Scale about the layer centre.
            let center = bounds.center();
            (scale, center * (Vec2::ONE - scale))
        }
        PreviewGizmo::Off => return Ok(()),
    };
    let Some(new_source) = shape.source.transformed(scale, offset) else {
        return Err("This layer type cannot be moved in 3D");
    };
    shape.source = new_source;
    layer.touch();
    Ok(())
}

/// One gizmo drag step from the 3D viewport. Returns true when the document
/// changed.
pub(crate) fn drag_selected_layer(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    viewport_size: egui::Vec2,
    drag: egui::Vec2,
) -> bool {
    let mode = state.preview_gizmo;
    if mode == PreviewGizmo::Off || drag.length_sq() < 1e-6 {
        return false;
    }
    let doc_delta = screen_delta_to_doc(
        drag,
        viewport_size.y,
        state.orbit_yaw,
        state.orbit_pitch,
        state.orbit_distance,
        state.preview_shape,
        Vec2::new(doc.0.size.x as f32, doc.0.size.y as f32),
    );
    let Some(path) = state.selected.clone() else {
        state.status = "Select a layer to move it in 3D".into();
        return false;
    };
    state.ensure_undo_gesture(&doc.0);
    let Some(layer) = doc.0.layer_by_path_mut(&path) else {
        return false;
    };
    match apply_layer_delta(layer, mode, doc_delta) {
        Ok(()) => {
            state.mark_dirty();
            true
        }
        Err(message) => {
            if state.status != message {
                state.status = message.into();
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::PreviewShape;
    use glam::UVec2;

    fn bounds_of(layer: &Layer) -> ns_core::Bounds {
        match &layer.kind {
            LayerKind::Shape(shape) => shape.source.bounds().expect("shape bounds"),
            _ => panic!("expected a shape layer"),
        }
    }

    fn shape_layer() -> Layer {
        Layer::shape(
            "panel",
            ShapeSource::Primitive(ns_core::Primitive::Rect {
                center: Vec2::splat(64.0),
                size: Vec2::splat(40.0),
                corner_radius: 4.0,
            }),
        )
    }

    #[test]
    fn front_view_drag_maps_to_document_axes() {
        let size = Vec2::splat(1024.0);
        // Camera at +Z looking at the origin: screen right is world +X and
        // screen down is world -Y, which is document +Y.
        let right = screen_delta_to_doc(
            egui::vec2(100.0, 0.0),
            600.0,
            0.0,
            0.0,
            3.0,
            PreviewShape::Plane,
            size,
        );
        assert!(right.x > 0.0 && right.y.abs() < 1e-4, "{right:?}");
        let down = screen_delta_to_doc(
            egui::vec2(0.0, 100.0),
            600.0,
            0.0,
            0.0,
            3.0,
            PreviewShape::Plane,
            size,
        );
        assert!(down.y > 0.0 && down.x.abs() < 1e-4, "{down:?}");
        // 2 * 3 * tan(19 deg) world units across 600 px.
        let expected = 100.0 * (2.0 * 3.0 * (PREVIEW_FOV * 0.5).tan() / 600.0) / 1.1 * 1024.0;
        assert!(
            (right.x - expected).abs() < 1e-3,
            "{} vs {expected}",
            right.x
        );
    }

    #[test]
    fn moving_a_layer_offsets_its_outline() {
        let mut layer = shape_layer();
        let before = bounds_of(&layer).center();
        apply_layer_delta(&mut layer, PreviewGizmo::Move, Vec2::new(12.0, -5.0)).unwrap();
        let after = bounds_of(&layer).center();
        assert!((after - before - Vec2::new(12.0, -5.0)).length() < 0.01);
    }

    #[test]
    fn stretching_keeps_the_centre_and_grows_one_axis() {
        let mut layer = shape_layer();
        let before = bounds_of(&layer);
        apply_layer_delta(&mut layer, PreviewGizmo::Stretch, Vec2::new(20.0, 0.0)).unwrap();
        let after = bounds_of(&layer);
        assert!((after.center() - before.center()).length() < 0.01);
        assert!(after.size().x > before.size().x + 15.0);
        assert!((after.size().y - before.size().y).abs() < 0.01);
    }

    #[test]
    fn non_shape_layers_report_a_message() {
        let mut layer = Layer::flat("flat");
        assert!(apply_layer_delta(&mut layer, PreviewGizmo::Move, Vec2::splat(4.0)).is_err());
    }

    #[test]
    fn drags_do_nothing_when_the_gizmo_is_off() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(128)));
        doc.0.layers.push(shape_layer());
        let mut state = EditorState {
            selected: Some(vec![0]),
            preview_gizmo: PreviewGizmo::Off,
            ..Default::default()
        };
        assert!(!drag_selected_layer(
            &mut state,
            &mut doc,
            egui::vec2(600.0, 400.0),
            egui::vec2(30.0, 30.0),
        ));
    }
}
