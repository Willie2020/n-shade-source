//! Smart-guide solver and target collection.
//!
//! Canvas-space geometry only: no egui, Bevy or `EditorState`. Every tool feeds
//! the same [`SnapTargets`] and receives a delta plus drawable guides, so the
//! rules stay identical between drawing and editing.
//!
//! Priority follows the pen's existing behavior: a vertex/point snap wins, then
//! the closest of an axis guide and a parallel/perpendicular guide. Axis values
//! are compared in canvas pixels; callers convert the screen tolerance once per
//! frame with `CanvasView::scale()`.
//!
//! Targets are collected once per gesture from the visible outlines. The module
//! is staged for the remaining gesture wiring; remove this allow once equal
//! spacing/size consume the rect targets.
#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::HashSet;

use glam::Vec2;
use ns_core::{Bounds, Document, Layer, LayerId, LayerKind, Path, Primitive, ShapeSource};

use crate::app::{GuideKind, SmartGuide};

/// Two axis values closer than this are the same guide line.
const SAME_VALUE: f32 = 1e-3;
/// A held snap releases only beyond this multiple of the tolerance.
const HINT_FACTOR: f32 = 1.5;
/// Canvas-pixel floor so a sub-pixel tolerance still resolves.
const MIN_TOLERANCE: f32 = 0.01;
/// Squared distance used to recognise the hinted point target.
const HINT_EPS_SQ: f32 = 1e-6;

/// One target line on the X or Y axis.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AxisTarget {
    pub value: f32,
    /// A point the guide line runs through, for drawing the overlay.
    pub anchor: Vec2,
    pub kind: GuideKind,
    pub source: Option<LayerId>,
}

/// A vertex, path anchor or primitive corner.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PointTarget {
    pub pos: Vec2,
    pub source: Option<LayerId>,
}

/// A straight edge used by parallel/perpendicular direction snaps.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SegmentTarget {
    pub a: Vec2,
    pub b: Vec2,
    pub source: Option<LayerId>,
}

/// A bounding box used by equal spacing/size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RectTarget {
    pub rect: Bounds,
    pub source: Option<LayerId>,
}

/// Sorted reference geometry for one gesture.
#[derive(Clone, Debug, Default)]
pub(crate) struct SnapTargets {
    pub xs: Vec<AxisTarget>,
    pub ys: Vec<AxisTarget>,
    pub points: Vec<PointTarget>,
    pub segments: Vec<SegmentTarget>,
    pub rects: Vec<RectTarget>,
}

impl SnapTargets {
    /// Sort every list; call once after adding targets and before snapping.
    pub(crate) fn finish(&mut self) {
        self.xs.sort_by(|a, b| {
            a.value
                .total_cmp(&b.value)
                .then_with(|| target_rank(a.kind).cmp(&target_rank(b.kind)))
                .then_with(|| a.source.cmp(&b.source))
        });
        self.ys.sort_by(|a, b| {
            a.value
                .total_cmp(&b.value)
                .then_with(|| target_rank(a.kind).cmp(&target_rank(b.kind)))
                .then_with(|| a.source.cmp(&b.source))
        });
        self.points
            .sort_by(|a, b| point_cmp(a.pos, b.pos).then_with(|| a.source.cmp(&b.source)));
        self.segments.sort_by(|a, b| {
            point_cmp(a.a, b.a)
                .then_with(|| point_cmp(a.b, b.b))
                .then_with(|| a.source.cmp(&b.source))
        });
        self.rects.sort_by(|a, b| {
            point_cmp(a.rect.min, b.rect.min)
                .then_with(|| point_cmp(a.rect.max, b.rect.max))
                .then_with(|| a.source.cmp(&b.source))
        });
    }

    /// A point target also produces its horizontal and vertical extensions.
    pub(crate) fn add_point(&mut self, pos: Vec2, source: Option<LayerId>) {
        if !pos.is_finite() {
            return;
        }
        self.points.push(PointTarget { pos, source });
        self.xs.push(AxisTarget {
            value: pos.x,
            anchor: pos,
            kind: GuideKind::Vertical,
            source,
        });
        self.ys.push(AxisTarget {
            value: pos.y,
            anchor: pos,
            kind: GuideKind::Horizontal,
            source,
        });
    }

    pub(crate) fn add_segment(&mut self, a: Vec2, b: Vec2, source: Option<LayerId>) {
        if !a.is_finite() || !b.is_finite() || a.distance_squared(b) < 1e-8 {
            return;
        }
        self.segments.push(SegmentTarget { a, b, source });
    }

