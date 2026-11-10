//! Painter-drawn icons from SVG path data.
//!
//! The viewport bar and the tool rail use the 24 x 24 sprite from
//! `nshade-viewport-and-left-panel.html`. egui cannot rasterise path data, so
//! the `d` strings are parsed here and drawn as polylines: cubic Béziers are
//! flattened into 16 segments and arcs are sampled every 15° (SVG spec F.6.5).
//! Stroke weight is derived from the nominal 1.6 px sprite stroke, which keeps
//! the glyphs crisp at every DPI instead of scaling a bitmap.
//!
//! Supported commands: M L H V C S A Z, absolute and relative, with implicit
//! repeats and compact numbers ("6.1.8-4.5"). Arc flags are single characters,
//! so the compact form used by the sprite ("a1.5 1.5 0 013 0") parses too.

use super::*;
use std::f32::consts::{PI, TAU};

#[derive(Default)]
pub(crate) struct ParsedSubpath {
    pub points: Vec<egui::Vec2>,
    pub closed: bool,
}

struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Scanner<'_> {
    fn skip_separators(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b',')
        {
            self.at += 1;
        }
    }

    fn done(&mut self) -> bool {
        self.skip_separators();
        self.at >= self.bytes.len()
    }

    fn command(&mut self) -> Option<u8> {
        self.skip_separators();
        let byte = *self.bytes.get(self.at)?;
        if byte.is_ascii_alphabetic() {
            self.at += 1;
            Some(byte)
        } else {
            None
        }
    }

    fn number(&mut self) -> f32 {
        self.skip_separators();
        let start = self.at;
        if matches!(self.bytes.get(self.at), Some(b'+') | Some(b'-')) {
            self.at += 1;
        }
        let mut seen_dot = false;
        while let Some(&byte) = self.bytes.get(self.at) {
            if byte.is_ascii_digit() {
                self.at += 1;
            } else if byte == b'.' && !seen_dot {
                seen_dot = true;
                self.at += 1;
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(0.0)
    }

    /// Arc flags are a single `0`/`1`, so they must be read one byte at a time
    /// rather than through [`Self::number`].
    fn flag(&mut self) -> bool {
        self.skip_separators();
        let flag = self.bytes.get(self.at).is_some_and(|byte| *byte == b'1');
        self.at += 1;
        flag
    }
}

fn flatten_cubic(
    p0: egui::Vec2,
    p1: egui::Vec2,
    p2: egui::Vec2,
    p3: egui::Vec2,
    out: &mut Vec<egui::Vec2>,
) {
    for step in 1..=16 {
        let t = step as f32 / 16.0;
        let u = 1.0 - t;
        out.push(
            p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t),
        );
    }
}

/// Endpoint-to-centre conversion from SVG spec F.6.5, sampled every ≤15°.
#[allow(clippy::too_many_arguments)]
fn flatten_arc(
    from: egui::Vec2,
    mut rx: f32,
    mut ry: f32,
    rotation_degrees: f32,
    large: bool,
    sweep: bool,
    to: egui::Vec2,
    out: &mut Vec<egui::Vec2>,
) {
    if rx == 0.0 || ry == 0.0 {
        out.push(to);
        return;
    }
    rx = rx.abs();
    ry = ry.abs();
    let rotation = rotation_degrees.to_radians();
    let (cos, sin) = (rotation.cos(), rotation.sin());
    let delta = (from - to) / 2.0;
    let x1 = cos * delta.x + sin * delta.y;
    let y1 = -sin * delta.x + cos * delta.y;
    let scale = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    if scale > 1.0 {
        let factor = scale.sqrt();
        rx *= factor;
        ry *= factor;
    }
    let numerator = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let denominator = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut centre_factor = (numerator / denominator).max(0.0).sqrt();
    if large == sweep {
        centre_factor = -centre_factor;
    }
    let (cx1, cy1) = (centre_factor * rx * y1 / ry, -centre_factor * ry * x1 / rx);
    let centre = egui::vec2(cos * cx1 - sin * cy1, sin * cx1 + cos * cy1) + (from + to) / 2.0;
    let angle = |u: egui::Vec2, v: egui::Vec2| (u.x * v.y - u.y * v.x).atan2(u.x * v.x + u.y * v.y);
    let u = egui::vec2((x1 - cx1) / rx, (y1 - cy1) / ry);
    let v = egui::vec2((-x1 - cx1) / rx, (-y1 - cy1) / ry);
    let start = angle(egui::vec2(1.0, 0.0), u);
    let mut sweep_angle = angle(u, v);
    if !sweep && sweep_angle > 0.0 {
        sweep_angle -= TAU;
    } else if sweep && sweep_angle < 0.0 {
        sweep_angle += TAU;
    }
    let steps = ((sweep_angle.abs() / (PI / 12.0)).ceil() as usize).max(2);
    for step in 1..=steps {
        let theta = start + sweep_angle * step as f32 / steps as f32;
        let (x, y) = (rx * theta.cos(), ry * theta.sin());
        out.push(centre + egui::vec2(cos * x - sin * y, sin * x + cos * y));
    }
}

