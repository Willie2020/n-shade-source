use glam::{Mat2, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::grid::{Grid, Wrap};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum BrushMode {
    Raise,
    Lower,
    Smooth,
    Flatten(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRect {
    /// Inclusive minimum.
    pub x0: u32,
    pub y0: u32,
    /// Exclusive maximum.
    pub x1: u32,
    pub y1: u32,
}

impl DirtyRect {
    pub fn width(self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }
}

/// Stamp one round brush dab into a height field.
pub fn stamp(
    height: &mut Grid<f32>,
    centre: Vec2,
    radius: f32,
    hardness: f32,
    flow: f32,
    mode: BrushMode,
) -> Option<DirtyRect> {
    stamp_tip(
        height,
        None,
        &DabTransform::round(centre, radius),
        hardness,
        flow,
        mode,
    )
}

/// Mirror and radial multiplication applied to every brush dab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Symmetry {
    #[default]
    Off,
    MirrorX,
    MirrorY,
    MirrorXY,
    Radial,
}

impl Symmetry {
    pub const ALL: [Self; 5] = [
        Self::Off,
        Self::MirrorX,
        Self::MirrorY,
        Self::MirrorXY,
        Self::Radial,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::MirrorX => "Mirror X",
            Self::MirrorY => "Mirror Y",
            Self::MirrorXY => "Mirror XY",
            Self::Radial => "Radial",
        }
    }

    pub fn is_radial(self) -> bool {
        matches!(self, Self::Radial)
    }
}

/// One transformed dab placed by symmetry. `matrix` includes the brush angle
/// and any mirror, so local stamp space maps straight into document space.
#[derive(Clone, Copy, Debug)]
pub struct StampPlacement {
    pub center: Vec2,
    pub matrix: Mat2,
}

/// A dab to stamp: centre, radius and the linear part of the stamp transform.
#[derive(Clone, Copy, Debug)]
pub struct DabTransform {
    pub center: Vec2,
    pub radius: f32,
    pub matrix: Mat2,
}

impl DabTransform {
    pub fn round(center: Vec2, radius: f32) -> Self {
        Self {
            center,
            radius,
            matrix: Mat2::IDENTITY,
        }
    }
}

/// Build the dab placements for the selected symmetry mode. Mirror axes and
/// the radial pivot are in document pixels. The first placement is always the
/// unmodified dab; exact duplicates are dropped so a brush centred on a mirror
/// axis is not stamped twice.
pub fn symmetry_placements(
    center: Vec2,
    angle: f32,
    symmetry: Symmetry,
    radial_count: u32,
    pivot: Vec2,
) -> Vec<StampPlacement> {
    let base = Mat2::from_angle(angle);
    let mut out = Vec::with_capacity(9);
    push_unique(
        &mut out,
        StampPlacement {
            center,
            matrix: base,
        },
    );
    match symmetry {
        Symmetry::Off => {}
        Symmetry::MirrorX => push_unique(
            &mut out,
            StampPlacement {
                center: Vec2::new(2.0 * pivot.x - center.x, center.y),
                matrix: Mat2::from_diagonal(Vec2::new(-1.0, 1.0)) * base,
            },
        ),
        Symmetry::MirrorY => push_unique(
            &mut out,
            StampPlacement {
                center: Vec2::new(center.x, 2.0 * pivot.y - center.y),
                matrix: Mat2::from_diagonal(Vec2::new(1.0, -1.0)) * base,
            },
        ),
        Symmetry::MirrorXY => push_unique(
            &mut out,
            StampPlacement {
                center: Vec2::new(2.0 * pivot.x - center.x, 2.0 * pivot.y - center.y),
                matrix: Mat2::from_diagonal(Vec2::new(-1.0, -1.0)) * base,
            },
        ),
        Symmetry::Radial => {
            let count = radial_count.clamp(2, 32);
            let offset = center - pivot;
            for index in 1..count {
                let step = std::f32::consts::TAU * index as f32 / count as f32;
                push_unique(
                    &mut out,
                    StampPlacement {
                        center: pivot + Mat2::from_angle(step) * offset,
                        matrix: Mat2::from_angle(step) * base,
                    },
                );
            }
        }
    }
    out
}

fn push_unique(out: &mut Vec<StampPlacement>, placement: StampPlacement) {
    let duplicate = out.iter().any(|existing| {
        if existing.center.distance_squared(placement.center) > 1e-4 {
            return false;
        }
        let a = existing.matrix.to_cols_array();
        let b = placement.matrix.to_cols_array();
        a.iter().zip(b.iter()).all(|(a, b)| (a - b).abs() <= 1e-4)
    });
    if !duplicate {
        out.push(placement);
    }
}

/// An imported alpha or normal-map brush tip.
pub struct BrushTip {
    pub name: String,
    /// Coverage in 0..=1. `None` is the analytic round brush.
    pub alpha: Option<Grid<f32>>,
    /// Canonical OpenGL tangent-space normals for normal brushes.
    pub normal: Option<Grid<Vec3>>,
}

impl Clone for BrushTip {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            alpha: self.alpha.clone(),
            normal: self.normal.clone(),
        }
    }
}