    /// Bounding-box edges and the center, as axis guide targets.
    pub(crate) fn add_rect(
        &mut self,
        rect: Bounds,
        source: Option<LayerId>,
        edge_kind: GuideKind,
        center_kind: GuideKind,
    ) {
        if !rect.min.is_finite() || !rect.max.is_finite() {
            return;
        }
        let center = rect.center();
        self.xs.extend([
            AxisTarget {
                value: rect.min.x,
                anchor: Vec2::new(rect.min.x, center.y),
                kind: edge_kind,
                source,
            },
            AxisTarget {
                value: rect.max.x,
                anchor: Vec2::new(rect.max.x, center.y),
                kind: edge_kind,
                source,
            },
            AxisTarget {
                value: center.x,
                anchor: center,
                kind: center_kind,
                source,
            },
        ]);
        self.ys.extend([
            AxisTarget {
                value: rect.min.y,
                anchor: Vec2::new(center.x, rect.min.y),
                kind: edge_kind,
                source,
            },
            AxisTarget {
                value: rect.max.y,
                anchor: Vec2::new(center.x, rect.max.y),
                kind: edge_kind,
                source,
            },
            AxisTarget {
                value: center.y,
                anchor: center,
                kind: center_kind,
                source,
            },
        ]);
    }
}

/// Which guide families the solver may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SnapKinds {
    pub edges: bool,
    pub centers: bool,
    pub canvas: bool,
    pub vertices: bool,
    pub extensions: bool,
    pub direction: bool,
}

impl Default for SnapKinds {
    fn default() -> Self {
        Self {
            edges: true,
            centers: true,
            canvas: true,
            vertices: true,
            extensions: true,
            direction: true,
        }
    }
}

impl SnapKinds {
    fn allows(self, kind: GuideKind) -> bool {
        match kind {
            GuideKind::Vertex => self.vertices,
            GuideKind::Horizontal | GuideKind::Vertical => self.extensions,
            GuideKind::Edge => self.edges,
            GuideKind::Center => self.centers,
            GuideKind::Canvas => self.canvas,
            GuideKind::Parallel | GuideKind::Perpendicular => self.direction,
        }
    }
}

/// Per-frame snapping options. `tolerance` is already in canvas pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SnapOptions {
    pub tolerance: f32,
    pub kinds: SnapKinds,
}

/// Which sides of a rect follow the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MovingEdges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl MovingEdges {
    pub const ALL: Self = Self {
        left: true,
        right: true,
        top: true,
        bottom: true,
    };

    pub(crate) fn is_move(self) -> bool {
        self == Self::ALL
    }
}

/// The raw subject before snapping.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Subject {
    /// Cursor, node or lasso point.
    Point(Vec2),
    /// Bounding box of a moved/resized/created shape.
    Rect { rect: Bounds, moving: MovingEdges },
}

impl Subject {
    fn is_finite(self) -> bool {
        match self {
            Subject::Point(point) => point.is_finite(),
            Subject::Rect { rect, .. } => rect.min.is_finite() && rect.max.is_finite(),
        }
    }

    /// Values compared against vertical targets.
    ///
    /// A full move includes the center so the subject can center-align. A
    /// resize only compares the moving edge: the center would move half as far,
    /// so a center snap would need a different delta and is deliberately left
    /// out.
    fn x_values(self) -> ([f32; 3], usize) {
        match self {
            Subject::Point(point) => ([point.x, 0.0, 0.0], 1),
            Subject::Rect { rect, moving } if moving.is_move() => {
                ([rect.min.x, rect.center().x, rect.max.x], 3)
            }
            Subject::Rect { rect, moving } => {
                let mut values = [0.0; 3];
                let mut count = 0;
                if moving.left {
                    values[count] = rect.min.x;
                    count += 1;
                }
                if moving.right {
                    values[count] = rect.max.x;
                    count += 1;
                }
                (values, count)
            }
        }
    }

    fn y_values(self) -> ([f32; 3], usize) {
        match self {
            Subject::Point(point) => ([point.y, 0.0, 0.0], 1),
            Subject::Rect { rect, moving } if moving.is_move() => {
                ([rect.min.y, rect.center().y, rect.max.y], 3)
            }
            Subject::Rect { rect, moving } => {
                let mut values = [0.0; 3];
                let mut count = 0;
                if moving.top {
                    values[count] = rect.min.y;
                    count += 1;
                }
                if moving.bottom {
                    values[count] = rect.max.y;
                    count += 1;
                }
                (values, count)
            }
        }
    }

