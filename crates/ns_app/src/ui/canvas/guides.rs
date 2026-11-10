//! Smart alignment guides while drawing.
//!
//! Illustrator-style guides: while the pen places a node it looks at the
//! vertices and straight edges of the visible shapes (and of the draft itself)
//! and pulls the pending node onto them, drawing the constraint so it is clear
//! what the node aligned to. Vertex snapping wins over axis alignment, which
//! wins over parallel/perpendicular, matching what a point is most obviously
//! near. Guides are authoring-only: nothing is written to the document.
//!
//! Shift first projects the point onto its fixed 45 degree or ruler angle, and
//! the guides can then pull that point onto nearby geometry. Keeping the guide
//! pass means horizontal/vertical alignment still snaps while Shift is held; a
//! cut line only drifts when a visible guide is within the snap tolerance.

use super::*;

use std::collections::HashSet;

/// Snap distance in screen pixels.
pub(crate) const GUIDE_TOLERANCE_PX: f32 = 8.0;

/// Resolve where the pen should place a node.
///
/// Shift first offers the raw pointer to the smart guides, so axis and vertex
/// snapping (including a horizontal guide) keep working while Shift is held.
/// When no guide is near, Shift falls back to its fixed 45 degree or ruler
/// angle. Without Shift the pointer is snapped directly. The guides that
/// constrained the point are returned for the canvas overlay.
pub(crate) fn pen_place(
    state: &EditorState,
    doc: &DocumentRes,
    view: &CanvasView,
    pointer: Vec2,
    shift: bool,
) -> (Vec2, Vec<SmartGuide>) {
    if state.smart_guides {
        if let Some(draft) = &state.vector_draft {
            if draft.drag_index.is_none() {
                let (snapped, guides) =
                    smart_snap(draft, doc, pointer, GUIDE_TOLERANCE_PX / view.scale());
                if !shift || !guides.is_empty() {
                    return (snapped, guides);
                }
            }
        }
    }
    (snapped_pen_point(state, pointer, shift), Vec::new())
}

/// Resolve where a straight line cut should end.
///
/// The smart guides get the first chance, so the cut can end exactly on a
/// vertex, an axis, or parallel/perpendicular to a visible edge. Shift falls
/// back to its fixed 45° or ruler angle when no guide is near, mirroring the
/// pen; when a guide is near, the guide wins so an axis snap is never lost.
pub(crate) fn line_place(
    state: &EditorState,
    doc: &DocumentRes,
    view: &CanvasView,
    start: Vec2,
    pointer: Vec2,
    shift: bool,
) -> (Vec2, Vec<SmartGuide>) {
    if state.smart_guides {
        // One node is enough: the pending segment is start → pointer, and the
        // start itself is filtered out as a target by `smart_snap`.
        let draft = VectorDraft {
            nodes: vec![Node::corner(start)],
            ..Default::default()
        };
        let (snapped, guides) = smart_snap(&draft, doc, pointer, GUIDE_TOLERANCE_PX / view.scale());
        if !shift || !guides.is_empty() {
            return (snapped, guides);
        }
    }
    (
        project_onto(start, pointer, snap_angle(start, pointer, state.ruler)),
        Vec::new(),
    )
}

/// Resolve a point for the polygon lasso and the cable tool.
///
/// Both place corners one click at a time like the pen, so they use the same
/// smart-guide targets: document vertices and edges, plus the corners already
/// placed, so the point can sit exactly along an existing axis or edge. Unlike
/// the pen there is no Shift angle lock here because Shift restores the lassoed
/// region instead of constraining the point.
pub(crate) fn polyline_place(
    state: &EditorState,
    doc: &DocumentRes,
    view: &CanvasView,
    pointer: Vec2,
) -> (Vec2, Vec<SmartGuide>) {
    if !state.smart_guides {
        return (pointer, Vec::new());
    }
    let nodes = state
        .lasso_draft
        .as_ref()
        .map(|draft| {
            draft
                .points
                .iter()
                .map(|point| Node::corner(*point))
                .collect()
        })
        .unwrap_or_default();
    let draft = VectorDraft {
        nodes,
        ..Default::default()
    };
    smart_snap(&draft, doc, pointer, GUIDE_TOLERANCE_PX / view.scale())
}

