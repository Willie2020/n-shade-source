use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    /// Add slopes in height space. Exact for height-derived normals.
    Derivative,
    /// Reoriented Normal Mapping. Best for detail normals over a base.
    Rnm,
    /// Replace the base, with opacity fading between the two surfaces in slope
    /// space (the normal-map counterpart of a height crossfade).
    Replace,
    /// Keep the taller surface where either layer covers. Coverage-aware; the
    /// document compositor uses the height fields, not [`blend`] alone.
    BooleanUnion,
    /// Keep the shorter surface only where the base and layer both cover.
    BooleanIntersect,
    /// Draw this layer's surface over the base. Its full bevel cuts into the
    /// lower shape while the layer itself stays visible.
    BooleanDifference,
    /// Partial cut: this layer's height profile is subtracted from the surface
    /// below, leaving a floor. The layer's Depth, Radius and Profile are the
    /// pocket's depth, wall width and wall shape.
    PartialCut,
    /// Partial slash: a partial cut with a taper at either end of the layer's
    /// long axis. The layer's outline is the groove.
    PartialSlash,
    /// Clip divide: the layer's band is cut through the surface below and the
    /// band's bevel becomes the two seam walls. The far side can be stepped.
    ClipDivide,
    /// Whiteout: add the XY components and multiply Z.
    ///
    /// `normalize(vec3(a.xy + b.xy, a.z * b.z))`. A detail blend that keeps a
    /// flat input neutral and shows more detail than partial derivatives over a
    /// steep base. Cheap and widely used for wrinkles and surface grain.
    Whiteout,
    /// UDN: add the XY components and keep the base Z.
    ///
    /// `normalize(vec3(a.xy + b.xy, a.z))`. The cheapest detail blend; the
    /// layer's Z is ignored, so detail is slightly subdued over flat bases.
    Udn,
}

/// A boolean operation applied to a referenced layer operand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BooleanOp {
    #[default]
    Union,
    Intersect,
    Difference,
}

impl BooleanOp {
    pub const ALL: [Self; 3] = [Self::Union, Self::Intersect, Self::Difference];

    pub fn label(self) -> &'static str {
        match self {
            Self::Union => "Union",
            Self::Intersect => "Intersect",
            Self::Difference => "Difference",
        }
    }

    pub fn blend_mode(self) -> BlendMode {
        match self {
            Self::Union => BlendMode::BooleanUnion,
            Self::Intersect => BlendMode::BooleanIntersect,
            Self::Difference => BlendMode::BooleanDifference,
        }
    }
}

impl BlendMode {
    pub fn is_boolean(self) -> bool {
        matches!(
            self,
            Self::BooleanUnion | Self::BooleanIntersect | Self::BooleanDifference
        )
    }

    /// Cut modes modify the accumulated surface instead of adding to it.
    pub fn is_cut(self) -> bool {
        matches!(
            self,
            Self::PartialCut | Self::PartialSlash | Self::ClipDivide
        )
    }

    /// Modes that need the height fields below the layer to composite.
    pub fn uses_surface(self) -> bool {
        self.is_boolean() || self.is_cut()
    }
}

/// Tangent-space slope `(dh/dx, dh/dy)` for a normal.
#[inline]
pub fn slope(n: Vec3) -> Vec2 {
    n.truncate() / n.z.max(1e-4)
}

/// Scale a normal's slope by opacity without changing its direction.
#[inline]
pub fn with_slope_opacity(n: Vec3, opacity: f32) -> Vec3 {
    let s = slope(n) * opacity.clamp(0.0, 1.0);
    Vec3::new(s.x, s.y, 1.0).normalize()
}

/// Exact additive height-domain blend.
#[inline]
pub fn derivative_blend(a: Vec3, b: Vec3) -> Vec3 {
    let s = slope(a) + slope(b);
    Vec3::new(s.x, s.y, 1.0).normalize()
}

