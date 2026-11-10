//! A small, fully parametric demo document used by the CLI and the editor.

use glam::{UVec2, Vec2};

use crate::layers::{Document, Layer, LayerKind};
use crate::path::{Primitive, ShapeSource};
use crate::sdf::Profile;
use crate::text::{BuiltinFont, FontSource, TextAlign, TextSource};

/// Build a demo scene with three beveled shapes. All distances scale with
/// `size`, so the composition looks the same at 512 or 4096 pixels.
pub fn demo_document(size: u32) -> Document {
    let s = size as f32 / 1024.0;
    let mut doc = Document::new_with_base(UVec2::splat(size));
    doc.height_scale_px = 64.0 * s;

    // Raised rounded panel.
    let mut panel = Layer::shape(
        "panel",
        ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::splat(size as f32 * 0.5),
            size: Vec2::new(720.0 * s, 480.0 * s),
            corner_radius: 56.0 * s,
        }),
    );
    if let LayerKind::Shape(shape) = &mut panel.kind {
        shape.radius_px = 48.0 * s;
        shape.height_px = 24.0 * s;
        shape.invert = true;
        shape.profile = Profile::Round;
    }
    doc.layers.push(panel);

    // Inset circular dial.
    let mut dial = Layer::shape(
        "dial",
        ShapeSource::Primitive(Primitive::Ellipse {
            center: Vec2::splat(size as f32 * 0.5),
            radii: Vec2::splat(150.0 * s),
        }),
    );
    if let LayerKind::Shape(shape) = &mut dial.kind {
        shape.radius_px = 28.0 * s;
        shape.height_px = 16.0 * s;
        shape.invert = false;
    }
    doc.layers.push(dial);

    // Small raised emblem for a second bevel scale.
    let mut star = Layer::shape(
        "emblem",
        ShapeSource::Primitive(Primitive::Star {
            center: Vec2::splat(size as f32 * 0.5),
            inner_radius: 44.0 * s,
            outer_radius: 96.0 * s,
            points: 5,
            rotation: -std::f32::consts::FRAC_PI_2,
        }),
    );
    if let LayerKind::Shape(shape) = &mut star.kind {
        shape.radius_px = 14.0 * s;
        shape.height_px = 10.0 * s;
        shape.invert = true;
    }
    doc.layers.push(star);

    doc
}

/// Text and font showcase: a recessed panel with a semibold title, a tracked
/// mono subtitle and an outline-stroked line, all live editable text layers.
pub fn demo_text_document(size: u32) -> Document {
    let s = size as f32 / 1024.0;
    let mut doc = Document::new_with_base(UVec2::splat(size));
    doc.height_scale_px = 64.0 * s;

    let mut panel = Layer::shape(
        "Backplate",
        ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::new(512.0, 512.0) * s,
            size: Vec2::new(880.0, 720.0) * s,
            corner_radius: 64.0 * s,
        }),
    );
    if let LayerKind::Shape(shape) = &mut panel.kind {
        // Raised, not carved: direct height export composites with `max`, so a
        // recessed floor would leave a visible rectangle under the text block.
        shape.radius_px = 40.0 * s;
        shape.height_px = 8.0 * s;
        shape.invert = false;
    }
    doc.layers.push(panel);

    let text =
        |name: &str, content: &str, font: FontSource, anchor: Vec2, size_px: f32, tracking: f32| {
            let mut layer = Layer::shape(
                name,
                ShapeSource::Text(TextSource {
                    content: content.to_string(),
                    font,
                    anchor: anchor * s,
                    size_px: size_px * s,
                    tracking_px: tracking * s,
                    line_height: 1.0,
                    align: TextAlign::Left,
                }),
            );
            layer.touch();
            layer
        };

    let mut title = text(
        "N-SHADE",
        "N-SHADE",
        FontSource::Builtin(BuiltinFont::InterSemiBold),
        Vec2::new(146.0, 214.0),
        176.0,
        2.0,
    );
    if let LayerKind::Shape(shape) = &mut title.kind {
        shape.radius_px = 10.0 * s;
        shape.height_px = 22.0 * s;
        shape.edge_aa_px = 0.5 * s;
    }
    doc.layers.push(title);

    let mut subtitle = text(
        "NORMAL MAP STUDIO",
        "NORMAL MAP STUDIO",
        FontSource::Builtin(BuiltinFont::JetBrainsMono),
        Vec2::new(150.0, 470.0),
        56.0,
        10.0,
    );
    if let LayerKind::Shape(shape) = &mut subtitle.kind {
        shape.radius_px = 3.0 * s;
        shape.height_px = 10.0 * s;
        shape.edge_aa_px = 0.4 * s;
    }
    doc.layers.push(subtitle);

    let mut outline = text(
        "TEXT & FONTS",
        "TEXT & FONTS",
        FontSource::Builtin(BuiltinFont::Inter),
        Vec2::new(150.0, 650.0),
        96.0,
        4.0,
    );
    if let LayerKind::Shape(shape) = &mut outline.kind {
        shape.style = crate::layers::ShapeStyle::Stroke;
        shape.stroke_width_px = 8.0 * s;
        shape.radius_px = 3.5 * s;
        shape.height_px = 12.0 * s;
        shape.edge_aa_px = 0.4 * s;
    }
    doc.layers.push(outline);

    doc
}

