//! Cable geometry.
//!
//! A cable is authored as waypoints, but it is stored as one filled outline:
//! the tube band and its optional terminals are unioned once, so the normal
//! evaluator sees a single closed polygon. That keeps the cable on the fast
//! analytic path (including the GPU evaluator) instead of expanding a stroke
//! band and beveling its raster coverage on every edit.
//!
//! The waypoints and terminal settings stay on the layer as [`CableMeta`], so
//! Properties can rebuild the outline after creation without keeping a group
//! of internal parts.

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::layers::{Layer, LayerKind, ShapeStyle};
use crate::path::{
    flatten_path_tolerance_loops, stroke_polyline_outline, FillRule, Path, ShapeSource,
};
use crate::sdf::Profile;

/// Terminal shape drawn at both ends of a cable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CableCap {
    /// Rounded connector, the standard cable end.
    #[default]
    Round,
    /// Rounded square terminal.
    Square,
    /// Six-sided connector.
    Hex,
    /// Star-shaped raised terminal.
    Star,
    /// Leave the tube's own round cap.
    None,
}

impl CableCap {
    pub const ALL: [Self; 5] = [Self::Round, Self::Square, Self::Hex, Self::Star, Self::None];

    pub fn label(self) -> &'static str {
        match self {
            Self::Round => "Round",
            Self::Square => "Square",
            Self::Hex => "Hexagon",
            Self::Star => "Star",
            Self::None => "None (tube end)",
        }
    }
}

fn default_cap_scale() -> f32 {
    1.5
}

/// Authoring data kept on a cable layer. The baked outline is derived from
/// these values, so editing any of them rebuilds the same tube.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CableMeta {
    /// Legacy click-to-place waypoints. New cables store a path instead.
    #[serde(default)]
    pub waypoints: Vec<Vec2>,
    /// Custom vector path used as the centreline. When present it wins over
    /// `waypoints`; the path keeps Bézier handles from the Pen tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centerline: Option<Path>,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub cap: CableCap,
    #[serde(default = "default_cap_scale")]
    pub cap_scale: f32,
}

impl CableMeta {
    pub fn new(
        waypoints: Vec<Vec2>,
        width: f32,
        height: f32,
        cap: CableCap,
        cap_scale: f32,
    ) -> Self {
        Self {
            waypoints,
            centerline: None,
            width,
            height,
            cap,
            cap_scale,
        }
    }

    /// Use one subpath of a custom vector path as the cable's centreline.
    pub fn from_path(
        path: &Path,
        width: f32,
        height: f32,
        cap: CableCap,
        cap_scale: f32,
    ) -> Option<Self> {
        let subpath = path
            .subpaths
            .iter()
            .find(|subpath| subpath.nodes.len() >= 2)?
            .clone();
        let mut centerline = Path::new(FillRule::NonZero);
        centerline.subpaths.push(subpath);
        Some(Self {
            waypoints: Vec::new(),
            centerline: Some(centerline),
            width,
            height,
            cap,
            cap_scale,
        })
    }

    /// Flattened centreline and whether it is a closed loop.
    fn centerline_points(&self) -> Option<(Vec<Vec2>, bool)> {
        if let Some(path) = &self.centerline {
            let mut loops = flatten_path_tolerance_loops(path, 0.25);
            let (mut points, closed) = loops.drain(..).max_by_key(|(points, _)| points.len())?;
            if points.len() < 2 {
                return None;
            }
            if closed
                && points
                    .first()
                    .is_some_and(|first| first.distance(*points.last().unwrap()) > 1e-3)
            {
                points.push(points[0]);
            }
            Some((points, closed))
        } else {
            Some((smooth_cable(&self.waypoints)?, false))
        }
    }
}

/// Maximum distance between sampled centreline vertices, in document pixels.
const CABLE_STEP_PX: f32 = 4.0;
/// Samples per segment, so short cables still curve smoothly.
const MIN_SAMPLES: usize = 2;

