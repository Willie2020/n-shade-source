//! SVG import: parse filled SVG shapes into editable [`Path`] geometry.
//!
//! Supports `<path>`, `<rect>`, `<circle>`, `<ellipse>`, `<polygon>` and
//! `<polyline>` elements, nested `<g>` transforms, `viewBox` fitting, fill
//! rules, and inherited `fill`/`display` state. Strokes, `<use>`, patterns,
//! clip paths and masks are intentionally ignored; filled geometry is the
//! useful subset for normal-map authoring.

use std::path::Path as FsPath;

use glam::{Affine2, Mat2, UVec2, Vec2};
use roxmltree::Node as XmlNode;
use svgtypes::{
    NumberListParser, PointsParser, SimplifyingPathParser, TransformListParser, TransformListToken,
};

use crate::path::{Bounds, FillRule, Node, NodeKind, Path, SubPath};
use crate::{CoreError, Result};

/// One filled SVG element converted to document geometry.
#[derive(Clone, Debug)]
pub struct SvgShape {
    pub name: String,
    pub path: Path,
}

/// Load and convert an SVG file to paths fitted into a document of `target`
/// pixels.
pub fn load_svg(path: impl AsRef<FsPath>, target: UVec2) -> Result<Vec<SvgShape>> {
    let text = std::fs::read_to_string(path)?;
    parse_svg(&text, target)
}

/// Parse SVG markup and convert it to paths fitted into a document of `target`
/// pixels.
pub fn parse_svg(text: &str, target: UVec2) -> Result<Vec<SvgShape>> {
    let document = roxmltree::Document::parse(text)
        .map_err(|error| CoreError::Message(format!("SVG parse error: {error}")))?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" {
        return Err(CoreError::Message("SVG root element must be <svg>".into()));
    }

    let inherited = Style::default();
    let root_transform = root
        .attribute("transform")
        .map(parse_transform)
        .unwrap_or(Affine2::IDENTITY);
    let mut shapes = Vec::new();
    let mut counter = 0usize;
    for child in root.children().filter(XmlNode::is_element) {
        visit(child, root_transform, inherited, &mut shapes, &mut counter);
    }

    if shapes.is_empty() {
        return Err(CoreError::Message(
            "SVG contains no filled paths or shapes".into(),
        ));
    }

    let source = root
        .attribute("viewBox")
        .and_then(parse_view_box)
        .or_else(|| {
            let width = root.attribute("width").and_then(parse_length);
            let height = root.attribute("height").and_then(parse_length);
            width
                .zip(height)
                .filter(|(w, h)| *w > 0.0 && *h > 0.0)
                .map(|(w, h)| Bounds {
                    min: Vec2::ZERO,
                    max: Vec2::new(w, h),
                })
        })
        .or_else(|| {
            let mut min = Vec2::splat(f32::INFINITY);
            let mut max = Vec2::splat(f32::NEG_INFINITY);
            let mut any = false;
            for shape in &shapes {
                if let Some(bounds) = shape.path.bounds() {
                    min = min.min(bounds.min);
                    max = max.max(bounds.max);
                    any = true;
                }
            }
            any.then_some(Bounds { min, max })
        })
        .unwrap_or(Bounds {
            min: Vec2::ZERO,
            max: Vec2::splat(100.0),
        });

    let fit = fit_transform(source, target);
    for shape in &mut shapes {
        shape.path.apply_transform(fit);
    }
    shapes.retain(|shape| {
        shape
            .path
            .bounds()
            .is_some_and(|bounds| bounds.size().min_element() > 0.01)
    });
    if shapes.is_empty() {
        return Err(CoreError::Message("SVG shapes have no visible area".into()));
    }
    Ok(shapes)
}

#[derive(Clone, Copy)]
struct Style {
    fill: bool,
    fill_rule: FillRule,
    visible: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: true,
            fill_rule: FillRule::NonZero,
            visible: true,
        }
    }
}

impl Style {
    fn for_node(self, node: XmlNode) -> Self {
        let mut style = self;
        if let Some(value) = node.attribute("fill") {
            style.fill = !value.trim().eq_ignore_ascii_case("none");
        }
        if let Some(value) = node.attribute("fill-rule") {
            style.fill_rule = parse_fill_rule(value);
        }
        if node
            .attribute("display")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("none"))
        {
            style.visible = false;
        }
        if let Some(declarations) = node.attribute("style") {
            for declaration in declarations.split(';') {
                let Some((key, value)) = declaration.split_once(':') else {
                    continue;
                };
                match key.trim().to_ascii_lowercase().as_str() {
                    "fill" => style.fill = !value.trim().eq_ignore_ascii_case("none"),
                    "fill-rule" => style.fill_rule = parse_fill_rule(value),
                    "display" if value.trim().eq_ignore_ascii_case("none") => {
                        style.visible = false;
                    }
                    _ => {}
                }
            }
        }
        style
    }
}