/// Resolve a corner for the rectangular and elliptical lassos.
///
/// The selected layer's box is the first reference: its centre, the four edge
/// midpoints and the four corners snap when the pointer is near, so a cleanup
/// box can start or end exactly on the middle of the object. Everything else
/// falls back to the same smart-guide targets the pen uses, with the corners
/// already placed as extra references.
pub(crate) fn lasso_place(
    state: &EditorState,
    doc: &DocumentRes,
    view: &CanvasView,
    pointer: Vec2,
    anchors: &[Vec2],
) -> (Vec2, Vec<SmartGuide>) {
    if !state.smart_guides {
        return (pointer, Vec::new());
    }
    let tolerance = GUIDE_TOLERANCE_PX / view.scale();
    if let Some(bounds) = selection_bounds(&doc.0, state) {
        let center = bounds.center();
        let candidates = [
            center,
            Vec2::new(center.x, bounds.min.y),
            Vec2::new(center.x, bounds.max.y),
            Vec2::new(bounds.min.x, center.y),
            Vec2::new(bounds.max.x, center.y),
            bounds.min,
            Vec2::new(bounds.max.x, bounds.min.y),
            Vec2::new(bounds.min.x, bounds.max.y),
            bounds.max,
        ];
        if let Some(point) = candidates
            .into_iter()
            .filter(|candidate| candidate.distance(pointer) <= tolerance)
            .min_by(|a, b| a.distance(pointer).total_cmp(&b.distance(pointer)))
        {
            let kind = if point.distance(center) < 1e-4 {
                GuideKind::Center
            } else {
                GuideKind::Edge
            };
            return (
                point,
                vec![
                    SmartGuide {
                        kind,
                        anchor: point,
                        direction: Vec2::Y,
                        segment: None,
                    },
                    SmartGuide {
                        kind,
                        anchor: point,
                        direction: Vec2::X,
                        segment: None,
                    },
                ],
            );
        }
    }
    let draft = VectorDraft {
        nodes: anchors.iter().map(|point| Node::corner(*point)).collect(),
        ..Default::default()
    };
    smart_snap(&draft, doc, pointer, tolerance)
}

/// A parallel/perpendicular candidate from the nearest-edge search.
struct AngleSnap {
    distance: f32,
    point: Vec2,
    kind: GuideKind,
    direction: Vec2,
    segment: (Vec2, Vec2),
    /// Candidate from the segment that ends at the pen anchor.
    latest: bool,
}