/// Subtract one normal's slopes from another, used by boolean difference.
#[inline]
pub fn subtract_normals(base: Vec3, layer: Vec3, opacity: f32) -> Vec3 {
    let o = opacity.clamp(0.0, 1.0);
    let s = slope(base) - slope(layer) * o;
    Vec3::new(s.x, s.y, 1.0).normalize()
}

/// Whiteout blend: `normalize(vec3(a.xy + b.xy, a.z * b.z))`.
///
/// Detail is added in XY while Z is multiplied, so a flat input leaves the
/// other normal unchanged. This is the AMD "Ruby: Whiteout" blend from
/// Oat's SIGGRAPH 2007 course and a common alternative to RNM.
#[inline]
pub fn whiteout(a: Vec3, b: Vec3) -> Vec3 {
    Vec3::new(a.x + b.x, a.y + b.y, a.z * b.z).normalize()
}

/// UDN blend: `normalize(vec3(a.xy + b.xy, a.z))`.
///
/// The Unreal Developer Network detail blend. It drops the layer's Z, which
/// makes it the cheapest option but slightly reduces detail on flat bases.
#[inline]
pub fn udn(a: Vec3, b: Vec3) -> Vec3 {
    Vec3::new(a.x + b.x, a.y + b.y, a.z).normalize()
}

/// Replace the base, fading between the two surfaces in slope space.
///
/// Interpolating the normals themselves shortens them and flattens both
/// surfaces around the halfway point. Slopes describe height, so lerping them
/// is the fade that keeps the surface shape: `o = 0` is the base, `o = 1` is
/// the layer.
#[inline]
pub fn replace_blend(base: Vec3, layer: Vec3, opacity: f32) -> Vec3 {
    let o = opacity.clamp(0.0, 1.0);
    let s = slope(base).lerp(slope(layer), o);
    Vec3::new(s.x, s.y, 1.0).normalize()
}

/// Reoriented Normal Mapping (RNM).
///
/// `rnm(base, flat) == base` and `rnm(flat, detail) == detail`.
#[inline]
pub fn rnm(base: Vec3, detail: Vec3) -> Vec3 {
    let t = base + Vec3::Z;
    let u = detail * Vec3::new(-1.0, -1.0, 1.0);
    let denom = t.z.max(1e-4);
    (t * t.dot(u) / denom - u).normalize()
}

