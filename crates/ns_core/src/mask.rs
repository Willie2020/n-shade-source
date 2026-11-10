//! Non-destructive layer masks.
//!
//! A layer's mask is built from three parts:
//!
//! * the painted bitmap ([`crate::Layer::mask`]), adjusted by invert, levels,
//!   feather and density;
//! * static generators (tileable noise breakup) that only depend on the
//!   document;
//! * smart generators that read the surface composited *below* the layer
//!   (slope, curvature). These are what make detail normals sit naturally:
//!   restrict a detail to flat plateaus, keep it off bevels, or push wear onto
//!   convex edges.
//!
//! Static parts are evaluated once per mask revision and can feed clipping
//! masks. Smart parts depend on the stack position, so they are evaluated at
//! composite time and only affect the layer itself.

use glam::{UVec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::derived::normal_curvature;
use crate::filter::gaussian_blur;
use crate::grid::{Grid, Wrap};
use crate::height::height_to_normal;

/// Per-layer mask adjustments and generators. All defaults are neutral, so
/// older projects load unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaskSettings {
    /// Turns the painted mask and every generator off without deleting them.
    pub enabled: bool,
    /// Inverts the painted bitmap (black reveals, white hides).
    pub invert: bool,
    /// Levels on the painted bitmap: values at or below `black_point` hide,
    /// values at or above `white_point` reveal. Narrowing the range hardens
    /// soft brush edges.
    pub black_point: f32,
    pub white_point: f32,
    /// Gaussian softening of the static mask, in document pixels.
    pub feather_px: f32,
    /// How strongly the static mask hides the layer (0 = ignored).
    pub density: f32,
    /// Use the outside of the clipping layer instead of its inside.
    pub invert_clip: bool,
    /// Grow (positive) or shrink (negative) the clipping mask, in document
    /// pixels. A small positive offset pushes the clipped layer slightly past
    /// the shape below, giving a clean rim before other shapes.
    pub clip_offset_px: f32,
    /// Softens the clipping mask edge, in document pixels.
    pub clip_feather_px: f32,
    /// Procedural masks, multiplied together with the painted mask.
    pub generators: Vec<MaskGenerator>,
}

impl Default for MaskSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            invert: false,
            black_point: 0.0,
            white_point: 1.0,
            feather_px: 0.0,
            density: 1.0,
            invert_clip: false,
            clip_offset_px: 0.0,
            clip_feather_px: 0.0,
            generators: Vec::new(),
        }
    }
}

impl MaskSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn adjusts_painted(&self) -> bool {
        self.invert
            || self.black_point > 0.0
            || self.white_point < 1.0
            || self.feather_px > 1e-3
            || (self.density - 1.0).abs() > 1e-6
    }

    fn active_generators(&self) -> impl Iterator<Item = &MaskGenerator> {
        self.generators
            .iter()
            .filter(move |generator| self.enabled && generator.enabled && generator.strength > 0.0)
    }

    /// True when at least one enabled generator reads the surface below.
    pub fn needs_base(&self) -> bool {
        self.active_generators()
            .any(|generator| generator.kind.needs_base())
    }

    /// True when the settings change anything for a layer that has
    /// (`painted`) or lacks a painted bitmap.
    pub fn has_effect(&self, painted: bool) -> bool {
        self.clip_is_adjusted()
            || (self.enabled && (painted || self.active_generators().next().is_some()))
    }

    /// True when the clipping mask is transformed (inverted, offset, soft).
    pub fn clip_is_adjusted(&self) -> bool {
        self.invert_clip || self.clip_offset_px.abs() > 1e-4 || self.clip_feather_px > 1e-3
    }
}

/// One procedural mask term.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaskGenerator {
    pub kind: GeneratorKind,
    pub enabled: bool,
    pub invert: bool,
    /// Blend between no effect (0) and the full generator (1).
    pub strength: f32,
}

impl Default for MaskGenerator {
    fn default() -> Self {
        Self::new(GeneratorKind::default())
    }
}