    /// Corners that can take a two-axis vertex snap. Only a whole-rect move
    /// probes corners; a resize would otherwise pull a fixed edge.
    fn point_probes(self) -> ([Vec2; 4], usize) {
        match self {
            Subject::Point(point) => ([point, Vec2::ZERO, Vec2::ZERO, Vec2::ZERO], 1),
            Subject::Rect { rect, moving } if moving.is_move() => (
                [
                    rect.min,
                    Vec2::new(rect.max.x, rect.min.y),
                    rect.max,
                    Vec2::new(rect.min.x, rect.max.y),
                ],
                4,
            ),
            Subject::Rect { .. } => ([Vec2::ZERO; 4], 0),
        }
    }
}

/// The solver result: add `delta` to the raw subject.
#[derive(Clone, Debug, Default)]
pub(crate) struct SnapResult {
    pub delta: Vec2,
    pub guides: Vec<SmartGuide>,
}

/// Sticky state for one gesture. Keep it on the gesture, never the document.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SnapHints {
    x: Option<f32>,
    y: Option<f32>,
    point: Option<Vec2>,
}

impl SnapHints {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Targets and sticky state captured at the start of one drag.
#[derive(Clone, Debug, Default)]
pub(crate) struct SnapSession {
    pub targets: SnapTargets,
    pub hints: SnapHints,
}

struct AxisSnap {
    delta: f32,
    value: f32,
    guides: Vec<SmartGuide>,
}

/// Snap a raw subject against the gesture's targets.
pub(crate) fn snap(
    subject: Subject,
    targets: &SnapTargets,
    options: &SnapOptions,
    hints: &mut SnapHints,
) -> SnapResult {
    if !options.tolerance.is_finite() || !subject.is_finite() {
        hints.reset();
        return SnapResult::default();
    }
    let tolerance = options.tolerance.max(MIN_TOLERANCE);

    if let Some((target, probe)) =
        point_snap(subject, targets, &options.kinds, tolerance, hints.point)
    {
        hints.x = Some(target.x);
        hints.y = Some(target.y);
        hints.point = Some(target);
        return SnapResult {
            delta: target - probe,
            guides: vec![SmartGuide {
                kind: GuideKind::Vertex,
                anchor: target,
                direction: Vec2::ZERO,
                segment: None,
            }],
        };
    }
    hints.point = None;

    let (x_values, x_count) = subject.x_values();
    let (y_values, y_count) = subject.y_values();
    let x = snap_axis(
        &x_values[..x_count],
        &targets.xs,
        tolerance,
        hints.x,
        &options.kinds,
        true,
    );
    let y = snap_axis(
        &y_values[..y_count],
        &targets.ys,
        tolerance,
        hints.y,
        &options.kinds,
        false,
    );

    let mut result = SnapResult::default();
    match x {
        Some(snap) => {
            result.delta.x = snap.delta;
            result.guides.extend(snap.guides);
            hints.x = Some(snap.value);
        }
        None => hints.x = None,
    }
    match y {
        Some(snap) => {
            result.delta.y = snap.delta;
            result.guides.extend(snap.guides);
            hints.y = Some(snap.value);
        }
        None => hints.y = None,
    }
    result
}

/// Point snaps fix both axes and outrank single-axis guides.
fn point_snap(
    subject: Subject,
    targets: &SnapTargets,
    kinds: &SnapKinds,
    tolerance: f32,
    hint: Option<Vec2>,
) -> Option<(Vec2, Vec2)> {
    if !kinds.vertices {
        return None;
    }
    let (probes, count) = subject.point_probes();
    if count == 0 || targets.points.is_empty() {
        return None;
    }
    if let Some(hint) = hint {
        if let Some(target) = targets
            .points
            .iter()
            .find(|target| target.pos.distance_squared(hint) <= HINT_EPS_SQ)
        {
            let nearest = probes[..count]
                .iter()
                .copied()
                .min_by(|a, b| a.distance(hint).total_cmp(&b.distance(hint)))?;
            if nearest.distance(hint) <= tolerance * HINT_FACTOR {
                return Some((target.pos, nearest));
            }
        }
    }

    let mut best: Option<(f32, Vec2, Vec2)> = None;
    for target in &targets.points {
        for probe in &probes[..count] {
            let distance = probe.distance(target.pos);
            if distance > tolerance {
                continue;
            }
            if best.is_none_or(|(closest, _, _)| distance < closest - 1e-6) {
                best = Some((distance, *probe, target.pos));
            }
        }
    }
    best.map(|(_, probe, target)| (target, probe))
}

fn snap_axis(
    values: &[f32],
    targets: &[AxisTarget],
    tolerance: f32,
    hint: Option<f32>,
    kinds: &SnapKinds,
    vertical: bool,
) -> Option<AxisSnap> {
    if values.is_empty() || targets.is_empty() {
        return None;
    }

    // Hysteresis: keep the previous target while the raw value is still within
    // the release distance, even if a different target is briefly closer.
    if let Some(hint) = hint.filter(|hint| hint.is_finite()) {
        if let Some(target) = target_at(targets, hint, kinds) {
            let nearest = values
                .iter()
                .copied()
                .filter(|value| value.is_finite())
                .min_by(|a, b| (a - hint).abs().total_cmp(&(b - hint).abs()))?;
            if (nearest - hint).abs() <= tolerance * HINT_FACTOR {
                return Some(axis_snap(target.value, nearest, targets, kinds, vertical));
            }
        }
    }

    let mut best: Option<(f32, f32, AxisTarget)> = None;
    for &value in values {
        if !value.is_finite() {
            continue;
        }
        let start = targets.partition_point(|target| target.value < value - tolerance);
        for target in &targets[start..] {
            if target.value > value + tolerance {
                break;
            }
            if !target.value.is_finite() || !kinds.allows(target.kind) {
                continue;
            }
            let distance = (target.value - value).abs();
            if distance > tolerance {
                continue;
            }
            let better = match best {
                None => true,
                Some((closest, _, current)) => {
                    distance < closest - 1e-6
                        || ((distance - closest).abs() <= 1e-6
                            && (target_rank(target.kind), target.source)
                                < (target_rank(current.kind), current.source))
                }
            };
            if better {
                best = Some((distance, value, *target));
            }
        }
    }

    let (_, value, target) = best?;
    Some(axis_snap(target.value, value, targets, kinds, vertical))
}

/// A target still present at the hinted value, most specific first.
fn target_at(targets: &[AxisTarget], value: f32, kinds: &SnapKinds) -> Option<AxisTarget> {
    let start = targets.partition_point(|target| target.value < value - SAME_VALUE);
    targets[start..]
        .iter()
        .take_while(|target| target.value <= value + SAME_VALUE)
        .filter(|target| kinds.allows(target.kind))
        .min_by_key(|target| (target_rank(target.kind), target.source))
        .copied()
}

fn axis_snap(
    value: f32,
    subject: f32,
    targets: &[AxisTarget],
    kinds: &SnapKinds,
    vertical: bool,
) -> AxisSnap {
    let start = targets.partition_point(|target| target.value < value - SAME_VALUE);
    let mut guides = Vec::new();
    for target in &targets[start..] {
        if target.value > value + SAME_VALUE {
            break;
        }
        if kinds.allows(target.kind) {
            guides.push(SmartGuide {
                kind: target.kind,
                anchor: target.anchor,
                direction: if vertical { Vec2::Y } else { Vec2::X },
                segment: None,
            });
        }
    }
    AxisSnap {
        delta: value - subject,
        value,
        guides,
    }
}

/// Specificity used for deterministic ties: extensions, edges, centers, canvas.
fn target_rank(kind: GuideKind) -> u8 {
    match kind {
        GuideKind::Vertex => 0,
        GuideKind::Vertical | GuideKind::Horizontal => 1,
        GuideKind::Edge => 2,
        GuideKind::Center => 3,
        GuideKind::Canvas => 4,
        GuideKind::Parallel | GuideKind::Perpendicular => 5,
    }
}

fn point_cmp(a: Vec2, b: Vec2) -> Ordering {
    a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y))
}

