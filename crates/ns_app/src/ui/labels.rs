//! Display labels for blend modes, layer kinds, profiles, and sources.

use super::*;
/// Blend modes offered in the UI.
///
/// The legacy surface boolean modes (Union/Intersect/Difference) are still
/// understood so older documents keep their setting, but booleans are now
/// vector operations on a boolean layer group, so they are not selectable.
pub(crate) const BLEND_MODES: [BlendMode; 5] = [
    BlendMode::Derivative,
    BlendMode::Rnm,
    BlendMode::Whiteout,
    BlendMode::Udn,
    BlendMode::Replace,
];

pub(crate) fn blend_label(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Derivative => "Derivative",
        BlendMode::Rnm => "RNM",
        BlendMode::Whiteout => "Whiteout",
        BlendMode::Udn => "UDN",
        BlendMode::Replace => "Replace",
        BlendMode::BooleanUnion => "Boolean Union",
        BlendMode::BooleanIntersect => "Boolean Intersect",
        BlendMode::BooleanDifference => "Boolean Difference",
        BlendMode::PartialCut => "Partial cut",
        BlendMode::PartialSlash => "Partial slash",
        BlendMode::ClipDivide => "Clip divide",
    }
}

/// One-line explanation used by the blend dropdown and the row tag tooltip.
pub(crate) fn blend_hint(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Derivative => {
            "Partial-derivative add: sums the height slopes. Exact for height-derived \
             layers, but can subdue detail over a steep base."
        }
        BlendMode::Rnm => {
            "Reoriented normal mapping: rotates the layer into the base's tangent frame. \
             Keeps the most detail; the recommended detail blend."
        }
        BlendMode::Whiteout => {
            "Whiteout: adds XY and multiplies Z. Keeps a flat input neutral and shows \
             more detail than the derivative add over a steep base."
        }
        BlendMode::Udn => {
            "UDN: adds XY and keeps the base Z. The cheapest detail blend; detail is \
             slightly softer over flat bases."
        }
        BlendMode::Replace => {
            "Replace the base. Opacity fades between the two surfaces in slope space, \
             so the mid-point keeps its height instead of flattening."
        }
        BlendMode::BooleanUnion => "Legacy surface union; new work uses a boolean group.",
        BlendMode::BooleanIntersect => "Legacy surface intersect; new work uses a boolean group.",
        BlendMode::BooleanDifference => "Legacy surface difference; new work uses a boolean group.",
        BlendMode::PartialCut => "Cut the layer's profile into the surface below.",
        BlendMode::PartialSlash => "Cut a tapered groove into the surface below.",
        BlendMode::ClipDivide => "Split the surface below along the layer's band.",
    }
}

/// Compact labels for the per-row blend dropdown. The full name is still used
/// in Properties and in the dropdown popup/tooltip.
pub(crate) fn blend_short_label(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Derivative => "Derivative",
        BlendMode::Rnm => "RNM",
        BlendMode::Whiteout => "Whiteout",
        BlendMode::Udn => "UDN",
        BlendMode::Replace => "Replace",
        BlendMode::BooleanUnion => "Union",
        BlendMode::BooleanIntersect => "Intersect",
        BlendMode::BooleanDifference => "Difference",
        BlendMode::PartialCut => "Cut",
        BlendMode::PartialSlash => "Slash",
        BlendMode::ClipDivide => "Divide",
    }
}

/// Operator glyph shown before the short blend label, as in the reference's
/// `= Derivative` tag.
pub(crate) fn blend_operator(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Derivative => "=",
        BlendMode::Rnm => "~",
        BlendMode::Whiteout => "+",
        BlendMode::Udn => "^",
        BlendMode::Replace => ">",
        BlendMode::BooleanUnion => "+",
        BlendMode::BooleanIntersect => "&",
        BlendMode::BooleanDifference => "-",
        BlendMode::PartialCut => "▽",
        BlendMode::PartialSlash => "/",
        BlendMode::ClipDivide => "//",
    }
}

/// Operator glyph for a live vector boolean group, in the same tag slot as the
/// blend operator. ASCII only: the bundled mono face is guaranteed to have it.
pub(crate) fn vector_op_operator(op: VectorOp) -> &'static str {
    match op {
        VectorOp::Add => "+",
        VectorOp::Subtract => "-",
        VectorOp::Intersect => "&",
        VectorOp::Xor => "^",
        VectorOp::Divide => "/",
        VectorOp::PartialCut => "v",
        VectorOp::PartialSlash => "/",
        VectorOp::ClipDivide => "//",
    }
}

/// Sentence-case type label shown under the layer name. All-caps metadata was
/// the least legible text in the row; the size/brightness carries the hierarchy
/// instead of shouting.
pub(crate) fn kind_label(kind: &LayerKind) -> &'static str {
    match kind {
        LayerKind::Flat => "Flat",
        LayerKind::ImportedNormal { .. } => "Normal",
        LayerKind::NormalPaint { .. } => "Normal paint",
        LayerKind::PhotoHeight(photo) if photo.alpha => "Alpha",
        LayerKind::PhotoHeight(_) => "Photo",
        LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Text(_)) => "Text",
        LayerKind::Shape(shape)
            if matches!(
                shape.source,
                ShapeSource::Raster(_) | ShapeSource::RasterRegion(_)
            ) =>
        {
            "Raster"
        }
        LayerKind::Shape(_) => "Shape",
        LayerKind::HeightPaint { .. } => "Paint",
        LayerKind::Fill { .. } => "Fill",
        LayerKind::Group { .. } => "Group",
    }
}

pub(crate) fn profile_label(profile: &Profile) -> String {
    match profile {
        Profile::Round => "Round".into(),
        Profile::Chamfer { degrees } => format!("Chamfer {degrees:.0}°"),
        Profile::Linear => "Linear".into(),
        Profile::Convex => "Convex".into(),
        Profile::Concave => "Concave".into(),
        Profile::Smooth => "S-Curve".into(),
        Profile::Custom(_) => "Custom".into(),
    }
}

/// Built-in bevel choices shared by single-shape and group Properties menus.
pub(crate) fn profile_choices() -> Vec<(Profile, &'static str)> {
    vec![
        (Profile::Round, "Round"),
        (Profile::Chamfer { degrees: 45.0 }, "Chamfer 45°"),
        (Profile::Chamfer { degrees: 30.0 }, "Chamfer 30°"),
        (Profile::Linear, "Linear"),
        (Profile::Convex, "Convex"),
        (Profile::Concave, "Concave"),
        (Profile::Smooth, "S-Curve"),
    ]
}