fn visit(
    node: XmlNode,
    parent: Affine2,
    inherited: Style,
    shapes: &mut Vec<SvgShape>,
    counter: &mut usize,
) {
    let style = inherited.for_node(node);
    if !style.visible {
        return;
    }
    let local = node
        .attribute("transform")
        .map(parse_transform)
        .unwrap_or(Affine2::IDENTITY);
    let transform = parent * local;

    match node.tag_name().name() {
        "g" | "a" | "svg" => {
            for child in node.children().filter(XmlNode::is_element) {
                visit(child, transform, style, shapes, counter);
            }
        }
        "defs" | "clipPath" | "mask" | "symbol" | "marker" | "pattern" | "metadata" | "title"
        | "desc" | "style" | "script" | "use" | "line" => {}
        "path" => {
            if !style.fill {
                return;
            }
            let Some(data) = node.attribute("d") else {
                return;
            };
            let path = parse_path_data(data, style.fill_rule);
            push_shape(path, node, transform, shapes, counter);
        }
        "rect" => {
            if !style.fill {
                return;
            }
            let x = attr_number(node, "x", 0.0);
            let y = attr_number(node, "y", 0.0);
            let width = attr_number(node, "width", 0.0);
            let height = attr_number(node, "height", 0.0);
            let mut rx = attr_number(node, "rx", 0.0);
            let mut ry = attr_number(node, "ry", rx);
            if rx <= 1e-4 && ry > 1e-4 {
                rx = ry;
            } else if ry <= 1e-4 && rx > 1e-4 {
                ry = rx;
            }
            if width > 0.0 && height > 0.0 {
                let path = rect_path(x, y, width, height, rx, ry, style.fill_rule);
                push_shape(path, node, transform, shapes, counter);
            }
        }
        "circle" => {
            if !style.fill {
                return;
            }
            let cx = attr_number(node, "cx", 0.0);
            let cy = attr_number(node, "cy", 0.0);
            let r = attr_number(node, "r", 0.0);
            if r > 0.0 {
                let path = ellipse_path(cx, cy, r, r, style.fill_rule);
                push_shape(path, node, transform, shapes, counter);
            }
        }
        "ellipse" => {
            if !style.fill {
                return;
            }
            let cx = attr_number(node, "cx", 0.0);
            let cy = attr_number(node, "cy", 0.0);
            let rx = attr_number(node, "rx", 0.0);
            let ry = attr_number(node, "ry", 0.0);
            if rx > 0.0 && ry > 0.0 {
                let path = ellipse_path(cx, cy, rx, ry, style.fill_rule);
                push_shape(path, node, transform, shapes, counter);
            }
        }
        "polygon" | "polyline" => {
            if !style.fill {
                return;
            }
            let Some(points) = node.attribute("points") else {
                return;
            };
            let points: Vec<Vec2> = PointsParser::from(points)
                .map(|(x, y)| Vec2::new(x as f32, y as f32))
                .collect();
            if points.len() >= 3 {
                let path = polygon_path(&points, style.fill_rule);
                push_shape(path, node, transform, shapes, counter);
            }
        }
        _ => {}
    }
}

fn push_shape(
    mut path: Path,
    node: XmlNode,
    transform: Affine2,
    shapes: &mut Vec<SvgShape>,
    counter: &mut usize,
) {
    if path.node_count() < 3 {
        return;
    }
    path.apply_transform(transform);
    if path
        .bounds()
        .is_none_or(|bounds| bounds.size().min_element() <= 0.01)
    {
        return;
    }
    *counter += 1;
    let name = node
        .attribute("id")
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            node.attributes()
                .find(|attribute| attribute.name() == "label")
                .map(|attribute| attribute.value().to_owned())
        })
        .unwrap_or_else(|| format!("SVG path {counter}"));
    shapes.push(SvgShape { name, path });
}