/// Collect geometry from every visible, non-excluded outline, plus the canvas.
///
/// Bounds are the outline's flattened bounds; a stroked shape therefore
/// contributes its centerline, matching what the pen and knife target. Groups
/// add their union box and then recurse into their children. A group whose
/// children are all excluded contributes no box, so a dragged group cannot
/// snap to its own silhouette.
pub(crate) fn collect_targets(doc: &Document, excluded: &HashSet<LayerId>) -> SnapTargets {
    let mut targets = SnapTargets::default();
    let canvas = Vec2::new(doc.size.x as f32, doc.size.y as f32);
    if canvas.is_finite() && canvas.x > 0.0 && canvas.y > 0.0 {
        targets.add_rect(
            Bounds {
                min: Vec2::ZERO,
                max: canvas,
            },
            None,
            GuideKind::Canvas,
            GuideKind::Canvas,
        );
    }
    collect_layers(&doc.layers, excluded, &mut targets);
    targets.finish();
    targets
}

fn collect_layers(layers: &[Layer], excluded: &HashSet<LayerId>, targets: &mut SnapTargets) {
    for layer in layers {
        if !layer.visible || excluded.contains(&layer.id) {
            continue;
        }
        match &layer.kind {
            LayerKind::Shape(shape) => {
                if let Some(bounds) = shape.source.bounds() {
                    targets.add_rect(bounds, Some(layer.id), GuideKind::Edge, GuideKind::Center);
                    targets.rects.push(RectTarget {
                        rect: bounds,
                        source: Some(layer.id),
                    });
                }
                add_source_geometry(&shape.source, layer.id, targets);
            }
            LayerKind::Group { children } => {
                if let Some(bounds) = group_bounds(children, excluded) {
                    targets.add_rect(bounds, Some(layer.id), GuideKind::Edge, GuideKind::Center);
                    targets.rects.push(RectTarget {
                        rect: bounds,
                        source: Some(layer.id),
                    });
                }
                collect_layers(children, excluded, targets);
            }
            _ => {}
        }
    }
}