impl std::fmt::Debug for BrushTip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrushTip")
            .field("name", &self.name)
            .field("alpha", &self.alpha.as_ref().map(|g| (g.w, g.h)))
            .field("normal", &self.normal.as_ref().map(|g| (g.w, g.h)))
            .finish()
    }
}

impl Default for BrushTip {
    fn default() -> Self {
        Self::round()
    }
}

impl BrushTip {
    pub fn round() -> Self {
        Self {
            name: "Soft round".into(),
            alpha: None,
            normal: None,
        }
    }

    pub fn from_alpha(name: impl Into<String>, alpha: Grid<f32>) -> Self {
        Self {
            name: name.into(),
            alpha: Some(alpha),
            normal: None,
        }
    }

    /// Builds a normal brush. Coverage comes from `coverage` when supplied
    /// (the source alpha channel) and is gated by the normal's slope so flat
    /// background pixels in a normal map do not stamp a full-square dab.
    pub fn from_normal(
        name: impl Into<String>,
        normal: Grid<Vec3>,
        coverage: Option<Grid<f32>>,
    ) -> Self {
        let w = normal.w;
        let h = normal.h;
        let alpha = Grid::from_fn(w, h, |x, y| {
            let n = *normal.get(x, y);
            let slope = (n.x * n.x + n.y * n.y).sqrt();
            let gate = smoothstep(0.01, 0.12, slope);
            let base = coverage
                .as_ref()
                .map(|grid| {
                    if grid.w == w && grid.h == h {
                        *grid.get(x, y)
                    } else {
                        let u = x as f32 / (w.max(1) - 1) as f32;
                        let v = y as f32 / (h.max(1) - 1) as f32;
                        sample_bilinear_scalar(grid, u, v)
                    }
                })
                .unwrap_or(1.0);
            (base * gate).clamp(0.0, 1.0)
        });
        Self {
            name: name.into(),
            alpha: Some(alpha),
            normal: Some(normal),
        }
    }

    pub fn is_normal(&self) -> bool {
        self.normal.is_some()
    }

    pub fn alpha_at(&self, u: f32, v: f32) -> f32 {
        match &self.alpha {
            Some(grid) => sample_bilinear_scalar(grid, u, v),
            None => 1.0,
        }
    }

    pub fn normal_at(&self, u: f32, v: f32) -> Option<Vec3> {
        let grid = self.normal.as_ref()?;
        let n = sample_bilinear_normal(grid, u, v);
        if n.length_squared() > 1e-8 {
            Some(n.normalize())
        } else {
            None
        }
    }

    /// True when this tip draws from an image rather than the analytic circle.
    pub fn is_image(&self) -> bool {
        self.alpha.is_some()
    }
}