/// Build the cable layer. The tube band is baked into one closed fill so the
/// GPU can shade it analytically; the terminals stay separate shape layers so
/// their height can differ. The whole group keeps identity opacity and
/// derivative blending, which lets the GPU flatten it instead of evaluating the
/// tree on the CPU.
///
/// Returns `None` when the waypoints cannot make a tube (fewer than two points).
pub fn cable_layer(meta: &CableMeta) -> Option<Layer> {
    let (centerline, closed) = meta.centerline_points()?;
    let width = meta.width.clamp(2.0, 256.0);
    let height = meta.height.clamp(0.5, 256.0);

    let band = stroke_polyline_outline(&centerline, width)?;
    let path = Path::from_loops(vec![band], FillRule::NonZero);
    let mut body = Layer::shape("Cable body", ShapeSource::Path(path));
    if let LayerKind::Shape(shape) = &mut body.kind {
        // The band is already the tube's area; beveling it with half the tube
        // width makes the cross-section semicircular instead of flat-topped.
        shape.style = ShapeStyle::Fill;
        shape.radius_px = width * 0.5;
        shape.height_px = height;
        shape.profile = Profile::Round;
        shape.softness_px = 0.0;
        // Analytic polygon SDF on the GPU: no coverage smoothing needed. A
        // 2 px fade keeps the long diagonal silhouettes from stair-stepping.
        shape.smooth_px = 0.0;
        shape.edge_aa_px = 2.0;
    }

    let mut children = vec![body];
    // A closed centreline is already a loop; terminal caps on the seam would
    // only pinch it.
    if !closed && meta.cap != CableCap::None {
        let cap_size = width * meta.cap_scale.clamp(0.5, 4.0);
        let count = centerline.len();
        for (index, (end, tangent)) in [
            (centerline[0], centerline[1] - centerline[0]),
            (
                centerline[count - 1],
                centerline[count - 1] - centerline[count - 2],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let Some(source) = cable_cap_source(meta.cap, end, tangent, cap_size) else {
                continue;
            };
            let name = if index == 0 {
                "Cable cap A"
            } else {
                "Cable cap B"
            };
            let mut cap = Layer::shape(name, source);
            if let LayerKind::Shape(shape) = &mut cap.kind {
                shape.radius_px = cap_size * 0.42;
                shape.height_px = height * 1.05;
                shape.profile = Profile::Round;
                shape.smooth_px = 0.0;
                shape.edge_aa_px = 2.0;
            }
            children.push(cap);
        }
    }

    let mut group = Layer::group("Cable", children);
    group.cable = Some(meta.clone());
    Some(group)
}

/// Terminal geometry for one end. The shape is authored around the origin and
/// then rotated along the cable so square, hex and star terminals follow the
/// tube instead of staying axis-aligned.
pub fn cable_cap_source(cap: CableCap, end: Vec2, tangent: Vec2, size: f32) -> Option<ShapeSource> {
    let size = size.max(0.5);
    let primitive = match cap {
        CableCap::Round | CableCap::None => crate::path::Primitive::Ellipse {
            center: Vec2::ZERO,
            radii: Vec2::splat(size * 0.5),
        },
        CableCap::Square => crate::path::Primitive::Rect {
            center: Vec2::ZERO,
            size: Vec2::splat(size),
            corner_radius: size * 0.18,
        },
        CableCap::Hex => crate::path::Primitive::Polygon {
            points: regular_polygon(6, size * 0.5),
        },
        CableCap::Star => crate::path::Primitive::Star {
            center: Vec2::ZERO,
            inner_radius: size * 0.25,
            outer_radius: size * 0.5,
            points: 5,
            rotation: 0.0,
        },
    };
    let angle = if tangent.length_squared() > 1e-8 {
        tangent.y.atan2(tangent.x)
    } else {
        0.0
    };
    ShapeSource::Primitive(primitive).transformed_affine(Vec2::ONE, angle, end)
}

/// One closed outline for a terminal at `end`, following `tangent`.
pub fn cable_cap_outline(cap: CableCap, end: Vec2, tangent: Vec2, size: f32) -> Option<Vec<Vec2>> {
    let source = cable_cap_source(cap, end, tangent, size)?;
    source.outline().into_iter().max_by_key(Vec::len)
}

/// Recover cable authoring data from a group saved by the grouped-stroke
/// version of the tool, so old projects get the Cable card too.
pub fn infer_cable_meta(layer: &Layer) -> Option<CableMeta> {
    if let Some(meta) = &layer.cable {
        return Some(meta.clone());
    }
    if layer.name != "Cable" {
        return None;
    }
    let LayerKind::Group { children } = &layer.kind else {
        return None;
    };
    let body = children.iter().find(|child| child.name == "Cable body")?;
    let LayerKind::Shape(body_shape) = &body.kind else {
        return None;
    };
    let ShapeSource::Path(path) = &body_shape.source else {
        return None;
    };
    let mut waypoints = Vec::new();
    for subpath in &path.subpaths {
        waypoints.extend(subpath.nodes.iter().map(|node| node.pos));
    }
    if waypoints.len() < 2 {
        return None;
    }
    let width = if body_shape.style.is_stroke() {
        body_shape.stroke_width()
    } else {
        body_shape.radius_px * 2.0
    };
    let cap_layer = children
        .iter()
        .find(|child| child.name.starts_with("Cable cap"));
    let cap = cap_layer
        .and_then(|child| match &child.kind {
            LayerKind::Shape(shape) => Some(cap_from_source(&shape.source)),
            _ => None,
        })
        .unwrap_or(CableCap::None);
    let cap_scale = cap_layer
        .and_then(|child| match &child.kind {
            LayerKind::Shape(shape) => {
                Some((shape.radius_px / 0.42 / width.max(1.0)).clamp(0.5, 4.0))
            }
            _ => None,
        })
        .unwrap_or(1.5);
    Some(CableMeta::new(
        waypoints,
        width,
        body_shape.height_px,
        cap,
        cap_scale,
    ))
}

/// Add metadata to cables saved before it existed. The geometry is left alone
/// until the user edits a cable parameter, which rebuilds the tube.
pub fn migrate_cables(doc: &mut crate::layers::Document) {
    fn visit(layers: &mut [Layer]) {
        for layer in layers {
            if let LayerKind::Group { children } = &mut layer.kind {
                visit(children);
            }
            if layer.cable.is_none() {
                if let Some(meta) = infer_cable_meta(layer) {
                    // Rebuild the old grouped-stroke cable into the baked form so
                    // it also gets the GPU path; the inferred waypoints follow the
                    // same centreline.
                    if let Some(rebuilt) = cable_layer(&meta) {
                        layer.kind = rebuilt.kind;
                        layer.touch();
                    }
                    layer.cable = Some(meta);
                }
            }
        }
    }
    visit(&mut doc.layers);
}

fn cap_from_source(source: &ShapeSource) -> CableCap {
    match source {
        ShapeSource::Primitive(crate::path::Primitive::Ellipse { .. }) => CableCap::Round,
        ShapeSource::Primitive(crate::path::Primitive::Rect { .. }) => CableCap::Square,
        ShapeSource::Primitive(crate::path::Primitive::Polygon { points }) => {
            if points.len() == 6 {
                CableCap::Hex
            } else {
                CableCap::Round
            }
        }
        ShapeSource::Primitive(crate::path::Primitive::Star { .. }) => CableCap::Star,
        ShapeSource::Path(path) => {
            let nodes = path
                .subpaths
                .first()
                .map_or(0, |subpath| subpath.nodes.len());
            match nodes {
                4 => CableCap::Square,
                6 => CableCap::Hex,
                10 => CableCap::Star,
                _ => CableCap::Round,
            }
        }
        _ => CableCap::Round,
    }
}

/// Catmull-Rom through the clicked waypoints. The curve passes through every
/// waypoint, so the cable follows the points the user placed while the bends
/// stay round instead of becoming polyline elbows.
pub fn smooth_cable(points: &[Vec2]) -> Option<Vec<Vec2>> {
    let mut waypoints: Vec<Vec2> = Vec::new();
    for point in points {
        if point.is_finite()
            && waypoints
                .last()
                .is_none_or(|last| last.distance(*point) > 1e-3)
        {
            waypoints.push(*point);
        }
    }
    if waypoints.len() < 2 {
        return None;
    }
    if waypoints.len() == 2 {
        return Some(waypoints);
    }

    let mut out = Vec::with_capacity(waypoints.len() * 8);
    for index in 0..waypoints.len() - 1 {
        let p0 = if index == 0 {
            waypoints[0]
        } else {
            waypoints[index - 1]
        };
        let p1 = waypoints[index];
        let p2 = waypoints[index + 1];
        let p3 = if index + 2 < waypoints.len() {
            waypoints[index + 2]
        } else {
            waypoints[index + 1]
        };
        let steps = (((p2 - p1).length() / CABLE_STEP_PX).ceil() as usize).max(MIN_SAMPLES);
        for step in 0..steps {
            let t = step as f32 / steps as f32;
            out.push(catmull_rom(p0, p1, p2, p3, t));
        }
    }
    out.push(*waypoints.last().expect("two waypoints"));
    Some(out)
}

fn catmull_rom(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let t2 = t * t;
    let t3 = t2 * t;
    (p1 * 2.0
        + (p2 - p0) * t
        + (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * t2
        + (p1 * 3.0 - p0 - p2 * 3.0 + p3) * t3)
        * 0.5
}

fn regular_polygon(sides: u32, radius: f32) -> Vec<Vec2> {
    (0..sides)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / sides as f32;
            Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> CableMeta {
        CableMeta::new(
            vec![Vec2::new(20.0, 50.0), Vec2::new(120.0, 50.0)],
            26.0,
            12.0,
            CableCap::Round,
            1.5,
        )
    }

    #[test]
    fn a_straight_cable_keeps_its_endpoints() {
        let points = [Vec2::new(20.0, 40.0), Vec2::new(120.0, 40.0)];
        let line = smooth_cable(&points).expect("two points make a cable");
        assert!(line.len() >= 2);
        assert!(line[0].distance(points[0]) < 1e-4);
        assert!(line.last().unwrap().distance(points[1]) < 1e-4);
        assert!(
            line.iter().all(|point| (point.y - 40.0).abs() < 1e-3),
            "straight stays straight"
        );
    }

    #[test]
    fn a_bend_is_smoothed_between_waypoints() {
        let points = [
            Vec2::new(20.0, 80.0),
            Vec2::new(80.0, 80.0),
            Vec2::new(80.0, 20.0),
        ];
        let line = smooth_cable(&points).expect("three points make a cable");
        assert!(line.len() > 6);
        assert!(line
            .iter()
            .any(|point| point.distance(Vec2::new(80.0, 80.0)) < 0.5));
        assert!(line.iter().any(|point| point.x > 80.0 && point.y < 80.0));
    }

    #[test]
    fn a_cable_is_a_gpu_packable_group_with_a_baked_band() {
        let layer = cable_layer(&meta()).expect("cable");
        assert_eq!(layer.name, "Cable");
        let LayerKind::Group { children } = &layer.kind else {
            panic!("cable is a group");
        };
        assert_eq!(children.len(), 3, "body and two terminals");
        let LayerKind::Shape(body) = &children[0].kind else {
            panic!("body is a shape");
        };
        assert!(!body.style.is_stroke(), "the tube is baked into the fill");
        assert!((body.radius_px - 13.0).abs() < 1e-4);
        assert!((body.height_px - 12.0).abs() < 1e-4);
        assert_eq!(body.smooth_px, 0.0, "the GPU path cannot smooth coverage");
        let ShapeSource::Path(path) = &body.source else {
            panic!("the cable stores its outline");
        };
        assert_eq!(path.subpaths.len(), 1, "one simple loop");
        assert!(path.subpaths[0].closed);
        assert_eq!(path.fill_rule, FillRule::NonZero);
        assert!(
            body.source.as_polygon().is_some(),
            "the GPU gate accepts the baked tube"
        );
        for child in &children[1..] {
            let LayerKind::Shape(cap) = &child.kind else {
                panic!("terminal is a shape");
            };
            assert_eq!(cap.smooth_px, 0.0);
            assert!(
                cap.source.as_polygon().is_some(),
                "the GPU gate accepts the terminal"
            );
        }
        let data = layer.cable.as_ref().expect("cable authoring data");
        assert_eq!(data.waypoints, meta().waypoints);
        assert!((data.width - meta().width).abs() < 1e-4);
        assert!((data.height - meta().height).abs() < 1e-4);
        assert_eq!(data.cap, meta().cap);
        assert!(data.centerline.is_none(), "legacy waypoint cable");
    }

    #[test]
    fn terminal_choice_adds_caps_and_none_skips_them() {
        let mut hex = meta();
        hex.cap = CableCap::Hex;
        let hex_layer = cable_layer(&hex).expect("hex cable");
        let LayerKind::Group { children } = &hex_layer.kind else {
            panic!("hex cable is a group");
        };
        assert_eq!(children.len(), 3, "body and two hex terminals");

        let mut no_cap = hex;
        no_cap.cap = CableCap::None;
        let plain = cable_layer(&no_cap).expect("plain cable");
        let LayerKind::Group { children } = &plain.kind else {
            panic!("plain cable is a group");
        };
        assert_eq!(children.len(), 1, "the tube's own round cap remains");
    }

    #[test]
    fn an_old_grouped_cable_gains_authoring_data() {
        let centerline = vec![
            Vec2::new(20.0, 50.0),
            Vec2::new(80.0, 50.0),
            Vec2::new(120.0, 70.0),
        ];
        let mut body = Layer::shape(
            "Cable body",
            ShapeSource::Path(Path::from_polygon(&centerline)),
        );
        if let LayerKind::Shape(shape) = &mut body.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 26.0;
            shape.radius_px = 13.0;
            shape.height_px = 12.0;
        }
        let cap_source =
            cable_cap_source(CableCap::Round, centerline[0], Vec2::X, 39.0).expect("round cap");
        let mut cap = Layer::shape("Cable cap A", cap_source);
        if let LayerKind::Shape(shape) = &mut cap.kind {
            shape.radius_px = 39.0 * 0.42;
        }
        let group = Layer::group("Cable", vec![body, cap]);
        let meta = infer_cable_meta(&group).expect("old cable is recognised");
        assert!((meta.width - 26.0).abs() < 1e-4);
        assert!((meta.height - 12.0).abs() < 1e-4);
        assert_eq!(meta.cap, CableCap::Round);
        assert!((meta.cap_scale - 1.5).abs() < 1e-3);

        let mut doc = crate::layers::Document::new(glam::UVec2::splat(128));
        doc.layers.push(group);
        migrate_cables(&mut doc);
        assert!(doc.layers[0].cable.is_some());
        let LayerKind::Group { children } = &doc.layers[0].kind else {
            panic!("cable stays a group");
        };
        let LayerKind::Shape(body) = &children[0].kind else {
            panic!("body is a shape");
        };
        assert!(!body.style.is_stroke(), "migration bakes the band");
        assert_eq!(body.smooth_px, 0.0);
        assert!(
            body.source.as_polygon().is_some(),
            "migration enables the GPU path"
        );
    }

    #[test]
    fn one_waypoint_is_not_a_cable() {
        let mut short = meta();
        short.waypoints = vec![Vec2::new(10.0, 10.0)];
        assert!(cable_layer(&short).is_none());
        assert!(smooth_cable(&[]).is_none());
    }

    #[test]
    fn a_diagonal_cable_with_hex_terminals_builds() {
        let meta = CableMeta::new(
            vec![Vec2::new(30.0, 40.0), Vec2::new(140.0, 80.0)],
            30.0,
            10.0,
            CableCap::Hex,
            1.5,
        );
        let layer = cable_layer(&meta).expect("diagonal hex cable");
        let LayerKind::Group { children } = &layer.kind else {
            panic!("cable is a group");
        };
        assert_eq!(children.len(), 3);
        assert!(children.iter().all(|child| match &child.kind {
            LayerKind::Shape(shape) => shape.source.as_polygon().is_some(),
            _ => false,
        }));
    }

    #[test]
    fn a_bezier_path_becomes_a_cable_centerline() {
        use crate::path::{Node, SubPath};
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(20.0, 40.0)));
        subpath.nodes.push(Node::smooth(
            Vec2::new(80.0, 40.0),
            Vec2::new(-20.0, 0.0),
            Vec2::new(20.0, 0.0),
        ));
        subpath.nodes.push(Node::corner(Vec2::new(140.0, 80.0)));
        path.subpaths.push(subpath);

        let meta =
            CableMeta::from_path(&path, 26.0, 12.0, CableCap::Round, 1.5).expect("path cable meta");
        assert!(meta.centerline.is_some(), "the vector path is stored");
        let layer = cable_layer(&meta).expect("cable from path");
        let LayerKind::Group { children } = &layer.kind else {
            panic!("cable is a group");
        };
        assert_eq!(children.len(), 3, "body and two round terminals");
    }

    #[test]
    fn a_closed_path_makes_a_loop_without_terminal_caps() {
        let path = Path::from_polygon(&[
            Vec2::new(30.0, 30.0),
            Vec2::new(120.0, 30.0),
            Vec2::new(75.0, 110.0),
        ]);
        let mut meta = CableMeta::from_path(&path, 20.0, 8.0, CableCap::None, 1.5)
            .expect("closed path cable meta");
        meta.cap = CableCap::Hex;
        let layer = cable_layer(&meta).expect("looped cable");
        let LayerKind::Group { children } = &layer.kind else {
            panic!("cable is a group");
        };
        assert_eq!(children.len(), 1, "a loop has no ends to cap");
    }
}