fn group_bounds(children: &[Layer], excluded: &HashSet<LayerId>) -> Option<Bounds> {
    children
        .iter()
        .filter_map(|child| layer_bounds_excluding(child, excluded))
        .reduce(union_bounds)
}

fn layer_bounds_excluding(layer: &Layer, excluded: &HashSet<LayerId>) -> Option<Bounds> {
    if !layer.visible || excluded.contains(&layer.id) {
        return None;
    }
    match &layer.kind {
        LayerKind::Shape(shape) => shape.source.bounds(),
        LayerKind::Group { children } => group_bounds(children, excluded),
        _ => None,
    }
}

fn union_bounds(a: Bounds, b: Bounds) -> Bounds {
    Bounds {
        min: a.min.min(b.min),
        max: a.max.max(b.max),
    }
}

fn add_source_geometry(source: &ShapeSource, id: LayerId, targets: &mut SnapTargets) {
    match source {
        ShapeSource::Path(path) => add_path_geometry(path, id, targets),
        ShapeSource::Primitive(Primitive::Ellipse { center, radii }) => {
            // An ellipse's polygon is all short tangents; only its center and
            // quadrant points are useful references.
            targets.add_point(*center, Some(id));
            for offset in [
                Vec2::new(radii.x, 0.0),
                Vec2::new(-radii.x, 0.0),
                Vec2::new(0.0, radii.y),
                Vec2::new(0.0, -radii.y),
            ] {
                targets.add_point(*center + offset, Some(id));
            }
        }
        ShapeSource::Primitive(_) => {
            if let Some(path) = source.to_path() {
                add_path_geometry(&path, id, targets);
            }
        }
        ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) => {}
        // Text contributes its block bounds (added by the caller), not every
        // glyph vertex: snapping targets must stay cheap to collect.
        ShapeSource::Text(_) => {}
    }
}