/// Deterministic stress scene: a grid of parametric shapes, useful for
/// benchmarking 100+ layer stacks and the GPU AABB-culling path.
pub fn stress_document(size: u32, layers: usize) -> Document {
    let s = size as f32;
    let mut doc = Document::new(UVec2::splat(size));
    doc.height_scale_px = 64.0 * (s / 1024.0).max(0.25);
    doc.layers.push(Layer::flat("stress base"));

    let columns = (layers as f32).sqrt().ceil().max(1.0) as usize;
    let cell = s / columns as f32;
    let radius = cell * 0.30;

    for i in 0..layers {
        let cx = (i % columns) as f32 * cell + cell * 0.5;
        let cy = (i / columns) as f32 * cell + cell * 0.5;
        let center = Vec2::new(cx, cy);
        let primitive = match i % 3 {
            0 => Primitive::Rect {
                center,
                size: Vec2::splat(cell * 0.62),
                corner_radius: cell * 0.10,
            },
            1 => Primitive::Ellipse {
                center,
                radii: Vec2::splat(radius),
            },
            _ => Primitive::Star {
                center,
                inner_radius: radius * 0.45,
                outer_radius: radius,
                points: 5,
                rotation: -std::f32::consts::FRAC_PI_2,
            },
        };
        let mut layer = Layer::shape(format!("stress {i:03}"), ShapeSource::Primitive(primitive));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = (cell * 0.08).max(1.0);
            shape.height_px = (cell * 0.04).max(0.5);
            shape.invert = i % 2 == 0;
            shape.profile = Profile::Round;
        }
        doc.layers.push(layer);
    }

    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_scene_has_beveled_normals() {
        let doc = demo_document(256);
        let normals = doc.evaluate();
        assert!(normals
            .iter()
            .any(|n| (n.length() - 1.0).abs() < 1e-4 && n.z < 0.999));
        // Corners outside the panel stay flat.
        assert!((*normals.get(8, 8) - glam::Vec3::Z).length() < 1e-3);
    }

    #[test]
    fn demo_scene_scales_with_size() {
        let small = demo_document(512);
        let large = demo_document(1024);
        assert_eq!(small.size, UVec2::splat(512));
        assert_eq!(large.size, UVec2::splat(1024));
        assert!((small.height_scale_px * 2.0 - large.height_scale_px).abs() < 1e-4);
    }

    #[test]
    fn text_demo_has_live_editable_text() {
        let doc = demo_text_document(512);
        let text_layers = doc
            .layers
            .iter()
            .filter(|layer| {
                matches!(&layer.kind, LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Text(_)))
            })
            .count();
        assert_eq!(text_layers, 3);
        let normals = doc.evaluate();
        assert!(normals
            .iter()
            .any(|normal| (normal.length() - 1.0).abs() < 1e-4 && normal.z < 0.99));
    }

    #[test]
    fn stress_document_builds_requested_shapes() {
        let doc = stress_document(256, 100);
        assert_eq!(doc.layers.len(), 101);
        assert!(doc.layers[1..]
            .iter()
            .all(|layer| matches!(&layer.kind, LayerKind::Shape(_))));
        assert!(doc.layers.iter().filter(|layer| layer.visible).count() > 100);
    }
}