/// Blend one layer onto the accumulator.
pub fn blend(base: Vec3, layer: Vec3, mode: BlendMode, opacity: f32) -> Vec3 {
    let o = opacity.clamp(0.0, 1.0);
    if o <= 0.0 {
        return base;
    }
    match mode {
        BlendMode::Derivative | BlendMode::BooleanUnion | BlendMode::BooleanIntersect => {
            derivative_blend(base, with_slope_opacity(layer, o))
        }
        BlendMode::Rnm => rnm(base, with_slope_opacity(layer, o)),
        BlendMode::Replace => replace_blend(base, layer, o),
        BlendMode::Whiteout => whiteout(base, with_slope_opacity(layer, o)),
        BlendMode::Udn => udn(base, with_slope_opacity(layer, o)),
        BlendMode::BooleanDifference => subtract_normals(base, layer, o),
        BlendMode::PartialCut | BlendMode::PartialSlash => subtract_normals(base, layer, o),
        // The clip divide attenuates the base surface; the document compositor
        // owns that calculation because it needs the height fields.
        BlendMode::ClipDivide => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat() -> Vec3 {
        Vec3::Z
    }

    fn sample_normal() -> Vec3 {
        Vec3::new(0.2, -0.35, 0.915).normalize()
    }

    #[test]
    fn rnm_identity_with_flat_detail() {
        let base = sample_normal();
        assert!((rnm(base, flat()) - base).length() < 1e-5);
    }

    #[test]
    fn rnm_identity_with_flat_base() {
        let detail = sample_normal();
        assert!((rnm(flat(), detail) - detail).length() < 1e-5);
    }

    #[test]
    fn derivative_flat_is_flat() {
        assert!((derivative_blend(flat(), flat()) - flat()).length() < 1e-5);
    }

    #[test]
    fn derivative_adds_slopes() {
        let a = Vec3::new(0.1, 0.0, 0.994_987_4).normalize();
        let b = Vec3::new(0.2, 0.0, 0.9797959).normalize();
        let c = derivative_blend(a, b);
        let expected = slope(a) + slope(b);
        assert!((slope(c) - expected).length() < 1e-5);
    }

    #[test]
    fn opacity_scales_slope() {
        let n = sample_normal();
        let half = with_slope_opacity(n, 0.5);
        assert!((slope(half) - slope(n) * 0.5).length() < 1e-5);
    }

    #[test]
    fn all_blends_return_unit_vectors() {
        let a = sample_normal();
        let b = Vec3::new(-0.3, 0.2, 0.932).normalize();
        for mode in [
            BlendMode::Derivative,
            BlendMode::Rnm,
            BlendMode::Replace,
            BlendMode::Whiteout,
            BlendMode::Udn,
            BlendMode::BooleanUnion,
            BlendMode::BooleanIntersect,
            BlendMode::BooleanDifference,
            BlendMode::PartialCut,
            BlendMode::PartialSlash,
            BlendMode::ClipDivide,
        ] {
            let r = blend(a, b, mode, 0.7);
            assert!((r.length() - 1.0).abs() < 1e-4, "{mode:?} -> {r:?}");
        }
    }

    #[test]
    fn whiteout_keeps_a_flat_input_neutral() {
        let detail = sample_normal();
        assert!((whiteout(detail, flat()) - detail).length() < 1e-5);
        assert!((whiteout(flat(), detail) - detail).length() < 1e-5);
    }

    #[test]
    fn whiteout_matches_the_reference_formula() {
        let a = sample_normal();
        let b = Vec3::new(-0.25, 0.3, 0.921).normalize();
        let expected = Vec3::new(a.x + b.x, a.y + b.y, a.z * b.z).normalize();
        assert!((whiteout(a, b) - expected).length() < 1e-5);
        // A flat base contributes no XY, so the layer shows through unchanged.
        assert!((whiteout(flat(), b) - b).length() < 1e-5);
    }

    #[test]
    fn udn_adds_xy_and_keeps_the_base_z() {
        let base = sample_normal();
        let detail = Vec3::new(0.4, -0.1, 0.910).normalize();
        let expected = Vec3::new(base.x + detail.x, base.y + detail.y, base.z).normalize();
        assert!((udn(base, detail) - expected).length() < 1e-5);
        assert!((udn(base, flat()) - base).length() < 1e-5);
    }

    #[test]
    fn replace_fades_between_surfaces_without_flattening_them() {
        let base = Vec3::new(0.2, 0.0, 0.979_795_9).normalize();
        let layer = Vec3::new(-0.4, 0.25, 0.882).normalize();
        assert!((blend(base, layer, BlendMode::Replace, 0.0) - base).length() < 1e-5);
        assert!((blend(base, layer, BlendMode::Replace, 1.0) - layer).length() < 1e-5);

        let mid = blend(base, layer, BlendMode::Replace, 0.5);
        let expected = Vec3::new(
            (slope(base).x + slope(layer).x) * 0.5,
            (slope(base).y + slope(layer).y) * 0.5,
            1.0,
        )
        .normalize();
        assert!((mid - expected).length() < 1e-5);
        // A plain vector lerp would flatten the result; the slope fade keeps
        // the full height of the two surfaces.
        let vector_lerp = (base + layer).normalize();
        assert!((mid - vector_lerp).length() > 1e-3);
    }

    #[test]
    fn boolean_difference_subtracts_slope() {
        let a = Vec3::new(0.2, 0.0, 0.9797959).normalize();
        let b = Vec3::new(0.1, 0.0, 0.994_987_4).normalize();
        let r = subtract_normals(a, b, 1.0);
        assert!((slope(r) - (slope(a) - slope(b))).length() < 1e-5);
    }
}
