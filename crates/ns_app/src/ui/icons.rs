//! Hand-drawn vector icons and icon buttons.

use super::*;
#[derive(Clone, Copy)]
pub(crate) enum Icon {
    Select,
    Rectangle,
    Ellipse,
    Star,
    Polygon,
    Cable,
    Ruler,
    Lasso,
    LassoPolygon,
    LassoFree,
    LassoLine,
    Grid,
    Uv,
    Rotate,
    Undo,
    Redo,
    Pen,
    Brush,
    NodeEdit,
    AddAnchor,
    DeleteAnchor,
    ConvertPoint,
    CornerNode,
    SmoothNode,
    SymmetricNode,
    ResetHandles,
    Settings,
    Eye,
    Opacity,
    Mask,
    Lock,
    Unlock,
    Trash,
    Fit,
    Pan,
    Download,
    Upload,
    Sliders,
    Cube,
    Projection,
    Bake,
    Sun,
    Pbr,
    PanelLeft,
    PanelRight,
    Stretch,
    Split,
    SplitStacked,
    PathAdd,
    PathSubtract,
    PathIntersect,
    PathXor,
    PathDivide,
    Detach,
}
/// Icons for the layers footer. The tool rail and the viewport bar draw their
/// glyphs from path data instead (see `svg_path`), so only the pack entries the
/// layer rows still use live here.
pub(crate) const PACK_BRUSH: egui::ImageSource<'static> =
    egui::include_image!("../../../../assets/icons/brush.svg");
pub(crate) const PACK_ARROW_UP: egui::ImageSource<'static> =
    egui::include_image!("../../../../assets/icons/arrow-up.svg");
pub(crate) const PACK_ARROW_DOWN: egui::ImageSource<'static> =
    egui::include_image!("../../../../assets/icons/arrow-down.svg");
pub(crate) const PACK_TRASH: egui::ImageSource<'static> =
    egui::include_image!("../../../../assets/icons/trash.svg");