fn add_path_geometry(path: &Path, id: LayerId, targets: &mut SnapTargets) {
    for subpath in &path.subpaths {
        for node in &subpath.nodes {
            targets.add_point(node.pos, Some(id));
        }
        for pair in subpath.nodes.windows(2) {
            targets.add_segment(pair[0].pos, pair[1].pos, Some(id));
        }
        if subpath.closed {
            if let (Some(first), Some(last)) = (subpath.nodes.first(), subpath.nodes.last()) {
                targets.add_segment(last.pos, first.pos, Some(id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;
    use proptest::prelude::*;

    fn axis(value: f32, kind: GuideKind) -> AxisTarget {
        AxisTarget {
            value,
            anchor: Vec2::new(value, 0.0),
            kind,
            source: None,
        }
    }

    fn point(x: f32, y: f32) -> PointTarget {
        PointTarget {
            pos: Vec2::new(x, y),
            source: None,
        }
    }

    fn targets_with(
        xs: Vec<AxisTarget>,
        ys: Vec<AxisTarget>,
        points: Vec<PointTarget>,
    ) -> SnapTargets {
        let mut targets = SnapTargets {
            xs,
            ys,
            points,
            ..Default::default()
        };
        targets.finish();
        targets
    }

    fn opts(tolerance: f32) -> SnapOptions {
        SnapOptions {
            tolerance,
            kinds: SnapKinds::default(),
        }
    }

    fn rect_layer(name: &str, center: Vec2, size: Vec2) -> Layer {
        Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size,
                corner_radius: 0.0,
            }),
        )
    }

    #[test]
    fn a_point_snaps_to_a_vertical_target_inside_tolerance() {
        let targets = targets_with(vec![axis(100.0, GuideKind::Edge)], Vec::new(), Vec::new());
        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Point(Vec2::new(93.0, 40.0)),
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert!((result.delta.x - 7.0).abs() < 1e-4);
        assert_eq!(result.delta.y, 0.0);
        assert_eq!(result.guides.len(), 1);
        assert_eq!(result.guides[0].kind, GuideKind::Edge);

        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Point(Vec2::new(80.0, 40.0)),
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert_eq!(result.delta, Vec2::ZERO);
        assert!(result.guides.is_empty());
    }

    #[test]
    fn a_point_snaps_to_a_horizontal_target_inside_tolerance() {
        let targets = targets_with(Vec::new(), vec![axis(100.0, GuideKind::Edge)], Vec::new());
        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Point(Vec2::new(40.0, 93.0)),
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert_eq!(result.delta.x, 0.0);
        assert!((result.delta.y - 7.0).abs() < 1e-4);
        assert_eq!(result.guides.len(), 1);
        assert_eq!(result.guides[0].kind, GuideKind::Edge);
        assert_eq!(result.guides[0].direction, Vec2::X);
    }

    #[test]
    fn a_rect_move_uses_the_nearest_edge_or_center() {
        let bounds = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(100.0, 50.0),
        };
        let moving = MovingEdges::ALL;

        // The right edge is 3 px from 103, the center 53 px away.
        let targets = targets_with(vec![axis(103.0, GuideKind::Edge)], Vec::new(), Vec::new());
        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Rect {
                rect: bounds,
                moving,
            },
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert!((result.delta.x - 3.0).abs() < 1e-4);
        assert_eq!(result.guides[0].kind, GuideKind::Edge);

        // A center target near the center wins when the edges are far away.
        let bounds = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(80.0, 50.0),
        };
        let targets = targets_with(vec![axis(45.0, GuideKind::Center)], Vec::new(), Vec::new());
        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Rect {
                rect: bounds,
                moving,
            },
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert!((result.delta.x - 5.0).abs() < 1e-4);
        assert_eq!(result.guides[0].kind, GuideKind::Center);
    }

    #[test]
    fn several_targets_at_one_value_emit_several_guides() {
        let targets = targets_with(
            vec![axis(100.0, GuideKind::Edge), axis(100.0, GuideKind::Canvas)],
            Vec::new(),
            Vec::new(),
        );
        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Point(Vec2::new(95.0, 20.0)),
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert!((result.delta.x - 5.0).abs() < 1e-4);
        assert_eq!(result.guides.len(), 2);
        assert!(result
            .guides
            .iter()
            .any(|guide| guide.kind == GuideKind::Edge));
        assert!(result
            .guides
            .iter()
            .any(|guide| guide.kind == GuideKind::Canvas));
    }

    #[test]
    fn a_vertex_fixes_both_axes_and_outranks_an_axis() {
        let targets = targets_with(
            vec![axis(103.0, GuideKind::Edge)],
            Vec::new(),
            vec![point(100.0, 100.0)],
        );
        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Point(Vec2::new(103.0, 97.0)),
            &targets,
            &opts(8.0),
            &mut hints,
        );
        // The vertical target would fix x alone; the vertex wins and adds -3 Y.
        assert!((result.delta - Vec2::new(-3.0, 3.0)).length() < 1e-4);
        assert_eq!(result.guides.len(), 1);
        assert_eq!(result.guides[0].kind, GuideKind::Vertex);
    }

    #[test]
    fn a_moving_rect_corner_can_take_a_vertex_snap() {
        let target = point(100.0, 100.0);
        let targets = targets_with(Vec::new(), Vec::new(), vec![target]);
        let rect = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(93.0, 100.0),
        };
        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Rect {
                rect,
                moving: MovingEdges::ALL,
            },
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert!((result.delta - Vec2::new(7.0, 0.0)).length() < 1e-4);
        assert_eq!(result.guides[0].kind, GuideKind::Vertex);
    }

    #[test]
    fn a_resize_only_compares_the_moving_edge() {
        let bounds = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(100.0, 50.0),
        };
        // The center is exactly on 50, but a resize must ignore it.
        let targets = targets_with(
            vec![axis(50.0, GuideKind::Center), axis(104.0, GuideKind::Edge)],
            Vec::new(),
            Vec::new(),
        );
        let mut hints = SnapHints::default();
        let result = snap(
            Subject::Rect {
                rect: bounds,
                moving: MovingEdges {
                    left: false,
                    right: true,
                    top: false,
                    bottom: false,
                },
            },
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert!((result.delta.x - 4.0).abs() < 1e-4);
        assert_eq!(result.guides.len(), 1);
        assert_eq!(result.guides[0].kind, GuideKind::Edge);
    }

    #[test]
    fn hysteresis_holds_to_one_and_a_half_tolerance() {
        let targets = targets_with(vec![axis(100.0, GuideKind::Edge)], Vec::new(), Vec::new());
        let mut hints = SnapHints::default();
        let first = snap(
            Subject::Point(Vec2::new(95.0, 0.0)),
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert!((first.delta.x - 5.0).abs() < 1e-4);

        // 11.5 px away: outside the 8 px acquire band, inside the 12 px hold.
        let held = snap(
            Subject::Point(Vec2::new(88.5, 0.0)),
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert!((held.delta.x - 11.5).abs() < 1e-4);
        assert_eq!(held.guides.len(), 1);

        // 13 px away: released.
        let released = snap(
            Subject::Point(Vec2::new(87.0, 0.0)),
            &targets,
            &opts(8.0),
            &mut hints,
        );
        assert_eq!(released.delta, Vec2::ZERO);
        assert!(released.guides.is_empty());
    }

    #[test]
    fn disabled_kinds_never_snap() {
        let targets = targets_with(vec![axis(100.0, GuideKind::Canvas)], Vec::new(), Vec::new());
        let mut hints = SnapHints::default();
        let mut options = opts(8.0);
        options.kinds.canvas = false;
        let result = snap(
            Subject::Point(Vec2::new(95.0, 0.0)),
            &targets,
            &options,
            &mut hints,
        );
        assert_eq!(result.delta, Vec2::ZERO);
        assert!(result.guides.is_empty());

        // A point target is ignored when vertices are off.
        let targets = targets_with(Vec::new(), Vec::new(), vec![point(100.0, 100.0)]);
        let mut options = opts(8.0);
        options.kinds.vertices = false;
        let result = snap(
            Subject::Point(Vec2::new(103.0, 97.0)),
            &targets,
            &options,
            &mut SnapHints::default(),
        );
        assert_eq!(result.delta, Vec2::ZERO);
    }

    #[test]
    fn collect_targets_adds_edges_centers_points_and_canvas() {
        let mut doc = Document::new(UVec2::splat(200));
        doc.layers.push(rect_layer(
            "panel",
            Vec2::new(100.0, 100.0),
            Vec2::splat(40.0),
        ));
        let targets = collect_targets(&doc, &HashSet::new());

        // Shape box: 80..120, center 100.
        assert!(targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Edge && (target.value - 80.0).abs() < 1e-4));
        assert!(targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Center && (target.value - 100.0).abs() < 1e-4));
        // Canvas: 0, 100, 200.
        assert!(targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Canvas && (target.value - 200.0).abs() < 1e-4));
        assert_eq!(targets.points.len(), 4, "rect corners");
        assert_eq!(targets.segments.len(), 4, "rect edges");
        assert_eq!(targets.rects.len(), 1, "one spacing rect");
    }

    #[test]
    fn hidden_and_excluded_layers_are_not_targets() {
        let mut doc = Document::new(UVec2::splat(200));
        let mut hidden = rect_layer("hidden", Vec2::splat(60.0), Vec2::splat(20.0));
        hidden.visible = false;
        let hidden_id = hidden.id;
        doc.layers.push(hidden);
        let targets = collect_targets(&doc, &HashSet::new());
        assert!(!targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Edge));
        assert!(targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Canvas));

        let mut visible = rect_layer("visible", Vec2::splat(60.0), Vec2::splat(20.0));
        visible.visible = true;
        let visible_id = visible.id;
        doc.layers.push(visible);
        let excluded = HashSet::from([visible_id, hidden_id]);
        let targets = collect_targets(&doc, &excluded);
        assert!(!targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Edge));
        assert!(targets.points.is_empty());
        assert!(targets.rects.is_empty());
    }

    #[test]
    fn a_group_contributes_its_union_box() {
        let a = rect_layer("a", Vec2::new(60.0, 100.0), Vec2::splat(20.0)); // 50..70
        let b = rect_layer("b", Vec2::new(140.0, 100.0), Vec2::splat(20.0)); // 130..150
        let group = Layer::group("group", vec![a, b]);
        let mut doc = Document::new(UVec2::splat(200));
        doc.layers.push(group);

        let targets = collect_targets(&doc, &HashSet::new());
        assert!(targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Edge && (target.value - 50.0).abs() < 1e-4));
        assert!(targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Edge && (target.value - 150.0).abs() < 1e-4));
        assert!(targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Center && (target.value - 100.0).abs() < 1e-4));
    }

    #[test]
    fn excluding_one_group_child_shrinks_the_group_box() {
        let a = rect_layer("a", Vec2::new(60.0, 100.0), Vec2::splat(20.0)); // 50..70
        let b = rect_layer("b", Vec2::new(140.0, 100.0), Vec2::splat(20.0)); // 130..150
        let b_id = b.id;
        let group = Layer::group("group", vec![a, b]);
        let mut doc = Document::new(UVec2::splat(200));
        doc.layers.push(group);

        let targets = collect_targets(&doc, &HashSet::from([b_id]));
        assert!(!targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Edge && (target.value - 130.0).abs() < 1e-4));
        assert!(targets
            .xs
            .iter()
            .any(|target| target.kind == GuideKind::Edge && (target.value - 70.0).abs() < 1e-4));
    }

    proptest! {
        #[test]
        fn axis_snaps_stay_within_the_tolerance(
            subject_x in -1000.0f32..1000.0f32,
            target_x in -1000.0f32..1000.0f32,
        ) {
            let targets = targets_with(vec![axis(target_x, GuideKind::Edge)], Vec::new(), Vec::new());
            let mut hints = SnapHints::default();
            let result = snap(
                Subject::Point(Vec2::new(subject_x, 0.0)),
                &targets,
                &opts(8.0),
                &mut hints,
            );
            prop_assert!(result.delta.x.is_finite() && result.delta.y.is_finite());
            prop_assert!(result.delta.x.abs() <= 8.0 + 1e-3);
            prop_assert_eq!(result.delta.y, 0.0);
        }

        #[test]
        fn snapping_an_already_snapped_value_is_idempotent(
            subject_x in -1000.0f32..1000.0f32,
            target_x in -1000.0f32..1000.0f32,
        ) {
            let targets = targets_with(vec![axis(target_x, GuideKind::Edge)], Vec::new(), Vec::new());
            let mut hints = SnapHints::default();
            let first = snap(
                Subject::Point(Vec2::new(subject_x, 0.0)),
                &targets,
                &opts(8.0),
                &mut hints,
            );
            let snapped = subject_x + first.delta.x;
            let mut hints = SnapHints::default();
            let second = snap(
                Subject::Point(Vec2::new(snapped, 0.0)),
                &targets,
                &opts(8.0),
                &mut hints,
            );
            prop_assert!(second.delta.x.abs() <= 1e-3);
        }

        #[test]
        fn results_are_deterministic(
            subject_x in -1000.0f32..1000.0f32,
            target_a in -1000.0f32..1000.0f32,
            target_b in -1000.0f32..1000.0f32,
        ) {
            let build = || targets_with(
                vec![axis(target_a, GuideKind::Edge), axis(target_b, GuideKind::Canvas)],
                Vec::new(),
                Vec::new(),
            );
            let subject = Subject::Point(Vec2::new(subject_x, 0.0));
            let first = snap(subject, &build(), &opts(8.0), &mut SnapHints::default());
            let second = snap(subject, &build(), &opts(8.0), &mut SnapHints::default());
            prop_assert_eq!(first.delta, second.delta);
            prop_assert_eq!(first.guides.len(), second.guides.len());
        }
    }
}