/// Stamp one dab, optionally using an imported alpha tip, into a height field.
pub fn stamp_tip(
    height: &mut Grid<f32>,
    tip: Option<&BrushTip>,
    dab: &DabTransform,
    hardness: f32,
    flow: f32,
    mode: BrushMode,
) -> Option<DirtyRect> {
    if dab.radius <= 0.0 || flow <= 0.0 {
        return None;
    }
    let rect = dab_bounds(dab, height.w, height.h)?;
    let hardness = hardness.clamp(0.0, 0.999);
    let inverse = dab.matrix.transpose();
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let local = inverse * (p - dab.center) / dab.radius;
            let alpha = dab_alpha(tip, local, hardness);
            if alpha <= 0.0 {
                continue;
            }
            let a = (alpha * flow).clamp(0.0, 1.0);
            let current = *height.get(x, y);

            let next = match mode {
                BrushMode::Raise => (current + a).min(1.0),
                BrushMode::Lower => (current - a).max(0.0),
                BrushMode::Smooth => {
                    let mut sum = 0.0;
                    let mut count = 0.0;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            sum += height.sample(x as i32 + dx, y as i32 + dy, Wrap::Clamp);
                            count += 1.0;
                        }
                    }
                    let avg = sum / count;
                    current + (avg - current) * a
                }
                BrushMode::Flatten(target) => current + (target.clamp(0.0, 1.0) - current) * a,
            };

            *height.get_mut(x, y) = next;
        }
    }

    Some(rect)
}

/// Stamp a normal-map brush into a tangent-space normal field. The stamp's
/// slopes are added to the existing surface, so detail accumulates over a
/// baked or painted base instead of replacing it.
pub fn stamp_normal_tip(
    normal: &mut Grid<Vec3>,
    tip: &BrushTip,
    dab: &DabTransform,
    flow: f32,
) -> Option<DirtyRect> {
    if dab.radius <= 0.0 || flow <= 0.0 || tip.normal.is_none() {
        return None;
    }
    let rect = dab_bounds(dab, normal.w, normal.h)?;
    let inverse = dab.matrix.transpose();
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let local = inverse * (p - dab.center) / dab.radius;
            let u = local.x * 0.5 + 0.5;
            let v = local.y * 0.5 + 0.5;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let alpha = tip.alpha_at(u, v) * flow;
            if alpha <= 0.0 {
                continue;
            }
            let Some(stamp) = tip.normal_at(u, v) else {
                continue;
            };
            // Rotation and mirroring are orthogonal, so the linear map itself
            // is the inverse transpose for normals.
            let mapped = dab.matrix * Vec2::new(stamp.x, stamp.y);
            let stamp = Vec3::new(mapped.x, mapped.y, stamp.z.max(1e-3)).normalize();
            let base = *normal.get(x, y);
            let base_dx = -base.x / base.z.max(1e-3);
            let base_dy = base.y / base.z.max(1e-3);
            let stamp_dx = -stamp.x / stamp.z.max(1e-3);
            let stamp_dy = stamp.y / stamp.z.max(1e-3);
            let dx = base_dx + stamp_dx * alpha;
            let dy = base_dy + stamp_dy * alpha;
            *normal.get_mut(x, y) = Vec3::new(-dx, dy, 1.0).normalize();
        }
    }
    Some(rect)
}

fn dab_alpha(tip: Option<&BrushTip>, local: Vec2, hardness: f32) -> f32 {
    if let Some(tip) = tip {
        if tip.alpha.is_some() {
            let u = local.x * 0.5 + 0.5;
            let v = local.y * 0.5 + 0.5;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                return 0.0;
            }
            return tip.alpha_at(u, v).clamp(0.0, 1.0);
        }
    }
    let r = local.length();
    if r >= 1.0 {
        0.0
    } else {
        smoothstep(1.0, hardness, r)
    }
}

fn dab_bounds(dab: &DabTransform, width: u32, height: u32) -> Option<DirtyRect> {
    let r = dab.radius;
    let corners = [
        Vec2::new(-r, -r),
        Vec2::new(r, -r),
        Vec2::new(r, r),
        Vec2::new(-r, r),
    ];
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for corner in corners {
        let p = dab.center + dab.matrix * corner;
        min = min.min(p);
        max = max.max(p);
    }
    let x0 = min.x.floor().max(0.0) as u32;
    let y0 = min.y.floor().max(0.0) as u32;
    let x1 = max.x.ceil().min(width as f32) as u32;
    let y1 = max.y.ceil().min(height as f32) as u32;
    if x1 <= x0 || y1 <= y0 {
        None
    } else {
        Some(DirtyRect { x0, y0, x1, y1 })
    }
}