pub(crate) fn parse(d: &str) -> Vec<ParsedSubpath> {
    let mut scanner = Scanner {
        bytes: d.as_bytes(),
        at: 0,
    };
    let mut subpaths = Vec::<ParsedSubpath>::new();
    let mut current = ParsedSubpath::default();
    let mut position = egui::Vec2::ZERO;
    let mut start = egui::Vec2::ZERO;
    let mut last_control: Option<egui::Vec2> = None;
    let mut command = b'M';

    while !scanner.done() {
        if let Some(next) = scanner.command() {
            command = next;
        }
        let relative = command.is_ascii_lowercase();
        let origin = if relative { position } else { egui::Vec2::ZERO };
        let mut next_control = None;

        match command.to_ascii_uppercase() {
            b'M' => {
                let point = egui::vec2(scanner.number(), scanner.number()) + origin;
                if !current.points.is_empty() {
                    subpaths.push(std::mem::take(&mut current));
                }
                current.points.push(point);
                position = point;
                start = point;
                // A repeated moveto pair is an implicit lineto.
                command = if relative { b'l' } else { b'L' };
            }
            b'L' => {
                let point = egui::vec2(scanner.number(), scanner.number()) + origin;
                current.points.push(point);
                position = point;
            }
            b'H' => {
                let x = scanner.number();
                position.x = if relative { position.x + x } else { x };
                current.points.push(position);
            }
            b'V' => {
                let y = scanner.number();
                position.y = if relative { position.y + y } else { y };
                current.points.push(position);
            }
            b'C' => {
                let first = egui::vec2(scanner.number(), scanner.number()) + origin;
                let second = egui::vec2(scanner.number(), scanner.number()) + origin;
                let end = egui::vec2(scanner.number(), scanner.number()) + origin;
                flatten_cubic(position, first, second, end, &mut current.points);
                next_control = Some(second);
                position = end;
            }
            b'S' => {
                let first = match last_control {
                    Some(control) => position * 2.0 - control,
                    None => position,
                };
                let second = egui::vec2(scanner.number(), scanner.number()) + origin;
                let end = egui::vec2(scanner.number(), scanner.number()) + origin;
                flatten_cubic(position, first, second, end, &mut current.points);
                next_control = Some(second);
                position = end;
            }
            b'A' => {
                let (rx, ry, rotation) = (scanner.number(), scanner.number(), scanner.number());
                let (large, sweep) = (scanner.flag(), scanner.flag());
                let end = egui::vec2(scanner.number(), scanner.number()) + origin;
                flatten_arc(
                    position,
                    rx,
                    ry,
                    rotation,
                    large,
                    sweep,
                    end,
                    &mut current.points,
                );
                position = end;
            }
            b'Z' => {
                current.closed = true;
                subpaths.push(std::mem::take(&mut current));
                position = start;
            }
            _ => break,
        }
        last_control = next_control;
    }

    if current.points.len() > 1 {
        subpaths.push(current);
    }
    subpaths.retain(|subpath| subpath.points.len() > 1);
    subpaths
}

/// Paint path `d` (24 x 24 view box) into a `size` square centred on `center`.
///
/// The sprite's stroke is 1.6 on a 24 grid. egui has no round caps, so open
/// paths get a dot at each end; closed paths use [`egui::Shape::closed_line`]
/// so corners join instead of showing seam gaps.
pub(crate) fn paint(
    painter: &egui::Painter,
    d: &str,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let scale = size / 24.0;
    let origin = center - egui::Vec2::splat(size / 2.0);
    let stroke_width = 1.6 * scale;
    let stroke = egui::Stroke::new(stroke_width, color);
    for subpath in parse(d) {
        let mut points: Vec<egui::Pos2> = subpath
            .points
            .iter()
            .map(|point| origin + *point * scale)
            .collect();
        if subpath.closed {
            if points.len() > 2 && points[0].distance(*points.last().unwrap()) < 1e-3 {
                points.pop();
            }
            painter.add(egui::Shape::closed_line(points, stroke));
        } else {
            let ends = [points[0], *points.last().unwrap()];
            painter.add(egui::Shape::line(points, stroke));
            for end in ends {
                painter.circle_filled(end, stroke_width / 2.0, color);
            }
        }
    }
}