pub(crate) fn draw_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: Icon,
    color: egui::Color32,
) {
    let c = rect.center();
    let size = rect.width().min(rect.height());
    let r = size * 0.5;
    // Tool glyphs are drawn on a shared 20 x 20 grid with one stroke weight,
    // so every icon has the same optical size and weight in the rail.
    let u = size / 20.0;
    let pt = |x: f32, y: f32| rect.min + egui::vec2(x, y) * u;
    let stroke = egui::Stroke::new((1.6 * u).max(1.25), color);
    let hairline = egui::Stroke::new((1.15 * u).max(0.9), color);

    let poly = |points: Vec<egui::Pos2>, closed: bool, stroke: egui::Stroke| {
        if closed {
            painter.add(egui::Shape::closed_line(points, stroke));
        } else {
            painter.add(egui::Shape::line(points, stroke));
        }
    };
    match icon {
        Icon::Select => {
            // Filled cursor arrow: the standard "active tool" silhouette.
            let points = vec![
                pt(5.0, 2.9),
                pt(5.0, 15.3),
                pt(8.3, 12.0),
                pt(10.5, 17.1),
                pt(12.7, 16.0),
                pt(10.5, 11.0),
                pt(15.1, 10.6),
            ];
            painter.add(egui::Shape::convex_polygon(
                points,
                color,
                egui::Stroke::new(1.0 * u, color),
            ));
        }
        Icon::Rectangle => {
            // Outline weights match Pen/Lasso so the rail reads as one set.
            let frame = egui::Rect::from_min_max(pt(3.6, 5.6), pt(16.4, 14.6));
            painter.rect_stroke(frame, 1.6 * u, stroke, egui::StrokeKind::Inside);
        }
        Icon::Ellipse => {
            painter.circle_stroke(pt(10.0, 10.2), 5.9 * u, stroke);
        }
        Icon::Star => {
            painter.add(egui::Shape::closed_line(
                star_points(pt(10.0, 10.3), 7.0 * u, 3.0 * u, 5),
                stroke,
            ));
        }
        Icon::Polygon => {
            // Regular pentagon: the click-to-place polygon tool.
            let centre = pt(10.0, 10.4);
            let points = (0..5)
                .map(|index| {
                    let angle =
                        -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * index as f32 / 5.0;
                    centre + egui::vec2(angle.cos(), angle.sin()) * (7.4 * u)
                })
                .collect();
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        Icon::Cable => {
            // A slack cable with a round terminal at each end.
            let points: Vec<egui::Pos2> = (0..=14)
                .map(|index| {
                    let t = index as f32 / 14.0;
                    pt(
                        3.2 + t * 13.6,
                        10.0 + (t * std::f32::consts::TAU).sin() * 3.2,
                    )
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
            painter.circle_stroke(pt(3.2, 10.0), 2.1 * u, stroke);
            painter.circle_stroke(pt(16.8, 10.0), 2.1 * u, stroke);
        }
        Icon::Grid => {
            // Artboard with a centre grid: the 2D canvas view.
            let frame = egui::Rect::from_min_max(pt(3.4, 4.8), pt(16.6, 15.2));
            painter.rect_stroke(frame, 1.4 * u, stroke, egui::StrokeKind::Inside);
            painter.line_segment([pt(10.0, 4.8), pt(10.0, 15.2)], hairline);
            painter.line_segment([pt(3.4, 10.0), pt(16.6, 10.0)], hairline);
        }
        Icon::Uv => {
            // A UV shell: square border with an island seam inside.
            let square = rect.shrink(size * 0.18);
            painter.rect_stroke(
                square,
                1.5,
                egui::Stroke::new(1.3_f32, color),
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    egui::pos2(square.left() + r * 0.55, square.top()),
                    egui::pos2(square.left() + r * 0.55, square.top() + r * 1.1),
                ],
                egui::Stroke::new(1.1_f32, color),
            );
            painter.line_segment(
                [
                    egui::pos2(square.left() + r * 0.55, square.top() + r * 1.1),
                    egui::pos2(square.right(), square.top() + r * 1.1),
                ],
                egui::Stroke::new(1.1_f32, color),
            );
            painter.circle_filled(
                egui::pos2(square.left() + r * 0.55, square.top() + r * 1.1),
                1.2,
                color,
            );
        }
        Icon::Rotate => {
            let points = arc_points(c, r * 0.68, -2.5, 1.25, 18);
            painter.add(egui::Shape::line(points, stroke));
            arrow_head(
                painter,
                c + egui::vec2(1.25f32.cos(), 1.25f32.sin()) * r * 0.68,
                -0.35,
                stroke,
            );
        }
        Icon::Undo => {
            let points = arc_points(c, r * 0.65, 0.2, 4.3, 16);
            painter.add(egui::Shape::line(points, stroke));
            arrow_head(
                painter,
                c + egui::vec2(0.2f32.cos(), 0.2f32.sin()) * r * 0.65,
                1.9,
                stroke,
            );
        }
        Icon::Redo => {
            let points = arc_points(c, r * 0.65, 1.5, 5.7, 16);
            painter.add(egui::Shape::line(points, stroke));
            arrow_head(
                painter,
                c + egui::vec2(5.7f32.cos(), 5.7f32.sin()) * r * 0.65,
                -1.2,
                stroke,
            );
        }
        Icon::Pen => {
            // Slender nib pointing to the lower left. Below 28 px the hole and
            // slit would close up, so the small size uses the solid silhouette
            // with its characteristic notched back edge instead.
            let tip = pt(4.2, 16.6);
            let back_left = pt(10.4, 3.2);
            let back_right = pt(14.8, 7.6);
            let notch = pt(11.2, 6.2);
            if size >= 28.0 {
                poly(vec![back_left, notch, back_right, tip], true, stroke);
                painter.circle_stroke(pt(9.6, 11.0), 1.5 * u, hairline);
                painter.line_segment([tip, pt(10.2, 10.4)], hairline);
            } else {
                painter.add(egui::Shape::Path(egui::epaint::PathShape {
                    points: vec![tip, back_right, notch, back_left],
                    closed: true,
                    fill: color,
                    stroke: egui::epaint::PathStroke::NONE,
                }));
            }
        }
        Icon::Ruler => {
            // Outline drafting ruler, matched to the supplied icon pack's
            // 1.75 px stroke style (the old version was a filled bar).
            let start = pt(3.4, 14.2);
            let end = pt(14.4, 3.2);
            let across = egui::vec2(4.4, 4.4) * u;
            poly(vec![start, start + across, end + across, end], true, stroke);
            for step in 1..4 {
                let t = step as f32 / 4.0;
                let base = start + (end - start) * t;
                let normal = egui::vec2(-0.707, 0.707) * u * 1.9;
                painter.line_segment([base + across, base + across - normal], hairline);
            }
        }
        Icon::Lasso => {
            // Rope loop with the knot tail, the classic lasso silhouette.
            poly(
                vec![
                    pt(6.6, 4.0),
                    pt(10.2, 2.8),
                    pt(13.8, 4.6),
                    pt(15.2, 8.2),
                    pt(13.8, 11.8),
                    pt(10.4, 13.4),
                    pt(7.0, 12.4),
                    pt(5.4, 9.4),
                ],
                true,
                stroke,
            );
            painter.line_segment([pt(5.6, 11.8), pt(3.6, 16.4)], hairline);
            painter.circle_stroke(pt(3.2, 17.0), 1.0 * u, hairline);
        }
        Icon::LassoPolygon => {
            // Straight-segment lasso: a pentagon with its vertices marked.
            let corners = vec![
                pt(10.0, 3.2),
                pt(16.4, 8.0),
                pt(13.8, 16.4),
                pt(6.2, 16.4),
                pt(3.6, 8.0),
            ];
            poly(corners.clone(), true, hairline);
            for corner in corners {
                painter.circle_filled(corner, 1.4 * u, color);
            }
        }
        Icon::LassoFree => {
            // Freehand loop: an irregular closed squiggle.
            poly(
                vec![
                    pt(4.2, 8.6),
                    pt(6.4, 4.2),
                    pt(10.4, 6.0),
                    pt(13.2, 3.0),
                    pt(16.2, 7.4),
                    pt(14.8, 12.4),
                    pt(11.2, 15.4),
                    pt(7.0, 13.6),
                    pt(3.6, 14.6),
                    pt(2.8, 11.0),
                ],
                true,
                hairline,
            );
        }
        Icon::LassoLine => {
            // Straight cut: a diagonal knife line with marked ends.
            let start = pt(3.6, 16.2);
            let end = pt(16.4, 3.8);
            painter.line_segment([start, end], stroke);
            painter.circle_filled(start, 1.5 * u, color);
            painter.circle_filled(end, 1.5 * u, color);
        }
        Icon::Brush => {
            // Brush: thin handle, a perpendicular ferrule band, then a bristle
            // fan with a flat end. The flat fan is what distinguishes it from
            // a pen or a blade.
            painter.line_segment(
                [pt(17.0, 3.0), pt(11.2, 8.8)],
                egui::Stroke::new((1.9 * u).max(1.3), color),
            );
            painter.line_segment(
                [pt(9.2, 9.4), pt(12.6, 12.8)],
                egui::Stroke::new((2.6 * u).max(1.8), color),
            );
            painter.add(egui::Shape::convex_polygon(
                vec![pt(11.7, 12.0), pt(8.2, 16.4), pt(4.6, 12.8), pt(9.1, 9.2)],
                color,
                egui::Stroke::new(0.8 * u, color),
            ));
        }
        Icon::NodeEdit => {
            let y = c.y;
            painter.line_segment(
                [egui::pos2(c.x - r * 0.72, y), egui::pos2(c.x + r * 0.72, y)],
                stroke,
            );
            painter.circle_stroke(egui::pos2(c.x - r * 0.72, y), r * 0.14, stroke);
            painter.circle_stroke(egui::pos2(c.x + r * 0.72, y), r * 0.14, stroke);
            let anchor = egui::Rect::from_center_size(c, egui::vec2(r * 0.46, r * 0.46));
            painter.rect_filled(anchor, 1.0, color);
            painter.rect_stroke(
                anchor,
                1.0,
                egui::Stroke::new(1.0_f32, APP_BG),
                egui::StrokeKind::Inside,
            );
        }
        Icon::AddAnchor | Icon::DeleteAnchor => {
            let y = c.y - r * 0.12;
            painter.line_segment(
                [egui::pos2(c.x - r * 0.82, y), egui::pos2(c.x + r * 0.82, y)],
                stroke,
            );
            painter.circle_stroke(egui::pos2(c.x - r * 0.82, y), r * 0.13, stroke);
            painter.circle_stroke(egui::pos2(c.x + r * 0.82, y), r * 0.13, stroke);
            let anchor =
                egui::Rect::from_center_size(egui::pos2(c.x, y), egui::vec2(r * 0.4, r * 0.4));
            painter.rect_filled(anchor, 1.0, color);
            let badge = egui::pos2(c.x + r * 0.72, c.y + r * 0.64);
            painter.circle_filled(badge, r * 0.36, color);
            let mark = egui::Stroke::new(1.2_f32, APP_BG);
            painter.line_segment(
                [
                    badge + egui::vec2(-r * 0.18, 0.0),
                    badge + egui::vec2(r * 0.18, 0.0),
                ],
                mark,
            );
            if matches!(icon, Icon::AddAnchor) {
                painter.line_segment(
                    [
                        badge + egui::vec2(0.0, -r * 0.18),
                        badge + egui::vec2(0.0, r * 0.18),
                    ],
                    mark,
                );
            }
        }
        Icon::ConvertPoint => {
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.75, c.y + r * 0.45),
                    egui::pos2(c.x, c.y - r * 0.1),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y - r * 0.1),
                    egui::pos2(c.x + r * 0.75, c.y + r * 0.45),
                ],
                stroke,
            );
            let anchor = egui::Rect::from_center_size(
                egui::pos2(c.x, c.y - r * 0.1),
                egui::vec2(r * 0.38, r * 0.38),
            );
            painter.rect_filled(anchor, 1.0, color);
            let arc = arc_points(egui::pos2(c.x, c.y + r * 0.7), r * 0.4, 0.3, 2.8, 8);
            painter.add(egui::Shape::line(arc, egui::Stroke::new(1.1_f32, color)));
        }
        Icon::CornerNode => {
            let vertex = egui::pos2(c.x + r * 0.35, c.y + r * 0.35);
            painter.line_segment([egui::pos2(c.x - r * 0.8, c.y + r * 0.35), vertex], stroke);
            painter.line_segment([vertex, egui::pos2(c.x + r * 0.35, c.y - r * 0.8)], stroke);
            painter.rect_filled(
                egui::Rect::from_center_size(vertex, egui::vec2(r * 0.34, r * 0.34)),
                0.5,
                color,
            );
        }
        Icon::SmoothNode => {
            let points = vec![
                egui::pos2(c.x - r * 0.85, c.y + r * 0.55),
                egui::pos2(c.x - r * 0.15, c.y - r * 0.55),
                egui::pos2(c.x + r * 0.85, c.y + r * 0.55),
            ];
            painter.add(egui::Shape::line(points, stroke));
            painter.circle_filled(egui::pos2(c.x - r * 0.15, c.y - r * 0.55), r * 0.2, color);
        }
        Icon::SymmetricNode => {
            let points = vec![
                egui::pos2(c.x - r * 0.85, c.y + r * 0.5),
                egui::pos2(c.x - r * 0.2, c.y - r * 0.5),
                egui::pos2(c.x + r * 0.2, c.y + r * 0.5),
                egui::pos2(c.x + r * 0.85, c.y - r * 0.5),
            ];
            painter.add(egui::Shape::line(points, stroke));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x, c.y - r * 0.22),
                    egui::pos2(c.x + r * 0.22, c.y),
                    egui::pos2(c.x, c.y + r * 0.22),
                    egui::pos2(c.x - r * 0.22, c.y),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        Icon::ResetHandles => {
            let arc = arc_points(c, r * 0.62, 0.4, 5.6, 10);
            painter.add(egui::Shape::line(arc, stroke));
            arrow_head(
                painter,
                c + egui::vec2(0.4f32.cos(), 0.4f32.sin()) * r * 0.62,
                0.4,
                stroke,
            );
            painter.circle_filled(c, r * 0.18, color);
        }
        Icon::Settings => {
            for index in 0..3 {
                let y = c.y + (index as f32 - 1.0) * r * 0.55;
                painter.line_segment(
                    [egui::pos2(c.x - r * 0.8, y), egui::pos2(c.x + r * 0.8, y)],
                    stroke,
                );
                let knob_x = c.x + (index as f32 - 1.0) * r * 0.38;
                painter.circle_filled(egui::pos2(knob_x, y), 2.3, color);
            }
        }
        Icon::Eye => {
            // Almond outline + pupil. The old concentric circles read as a
            // record/target button, which is exactly how the audit described it.
            let w = size * 0.36;
            let h = size * 0.22;
            let steps = 10;
            let mut upper = Vec::with_capacity(steps + 1);
            let mut lower = Vec::with_capacity(steps + 1);
            for index in 0..=steps {
                let t = index as f32 / steps as f32;
                let x = c.x - w + 2.0 * w * t;
                let bump = (std::f32::consts::PI * t).sin();
                upper.push(egui::pos2(x, c.y - h * bump));
                lower.push(egui::pos2(x, c.y + h * bump));
            }
            painter.add(egui::Shape::line(upper, stroke));
            painter.add(egui::Shape::line(lower, stroke));
            painter.circle_filled(c, r * 0.22, color);
        }
        Icon::Opacity => {
            // Half-filled disc: the standard opacity/coverage glyph. Fill
            // first, then outline, so the rim stays crisp at 16 px.
            let radius = r * 0.78;
            let mut half = Vec::with_capacity(14);
            half.push(c);
            for step in 0..=12 {
                let angle =
                    -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * step as f32 / 12.0;
                half.push(c + egui::vec2(angle.cos(), angle.sin()) * radius);
            }
            painter.add(egui::Shape::convex_polygon(
                half,
                color.gamma_multiply(0.45),
                egui::Stroke::NONE,
            ));
            painter.circle_stroke(c, radius, stroke);
        }
        Icon::Mask => {
            let frame = rect.shrink(size * 0.17);
            painter.rect_stroke(frame, 2.0, stroke, egui::StrokeKind::Inside);
            painter.circle_stroke(frame.center(), frame.width() * 0.23, stroke);
        }
        Icon::Lock | Icon::Unlock => {
            // Padlock: body, keyhole and a shackle that lifts off the body when
            // the layer is unlocked, so the two states read at 18 px.
            let body = egui::Rect::from_min_max(
                egui::pos2(c.x - r * 0.52, c.y - r * 0.06),
                egui::pos2(c.x + r * 0.52, c.y + r * 0.74),
            );
            painter.rect_stroke(body, (r * 0.16).max(1.0), stroke, egui::StrokeKind::Inside);
            painter.circle_filled(egui::pos2(c.x, c.y + r * 0.3), (r * 0.1).max(0.8), color);
            let locked = matches!(icon, Icon::Lock);
            let (centre, end) = if locked {
                (egui::pos2(c.x, c.y - r * 0.06), std::f32::consts::TAU)
            } else {
                // The shackle swings open to the right and sits a little higher.
                (
                    egui::pos2(c.x - r * 0.04, c.y - r * 0.2),
                    std::f32::consts::PI * 1.62,
                )
            };
            let radius = r * 0.3;
            let points: Vec<egui::Pos2> = (0..=12)
                .map(|index| {
                    let angle =
                        std::f32::consts::PI + (end - std::f32::consts::PI) * index as f32 / 12.0;
                    centre + egui::vec2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
        }
        Icon::Trash => {
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.7, c.y - r * 0.55),
                    egui::pos2(c.x + r * 0.7, c.y - r * 0.55),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.25, c.y - r * 0.8),
                    egui::pos2(c.x + r * 0.25, c.y - r * 0.8),
                ],
                stroke,
            );
            painter.rect_stroke(
                egui::Rect::from_min_max(
                    egui::pos2(c.x - r * 0.55, c.y - r * 0.4),
                    egui::pos2(c.x + r * 0.55, c.y + r * 0.8),
                ),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            for offset in [-0.22, 0.22] {
                painter.line_segment(
                    [
                        egui::pos2(c.x + r * offset, c.y - r * 0.18),
                        egui::pos2(c.x + r * offset, c.y + r * 0.58),
                    ],
                    egui::Stroke::new(1.1_f32, color),
                );
            }
        }
        Icon::Fit => {
            let outer = rect.shrink(size * 0.2);
            let arm = size * 0.22;
            for (corner, dx, dy) in [
                (outer.left_top(), 1.0, 1.0),
                (outer.right_top(), -1.0, 1.0),
                (outer.left_bottom(), 1.0, -1.0),
                (outer.right_bottom(), -1.0, -1.0),
            ] {
                painter.line_segment([corner, corner + egui::vec2(arm * dx, 0.0)], stroke);
                painter.line_segment([corner, corner + egui::vec2(0.0, arm * dy)], stroke);
            }
        }
        Icon::Pan => {
            // Four-way move arrows with solid heads: unambiguously "pan".
            let shaft = egui::Stroke::new((2.0 * u).max(1.4), color);
            painter.line_segment([pt(10.0, 4.3), pt(10.0, 15.7)], shaft);
            painter.line_segment([pt(4.3, 10.0), pt(15.7, 10.0)], shaft);
            for head in [
                [(10.0, 2.2), (7.3, 5.6), (12.7, 5.6)],
                [(10.0, 17.8), (7.3, 14.4), (12.7, 14.4)],
                [(2.2, 10.0), (5.6, 7.3), (5.6, 12.7)],
                [(17.8, 10.0), (14.4, 7.3), (14.4, 12.7)],
            ] {
                painter.add(egui::Shape::convex_polygon(
                    head.iter().map(|(x, y)| pt(*x, *y)).collect(),
                    color,
                    egui::Stroke::NONE,
                ));
            }
        }
        Icon::Download => {
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y - r * 0.8),
                    egui::pos2(c.x, c.y + r * 0.35),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y + r * 0.35),
                    egui::pos2(c.x - r * 0.38, c.y - r * 0.05),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y + r * 0.35),
                    egui::pos2(c.x + r * 0.38, c.y - r * 0.05),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.75, c.y + r * 0.72),
                    egui::pos2(c.x + r * 0.75, c.y + r * 0.72),
                ],
                stroke,
            );
        }
        Icon::Upload => {
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y + r * 0.8),
                    egui::pos2(c.x, c.y - r * 0.35),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y - r * 0.35),
                    egui::pos2(c.x - r * 0.38, c.y + r * 0.05),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y - r * 0.35),
                    egui::pos2(c.x + r * 0.38, c.y + r * 0.05),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.75, c.y - r * 0.72),
                    egui::pos2(c.x + r * 0.75, c.y - r * 0.72),
                ],
                stroke,
            );
        }
        Icon::Sliders => {
            let top = c.y - r * 0.45;
            let bottom = c.y + r * 0.45;
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.8, top),
                    egui::pos2(c.x + r * 0.8, top),
                ],
                stroke,
            );
            painter.circle_stroke(egui::pos2(c.x + r * 0.22, top), r * 0.26, stroke);
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.8, bottom),
                    egui::pos2(c.x + r * 0.8, bottom),
                ],
                stroke,
            );
            painter.circle_stroke(egui::pos2(c.x - r * 0.22, bottom), r * 0.26, stroke);
        }
        Icon::Cube => {
            let top = pt(10.0, 3.2);
            let left = pt(3.8, 6.7);
            let right = pt(16.2, 6.7);
            let middle = pt(10.0, 10.0);
            let bottom_left = pt(3.8, 13.3);
            let bottom_right = pt(16.2, 13.3);
            let bottom = pt(10.0, 16.8);
            poly(vec![top, right, middle, left], true, stroke);
            poly(vec![left, bottom_left, bottom, middle], true, stroke);
            poly(vec![right, bottom_right, bottom, middle], false, stroke);
        }
        Icon::Projection => {
            // A projection cage: the cube outline plus a transform cross.
            let top = pt(10.0, 3.2);
            let left = pt(3.8, 6.7);
            let right = pt(16.2, 6.7);
            let middle = pt(10.0, 10.0);
            let bottom_left = pt(3.8, 13.3);
            let bottom_right = pt(16.2, 13.3);
            let bottom = pt(10.0, 16.8);
            poly(vec![top, right, middle, left], true, stroke);
            poly(vec![left, bottom_left, bottom, middle], true, stroke);
            poly(vec![right, bottom_right, bottom, middle], false, stroke);
            let cross = pt(14.6, 15.2);
            painter.line_segment(
                [cross - egui::vec2(2.2, 0.0), cross + egui::vec2(2.2, 0.0)],
                stroke,
            );
            painter.line_segment(
                [cross - egui::vec2(0.0, 2.2), cross + egui::vec2(0.0, 2.2)],
                stroke,
            );
        }
        Icon::Bake => {
            let low = egui::Rect::from_center_size(
                c + egui::vec2(0.0, r * 0.52),
                egui::vec2(size * 0.66, size * 0.24),
            );
            painter.rect_stroke(
                low,
                1.5,
                egui::Stroke::new(1.3_f32, color),
                egui::StrokeKind::Inside,
            );
            let high = egui::Rect::from_center_size(
                c + egui::vec2(0.0, -r * 0.55),
                egui::vec2(size * 0.34, size * 0.2),
            );
            painter.rect_stroke(
                high,
                1.5,
                egui::Stroke::new(1.3_f32, color),
                egui::StrokeKind::Inside,
            );
            let start = egui::pos2(c.x, high.bottom() + 1.0);
            let end = egui::pos2(c.x, low.top() - 1.0);
            painter.line_segment([start, end], stroke);
            arrow_head(painter, end, std::f32::consts::FRAC_PI_2, stroke);
        }
        Icon::Sun => {
            painter.circle_stroke(c, r * 0.38, stroke);
            for ray in 0..8 {
                let angle = ray as f32 * std::f32::consts::TAU / 8.0;
                let direction = egui::vec2(angle.cos(), angle.sin());
                painter.line_segment([c + direction * r * 0.62, c + direction * r * 0.92], stroke);
            }
        }
        Icon::Pbr => {
            // Shaded sphere: a full ring plus a lower-half shade and a
            // highlight. The old diagonal made it read as a "disabled" glyph.
            painter.circle_stroke(c, r * 0.76, stroke);
            let shade: Vec<egui::Pos2> = (0..=10)
                .map(|index| {
                    let angle = 0.15 + (std::f32::consts::PI - 0.3) * (index as f32 / 10.0);
                    egui::pos2(c.x + angle.cos() * r * 0.74, c.y + angle.sin() * r * 0.74)
                })
                .collect();
            painter.add(egui::Shape::convex_polygon(
                shade,
                color.gamma_multiply(0.30),
                egui::Stroke::NONE,
            ));
            painter.circle_filled(egui::pos2(c.x - r * 0.24, c.y - r * 0.26), r * 0.15, color);
        }
        Icon::Stretch => {
            // Double-headed arrow against a fixed centre: stretch.
            let shaft = egui::Stroke::new((1.7 * u).max(1.2), color);
            painter.line_segment([pt(10.0, 5.5), pt(10.0, 14.5)], shaft);
            painter.line_segment([pt(3.6, 10.0), pt(16.4, 10.0)], shaft);
            for head in [
                [(2.4, 10.0), (6.0, 7.6), (6.0, 12.4)],
                [(17.6, 10.0), (14.0, 7.6), (14.0, 12.4)],
            ] {
                painter.add(egui::Shape::convex_polygon(
                    head.iter().map(|(x, y)| pt(*x, *y)).collect(),
                    color,
                    egui::Stroke::NONE,
                ));
            }
        }
        Icon::PanelLeft | Icon::PanelRight => {
            let panel = rect.shrink(size * 0.18);
            painter.rect_stroke(panel, 2.0, stroke, egui::StrokeKind::Inside);
            let split_x = if matches!(icon, Icon::PanelLeft) {
                panel.left() + panel.width() * 0.36
            } else {
                panel.right() - panel.width() * 0.36
            };
            painter.line_segment(
                [
                    egui::pos2(split_x, panel.top()),
                    egui::pos2(split_x, panel.bottom()),
                ],
                stroke,
            );
        }
        Icon::PathAdd
        | Icon::PathSubtract
        | Icon::PathIntersect
        | Icon::PathXor
        | Icon::PathDivide => {
            // Two offset rounded squares: the filled regions show exactly what
            // the operation keeps, so the five buttons read like a pathfinder.
            let a = egui::Rect::from_min_max(pt(2.8, 4.4), pt(12.2, 13.2));
            let b = egui::Rect::from_min_max(pt(7.8, 6.8), pt(17.2, 15.6));
            let overlap = egui::Rect::from_min_max(pt(7.8, 6.8), pt(12.2, 13.2));
            let a_top = egui::Rect::from_min_max(pt(2.8, 4.4), pt(12.2, 6.8));
            let a_left = egui::Rect::from_min_max(pt(2.8, 6.8), pt(7.8, 13.2));
            let b_right = egui::Rect::from_min_max(pt(12.2, 6.8), pt(17.2, 15.6));
            let b_bottom = egui::Rect::from_min_max(pt(7.8, 13.2), pt(12.2, 15.6));
            let fill = |rect: egui::Rect, color: egui::Color32| {
                painter.rect_filled(rect, 0.0, color);
            };
            let outline = |rect: egui::Rect| {
                painter.rect_stroke(rect, 0.9 * u, hairline, egui::StrokeKind::Inside);
            };
            match icon {
                Icon::PathAdd => {
                    fill(a, color);
                    fill(b, color);
                }
                Icon::PathSubtract => {
                    fill(a_top, color);
                    fill(a_left, color);
                    outline(b);
                }
                Icon::PathIntersect => {
                    fill(overlap, color);
                    outline(a);
                    outline(b);
                }
                Icon::PathXor => {
                    fill(a_top, color);
                    fill(a_left, color);
                    fill(b_right, color);
                    fill(b_bottom, color);
                    outline(a);
                    outline(b);
                }
                Icon::PathDivide => {
                    fill(a_top, color);
                    fill(a_left, color);
                    fill(b_right, color);
                    fill(b_bottom, color);
                    fill(overlap, color.gamma_multiply(0.45));
                    outline(a);
                    outline(b);
                }
                _ => {}
            }
        }
        Icon::Detach => {
            // Bracket opening toward an arrow: remove the operand from the
            // compound while keeping it as a layer.
            poly(
                vec![
                    pt(10.4, 9.4),
                    pt(10.4, 15.4),
                    pt(3.4, 15.4),
                    pt(3.4, 7.4),
                    pt(8.6, 7.4),
                ],
                false,
                stroke,
            );
            painter.line_segment([pt(8.6, 11.6), pt(16.0, 4.2)], stroke);
            poly(
                vec![pt(16.0, 4.2), pt(11.4, 4.6), pt(15.6, 8.8)],
                true,
                stroke,
            );
        }
        Icon::Split | Icon::SplitStacked => {
            // Two panes inside one frame; the filled half is the 2D canvas.
            let frame = egui::Rect::from_min_max(pt(3.4, 5.2), pt(16.6, 14.8));
            painter.rect_stroke(frame, 1.4 * u, stroke, egui::StrokeKind::Inside);
            if matches!(icon, Icon::Split) {
                let right = egui::Rect::from_min_max(pt(10.3, 5.5), pt(16.3, 14.5));
                painter.rect_filled(right, 0.8 * u, color.gamma_multiply(0.30));
                painter.line_segment([pt(10.0, 5.2), pt(10.0, 14.8)], stroke);
            } else {
                let bottom = egui::Rect::from_min_max(pt(3.7, 10.3), pt(16.3, 14.5));
                painter.rect_filled(bottom, 0.8 * u, color.gamma_multiply(0.30));
                painter.line_segment([pt(3.4, 10.0), pt(16.6, 10.0)], stroke);
            }
        }
    }
}