/// Snap `pointer` to the nearest vertex, axis or parallel/perpendicular
/// constraint among the draft and the document's visible shapes.
pub(crate) fn smart_snap(
    draft: &VectorDraft,
    doc: &DocumentRes,
    pointer: Vec2,
    tolerance: f32,
) -> (Vec2, Vec<SmartGuide>) {
    let tolerance = tolerance.max(0.25);
    let mut points = Vec::new();
    let mut segments = Vec::new();
    // Every visible outline contributes vertices and straight edges. The
    // collector also provides box/canvas targets for the editing gestures, but
    // the pen only consumes points and segments, exactly as before.
    let targets = crate::snapping::collect_targets(&doc.0, &HashSet::new());
    points.extend(targets.points.iter().map(|target| target.pos));
    segments.extend(targets.segments.iter().map(|target| (target.a, target.b)));
    // What has already been drawn is a reference too: aligning the next node
    // to an earlier one is the common case.
    points.extend(draft.nodes.iter().map(|node| node.pos));
    let draft_segments_start = segments.len();
    for pair in draft.nodes.windows(2) {
        segments.push((pair[0].pos, pair[1].pos));
    }
    // The segment ending at the anchor is the one the pending segment grows
    // from, so it wins ties against older edges with the same orientation.
    let latest_segment = (segments.len() > draft_segments_start).then(|| segments.len() - 1);
    let anchor = draft.nodes.last().map(|node| node.pos);
    if let Some(anchor) = anchor {
        // The anchor itself is not a useful target: snapping a node onto the
        // one it grows from would only make a zero-length segment.
        points.retain(|point| point.distance(anchor) > tolerance * 0.5);
    }

    // 1. Vertices: the strongest alignment, so they win over everything else.
    if let Some(point) = nearest_vertex(&points, pointer, tolerance) {
        return (
            point,
            vec![SmartGuide {
                kind: GuideKind::Vertex,
                anchor: point,
                direction: Vec2::ZERO,
                segment: None,
            }],
        );
    }

    // 2. Axis alignment, solved per axis so a corner can be formed from the X
    //    of one vertex and the Y of another.
    let vertical = nearest_axis(&points, pointer, tolerance, true);
    let horizontal = nearest_axis(&points, pointer, tolerance, false);
    let axis_cost = [
        vertical.map(|(distance, _)| distance),
        horizontal.map(|(distance, _)| distance),
    ]
    .into_iter()
    .flatten()
    .fold(f32::INFINITY, f32::min);

    // 3. Parallel / perpendicular to an edge, measured from the previous node
    //    so it is the pending segment that lines up.
    let angular = anchor
        .and_then(|anchor| nearest_angle(&segments, anchor, pointer, tolerance, latest_segment));

    if let (Some(anchor), Some(candidate)) =
        (anchor, angular.filter(|snap| snap.distance <= axis_cost))
    {
        // The angular guide fixes the pending segment's direction. The free
        // axis can still snap its endpoint, which is what turns a square's
        // last edge into an exact corner instead of a near miss. Only axis-
        // aligned guides combine; a diagonal guide cannot take an axis snap
        // without leaving its line.
        let mut point = candidate.point;
        let mut axis_guide = None;
        if candidate.direction.y.abs() < 0.05 {
            if let Some((_, reference)) =
                nearest_axis_avoiding(&points, point, tolerance, true, candidate.segment)
            {
                point.x = reference.x;
                axis_guide = Some(SmartGuide {
                    kind: GuideKind::Vertical,
                    anchor: reference,
                    direction: Vec2::Y,
                    segment: None,
                });
            }
        } else if candidate.direction.x.abs() < 0.05 {
            if let Some((_, reference)) =
                nearest_axis_avoiding(&points, point, tolerance, false, candidate.segment)
            {
                point.y = reference.y;
                axis_guide = Some(SmartGuide {
                    kind: GuideKind::Horizontal,
                    anchor: reference,
                    direction: Vec2::X,
                    segment: None,
                });
            }
        }
        let mut guides = vec![SmartGuide {
            kind: candidate.kind,
            anchor,
            direction: candidate.direction,
            segment: Some(candidate.segment),
        }];
        guides.extend(axis_guide);
        return (point, guides);
    }

    let mut snapped = pointer;
    let mut guides = Vec::new();
    if let Some((_, reference)) = vertical {
        snapped.x = reference.x;
        guides.push(SmartGuide {
            kind: GuideKind::Vertical,
            anchor: reference,
            direction: Vec2::Y,
            segment: None,
        });
    }
    if let Some((_, reference)) = horizontal {
        snapped.y = reference.y;
        guides.push(SmartGuide {
            kind: GuideKind::Horizontal,
            anchor: reference,
            direction: Vec2::X,
            segment: None,
        });
    }
    (snapped, guides)
}

fn nearest_vertex(points: &[Vec2], pointer: Vec2, tolerance: f32) -> Option<Vec2> {
    let mut best: Option<(f32, Vec2)> = None;
    for point in points {
        let distance = point.distance(pointer);
        if distance <= tolerance && best.is_none_or(|(closest, _)| distance < closest) {
            best = Some((distance, *point));
        }
    }
    best.map(|(_, point)| point)
}

/// Nearest vertex sharing `pointer`'s X (`vertical`) or Y, with its distance.
fn nearest_axis(
    points: &[Vec2],
    pointer: Vec2,
    tolerance: f32,
    vertical: bool,
) -> Option<(f32, Vec2)> {
    let mut best: Option<(f32, Vec2)> = None;
    for point in points {
        let distance = if vertical {
            (pointer.x - point.x).abs()
        } else {
            (pointer.y - point.y).abs()
        };
        if distance <= tolerance && best.is_none_or(|(closest, _)| distance < closest) {
            best = Some((distance, *point));
        }
    }
    best
}