/// Path data for the icon sprite in the design reference.
///
/// Ids match the `<symbol>` ids in `nshade-viewport-and-left-panel.html`; the
/// last five come from the editor's own icon pack so the pen flyout can share
/// the painter pipeline. Returns `None` for unknown names so callers can fall
/// back to a different glyph.
pub(crate) fn icon_path(name: &str) -> Option<&'static str> {
    Some(match name {
        "select" => "M5 3l14 8-6 2-2 6z",
        "rect" => {
            "M6 6h12a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-12a2 2 0 0 1 -2 -2v-8a2 2 0 0 1 2 -2z"
        }
        "ellipse" => "M4 12a8 8 0 1 1 16 0a8 8 0 1 1 -16 0z",
        "star" => {
            "M12 3l2.7 5.6 6.1.8-4.5 4.2 1.1 6-5.4-3-5.4 3 1.1-6L3.2 9.4l6.1-.8z"
        }
        "pen" => "m12 3 7 7-7.1 9.2L5 10ZM12 3 5 10M12 19.2V22M10.5 11.1a1.5 1.5 0 1 1 3 0a1.5 1.5 0 1 1 -3 0z",
        "lasso" => "M8 20c2 1 5-1 4-4s4-3 6-6c1-4-5-6-9-4S3 12 6 15",
        "paint" => "M4 11l8-7 8 7-2 2v7H6v-7z",
        "ruler" => "M5 19L19 5M8 12l2 2M12 8l2 2M5 19l3-1-2-2z",
        "pan" => "M9 11V5a1.5 1.5 0 0 1 3 0v5m0-1V4a1.5 1.5 0 0 1 3 0v6m0-3a1.5 1.5 0 0 1 3 0v7a6 6 0 0 1 -6 6h-1a6 6 0 0 1 -5 -3l-3 -5a1.5 1.5 0 0 1 2.5 -1.5L9 14",
        "cube" => "M12 3l8 4.5v9L12 21l-8-4.5v-9zM12 12l8-4.5M12 12v9M12 12L4 7.5",
        "grid2d" => "M7 4h10a3 3 0 0 1 3 3v10a3 3 0 0 1 -3 3h-10a3 3 0 0 1 -3 -3v-10a3 3 0 0 1 3 -3zM8 12h8",
        "crop" => "M7 3v14h14M3 7h14v14",
        "cam" => "M4 8h3l2-3h6l2 3h3v11H4zM8.5 13a3.5 3.5 0 1 1 7 0a3.5 3.5 0 1 1 -7 0z",
        "ops" => "M5.5 4h4a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1 -1.5 1.5h-4a1.5 1.5 0 0 1 -1.5 -1.5v-4a1.5 1.5 0 0 1 1.5 -1.5zM14.5 4h4a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1 -1.5 1.5h-4a1.5 1.5 0 0 1 -1.5 -1.5v-4a1.5 1.5 0 0 1 1.5 -1.5zM5.5 13h4a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1 -1.5 1.5h-4a1.5 1.5 0 0 1 -1.5 -1.5v-4a1.5 1.5 0 0 1 1.5 -1.5z",
        "fit" => "M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5",
        "zoom" | "zoom_out" => "M5 11a6 6 0 1 1 12 0a6 6 0 1 1 -12 0zM20 20l-4.5-4.5M8.5 11h5",
        "zoom_in" => "M5 11a6 6 0 1 1 12 0a6 6 0 1 1 -12 0zM20 20l-4.5-4.5M8.5 11h5M11 8.5v5",
        "down" => "M6 9l6 6 6-6",
        // App-only glyphs, drawn in the sprite's 24-grid and stroke weight.
        "shapes" => "M6 8h8a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-8a2 2 0 0 1 -2 -2v-8a2 2 0 0 1 2 -2zM10.5 8.5a5 5 0 1 1 10 0a5 5 0 1 1 -10 0z",
        "cable" => "M4 17c2 1 4-2 5-5s4-6 7-4 1 6 4 7M2.8 17a1.3 1.3 0 1 1 2.6 0a1.3 1.3 0 1 1 -2.6 0zM17 15a1.3 1.3 0 1 1 2.6 0a1.3 1.3 0 1 1 -2.6 0z",
        "text" => "M5 5.5h14M12 5.5V18M8.6 18h6.8",
        "node" => "M4 12h16M2.6 12a1.9 1.9 0 1 1 3.8 0a1.9 1.9 0 1 1 -3.8 0zM17.6 12a1.9 1.9 0 1 1 3.8 0a1.9 1.9 0 1 1 -3.8 0zM10.6 9.6h2.8a1 1 0 0 1 1 1v2.8a1 1 0 0 1 -1 1h-2.8a1 1 0 0 1 -1 -1v-2.8a1 1 0 0 1 1 -1z",
        "add_anchor" => "M3.5 9.5h13M2.4 9.5a1.8 1.8 0 1 1 3.6 0a1.8 1.8 0 1 1 -3.6 0zM14 9.5a1.8 1.8 0 1 1 3.6 0a1.8 1.8 0 1 1 -3.6 0zM9.3 7.1h2.4a1 1 0 0 1 1 1v2.4a1 1 0 0 1 -1 1h-2.4a1 1 0 0 1 -1 -1v-2.4a1 1 0 0 1 1 -1zM18.6 13.8v6M15.6 16.8h6",
        "delete_anchor" => "M3.5 9.5h13M2.4 9.5a1.8 1.8 0 1 1 3.6 0a1.8 1.8 0 1 1 -3.6 0zM14 9.5a1.8 1.8 0 1 1 3.6 0a1.8 1.8 0 1 1 -3.6 0zM9.3 7.1h2.4a1 1 0 0 1 1 1v2.4a1 1 0 0 1 -1 1h-2.4a1 1 0 0 1 -1 -1v-2.4a1 1 0 0 1 1 -1zM15.6 16.8h6",
        "convert_point" => "M3.5 17.5 9.5 7l6 10.5M8.4 4.9h2.2a1 1 0 0 1 1 1v2.2a1 1 0 0 1 -1 1h-2.2a1 1 0 0 1 -1 -1v-2.2a1 1 0 0 1 1 -1zM16.5 12.5c2.2.5 3.6 2.4 3.3 4.5M18.3 17.7l1.6-.8.3 1.8",
        _ => return None,
    })
}