impl MaskGenerator {
    pub fn new(kind: GeneratorKind) -> Self {
        Self {
            kind,
            enabled: true,
            invert: false,
            strength: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CurvatureMode {
    /// Convex ridges and outer bevel tops.
    #[default]
    Edges,
    /// Concave creases and inner corners.
    Cavities,
    /// Both, by magnitude.
    Both,
}

impl CurvatureMode {
    pub const ALL: [Self; 3] = [Self::Edges, Self::Cavities, Self::Both];

    pub fn label(self) -> &'static str {
        match self {
            Self::Edges => "Edges (convex)",
            Self::Cavities => "Cavities (concave)",
            Self::Both => "Edges + cavities",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GeneratorKind {
    /// Reveals where the surface below tilts between `min_deg` and `max_deg`.
    /// `0..=12` keeps a detail on flat plateaus; `20..=90` keeps it on bevels.
    Slope {
        min_deg: f32,
        max_deg: f32,
        softness_deg: f32,
    },
    /// Reveals the curved parts of the surface below.
    Curvature {
        mode: CurvatureMode,
        /// Widens the selected band, in document pixels.
        spread_px: f32,
        /// Gain applied before the response is clamped to `0..=1`.
        contrast: f32,
    },
    /// Tileable fractal noise for breaking up uniform details.
    Noise {
        scale_px: f32,
        octaves: u32,
        seed: u32,
        /// Coverage threshold (0 = fully revealed, 1 = hidden).
        threshold: f32,
        softness: f32,
    },
}

impl Default for GeneratorKind {
    fn default() -> Self {
        Self::slope()
    }
}

impl GeneratorKind {
    pub fn slope() -> Self {
        Self::Slope {
            min_deg: 0.0,
            max_deg: 12.0,
            softness_deg: 4.0,
        }
    }

    pub fn curvature() -> Self {
        Self::Curvature {
            mode: CurvatureMode::Edges,
            spread_px: 2.0,
            contrast: 1.5,
        }
    }

    pub fn noise() -> Self {
        Self::Noise {
            scale_px: 96.0,
            octaves: 4,
            seed: 1,
            threshold: 0.45,
            softness: 0.2,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Slope { .. } => "Slope",
            Self::Curvature { .. } => "Curvature",
            Self::Noise { .. } => "Noise breakup",
        }
    }

    pub fn needs_base(&self) -> bool {
        matches!(self, Self::Slope { .. } | Self::Curvature { .. })
    }
}

/// Static part of a layer's mask at document resolution: the adjusted
/// painted bitmap times every static generator. `None` means fully revealed.
pub fn static_mask(
    painted: Option<&Grid<f32>>,
    settings: &MaskSettings,
    size: UVec2,
    wrap: Wrap,
) -> Option<Grid<f32>> {
    if !settings.enabled {
        return None;
    }
    let mut mask = painted.map(|grid| {
        let mut grid = if grid.w == size.x && grid.h == size.y {
            grid.clone()
        } else {
            grid.resized(size.x, size.y)
        };
        if settings.adjusts_painted() {
            let black = settings.black_point.clamp(0.0, 0.999);
            let white = settings.white_point.clamp(black + 1e-3, 1.0);
            for value in &mut grid.data {
                let mut v = value.clamp(0.0, 1.0);
                if settings.invert {
                    v = 1.0 - v;
                }
                *value = ((v - black) / (white - black)).clamp(0.0, 1.0);
            }
        }
        grid
    });

    for generator in settings.active_generators() {
        if generator.kind.needs_base() {
            continue;
        }
        let term = generator_term(generator, size, wrap, None);
        multiply_into(&mut mask, &term);
    }

    let mut mask = mask?;
    if settings.feather_px > 1e-3 {
        mask = gaussian_blur(&mask, settings.feather_px, wrap);
    }
    let density = settings.density.clamp(0.0, 1.0);
    if density < 1.0 {
        for value in &mut mask.data {
            *value = 1.0 - density * (1.0 - value.clamp(0.0, 1.0));
        }
    }
    Some(mask)
}

/// Smart part of a layer's mask, evaluated against the composited surface
/// below the layer. `None` means no smart generator is active.
pub fn smart_mask(
    settings: &MaskSettings,
    base: &BaseSurface<'_>,
    wrap: Wrap,
) -> Option<Grid<f32>> {
    if !settings.needs_base() {
        return None;
    }
    let normals = base.normals(wrap);
    let size = UVec2::new(normals.w, normals.h);
    let mut mask = None;
    for generator in settings.active_generators() {
        if !generator.kind.needs_base() {
            continue;
        }
        let term = generator_term(generator, size, wrap, Some(&normals));
        multiply_into(&mut mask, &term);
    }
    mask
}

/// Prepare a clipping source's coverage for use as a mask: optional inversion,
/// then grow/shrink by `offset_px` and soften by `feather_px` using the signed
/// distance to the coverage boundary.
pub fn clip_coverage(
    clip: &Grid<f32>,
    invert: bool,
    offset_px: f32,
    feather_px: f32,
    wrap: Wrap,
) -> Grid<f32> {
    let base = if invert {
        clip.map(|c| 1.0 - c.clamp(0.0, 1.0))
    } else {
        clip.map(|c| c.clamp(0.0, 1.0))
    };
    if offset_px.abs() <= 1e-4 && feather_px <= 1e-3 {
        return base;
    }
    // Signed distance to the mask boundary: positive inside, negative outside.
    // Each side is measured separately because `inside_distance` only grades
    // the interior (outside pixels all collapse to the same value).
    let inverted = base.map(|c| 1.0 - c.clamp(0.0, 1.0));
    let inside = crate::sdf::inside_distance_aa(&base, wrap);
    let outside = crate::sdf::inside_distance_aa(&inverted, wrap);
    let soft = (feather_px * 0.5).max(0.5);
    let edge = -offset_px;
    Grid::from_fn_par(base.w, base.h, |x, y| {
        let coverage = *base.get(x, y);
        let distance = if coverage > 0.5 {
            *inside.get(x, y)
        } else {
            -*outside.get(x, y)
        };
        smoothstep(edge - soft, edge + soft, distance)
    })
}

/// The surface below a layer, as normals or as a height field.
pub enum BaseSurface<'a> {
    Normal(&'a Grid<Vec3>),
    /// Normalized height plus the document height scale in pixels.
    Height(&'a Grid<f32>, f32),
}

impl BaseSurface<'_> {
    fn normals(&self, wrap: Wrap) -> std::borrow::Cow<'_, Grid<Vec3>> {
        match self {
            Self::Normal(normals) => std::borrow::Cow::Borrowed(*normals),
            Self::Height(height, scale) => {
                std::borrow::Cow::Owned(height_to_normal(height, scale.max(1e-3), wrap, false))
            }
        }
    }
}

fn multiply_into(mask: &mut Option<Grid<f32>>, term: &Grid<f32>) {
    match mask {
        Some(mask) => {
            for (dst, src) in mask.data.iter_mut().zip(term.data.iter()) {
                *dst *= src;
            }
        }
        None => *mask = Some(term.clone()),
    }
}

fn generator_term(
    generator: &MaskGenerator,
    size: UVec2,
    wrap: Wrap,
    base: Option<&Grid<Vec3>>,
) -> Grid<f32> {
    let raw = match &generator.kind {
        GeneratorKind::Slope {
            min_deg,
            max_deg,
            softness_deg,
        } => {
            let base = base.expect("slope generator needs the base surface");
            let lo = min_deg.min(*max_deg);
            let hi = min_deg.max(*max_deg);
            let soft = softness_deg.max(0.0);
            base.map(|n| {
                let angle = n.z.clamp(-1.0, 1.0).acos().to_degrees();
                window(angle, lo, hi, soft)
            })
        }
        GeneratorKind::Curvature {
            mode,
            spread_px,
            contrast,
        } => {
            let base = base.expect("curvature generator needs the base surface");
            let mut curvature = normal_curvature(base, wrap);
            if *spread_px > 1e-3 {
                curvature = gaussian_blur(&curvature, *spread_px, wrap);
            }
            let scale = robust_scale(&curvature);
            let gain = contrast.max(0.0) / scale;
            curvature.map(|c| {
                let v = match mode {
                    CurvatureMode::Edges => c.max(0.0),
                    CurvatureMode::Cavities => (-c).max(0.0),
                    CurvatureMode::Both => c.abs(),
                };
                (v * gain).clamp(0.0, 1.0)
            })
        }
        GeneratorKind::Noise {
            scale_px,
            octaves,
            seed,
            threshold,
            softness,
        } => {
            let noise = fbm_noise(size, *scale_px, *octaves, *seed, wrap);
            let soft = softness.max(1e-3) * 0.5;
            let t = threshold.clamp(0.0, 1.0);
            noise.map(|v| smoothstep(t - soft, t + soft, *v))
        }
    };
    let strength = generator.strength.clamp(0.0, 1.0);
    let invert = generator.invert;
    raw.map(|v| {
        let v = if invert { 1.0 - v } else { *v };
        1.0 - strength * (1.0 - v.clamp(0.0, 1.0))
    })
}

/// Smooth band-pass: 1 inside `[lo, hi]`, fading over `soft` on both sides.
/// A band starting at 0° keeps a hard 1 at 0° (perfectly flat stays in).
fn window(value: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    let rise = if lo <= 0.0 {
        1.0
    } else {
        smoothstep(lo - soft, lo + soft, value)
    };
    let fall = if hi >= 90.0 {
        1.0
    } else {
        1.0 - smoothstep(hi - soft, hi + soft, value)
    };
    (rise * fall).clamp(0.0, 1.0)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 98th percentile of `|c|`, so a few extreme pixels do not flatten the rest.
fn robust_scale(values: &Grid<f32>) -> f32 {
    let mut magnitudes: Vec<f32> = values
        .data
        .iter()
        .map(|v| v.abs())
        .filter(|v| *v > 1e-6)
        .collect();
    if magnitudes.is_empty() {
        return 1.0;
    }
    let index = ((magnitudes.len() - 1) as f32 * 0.98) as usize;
    let (_, value, _) = magnitudes.select_nth_unstable_by(index, |a, b| a.total_cmp(b));
    value.max(1e-6)
}

/// Fractal value noise in `0..=1`. With [`Wrap::Repeat`] the lattice period
/// divides the document size, so the noise tiles seamlessly.
pub fn fbm_noise(size: UVec2, scale_px: f32, octaves: u32, seed: u32, wrap: Wrap) -> Grid<f32> {
    let octaves = octaves.clamp(1, 8);
    let base_cells_x = (size.x as f32 / scale_px.max(2.0)).round().max(1.0) as u32;
    let base_cells_y = (size.y as f32 / scale_px.max(2.0)).round().max(1.0) as u32;
    let tile = wrap == Wrap::Repeat;
    Grid::from_fn_par(size.x, size.y, |x, y| {
        let mut sum = 0.0;
        let mut amplitude = 1.0;
        let mut total = 0.0;
        for octave in 0..octaves {
            let cells_x = base_cells_x << octave;
            let cells_y = base_cells_y << octave;
            let u = (x as f32 + 0.5) / size.x as f32 * cells_x as f32;
            let v = (y as f32 + 0.5) / size.y as f32 * cells_y as f32;
            let period = tile.then_some((cells_x, cells_y));
            sum += value_noise(u, v, seed.wrapping_add(octave * 1013), period) * amplitude;
            total += amplitude;
            amplitude *= 0.5;
        }
        sum / total
    })
}

fn value_noise(u: f32, v: f32, seed: u32, period: Option<(u32, u32)>) -> f32 {
    let x0 = u.floor();
    let y0 = v.floor();
    let fx = u - x0;
    let fy = v - y0;
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);
    let corner = |ix: i64, iy: i64| {
        let (ix, iy) = match period {
            Some((px, py)) => (ix.rem_euclid(px as i64), iy.rem_euclid(py as i64)),
            None => (ix, iy),
        };
        hash01(ix as u32, iy as u32, seed)
    };
    let (ix, iy) = (x0 as i64, y0 as i64);
    let a = corner(ix, iy);
    let b = corner(ix + 1, iy);
    let c = corner(ix, iy + 1);
    let d = corner(ix + 1, iy + 1);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sy
}

fn hash01(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x
        .wrapping_mul(0x8da6_b343)
        .wrapping_add(y.wrapping_mul(0xd816_3841))
        .wrapping_add(seed.wrapping_mul(0xcb1a_b31f));
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 0x00ff_ffff as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> UVec2 {
        UVec2::splat(32)
    }

    #[test]
    fn defaults_are_neutral() {
        let settings = MaskSettings::default();
        assert!(settings.is_default());
        assert!(!settings.has_effect(false));
        assert!(static_mask(None, &settings, size(), Wrap::Clamp).is_none());
        let painted = Grid::new(32, 32, 0.25);
        let mask = static_mask(Some(&painted), &settings, size(), Wrap::Clamp).unwrap();
        assert!((mask.get(5, 5) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn old_layers_deserialize_with_default_settings() {
        let settings: MaskSettings = ron::from_str("()").unwrap();
        assert!(settings.is_default());
    }

    #[test]
    fn invert_levels_and_density_shape_the_painted_mask() {
        let painted = Grid::from_fn(32, 32, |x, _| x as f32 / 31.0);
        let settings = MaskSettings {
            invert: true,
            ..Default::default()
        };
        let mask = static_mask(Some(&painted), &settings, size(), Wrap::Clamp).unwrap();
        assert!((mask.get(0, 0) - 1.0).abs() < 1e-6);
        assert!(mask.get(31, 0).abs() < 1e-6);

        let settings = MaskSettings {
            black_point: 0.4,
            white_point: 0.6,
            ..Default::default()
        };
        let mask = static_mask(Some(&painted), &settings, size(), Wrap::Clamp).unwrap();
        assert_eq!(*mask.get(5, 0), 0.0);
        assert_eq!(*mask.get(25, 0), 1.0);

        let settings = MaskSettings {
            density: 0.5,
            ..Default::default()
        };
        let black = Grid::new(32, 32, 0.0);
        let mask = static_mask(Some(&black), &settings, size(), Wrap::Clamp).unwrap();
        assert!((mask.get(3, 3) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn disabled_mask_is_ignored() {
        let settings = MaskSettings {
            enabled: false,
            generators: vec![MaskGenerator::new(GeneratorKind::noise())],
            ..Default::default()
        };
        let painted = Grid::new(32, 32, 0.0);
        assert!(static_mask(Some(&painted), &settings, size(), Wrap::Clamp).is_none());
        assert!(!settings.has_effect(true));
    }

    #[test]
    fn painted_masks_are_resampled_to_the_document() {
        let painted = Grid::from_fn(16, 16, |x, _| if x < 8 { 0.0 } else { 1.0 });
        let mask = static_mask(
            Some(&painted),
            &MaskSettings::default(),
            size(),
            Wrap::Clamp,
        )
        .unwrap();
        assert_eq!((mask.w, mask.h), (32, 32));
        assert!(*mask.get(2, 10) < 0.05);
        assert!(*mask.get(29, 10) > 0.95);
    }

    #[test]
    fn slope_generator_separates_flat_and_steep_areas() {
        let flat = Vec3::Z;
        let steep = Vec3::new(0.7, 0.0, 0.714).normalize();
        let base = Grid::from_fn(32, 32, |x, _| if x < 16 { flat } else { steep });
        let settings = MaskSettings {
            generators: vec![MaskGenerator::new(GeneratorKind::slope())],
            ..Default::default()
        };
        assert!(settings.needs_base());
        let mask = smart_mask(&settings, &BaseSurface::Normal(&base), Wrap::Clamp).unwrap();
        assert!((mask.get(4, 4) - 1.0).abs() < 1e-6);
        assert!(*mask.get(28, 4) < 1e-6);

        let mut inverted = settings.clone();
        inverted.generators[0].invert = true;
        let mask = smart_mask(&inverted, &BaseSurface::Normal(&base), Wrap::Clamp).unwrap();
        assert!(*mask.get(4, 4) < 1e-6);
        assert!((mask.get(28, 4) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn slope_generator_reads_a_height_base() {
        // A ramp on the right half: flat left, sloped right.
        let height = Grid::from_fn(
            32,
            32,
            |x, _| if x < 16 { 0.0 } else { (x - 16) as f32 / 16.0 },
        );
        let settings = MaskSettings {
            generators: vec![MaskGenerator::new(GeneratorKind::slope())],
            ..Default::default()
        };
        let mask = smart_mask(&settings, &BaseSurface::Height(&height, 16.0), Wrap::Clamp).unwrap();
        assert!(*mask.get(4, 4) > 0.99);
        assert!(*mask.get(24, 4) < 0.01);
    }

    #[test]
    fn curvature_edges_light_up_convex_ridges() {
        // A raised square: its bevel tops are convex, the floor next to it concave.
        let mut height = Grid::new(64, 64, 0.0);
        for y in 16..48 {
            for x in 16..48 {
                height.set(x, y, 1.0);
            }
        }
        let height = gaussian_blur(&height, 3.0, Wrap::Clamp);
        let normals = height_to_normal(&height, 12.0, Wrap::Clamp, false);
        let edges = MaskSettings {
            generators: vec![MaskGenerator::new(GeneratorKind::curvature())],
            ..Default::default()
        };
        let mask = smart_mask(&edges, &BaseSurface::Normal(&normals), Wrap::Clamp).unwrap();
        // Just inside the square edge (convex shoulder) vs just outside (concave foot).
        assert!(*mask.get(18, 32) > *mask.get(13, 32) + 0.3);
        assert!(*mask.get(32, 32) < 0.05, "flat plateau should be hidden");
    }

    #[test]
    fn noise_is_deterministic_bounded_and_tiles() {
        let a = fbm_noise(UVec2::splat(64), 16.0, 4, 7, Wrap::Repeat);
        let b = fbm_noise(UVec2::splat(64), 16.0, 4, 7, Wrap::Repeat);
        assert_eq!(a, b);
        assert!(a.data.iter().all(|v| (0.0..=1.0).contains(v)));
        let c = fbm_noise(UVec2::splat(64), 16.0, 4, 8, Wrap::Repeat);
        assert_ne!(a, c);
        // Opposite edges continue each other when tiled.
        let seam: f32 = (0..64)
            .map(|y| (a.get(0, y) - a.get(63, y)).abs())
            .sum::<f32>()
            / 64.0;
        let interior: f32 = (0..64)
            .map(|y| (a.get(31, y) - a.get(32, y)).abs())
            .sum::<f32>()
            / 64.0;
        assert!(
            seam < interior * 3.0 + 0.02,
            "seam {seam} vs interior {interior}"
        );
    }

    #[test]
    fn clip_offset_grows_and_shrinks_the_mask() {
        let mut clip = Grid::new(64, 64, 0.0);
        for y in 24..40 {
            for x in 24..40 {
                clip.set(x, y, 1.0);
            }
        }
        let exact = clip_coverage(&clip, false, 0.0, 0.0, Wrap::Clamp);
        assert_eq!(*exact.get(32, 32), 1.0);
        assert_eq!(*exact.get(20, 32), 0.0);

        let grown = clip_coverage(&clip, false, 5.0, 0.0, Wrap::Clamp);
        assert!(*grown.get(20, 32) > 0.9, "grow should reach 5 px out");
        assert!(*grown.get(12, 32) < 0.1);

        let shrunk = clip_coverage(&clip, false, -4.0, 0.0, Wrap::Clamp);
        assert!(*shrunk.get(32, 32) > 0.9);
        assert!(*shrunk.get(26, 32) < 0.1, "shrink should cut 4 px in");
    }

    #[test]
    fn clip_offset_follows_the_inverted_region() {
        let mut clip = Grid::new(64, 64, 0.0);
        for y in 24..40 {
            for x in 24..40 {
                clip.set(x, y, 1.0);
            }
        }
        // Growing the outside region eats 4 px into the square: the hole
        // shrinks, everything still outside stays included.
        let grown = clip_coverage(&clip, true, 4.0, 0.0, Wrap::Clamp);
        assert!(
            *grown.get(19, 32) > 0.9,
            "outside the square stays included"
        );
        assert!(*grown.get(27, 32) > 0.9, "grown past the edge by 3 px");
        assert!(*grown.get(29, 32) < 0.1, "5 px inside is cut away again");
        assert!(*grown.get(32, 32) < 0.1, "the middle stays masked out");
    }

    #[test]
    fn clip_feather_softens_the_boundary() {
        let mut clip = Grid::new(64, 64, 0.0);
        for y in 24..40 {
            for x in 24..40 {
                clip.set(x, y, 1.0);
            }
        }
        let soft = clip_coverage(&clip, false, 0.0, 8.0, Wrap::Clamp);
        let edge = *soft.get(24, 32);
        assert!(edge > 0.2 && edge < 0.8, "edge should be partial: {edge}");
        assert!(*soft.get(32, 32) > 0.95);
    }

    #[test]
    fn generator_strength_blends_towards_reveal() {
        let base = Grid::new(8, 8, Vec3::new(0.7, 0.0, 0.714).normalize());
        let mut generator = MaskGenerator::new(GeneratorKind::slope());
        generator.strength = 0.25;
        let settings = MaskSettings {
            generators: vec![generator],
            ..Default::default()
        };
        let mask = smart_mask(&settings, &BaseSurface::Normal(&base), Wrap::Clamp).unwrap();
        assert!((mask.get(2, 2) - 0.75).abs() < 1e-4);
    }
}