fn parse_path_data(data: &str, fill_rule: FillRule) -> Path {
    let mut subpaths = Vec::new();
    let mut current: Option<SubPath> = None;
    let mut position = Vec2::ZERO;
    let mut start = Vec2::ZERO;

    for segment in SimplifyingPathParser::from(data) {
        let Ok(segment) = segment else {
            break;
        };
        match segment {
            svgtypes::SimplePathSegment::MoveTo { x, y } => {
                finish_subpath(&mut subpaths, &mut current);
                position = Vec2::new(x as f32, y as f32);
                start = position;
                current = Some(SubPath {
                    nodes: vec![Node::corner(position)],
                    closed: false,
                });
            }
            svgtypes::SimplePathSegment::LineTo { x, y } => {
                let path = ensure_subpath(&mut current, position);
                position = Vec2::new(x as f32, y as f32);
                path.nodes.push(Node::corner(position));
            }
            svgtypes::SimplePathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let p0 = position;
                let p1 = Vec2::new(x1 as f32, y1 as f32);
                let p2 = Vec2::new(x2 as f32, y2 as f32);
                let p3 = Vec2::new(x as f32, y as f32);
                push_curve(&mut current, p0, p1, p2, p3);
                position = p3;
            }
            svgtypes::SimplePathSegment::Quadratic { x1, y1, x, y } => {
                let p0 = position;
                let control = Vec2::new(x1 as f32, y1 as f32);
                let p3 = Vec2::new(x as f32, y as f32);
                let p1 = p0 + (control - p0) * (2.0 / 3.0);
                let p2 = p3 + (control - p3) * (2.0 / 3.0);
                push_curve(&mut current, p0, p1, p2, p3);
                position = p3;
            }
            svgtypes::SimplePathSegment::ClosePath => {
                if let Some(mut subpath) = current.take() {
                    if subpath.nodes.len() >= 2 {
                        let first = subpath.nodes[0].pos;
                        if subpath
                            .nodes
                            .last()
                            .is_some_and(|node| (node.pos - first).length() < 1e-4)
                        {
                            let removed = subpath.nodes.pop().expect("nodes is non-empty");
                            if let Some(first_node) = subpath.nodes.first_mut() {
                                first_node.handle_in = removed.handle_in;
                            }
                        } else {
                            if let Some(last) = subpath.nodes.last_mut() {
                                last.handle_out = Vec2::ZERO;
                            }
                            if let Some(first_node) = subpath.nodes.first_mut() {
                                first_node.handle_in = Vec2::ZERO;
                            }
                        }
                        subpath.closed = true;
                        subpaths.push(subpath);
                    }
                }
                position = start;
            }
        }
    }
    finish_subpath(&mut subpaths, &mut current);
    Path {
        subpaths,
        fill_rule,
    }
}

fn ensure_subpath(current: &mut Option<SubPath>, position: Vec2) -> &mut SubPath {
    current.get_or_insert_with(|| SubPath {
        nodes: vec![Node::corner(position)],
        closed: false,
    })
}

fn push_curve(current: &mut Option<SubPath>, p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2) {
    let subpath = ensure_subpath(current, p0);
    if let Some(last) = subpath.nodes.last_mut() {
        last.handle_out = p1 - last.pos;
        last.kind = NodeKind::Smooth;
    }
    let mut end = Node::corner(p3);
    end.handle_in = p2 - p3;
    end.kind = NodeKind::Smooth;
    subpath.nodes.push(end);
}

fn finish_subpath(subpaths: &mut Vec<SubPath>, current: &mut Option<SubPath>) {
    if let Some(subpath) = current.take() {
        if subpath.nodes.len() >= 2 {
            subpaths.push(subpath);
        }
    }
}

fn rect_path(x: f32, y: f32, width: f32, height: f32, rx: f32, ry: f32, rule: FillRule) -> Path {
    let rx = rx.max(0.0).min(width * 0.5);
    let ry = ry.max(0.0).min(height * 0.5);
    let mut points = Vec::new();
    if rx <= 1e-4 || ry <= 1e-4 {
        points.extend([
            Vec2::new(x, y),
            Vec2::new(x + width, y),
            Vec2::new(x + width, y + height),
            Vec2::new(x, y + height),
        ]);
    } else {
        const STEPS: usize = 8;
        let corners = [
            (
                Vec2::new(x + width - rx, y + ry),
                -std::f32::consts::FRAC_PI_2,
            ),
            (Vec2::new(x + width - rx, y + height - ry), 0.0),
            (
                Vec2::new(x + rx, y + height - ry),
                std::f32::consts::FRAC_PI_2,
            ),
            (Vec2::new(x + rx, y + ry), std::f32::consts::PI),
        ];
        for (center, start_angle) in corners {
            for step in 0..=STEPS {
                let angle = start_angle + std::f32::consts::FRAC_PI_2 * step as f32 / STEPS as f32;
                points.push(Vec2::new(
                    center.x + angle.cos() * rx,
                    center.y + angle.sin() * ry,
                ));
            }
        }
    }
    Path {
        subpaths: vec![SubPath {
            nodes: points.iter().map(|point| Node::corner(*point)).collect(),
            closed: true,
        }],
        fill_rule: rule,
    }
}