/// Like [`nearest_axis`], but never returns an endpoint of the angular guide's
/// reference segment. Landing on that edge is not useful (it is the edge the
/// guide already came from), and would stop a square's free corner from
/// snapping to the opposite corner instead.
fn nearest_axis_avoiding(
    points: &[Vec2],
    pointer: Vec2,
    tolerance: f32,
    vertical: bool,
    segment: (Vec2, Vec2),
) -> Option<(f32, Vec2)> {
    let mut best: Option<(f32, Vec2)> = None;
    for point in points {
        if point.distance(segment.0) < 1e-3 || point.distance(segment.1) < 1e-3 {
            continue;
        }
        let distance = if vertical {
            (pointer.x - point.x).abs()
        } else {
            (pointer.y - point.y).abs()
        };
        if distance <= tolerance && best.is_none_or(|(closest, _)| distance < closest) {
            best = Some((distance, *point));
        }
    }
    best
}

fn nearest_angle(
    segments: &[(Vec2, Vec2)],
    anchor: Vec2,
    pointer: Vec2,
    tolerance: f32,
    latest: Option<usize>,
) -> Option<AngleSnap> {
    let mut best: Option<AngleSnap> = None;
    for (index, (a, b)) in segments.iter().enumerate() {
        let span = *b - *a;
        let length = span.length();
        if length < 1e-3 {
            continue;
        }
        let along = span / length;
        let recent = Some(index) == latest;
        for (kind, direction) in [
            (GuideKind::Parallel, along),
            (GuideKind::Perpendicular, Vec2::new(-along.y, along.x)),
        ] {
            // The guide is the line through the anchor: a node only moves
            // sideways onto it, so its distance along the line is free.
            let travel = (pointer - anchor).dot(direction);
            if travel.abs() < 1.0 {
                continue;
            }
            let point = anchor + direction * travel;
            let distance = point.distance(pointer);
            if distance > tolerance {
                continue;
            }
            let better = match &best {
                None => true,
                Some(best) => {
                    distance < best.distance - 1e-6
                        || ((distance - best.distance).abs() <= 1e-6 && recent && !best.latest)
                }
            };
            if better {
                best = Some(AngleSnap {
                    distance,
                    point,
                    kind,
                    direction,
                    segment: (*a, *b),
                    latest: recent,
                });
            }
        }
    }
    best
}

/// Angle of a guide line, folded to 0..180 so parallel and anti-parallel
/// directions read the same.
fn line_angle(direction: Vec2) -> f32 {
    direction
        .y
        .atan2(direction.x)
        .to_degrees()
        .rem_euclid(180.0)
}

fn guide_color(kind: GuideKind) -> egui::Color32 {
    match kind {
        GuideKind::Vertex => PRIMARY,
        GuideKind::Horizontal | GuideKind::Vertical | GuideKind::Edge | GuideKind::Center => {
            egui::Color32::from_rgb(255, 105, 210)
        }
        GuideKind::Canvas => PRIMARY_COOL,
        GuideKind::Parallel => egui::Color32::from_rgb(96, 226, 214),
        // Tertiary violet keeps perpendicular distinct from the cyan vertex
        // accent and the teal parallel guide.
        GuideKind::Perpendicular => egui::Color32::from_rgb(0xDD, 0xB4, 0xFF),
    }
}

