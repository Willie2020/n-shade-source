//! Angle snapping for the drawing tools.
//!
//! Shift straightens a segment: onto the ruler's angle when a ruler is placed,
//! otherwise onto the nearest 45 degree step. Points are projected onto the
//! constrained line rather than rotated around the anchor, so the node follows
//! the pointer along the line and the length is easy to judge.

use super::*;

/// Shift step when no ruler is placed: 45 degrees.
pub(crate) const SNAP_STEP: f32 = std::f32::consts::FRAC_PI_4;

/// Step used while setting the ruler itself, so guides land on tidy angles.
pub(crate) const RULER_STEP: f32 = std::f32::consts::PI / 12.0;

/// Nearest multiple of `step` to the direction of `delta`.
pub(crate) fn step_angle(delta: Vec2, step: f32) -> f32 {
    if delta.length_squared() < 1e-8 {
        return 0.0;
    }
    (delta.y.atan2(delta.x) / step).round() * step
}

/// `point` projected onto the line through `anchor` running at `angle`.
pub(crate) fn project_onto(anchor: Vec2, point: Vec2, angle: f32) -> Vec2 {
    let direction = Vec2::from_angle(angle);
    anchor + direction * (point - anchor).dot(direction)
}

/// `vector` rotated onto `angle`, keeping its length and which way it faces.
pub(crate) fn align_vector(vector: Vec2, angle: f32) -> Vec2 {
    let direction = Vec2::from_angle(angle) * vector.length();
    if vector.dot(direction) < 0.0 {
        -direction
    } else {
        direction
    }
}

/// The angle Shift snaps a segment from `anchor` to `point` onto.
pub(crate) fn snap_angle(anchor: Vec2, point: Vec2, ruler: Option<RulerState>) -> f32 {
    match ruler {
        Some(ruler) => ruler.angle,
        None => step_angle(point - anchor, SNAP_STEP),
    }
}

/// Document point the pen should place for a node at `point`.
pub(crate) fn snapped_pen_point(state: &EditorState, point: Vec2, shift: bool) -> Vec2 {
    if !shift {
        return point;
    }
    let Some(anchor) = state
        .vector_draft
        .as_ref()
        .and_then(|draft| draft.nodes.last())
    else {
        return point;
    };
    project_onto(
        anchor.pos,
        point,
        snap_angle(anchor.pos, point, state.ruler),
    )
}

/// Handle offset the pen should use while dragging out symmetric handles.
pub(crate) fn snapped_pen_handle(
    anchor: Vec2,
    handle: Vec2,
    ruler: Option<RulerState>,
    shift: bool,
) -> Vec2 {
    if !shift {
        return handle;
    }
    align_vector(handle, snap_angle(anchor, anchor + handle, ruler))
}