/// Paint a named sprite icon; returns `false` when the name is unknown.
pub(crate) fn paint_icon(
    painter: &egui::Painter,
    name: &str,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) -> bool {
    match icon_path(name) {
        Some(d) => {
            paint(painter, d, center, size, color);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_and_closes_subpaths() {
        let subpaths = parse("M5 3l14 8-6 2-2 6z");
        assert_eq!(subpaths.len(), 1);
        assert!(subpaths[0].closed);
        assert_eq!(subpaths[0].points.first(), Some(&egui::vec2(5.0, 3.0)));
        assert_eq!(subpaths[0].points.last(), Some(&egui::vec2(11.0, 19.0)));
    }

    #[test]
    fn relative_repeats_and_implicit_lineto_are_supported() {
        // The first moveto pair moves; the second pair is an implicit lineto.
        let subpaths = parse("m1 2 3 4 5 6");
        assert_eq!(subpaths.len(), 1);
        assert_eq!(
            subpaths[0].points,
            vec![
                egui::vec2(1.0, 2.0),
                egui::vec2(4.0, 6.0),
                egui::vec2(9.0, 12.0)
            ]
        );
    }

    #[test]
    fn arcs_are_flattened_into_the_current_subpath() {
        let subpaths = parse("M4 12a8 8 0 1 1 16 0a8 8 0 1 1 -16 0z");
        assert_eq!(subpaths.len(), 1);
        assert!(subpaths[0].closed);
        // Two 180° arcs sampled every ≤15° are at least 13 points each.
        assert!(subpaths[0].points.len() > 24);
        // An arc starts and ends on the requested endpoints, not at the origin.
        assert_eq!(subpaths[0].points.first(), Some(&egui::vec2(4.0, 12.0)));
        assert!((*subpaths[0].points.last().unwrap() - egui::vec2(4.0, 12.0)).length() < 1e-3);
    }

    #[test]
    fn compact_arc_flags_parse_like_the_sprite() {
        // "a1.5 1.5 0 013 0" is rotation 0, large=0, sweep=1, dx=3, dy=0.
        let subpaths = parse("M9 11V5a1.5 1.5 0 013 0");
        assert_eq!(subpaths.len(), 1);
        assert!((*subpaths[0].points.last().unwrap() - egui::vec2(12.0, 5.0)).length() < 1e-3);
    }

    #[test]
    fn every_sprite_id_in_the_reference_is_available() {
        for name in [
            "select", "rect", "ellipse", "star", "pen", "lasso", "paint", "ruler", "pan", "cube",
            "grid2d", "crop", "cam", "ops", "fit", "zoom",
        ] {
            assert!(icon_path(name).is_some(), "missing sprite icon {name}");
        }
        assert!(icon_path("does-not-exist").is_none());
    }

    #[test]
    fn sprite_paths_parse_into_drawable_subpaths() {
        for name in [
            "select", "rect", "ellipse", "star", "pen", "lasso", "paint", "ruler", "pan", "cube",
            "grid2d", "crop", "cam", "ops", "fit", "zoom", "shapes",
        ] {
            let subpaths = parse(icon_path(name).unwrap());
            assert!(!subpaths.is_empty(), "icon {name} parsed to nothing");
            assert!(subpaths.iter().all(|subpath| subpath.points.len() > 1));
        }
    }
}