/// Paint guide lines and markers; returns the last colour and display labels.
fn paint_guides(
    painter: &egui::Painter,
    view: &CanvasView,
    guides: &[SmartGuide],
) -> (egui::Color32, Vec<String>) {
    if guides.is_empty() {
        return (MUTED, Vec::new());
    }
    let mut parts: Vec<String> = Vec::with_capacity(guides.len());
    let mut color = guide_color(guides[0].kind);
    for guide in guides {
        color = guide_color(guide.kind);
        let anchor = view.doc_to_screen(guide.anchor);
        if guide.direction.length_squared() > 1e-6 {
            let along = view.doc_to_screen(guide.anchor + guide.direction) - anchor;
            if along.length() > 1e-3 {
                if let Some((start, end)) =
                    clip_line(anchor, along.normalized(), painter.clip_rect())
                {
                    painter.add(egui::Shape::dashed_line(
                        &[start, end],
                        egui::Stroke::new(1.0_f32, color.gamma_multiply(0.85)),
                        5.0,
                        4.0,
                    ));
                }
            }
        }
        if let Some((a, b)) = guide.segment {
            // Show the edge the segment is parallel/perpendicular to.
            painter.line_segment(
                [view.doc_to_screen(a), view.doc_to_screen(b)],
                egui::Stroke::new(3.0_f32, color.gamma_multiply(0.4)),
            );
        }
        let marker = egui::Rect::from_center_size(anchor, egui::vec2(7.0, 7.0));
        if guide.kind == GuideKind::Vertex {
            painter.rect_filled(marker, 1.0, color);
        }
        painter.rect_stroke(
            marker,
            1.0,
            egui::Stroke::new(1.4_f32, color),
            egui::StrokeKind::Outside,
        );
        parts.push(match guide.kind {
            GuideKind::Parallel | GuideKind::Perpendicular => {
                format!("{} {:.1}°", guide.kind.label(), line_angle(guide.direction))
            }
            kind => kind.label().to_string(),
        });
    }
    (color, parts)
}

/// Draw the pen's guides and a readout beside the pointer.
pub(crate) fn draw_smart_guides(painter: &egui::Painter, view: &CanvasView, draft: &VectorDraft) {
    if draft.guides.is_empty() {
        return;
    }
    let (color, parts) = paint_guides(painter, view, &draft.guides);

    // Readout: the constraint plus the pending segment's length.
    let Some(hover) = draft.hover else {
        return;
    };
    let length = draft
        .nodes
        .last()
        .map_or(0.0, |node| node.pos.distance(hover));
    let text = format!("{} · {length:.0} px", parts.join(" + "));
    let galley = painter.layout_no_wrap(text, egui::FontId::monospace(11.0), egui::Color32::WHITE);
    let pointer = view.doc_to_screen(hover);
    let pos = egui::pos2(pointer.x + 16.0, pointer.y - galley.size().y - 14.0);
    let chip = egui::Rect::from_min_size(pos, galley.size()).expand2(egui::vec2(5.0, 3.0));
    painter.rect_filled(chip, 3.0, egui::Color32::from_black_alpha(180));
    painter.rect_stroke(
        chip,
        3.0,
        egui::Stroke::new(1.0_f32, color),
        egui::StrokeKind::Inside,
    );
    painter.galley(pos, galley, color);
}