fn ellipse_path(cx: f32, cy: f32, rx: f32, ry: f32, rule: FillRule) -> Path {
    const KAPPA: f32 = 0.552_284_8;
    let hx = rx * KAPPA;
    let hy = ry * KAPPA;
    let nodes = vec![
        Node::smooth(
            Vec2::new(cx, cy - ry),
            Vec2::new(hx, 0.0),
            Vec2::new(-hx, 0.0),
        ),
        Node::smooth(
            Vec2::new(cx - rx, cy),
            Vec2::new(0.0, -hy),
            Vec2::new(0.0, hy),
        ),
        Node::smooth(
            Vec2::new(cx, cy + ry),
            Vec2::new(-hx, 0.0),
            Vec2::new(hx, 0.0),
        ),
        Node::smooth(
            Vec2::new(cx + rx, cy),
            Vec2::new(0.0, hy),
            Vec2::new(0.0, -hy),
        ),
    ];
    Path {
        subpaths: vec![SubPath {
            nodes,
            closed: true,
        }],
        fill_rule: rule,
    }
}

fn polygon_path(points: &[Vec2], rule: FillRule) -> Path {
    Path {
        subpaths: vec![SubPath {
            nodes: points.iter().map(|point| Node::corner(*point)).collect(),
            closed: true,
        }],
        fill_rule: rule,
    }
}

fn parse_view_box(value: &str) -> Option<Bounds> {
    let numbers: Vec<f32> = NumberListParser::from(value)
        .filter_map(|number| number.ok())
        .map(|number| number as f32)
        .collect();
    if numbers.len() < 4 || numbers[2] <= 0.0 || numbers[3] <= 0.0 {
        return None;
    }
    Some(Bounds {
        min: Vec2::new(numbers[0], numbers[1]),
        max: Vec2::new(numbers[0] + numbers[2], numbers[1] + numbers[3]),
    })
}

fn fit_transform(source: Bounds, target: UVec2) -> Affine2 {
    let target = Vec2::new(target.x.max(1) as f32, target.y.max(1) as f32);
    let size = source.size().max(Vec2::splat(1e-4));
    let scale = (target * 0.9 / size).min_element().max(1e-4);
    let offset = target * 0.5 - source.center() * scale;
    Affine2::from_scale_angle_translation(Vec2::splat(scale), 0.0, offset)
}

fn parse_transform(value: &str) -> Affine2 {
    let mut transform = Affine2::IDENTITY;
    for token in TransformListParser::from(value) {
        let Ok(token) = token else {
            break;
        };
        let next = match token {
            TransformListToken::Matrix { a, b, c, d, e, f } => Affine2::from_cols(
                Vec2::new(a as f32, b as f32),
                Vec2::new(c as f32, d as f32),
                Vec2::new(e as f32, f as f32),
            ),
            TransformListToken::Translate { tx, ty } => {
                Affine2::from_translation(Vec2::new(tx as f32, ty as f32))
            }
            TransformListToken::Scale { sx, sy } => {
                Affine2::from_scale(Vec2::new(sx as f32, sy as f32))
            }
            TransformListToken::Rotate { angle } => Affine2::from_scale_angle_translation(
                Vec2::ONE,
                (angle as f32).to_radians(),
                Vec2::ZERO,
            ),
            TransformListToken::SkewX { angle } => Affine2::from_mat2_translation(
                Mat2::from_cols(
                    Vec2::new(1.0, 0.0),
                    Vec2::new((angle as f32).to_radians().tan(), 1.0),
                ),
                Vec2::ZERO,
            ),
            TransformListToken::SkewY { angle } => Affine2::from_mat2_translation(
                Mat2::from_cols(
                    Vec2::new(1.0, (angle as f32).to_radians().tan()),
                    Vec2::new(0.0, 1.0),
                ),
                Vec2::ZERO,
            ),
        };
        transform *= next;
    }
    transform
}