pub(crate) fn vector_op_icon(op: VectorOp) -> Icon {
    match op {
        VectorOp::Add => Icon::PathAdd,
        VectorOp::Subtract => Icon::PathSubtract,
        VectorOp::Intersect => Icon::PathIntersect,
        VectorOp::Xor => Icon::PathXor,
        VectorOp::Divide => Icon::PathDivide,
        VectorOp::PartialCut => Icon::PathSubtract,
        VectorOp::PartialSlash => Icon::PathDivide,
        VectorOp::ClipDivide => Icon::PathDivide,
    }
}

pub(crate) fn star_points(
    center: egui::Pos2,
    outer: f32,
    inner: f32,
    points: usize,
) -> Vec<egui::Pos2> {
    let mut vertices = Vec::with_capacity(points * 2);
    for index in 0..points * 2 {
        let angle =
            -std::f32::consts::FRAC_PI_2 + index as f32 * std::f32::consts::PI / points as f32;
        let radius = if index % 2 == 0 { outer } else { inner };
        vertices.push(center + egui::vec2(angle.cos(), angle.sin()) * radius);
    }
    vertices
}

pub(crate) fn arc_points(
    center: egui::Pos2,
    radius: f32,
    start: f32,
    end: f32,
    steps: usize,
) -> Vec<egui::Pos2> {
    (0..=steps)
        .map(|index| {
            let angle = start + (end - start) * index as f32 / steps as f32;
            center + egui::vec2(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

pub(crate) fn arrow_head(
    painter: &egui::Painter,
    tip: egui::Pos2,
    direction: f32,
    stroke: egui::Stroke,
) {
    let size = 4.0;
    for offset in [2.5f32, -2.5] {
        let angle = direction + offset;
        painter.line_segment(
            [tip, tip - egui::vec2(angle.cos(), angle.sin()) * size],
            stroke,
        );
    }
}

pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    active: bool,
    enabled: bool,
    tooltip: &str,
) -> egui::Response {
    icon_button_sized(ui, icon, active, enabled, tooltip, egui::vec2(30.0, 30.0))
}

/// Destructive variant of the SVG icon button, used by the layer footer.
pub(crate) fn icon_button_svg(
    ui: &mut egui::Ui,
    source: egui::ImageSource<'static>,
    active: bool,
    enabled: bool,
    tooltip: &str,
    size: egui::Vec2,
    danger: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = enabled && response.hovered();
    if active {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            egui::CornerRadius::same(RADIUS_SM),
            ACCENT_WASH,
        );
    } else if hovered {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            egui::CornerRadius::same(RADIUS_SM),
            if danger {
                DANGER_WASH
            } else {
                hover_layer(CONTAINER_LOW)
            },
        );
    }
    let color = if !enabled {
        TEXT_DISABLED
    } else if active {
        ACCENT
    } else if danger && hovered {
        DANGER
    } else {
        MUTED
    };
    let glyph = rect.width().min(rect.height()) * 0.80;
    egui::Image::new(source).tint(color).paint_at(
        ui,
        egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(glyph)),
    );
    if enabled {
        response.on_hover_text(tooltip)
    } else {
        response.on_disabled_hover_text(tooltip)
    }
}

/// Compact icon button for dense rows. The glyph is centred in a square that
/// fits the requested slot, so callers can keep narrow fixed columns.
pub(crate) fn icon_button_sized(
    ui: &mut egui::Ui,
    icon: Icon,
    active: bool,
    enabled: bool,
    tooltip: &str,
    size: egui::Vec2,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = enabled && response.hovered();
    if active {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            egui::CornerRadius::same(RADIUS_SM),
            ACCENT_WASH,
        );
    } else if hovered {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            egui::CornerRadius::same(RADIUS_SM),
            hover_layer(CONTAINER_LOW),
        );
    }
    let color = if !enabled {
        TEXT_DISABLED
    } else if active {
        ACCENT
    } else {
        MUTED
    };
    let glyph = rect.width().min(rect.height()) * 0.72;
    draw_icon(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(glyph)),
        icon,
        color,
    );
    if enabled {
        response.on_hover_text(tooltip)
    } else {
        response.on_disabled_hover_text(tooltip)
    }
}