/// Draw guides produced by an editing gesture (move or duplicate drag).
pub(crate) fn draw_active_guides(
    painter: &egui::Painter,
    view: &CanvasView,
    guides: &[SmartGuide],
) {
    let _ = paint_guides(painter, view, guides);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_doc(visible: bool) -> DocumentRes {
        let mut layer = Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(100.0, 100.0),
                size: Vec2::splat(60.0),
                corner_radius: 0.0,
            }),
        );
        layer.visible = visible;
        layer.touch();
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        doc.0.layers.push(layer);
        doc
    }

    fn edge_doc() -> DocumentRes {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(40.0, 60.0)));
        subpath.nodes.push(Node::corner(Vec2::new(120.0, 60.0)));
        path.subpaths.push(subpath);
        let mut layer = Layer::shape("edge", ShapeSource::Path(path));
        layer.touch();
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        doc.0.layers.push(layer);
        doc
    }

    fn draft_at(nodes: &[Vec2]) -> VectorDraft {
        VectorDraft {
            nodes: nodes.iter().map(|point| Node::corner(*point)).collect(),
            ..Default::default()
        }
    }

    fn close(a: Vec2, b: Vec2) -> bool {
        a.distance(b) < 1e-3
    }

    #[test]
    fn a_vertex_snaps_straight_onto_the_node() {
        let doc = rect_doc(true);
        let draft = draft_at(&[Vec2::new(20.0, 20.0)]);
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(72.0, 73.0), 8.0);
        assert!(close(snapped, Vec2::new(70.0, 70.0)));
        assert_eq!(guides.len(), 1);
        assert_eq!(guides[0].kind, GuideKind::Vertex);
        assert!(close(guides[0].anchor, Vec2::new(70.0, 70.0)));
    }

    #[test]
    fn the_first_node_can_snap_to_an_existing_vertex() {
        let doc = rect_doc(true);
        let draft = draft_at(&[]);
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(131.0, 129.0), 8.0);
        assert!(close(snapped, Vec2::new(130.0, 130.0)));
        assert_eq!(guides[0].kind, GuideKind::Vertex);
    }

    #[test]
    fn axis_alignment_snaps_one_coordinate_at_a_time() {
        let doc = rect_doc(true);
        let draft = draft_at(&[Vec2::new(20.0, 20.0)]);
        // Same X as the left edge, far from any corner's Y.
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(73.0, 100.0), 8.0);
        assert!(close(snapped, Vec2::new(70.0, 100.0)));
        assert_eq!(guides.len(), 1);
        assert_eq!(guides[0].kind, GuideKind::Vertical);
        // Same Y as the bottom edge, far from any corner's X.
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(100.0, 127.0), 8.0);
        assert!(close(snapped, Vec2::new(100.0, 130.0)));
        assert_eq!(guides.len(), 1);
        assert_eq!(guides[0].kind, GuideKind::Horizontal);
    }

    #[test]
    fn parallel_guides_follow_an_existing_edge() {
        let doc = rect_doc(true);
        // The anchor sits left of the panel; the pointer is a hair off the
        // line of its top edge.
        let draft = draft_at(&[Vec2::new(20.0, 90.0)]);
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(100.0, 93.0), 8.0);
        assert!(close(snapped, Vec2::new(100.0, 90.0)));
        assert_eq!(guides.len(), 1);
        assert!(matches!(
            guides[0].kind,
            GuideKind::Parallel | GuideKind::Perpendicular
        ));
        assert!(guides[0].segment.is_some(), "the reference edge is shown");
    }

    #[test]
    fn perpendicular_guides_turn_a_corner() {
        let doc = edge_doc();
        let draft = draft_at(&[Vec2::new(100.0, 20.0)]);
        // Straight down from the anchor: perpendicular to the horizontal edge,
        // and closer than the horizontal alignment at y = 60.
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(100.0, 63.0), 8.0);
        assert!(close(snapped, Vec2::new(100.0, 63.0)));
        assert_eq!(guides.len(), 1);
        assert_eq!(guides[0].kind, GuideKind::Perpendicular);
    }

    #[test]
    fn a_square_corner_combines_perpendicular_and_axis_snaps() {
        // L-shaped draft: bottom edge, then the left edge. The pending top
        // edge should be perpendicular to the left edge *and* end above the
        // first node, closing the square's corner.
        let draft = draft_at(&[
            Vec2::new(100.0, 100.0),
            Vec2::new(20.0, 100.0),
            Vec2::new(20.0, 20.0),
        ]);
        let doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(99.0, 21.0), 8.0);
        assert!(close(snapped, Vec2::new(100.0, 20.0)));
        assert!(
            guides
                .iter()
                .any(|guide| guide.kind == GuideKind::Perpendicular),
            "the top edge turns the corner"
        );
        assert!(
            guides.iter().any(|guide| guide.kind == GuideKind::Vertical),
            "the endpoint aligns with the first node"
        );
    }

    #[test]
    fn the_last_edge_closes_the_square_on_both_axes() {
        // Three sides drawn; the pending right edge should be vertical and end
        // exactly on the bottom-right node.
        let draft = draft_at(&[
            Vec2::new(100.0, 100.0),
            Vec2::new(20.0, 100.0),
            Vec2::new(20.0, 20.0),
            Vec2::new(100.0, 20.0),
        ]);
        let doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        // Keep the pointer outside the 2D vertex radius so the combined axis +
        // direction path is what closes the corner.
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(101.0, 92.0), 8.0);
        assert!(close(snapped, Vec2::new(100.0, 100.0)));
        assert!(guides
            .iter()
            .any(|guide| guide.kind == GuideKind::Perpendicular));
        assert!(guides
            .iter()
            .any(|guide| guide.kind == GuideKind::Horizontal));
    }

    #[test]
    fn the_draft_aligns_to_its_own_nodes() {
        let doc = DocumentRes::new(Document::new(UVec2::splat(200)));
        // The anchor is the last node; the earlier one still offers its X.
        let draft = draft_at(&[Vec2::new(120.0, 90.0), Vec2::new(40.0, 40.0)]);
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(123.0, 150.0), 8.0);
        assert!(close(snapped, Vec2::new(120.0, 150.0)));
        assert_eq!(guides.len(), 1);
        assert_eq!(guides[0].kind, GuideKind::Vertical);
    }

    #[test]
    fn hidden_layers_are_not_references() {
        let doc = rect_doc(false);
        let draft = draft_at(&[Vec2::new(20.0, 20.0)]);
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(72.0, 73.0), 8.0);
        assert!(close(snapped, Vec2::new(72.0, 73.0)));
        assert!(guides.is_empty());
    }

    #[test]
    fn nothing_snaps_once_the_pointer_is_clear() {
        let doc = rect_doc(true);
        let draft = draft_at(&[Vec2::new(20.0, 20.0)]);
        let (snapped, guides) = smart_snap(&draft, &doc, Vec2::new(180.0, 190.0), 8.0);
        assert!(close(snapped, Vec2::new(180.0, 190.0)));
        assert!(guides.is_empty());
    }

    #[test]
    fn shift_constrains_the_angle_but_still_snaps_to_guides() {
        let doc = rect_doc(true);
        let state = EditorState {
            vector_draft: Some(draft_at(&[Vec2::new(100.0, 100.0)])),
            ..Default::default()
        };
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );
        // Shift still projects onto the 45° ray, but a nearby vertex wins.
        let (point, guides) = pen_place(&state, &doc, &view, Vec2::new(72.0, 72.0), true);
        assert!(close(point, Vec2::new(70.0, 70.0)));
        assert!(!guides.is_empty());
        // A horizontal guide still snaps with Shift, even though the 45° ray
        // would have placed the point at another Y.
        let (point, guides) = pen_place(&state, &doc, &view, Vec2::new(180.0, 131.0), true);
        assert!(close(point, Vec2::new(180.0, 130.0)));
        assert!(guides
            .iter()
            .any(|guide| guide.kind == GuideKind::Horizontal));
        // Far from geometry the fixed angle is kept and no guide is drawn.
        let (point, guides) = pen_place(&state, &doc, &view, Vec2::new(160.0, 170.0), true);
        assert!(close(
            point,
            snapped_pen_point(&state, Vec2::new(160.0, 170.0), true)
        ));
        assert!(guides.is_empty());
        // Turning the feature off leaves the constrained point untouched.
        let state = EditorState {
            smart_guides: false,
            ..state
        };
        let (point, guides) = pen_place(&state, &doc, &view, Vec2::new(72.0, 72.0), false);
        assert!(close(point, Vec2::new(72.0, 72.0)));
        assert!(guides.is_empty());
    }

    #[test]
    fn a_line_cut_snaps_to_a_vertex() {
        let doc = rect_doc(true);
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );
        let state = EditorState::default();

        let (snapped, guides) = line_place(
            &state,
            &doc,
            &view,
            Vec2::new(20.0, 20.0),
            Vec2::new(127.0, 127.0),
            false,
        );

        assert!(
            snapped.distance(Vec2::new(130.0, 130.0)) < 1e-3,
            "{snapped}"
        );
        assert!(guides.iter().any(|guide| guide.kind == GuideKind::Vertex));
    }

    #[test]
    fn shift_lines_fall_back_to_45_degrees_without_guides() {
        let doc = DocumentRes::new(Document::new(glam::UVec2::splat(200)));
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );
        let state = EditorState::default();

        let (snapped, guides) = line_place(
            &state,
            &doc,
            &view,
            Vec2::new(10.0, 10.0),
            Vec2::new(80.0, 50.0),
            true,
        );

        assert!(guides.is_empty());
        assert!((snapped.x - snapped.y).abs() < 1e-3, "{snapped}");
        assert!(snapped.distance(Vec2::new(65.0, 65.0)) < 1e-3, "{snapped}");
    }

    fn polygon_state(points: &[Vec2]) -> EditorState {
        EditorState {
            canvas_tool: CanvasTool::LassoPolygon,
            lasso_draft: Some(LassoDraft {
                points: points.to_vec(),
                hover: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn polygon_lasso_points_snap_to_document_vertices() {
        let doc = rect_doc(true);
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );
        let state = polygon_state(&[Vec2::new(20.0, 20.0)]);

        let (snapped, guides) = polyline_place(&state, &doc, &view, Vec2::new(127.0, 127.0));

        assert!(
            snapped.distance(Vec2::new(130.0, 130.0)) < 1e-3,
            "{snapped}"
        );
        assert!(guides.iter().any(|guide| guide.kind == GuideKind::Vertex));
    }

    #[test]
    fn polygon_lasso_aligns_with_the_corners_already_placed() {
        let doc = DocumentRes::new(Document::new(glam::UVec2::splat(200)));
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );
        let state = polygon_state(&[Vec2::new(50.0, 50.0), Vec2::new(90.0, 40.0)]);

        let (snapped, guides) = polyline_place(&state, &doc, &view, Vec2::new(53.0, 120.0));

        assert!((snapped.x - 50.0).abs() < 1e-3, "{snapped}");
        assert!(snapped.y > 100.0, "only the x axis should snap: {snapped}");
        assert!(guides.iter().any(|guide| guide.kind == GuideKind::Vertical));
    }

    #[test]
    fn polygon_lasso_respects_the_smart_guides_setting() {
        let doc = rect_doc(true);
        let view = CanvasView::for_test(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        );
        let mut state = polygon_state(&[Vec2::new(20.0, 20.0)]);
        state.smart_guides = false;

        let (snapped, guides) = polyline_place(&state, &doc, &view, Vec2::new(127.0, 127.0));

        assert!(snapped.distance(Vec2::new(127.0, 127.0)) < 1e-3);
        assert!(guides.is_empty());
    }

    fn lasso_view() -> CanvasView {
        CanvasView::for_test(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0)),
            Vec2::splat(200.0),
        )
    }

    /// The rectangle lasso's corners snap to the selected layer's box: centre,
    /// edge midpoints and corners, before the regular smart guides.
    #[test]
    fn a_lasso_corner_snaps_to_the_selected_objects_box() {
        // 60 px square centred on (100, 100): 70..130 on both axes.
        let doc = rect_doc(true);
        let state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        let view = lasso_view();

        let (centre, guides) = lasso_place(&state, &doc, &view, Vec2::new(103.0, 98.0), &[]);
        assert!(close(centre, Vec2::new(100.0, 100.0)), "{centre}");
        assert!(guides.iter().any(|guide| guide.kind == GuideKind::Center));

        let (edge, _) = lasso_place(&state, &doc, &view, Vec2::new(101.0, 72.0), &[]);
        assert!(close(edge, Vec2::new(100.0, 70.0)), "{edge}");

        let (corner, _) = lasso_place(&state, &doc, &view, Vec2::new(128.0, 129.0), &[]);
        assert!(close(corner, Vec2::new(130.0, 130.0)), "{corner}");
    }

    #[test]
    fn a_lasso_corner_without_a_selection_uses_the_smart_guides() {
        let doc = rect_doc(true);
        let state = EditorState::default();
        let view = lasso_view();

        let (snapped, guides) = lasso_place(&state, &doc, &view, Vec2::new(72.0, 71.0), &[]);

        assert!(close(snapped, Vec2::new(70.0, 70.0)), "{snapped}");
        assert!(!guides.is_empty());
    }

    #[test]
    fn a_lasso_corner_ignores_the_box_when_smart_guides_are_off() {
        let doc = rect_doc(true);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        state.smart_guides = false;
        let view = lasso_view();

        let (snapped, guides) = lasso_place(&state, &doc, &view, Vec2::new(103.0, 98.0), &[]);

        assert!(close(snapped, Vec2::new(103.0, 98.0)));
        assert!(guides.is_empty());
    }
}