fn parse_fill_rule(value: &str) -> FillRule {
    if value.trim().eq_ignore_ascii_case("evenodd") {
        FillRule::EvenOdd
    } else {
        FillRule::NonZero
    }
}

fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    if value.is_empty() || value.ends_with('%') {
        return None;
    }
    NumberListParser::from(value)
        .next()
        .and_then(|number| number.ok())
        .map(|number| number as f32)
        .filter(|number| number.is_finite() && *number > 0.0)
}

fn attr_number(node: XmlNode, name: &str, default: f32) -> f32 {
    node.attribute(name)
        .and_then(parse_length_unbounded)
        .unwrap_or(default)
}

fn parse_length_unbounded(value: &str) -> Option<f32> {
    let value = value.trim();
    if value.is_empty() || value.ends_with('%') {
        return None;
    }
    NumberListParser::from(value)
        .next()
        .and_then(|number| number.ok())
        .map(|number| number as f32)
        .filter(|number| number.is_finite())
}

/// Utility used by tests and callers that want to inspect path data only.
pub fn parse_path(data: &str, fill_rule: FillRule) -> Path {
    parse_path_data(data, fill_rule)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::ShapeSource;

    #[test]
    fn parses_basic_shapes_and_fit() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
            <rect x="10" y="10" width="40" height="30" fill="#fff"/>
            <circle cx="70" cy="30" r="15"/>
            <path d="M10 60 C20 50 30 70 40 60 L40 90 L10 90 Z" fill-rule="evenodd"/>
        </svg>"##;
        let shapes = parse_svg(svg, UVec2::splat(256)).unwrap();
        assert_eq!(shapes.len(), 3);
        assert_eq!(shapes[2].path.fill_rule, FillRule::EvenOdd);
        for shape in &shapes {
            let bounds = shape.path.bounds().unwrap();
            assert!(bounds.min.x >= 0.0 && bounds.min.y >= 0.0);
            assert!(bounds.max.x <= 256.0 && bounds.max.y <= 256.0);
            assert!(bounds.size().min_element() > 1.0);
        }
    }

    #[test]
    fn applies_group_transforms() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
            <g transform="translate(10 20) scale(2)">
                <rect x="0" y="0" width="20" height="10"/>
            </g>
        </svg>"##;
        let shapes = parse_svg(svg, UVec2::splat(100)).unwrap();
        let bounds = shapes[0].path.bounds().unwrap();
        assert!((bounds.min.x - 14.0).abs() < 0.5);
        assert!((bounds.min.y - 23.0).abs() < 0.5);
        assert!((bounds.max.x - 50.0).abs() < 0.5);
        assert!((bounds.max.y - 41.0).abs() < 0.5);
    }

    #[test]
    fn parses_quadratic_and_closes() {
        let path = parse_path("M0 0 L10 0 Q15 5 10 10 Z", FillRule::NonZero);
        assert_eq!(path.subpaths.len(), 1);
        assert_eq!(path.subpaths[0].nodes.len(), 3);
        assert!(path.subpaths[0].closed);
    }

    #[test]
    fn skips_unfilled_and_hidden_elements() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
            <rect x="0" y="0" width="5" height="5" fill="none"/>
            <rect x="1" y="1" width="5" height="5" display="none"/>
            <rect x="2" y="2" width="4" height="4"/>
        </svg>"##;
        let shapes = parse_svg(svg, UVec2::splat(64)).unwrap();
        assert_eq!(shapes.len(), 1);
    }

    #[test]
    fn evenodd_hole_rasterizes() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
            <path fill-rule="evenodd" d="M10 10 H90 V90 H10 Z M30 30 H70 V70 H30 Z"/>
        </svg>"##;
        let shapes = parse_svg(svg, UVec2::splat(100)).unwrap();
        let coverage = ShapeSource::Path(shapes[0].path.clone()).rasterize(100, 100);
        assert!(*coverage.get(50, 50) < 0.1);
        assert!(*coverage.get(14, 50) > 0.8);
    }

    #[test]
    fn rejects_non_svg_and_empty_input() {
        assert!(parse_svg("<html/>", UVec2::splat(16)).is_err());
        assert!(parse_svg(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
            UVec2::splat(16)
        )
        .is_err());
    }
}