fn sample_bilinear_scalar(grid: &Grid<f32>, u: f32, v: f32) -> f32 {
    if grid.w == 0 || grid.h == 0 {
        return 0.0;
    }
    let fx = u.clamp(0.0, 1.0) * (grid.w - 1) as f32;
    let fy = v.clamp(0.0, 1.0) * (grid.h - 1) as f32;
    let x0 = fx.floor() as u32;
    let y0 = fy.floor() as u32;
    let x1 = (x0 + 1).min(grid.w - 1);
    let y1 = (y0 + 1).min(grid.h - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let top = *grid.get(x0, y0) * (1.0 - tx) + *grid.get(x1, y0) * tx;
    let bottom = *grid.get(x0, y1) * (1.0 - tx) + *grid.get(x1, y1) * tx;
    top * (1.0 - ty) + bottom * ty
}

fn sample_bilinear_normal(grid: &Grid<Vec3>, u: f32, v: f32) -> Vec3 {
    if grid.w == 0 || grid.h == 0 {
        return Vec3::Z;
    }
    let fx = u.clamp(0.0, 1.0) * (grid.w - 1) as f32;
    let fy = v.clamp(0.0, 1.0) * (grid.h - 1) as f32;
    let x0 = fx.floor() as u32;
    let y0 = fy.floor() as u32;
    let x1 = (x0 + 1).min(grid.w - 1);
    let y1 = (y0 + 1).min(grid.h - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let top = grid.get(x0, y0).lerp(*grid.get(x1, y0), tx);
    let bottom = grid.get(x0, y1).lerp(*grid.get(x1, y1), tx);
    top.lerp(bottom, ty)
}

/// What a mask stroke moves values towards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrokeTarget {
    /// Paint a fixed value (1 = reveal, 0 = hide, anything between).
    Value(f32),
    /// Pull values towards a Gaussian-blurred copy of the mask.
    Blur,
}

/// One continuous mask stroke.
///
/// Dabs accumulate into a per-stroke coverage buffer that approaches
/// `opacity` but never exceeds it, and the mask is recomputed as
/// `lerp(before_stroke, target, coverage)`. Overlapping dabs therefore do not
/// build up into beads, and a 40% stroke stays 40% however often it is
/// scrubbed over the same spot. Start a new stroke to build further.
#[derive(Clone, Debug)]
pub struct MaskStroke {
    base: Grid<f32>,
    coverage: Grid<f32>,
    blurred: Option<Grid<f32>>,
    target: StrokeTarget,
    opacity: f32,
}

impl MaskStroke {
    /// `blur_sigma` sets the Blur target's softening, in mask pixels.
    pub fn new(mask: &Grid<f32>, target: StrokeTarget, opacity: f32, blur_sigma: f32) -> Self {
        let blurred = matches!(target, StrokeTarget::Blur)
            .then(|| crate::filter::gaussian_blur(mask, blur_sigma.max(0.5), Wrap::Clamp));
        Self {
            base: mask.clone(),
            coverage: Grid::new(mask.w, mask.h, 0.0),
            blurred,
            target,
            opacity: opacity.clamp(0.0, 1.0),
        }
    }

    /// Stamp one round dab. `centre` and `radius` are in mask pixels.
    pub fn dab(
        &mut self,
        mask: &mut Grid<f32>,
        centre: Vec2,
        radius: f32,
        hardness: f32,
        flow: f32,
    ) -> Option<DirtyRect> {
        self.dab_with_tip(
            mask,
            None,
            &DabTransform::round(centre, radius),
            hardness,
            flow,
        )
    }

    /// Stamp one dab using an optional alpha tip and symmetry transform.
    pub fn dab_with_tip(
        &mut self,
        mask: &mut Grid<f32>,
        tip: Option<&BrushTip>,
        dab: &DabTransform,
        hardness: f32,
        flow: f32,
    ) -> Option<DirtyRect> {
        if dab.radius <= 0.0 || flow <= 0.0 || mask.w != self.base.w || mask.h != self.base.h {
            return None;
        }
        let rect = dab_bounds(dab, mask.w, mask.h)?;
        let hardness = hardness.clamp(0.0, 0.999);
        let inverse = dab.matrix.transpose();
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let local = inverse * (p - dab.center) / dab.radius;
                let alpha = dab_alpha(tip, local, hardness);
                if alpha <= 0.0 {
                    continue;
                }
                let a = (alpha * flow).clamp(0.0, 1.0);
                let coverage = self.coverage.get_mut(x, y);
                *coverage += (self.opacity - *coverage).max(0.0) * a;
                let t = *coverage;
                let before = *self.base.get(x, y);
                let target = match self.target {
                    StrokeTarget::Value(value) => value.clamp(0.0, 1.0),
                    StrokeTarget::Blur => *self
                        .blurred
                        .as_ref()
                        .expect("blur strokes keep a blurred copy")
                        .get(x, y),
                };
                *mask.get_mut(x, y) = before + (target - before) * t;
            }
        }
        Some(rect)
    }
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raise_increases_height() {
        let mut h = Grid::new(32, 32, 0.0);
        let dirty = stamp(
            &mut h,
            Vec2::new(16.0, 16.0),
            5.0,
            0.5,
            1.0,
            BrushMode::Raise,
        );
        assert!(dirty.is_some());
        assert!(*h.get(16, 16) > 0.0);
        assert!(*h.get(0, 0) == 0.0);
    }

    #[test]
    fn lower_decreases_height() {
        let mut h = Grid::new(32, 32, 1.0);
        stamp(
            &mut h,
            Vec2::new(16.0, 16.0),
            5.0,
            0.5,
            1.0,
            BrushMode::Lower,
        );
        assert!(*h.get(16, 16) < 1.0);
    }

    #[test]
    fn flatten_moves_toward_target() {
        let mut h = Grid::new(32, 32, 0.0);
        stamp(
            &mut h,
            Vec2::new(16.0, 16.0),
            5.0,
            0.5,
            1.0,
            BrushMode::Flatten(0.5),
        );
        assert!((*h.get(16, 16) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn mask_stroke_is_capped_by_opacity() {
        let mut mask = Grid::new(32, 32, 1.0);
        let mut stroke = MaskStroke::new(&mask, StrokeTarget::Value(0.0), 0.4, 0.0);
        for _ in 0..50 {
            stroke.dab(&mut mask, Vec2::new(16.0, 16.0), 6.0, 0.8, 0.5);
        }
        // 40% opacity towards black, never further, however many dabs land.
        assert!(
            (mask.get(16, 16) - 0.6).abs() < 1e-3,
            "{}",
            mask.get(16, 16)
        );
        assert_eq!(*mask.get(0, 0), 1.0);
    }

    #[test]
    fn overlapping_dabs_do_not_bead() {
        // A dense line of dabs should give an even core, not bright knots
        // where consecutive dabs overlap.
        let mut mask = Grid::new(64, 32, 0.0);
        let mut stroke = MaskStroke::new(&mask, StrokeTarget::Value(1.0), 1.0, 0.0);
        for i in 0..40 {
            let x = 12.0 + i as f32;
            stroke.dab(&mut mask, Vec2::new(x, 16.0), 6.0, 0.5, 0.3);
        }
        let core: Vec<f32> = (20..44).map(|x| *mask.get(x, 16)).collect();
        let min = core.iter().cloned().fold(1.0, f32::min);
        let max = core.iter().cloned().fold(0.0, f32::max);
        assert!(max - min < 0.02, "uneven core: {min}..{max}");
        assert!(max <= 1.0);
    }

    #[test]
    fn blur_stroke_softens_a_hard_edge() {
        let mut mask = Grid::from_fn(32, 32, |x, _| if x < 16 { 0.0 } else { 1.0 });
        let mut stroke = MaskStroke::new(&mask, StrokeTarget::Blur, 1.0, 3.0);
        for _ in 0..10 {
            stroke.dab(&mut mask, Vec2::new(16.0, 16.0), 8.0, 0.9, 1.0);
        }
        let left = *mask.get(14, 16);
        let right = *mask.get(17, 16);
        assert!(
            left > 0.1 && right < 0.9,
            "edge not softened: {left} {right}"
        );
        // Outside the dab the hard edge is untouched.
        assert_eq!(*mask.get(14, 2), 0.0);
        assert_eq!(*mask.get(17, 2), 1.0);
    }

    #[test]
    fn brush_is_clipped_to_document() {
        let mut h = Grid::new(16, 16, 0.0);
        let dirty = stamp(&mut h, Vec2::new(0.0, 0.0), 8.0, 0.0, 1.0, BrushMode::Raise);
        let d = dirty.unwrap();
        assert_eq!(d.x0, 0);
        assert_eq!(d.y0, 0);
        assert!(d.x1 <= 16);
        assert!(d.y1 <= 16);
    }

    #[test]
    fn alpha_tip_stamps_only_where_the_image_is_opaque() {
        // Left half transparent, right half opaque: only the right side paints.
        let alpha = Grid::from_fn(8, 8, |x, _| if x >= 4 { 1.0 } else { 0.0 });
        let tip = BrushTip::from_alpha("half", alpha);
        let mut h = Grid::new(32, 32, 0.0);
        let dab = DabTransform::round(Vec2::splat(16.0), 8.0);
        stamp_tip(&mut h, Some(&tip), &dab, 0.5, 1.0, BrushMode::Raise);
        assert_eq!(*h.get(12, 16), 0.0);
        assert!(*h.get(20, 16) > 0.0);
    }

    #[test]
    fn normal_tip_adds_slope_to_a_flat_base() {
        let normal = Grid::new(4, 4, Vec3::new(0.5, 0.0, 0.866));
        let tip = BrushTip::from_normal("bevel", normal, None);
        let mut base = Grid::new(32, 32, Vec3::Z);
        let dab = DabTransform::round(Vec2::splat(16.0), 8.0);
        stamp_normal_tip(&mut base, &tip, &dab, 1.0).unwrap();
        let stamped = *base.get(16, 16);
        assert!(
            (stamped - Vec3::new(0.5, 0.0, 0.866).normalize()).length() < 1e-3,
            "a flat base should reproduce the stamp, got {stamped:?}"
        );
        assert!((stamped.length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn mirrored_symmetry_mirrors_the_dab_and_the_tip() {
        let placements = symmetry_placements(
            Vec2::new(10.0, 20.0),
            0.0,
            Symmetry::MirrorX,
            8,
            Vec2::new(32.0, 32.0),
        );
        assert_eq!(placements.len(), 2);
        assert!((placements[1].center.x - 54.0).abs() < 1e-4);
        // A mirror flips the local X axis, so the matrix determinant is negative.
        assert!(placements[1].matrix.determinant() < 0.0);
    }

    #[test]
    fn radial_symmetry_places_the_requested_number_of_dabs() {
        let pivot = Vec2::splat(32.0);
        let centre = Vec2::new(48.0, 32.0);
        let placements = symmetry_placements(centre, 0.0, Symmetry::Radial, 6, pivot);
        assert_eq!(placements.len(), 6);
        // Every placement keeps the same distance from the pivot.
        let radius = centre.distance(pivot);
        for placement in &placements {
            assert!((placement.center.distance(pivot) - radius).abs() < 1e-3);
        }
    }

    #[test]
    fn a_dab_on_the_mirror_axis_keeps_its_mirrored_copy() {
        let placements = symmetry_placements(
            Vec2::new(32.0, 20.0),
            0.0,
            Symmetry::MirrorX,
            8,
            Vec2::new(32.0, 32.0),
        );
        // The reflected dab sits at the same centre but flips the tip, which is
        // the correct symmetric result for an asymmetric brush.
        assert_eq!(placements.len(), 2);
        assert!((placements[1].center - placements[0].center).length() < 1e-4);
        assert!(placements[1].matrix.determinant() < 0.0);
    }
}