/// Human-readable angle for status lines and the ruler readout.
pub(crate) fn angle_label(angle: f32) -> String {
    format!("{:.1}°", angle.to_degrees().rem_euclid(360.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ruler(angle_degrees: f32) -> RulerState {
        RulerState {
            origin: Vec2::new(100.0, 100.0),
            angle: angle_degrees.to_radians(),
        }
    }

    fn close(a: Vec2, b: Vec2) -> bool {
        a.distance(b) < 1e-4
    }

    #[test]
    fn shift_straightens_a_nearly_horizontal_segment() {
        let anchor = Vec2::new(10.0, 50.0);
        let point = Vec2::new(90.0, 54.0);
        let angle = snap_angle(anchor, point, None);
        assert!((angle.to_degrees()).abs() < 1e-3);
        assert!(close(
            project_onto(anchor, point, angle),
            Vec2::new(90.0, 50.0)
        ));
    }

    #[test]
    fn shift_rounds_to_the_nearest_45_degree_step() {
        let anchor = Vec2::ZERO;
        // 40 degrees is closer to 45 than to 0.
        let point = Vec2::from_angle(40.0_f32.to_radians()) * 100.0;
        let angle = snap_angle(anchor, point, None);
        assert!((angle.to_degrees() - 45.0).abs() < 1e-3);
        // 80 degrees snaps up to vertical.
        let point = Vec2::from_angle(80.0_f32.to_radians()) * 100.0;
        assert!((snap_angle(anchor, point, None).to_degrees() - 90.0).abs() < 1e-3);
        // 130 degrees snaps back to 135.
        let point = Vec2::from_angle(130.0_f32.to_radians()) * 100.0;
        assert!((snap_angle(anchor, point, None).to_degrees() - 135.0).abs() < 1e-3);
    }

    #[test]
    fn a_ruler_angle_replaces_the_steps() {
        let anchor = Vec2::new(20.0, 20.0);
        let point = Vec2::new(120.0, 60.0);
        let angle = snap_angle(anchor, point, Some(ruler(30.0)));
        assert!((angle.to_degrees() - 30.0).abs() < 1e-3);
        let snapped = project_onto(anchor, point, angle);
        // The snapped point sits exactly on the 30 degree line.
        let offset = snapped - anchor;
        assert!((offset.y.atan2(offset.x).to_degrees() - 30.0).abs() < 1e-3);
    }

    #[test]
    fn projecting_behind_the_anchor_stays_on_the_line() {
        let anchor = Vec2::new(100.0, 100.0);
        let point = Vec2::new(40.0, 100.0);
        let snapped = project_onto(anchor, point, 0.0);
        assert!(close(snapped, Vec2::new(40.0, 100.0)));
        // A point behind the ruler origin projects to the other side too.
        let snapped = project_onto(anchor, Vec2::new(100.0, 20.0), std::f32::consts::FRAC_PI_2);
        assert!(close(snapped, Vec2::new(100.0, 20.0)));
    }

    #[test]
    fn aligning_a_handle_keeps_its_length() {
        let handle = Vec2::new(3.0, 40.0);
        let aligned = align_vector(handle, 0.0);
        assert!(close(aligned, Vec2::new(handle.length(), 0.0)));
        // The handle keeps the side it was pointing at, so aligning to the
        // reverse of a mostly-down handle ends up pointing up.
        let flipped = align_vector(handle, std::f32::consts::PI);
        assert!(close(flipped, Vec2::new(handle.length(), 0.0)));
        let reversed = align_vector(Vec2::new(-3.0, 40.0), std::f32::consts::PI);
        assert!(close(reversed, Vec2::new(-handle.length(), 0.0)));
    }

    #[test]
    fn pen_snapping_uses_the_last_node_and_the_ruler() {
        let state = EditorState {
            vector_draft: Some(VectorDraft {
                nodes: vec![Node::corner(Vec2::new(100.0, 100.0))],
                ..Default::default()
            }),
            ..Default::default()
        };
        // Without Shift the point is untouched.
        assert!(close(
            snapped_pen_point(&state, Vec2::new(160.0, 130.0), false),
            Vec2::new(160.0, 130.0)
        ));
        // With Shift but no ruler the segment becomes horizontal: 9.5
        // degrees is closer to 0 than to 45.
        assert!(close(
            snapped_pen_point(&state, Vec2::new(160.0, 110.0), true),
            Vec2::new(160.0, 100.0)
        ));
        // A ruler overrides the 45 degree steps: the node moves onto the
        // ruler's 45 degree line through the previous node.
        let state = EditorState {
            ruler: Some(ruler(45.0)),
            ..state
        };
        let snapped = snapped_pen_point(&state, Vec2::new(160.0, 130.0), true);
        let expected = project_onto(
            Vec2::new(100.0, 100.0),
            Vec2::new(160.0, 130.0),
            45.0_f32.to_radians(),
        );
        assert!(close(snapped, expected));
        assert!(
            (snapped.y - snapped.x).abs() < 1e-3,
            "on the 45 degree line"
        );
    }

    #[test]
    fn the_first_node_is_never_snapped() {
        let state = EditorState::default();
        assert!(close(
            snapped_pen_point(&state, Vec2::new(160.0, 130.0), true),
            Vec2::new(160.0, 130.0)
        ));
    }

    #[test]
    fn pen_handles_snap_like_segments() {
        let handle = Vec2::new(30.0, -6.0);
        let snapped = snapped_pen_handle(Vec2::ZERO, handle, None, true);
        assert!(close(snapped, Vec2::new(handle.length(), 0.0)));
        let snapped = snapped_pen_handle(Vec2::ZERO, handle, Some(ruler(90.0)), true);
        assert!(close(snapped, Vec2::new(0.0, -handle.length())));
        assert!(close(
            snapped_pen_handle(Vec2::ZERO, handle, None, false),
            handle
        ));
    }

    #[test]
    fn angle_labels_wrap_into_a_readable_range() {
        assert_eq!(angle_label(std::f32::consts::PI), "180.0°");
        assert_eq!(angle_label(-std::f32::consts::FRAC_PI_2), "270.0°");
    }
}
