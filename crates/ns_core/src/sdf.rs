use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::filter::gaussian_blur;
use crate::grid::{Grid, Wrap};

const INF: f32 = 1e20;

/// Bevel profile in normalized distance space `t = d / radius`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Profile {
    /// Quarter-circle profile.
    Round,
    /// Straight chamfer at the given angle.
    Chamfer { degrees: f32 },
    /// Linear ramp (a 45° chamfer in height space).
    Linear,
    /// Convex polynomial: rises quickly, then flattens out.
    Convex,
    /// Concave polynomial: starts flat, then rises steeply.
    Concave,
    /// Smoothstep S-curve.
    Smooth,
    /// 256-sample height LUT over `t` in `[0, 1]`.
    Custom(Vec<f32>),
}

impl Profile {
    pub const CUSTOM_LUT_SAMPLES: usize = 256;

    pub fn sample(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Profile::Round => (1.0 - (1.0 - t).powi(2)).sqrt(),
            Profile::Chamfer { degrees } => t * degrees.to_radians().tan().max(0.0),
            Profile::Linear => t,
            Profile::Convex => 1.0 - (1.0 - t).powi(3),
            Profile::Concave => t.powi(3),
            Profile::Smooth => t * t * (3.0 - 2.0 * t),
            Profile::Custom(lut) => {
                if lut.is_empty() {
                    return 0.0;
                }
                let max = (lut.len() - 1) as f32;
                let x = t * max;
                let i = x.floor() as usize;
                let f = x - i as f32;
                let a = lut[i.min(lut.len() - 1)];
                let b = lut[(i + 1).min(lut.len() - 1)];
                a + (b - a) * f
            }
        }
    }

    /// Builds a custom LUT from normalized control points `(t, value)`.
    ///
    /// Uses monotone cubic Hermite interpolation (Fritsch–Carlson tangents), so
    /// the curve is smooth through every point without overshooting. This is
    /// the same curve the floating editor draws, so what you edit is the bevel
    /// that is evaluated.
    pub fn lut_from_points(points: &[(f32, f32)]) -> Vec<f32> {
        let mut sorted: Vec<(f32, f32)> = points
            .iter()
            .map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
            .collect();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        sorted.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
        let count = Self::CUSTOM_LUT_SAMPLES;
        if sorted.is_empty() {
            return vec![0.0; count];
        }
        if sorted.len() == 1 {
            return vec![sorted[0].1; count];
        }

        // Secant slopes per segment and tangent slope at each point.
        let segments = sorted.len() - 1;
        let mut secants = Vec::with_capacity(segments);
        for pair in sorted.windows(2) {
            secants.push((pair[1].1 - pair[0].1) / (pair[1].0 - pair[0].0).max(1e-6));
        }
        let mut tangents = vec![0.0f32; sorted.len()];
        tangents[0] = secants[0];
        tangents[segments] = secants[segments - 1];
        for index in 1..segments {
            let before = secants[index - 1];
            let after = secants[index];
            // Flat or reversing segments are extrema: keep the tangent flat so
            // the smoothed curve cannot bulge past the authored value.
            tangents[index] = if before * after <= 0.0 {
                0.0
            } else {
                let average = (before + after) * 0.5;
                let limit = 3.0 * before.abs().min(after.abs());
                average.clamp(-limit, limit)
            };
        }

        let mut lut = Vec::with_capacity(count);
        let mut segment = 0usize;
        for index in 0..count {
            let t = index as f32 / (count - 1) as f32;
            if t <= sorted[0].0 {
                lut.push(sorted[0].1);
                continue;
            }
            if t >= sorted[segments].0 {
                lut.push(sorted[segments].1);
                continue;
            }
            while segment + 1 < segments && t > sorted[segment + 1].0 {
                segment += 1;
            }
            let (x0, y0) = sorted[segment];
            let (x1, y1) = sorted[segment + 1];
            let dx = (x1 - x0).max(1e-6);
            let u = ((t - x0) / dx).clamp(0.0, 1.0);
            let u2 = u * u;
            let u3 = u2 * u;
            let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
            let h10 = u3 - 2.0 * u2 + u;
            let h01 = -2.0 * u3 + 3.0 * u2;
            let h11 = u3 - u2;
            let value = h00 * y0
                + h10 * dx * tangents[segment]
                + h01 * y1
                + h11 * dx * tangents[segment + 1];
            lut.push(value.clamp(0.0, 1.0));
        }
        lut
    }

    /// Evenly samples a profile into editable control points.
    pub fn control_points(&self, count: usize) -> Vec<(f32, f32)> {
        let count = count.max(2);
        (0..count)
            .map(|index| {
                let t = index as f32 / (count - 1) as f32;
                (t, self.sample(t).clamp(0.0, 1.0))
            })
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BevelParams {
    pub radius_px: f32,
    pub height_px: f32,
    pub profile: Profile,
    /// Carve the bevel: the band stays put and the surface sinks into it.
    pub invert: bool,
    pub softness_px: f32,
    /// Blur applied to the coverage before the distance transform.
    ///
    /// Unlike `softness_px`, which smooths the slope of the bevel, this evens
    /// out the outline itself: jagged edges, noisy mask borders and faceted
    /// radii are averaged away and the profile stays as authored.
    #[serde(default)]
    pub smooth_px: f32,
    /// Width in pixels over which the bevel normal fades into the flat surface
    /// at the shape boundary. `0` keeps the hard profile edge; 1–2 px removes
    /// the stair-step at small radii and small shapes.
    #[serde(default)]
    pub edge_aa_px: f32,
}

impl Default for BevelParams {
    fn default() -> Self {
        Self {
            radius_px: 16.0,
            height_px: 8.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
        }
    }
}

/// Nearest path-edge lookup for per-edge bevel profiles.
///
/// `ids` and `second` hold the two closest source edges for each texel. `blend`
/// is the weight of `second` in `0..=0.5`: it fades to zero away from a corner,
/// so changing the profile between two edges does not leave a hard diagonal
/// seam in the normal map.
#[derive(Clone, Debug)]
pub struct EdgeProfileField {
    pub ids: Grid<u16>,
    pub second: Grid<u16>,
    pub blend: Grid<f32>,
    pub profiles: Vec<Option<Profile>>,
    pub default: Profile,
}

impl EdgeProfileField {
    pub fn profile(&self, id: u16) -> &Profile {
        if id == u16::MAX {
            return &self.default;
        }
        self.profiles
            .get(id as usize)
            .and_then(|profile| profile.as_ref())
            .unwrap_or(&self.default)
    }

    fn sample(&self, x: u32, y: u32, t: f32) -> f32 {
        let first = *self.ids.get(x, y);
        let second = *self.second.get(x, y);
        let weight = self.blend.get(x, y).clamp(0.0, 0.5);
        let a = self.profile(first).sample(t);
        if second == u16::MAX || second == first || weight <= 1e-4 {
            return a;
        }
        let b = self.profile(second).sample(t);
        a * (1.0 - weight) + b * weight
    }

    fn slope(&self, x: u32, y: u32, t: f32) -> f32 {
        let first = *self.ids.get(x, y);
        let second = *self.second.get(x, y);
        let weight = self.blend.get(x, y).clamp(0.0, 0.5);
        let a = profile_slope(self.profile(first), t);
        if second == u16::MAX || second == first || weight <= 1e-4 {
            return a;
        }
        let b = profile_slope(self.profile(second), t);
        a * (1.0 - weight) + b * weight
    }
}

/// `dP/dt` of a normalized bevel profile.
///
/// Round and Chamfer have exact derivatives; the other built-ins are analytic
/// too, and a custom LUT is differentiated over two adjacent samples. The
/// result is scaled by `height_px / radius_px` where it is used, which is the
/// bevel's rise over its run.
pub fn profile_slope(profile: &Profile, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match profile {
        Profile::Round => {
            let z = (1.0 - (1.0 - t) * (1.0 - t)).max(0.0).sqrt();
            if z < 1e-4 {
                0.0
            } else {
                (1.0 - t) / z
            }
        }
        Profile::Chamfer { degrees } => degrees.to_radians().tan().max(0.0),
        Profile::Linear => 1.0,
        Profile::Convex => 3.0 * (1.0 - t) * (1.0 - t),
        Profile::Concave => 3.0 * t * t,
        Profile::Smooth => 6.0 * t * (1.0 - t),
        Profile::Custom(_) => {
            const DT: f32 = 1.0 / 512.0;
            let lo_t = (t - DT).max(0.0);
            let hi_t = (t + DT).min(1.0);
            let span = hi_t - lo_t;
            if span <= 1e-6 {
                0.0
            } else {
                (profile.sample(hi_t) - profile.sample(lo_t)) / span
            }
        }
    }
}

/// Coverage with the bevel's outline smoothing applied.
///
/// Returns the input untouched when smoothing is off, so the common case does
/// not copy the full-resolution grid.
fn smoothed_coverage<'a>(
    coverage: &'a Grid<f32>,
    params: &BevelParams,
    wrap: Wrap,
) -> std::borrow::Cow<'a, Grid<f32>> {
    if params.smooth_px > 1e-4 {
        std::borrow::Cow::Owned(gaussian_blur(coverage, params.smooth_px, wrap))
    } else {
        std::borrow::Cow::Borrowed(coverage)
    }
}

/// 1D squared-distance transform (Felzenszwalb–Huttenlocher).
fn edt_1d(f: &[f32], d: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    if f.iter().all(|v| *v == 0.0) {
        d.fill(0.0);
        return;
    }
    if f.iter().all(|v| v.is_infinite()) {
        d.fill(INF);
        return;
    }

    let mut v = vec![0usize; n];
    let mut z = vec![0.0f32; n + 1];
    let mut k = 0usize;

    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;

    for q in 1..n {
        let mut s;
        loop {
            let p = v[k];
            s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] {
                if k == 0 {
                    break;
                }
                k -= 1;
            } else {
                break;
            }
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }

    k = 0;
    for (q, dq) in d.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        *dq = (q as f32 - p as f32).powi(2) + f[p];
    }
}

/// Squared Euclidean distance from every pixel to the nearest seed (`true`).
pub fn distance_transform(mask: &Grid<bool>) -> Grid<f32> {
    let w = mask.w as usize;
    let h = mask.h as usize;
    let mut f = vec![0.0f32; w * h];
    for (i, &inside) in mask.data.iter().enumerate() {
        f[i] = if inside { 0.0 } else { INF };
    }

    let mut col = vec![0.0f32; h];
    let mut out = vec![0.0f32; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = f[y * w + x];
        }
        edt_1d(&col, &mut out);
        for y in 0..h {
            f[y * w + x] = out[y];
        }
    }

    let mut row = vec![0.0f32; w];
    let mut out = vec![0.0f32; w];
    for y in 0..h {
        let start = y * w;
        row.copy_from_slice(&f[start..start + w]);
        edt_1d(&row, &mut out);
        f[start..start + w].copy_from_slice(&out);
    }

    let cap = (w * w + h * h) as f32;
    Grid::from_fn(w as u32, h as u32, |x, y| {
        f[(y as usize) * w + x as usize].min(cap).sqrt()
    })
}

fn tile_wrap(mask: &Grid<bool>) -> (Grid<bool>, u32, u32) {
    let w = mask.w;
    let h = mask.h;
    let mut out = Grid::new(w * 3, h * 3, false);
    for ty in 0..3 {
        for tx in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    out.set(tx * w + x, ty * h + y, *mask.get(x, y));
                }
            }
        }
    }
    (out, w, h)
}

fn crop_f32(src: &Grid<f32>, ox: u32, oy: u32, w: u32, h: u32) -> Grid<f32> {
    Grid::from_fn(w, h, |x, y| *src.get(ox + x, oy + y))
}

/// Distance from each inside pixel to the nearest outside pixel.
/// Positive inside, negative outside, with the edge at `-0.5`/`0.5`.
pub fn inside_distance(mask: &Grid<bool>, wrap: Wrap) -> Grid<f32> {
    // Seed the outside pixels, so inside pixels measure distance to the edge.
    let seeds = mask.map(|inside| !*inside);
    let mut d = distance_from_seeds(&seeds, wrap);
    for v in &mut d.data {
        *v -= 0.5;
    }
    d
}

/// Distance from each outside pixel to the nearest inside pixel.
pub fn outside_distance(mask: &Grid<bool>, wrap: Wrap) -> Grid<f32> {
    // Seed the inside pixels, so outside pixels measure distance to the edge.
    let mut d = distance_from_seeds(mask, wrap);
    for v in &mut d.data {
        *v -= 0.5;
    }
    d
}

fn distance_from_seeds(seeds: &Grid<bool>, wrap: Wrap) -> Grid<f32> {
    if wrap == Wrap::Repeat {
        let (tiled, ox, oy) = tile_wrap(seeds);
        let full = distance_transform(&tiled);
        crop_f32(&full, ox, oy, seeds.w, seeds.h)
    } else {
        distance_transform(seeds)
    }
}

/// Anti-aliased variant: threshold the coverage, then correct edge pixels by
/// their sub-pixel coverage. This removes stair-stepping from soft masks.
pub fn inside_distance_aa(coverage: &Grid<f32>, wrap: Wrap) -> Grid<f32> {
    let mask = coverage.map(|c| *c > 0.5);
    let mut d = inside_distance(&mask, wrap);
    for (i, &c) in coverage.data.iter().enumerate() {
        if c > 0.001 && c < 0.999 {
            d.data[i] -= c - 0.5;
        }
    }
    d
}

/// Distance to a band centred on the coverage boundary.
///
/// Positive inside the band and negative outside, with `width_px` being the
/// total stroke width. This uses a true signed distance so empty pixels far
/// from the outline stay far outside the band; the one-sided distance used by
/// filled bevels only knows the boundary value there.
pub fn stroke_distance_aa(coverage: &Grid<f32>, width_px: f32, wrap: Wrap) -> Grid<f32> {
    let distance = signed_distance_aa(coverage, wrap);
    let half = width_px.max(0.0) * 0.5;
    Grid::from_fn(distance.w, distance.h, |x, y| {
        half - distance.get(x, y).abs()
    })
}

/// Signed distance to the coverage boundary: positive inside, negative
/// outside. Soft edge pixels use their coverage as the sub-pixel distance.
pub fn signed_distance_aa(coverage: &Grid<f32>, wrap: Wrap) -> Grid<f32> {
    let mask = coverage.map(|c| *c > 0.5);
    let inside = inside_distance(&mask, wrap);
    let outside = outside_distance(&mask, wrap);
    let mut d = Grid::from_fn(coverage.w, coverage.h, |x, y| {
        if *mask.get(x, y) {
            *inside.get(x, y)
        } else {
            -*outside.get(x, y)
        }
    });
    for (i, &c) in coverage.data.iter().enumerate() {
        if c > 0.001 && c < 0.999 {
            d.data[i] = c - 0.5;
        }
    }
    d
}

/// Anti-aliased coverage of a band centred on the source outline.
pub fn stroke_coverage_aa(coverage: &Grid<f32>, width_px: f32, wrap: Wrap) -> Grid<f32> {
    let distance = stroke_distance_aa(coverage, width_px, wrap);
    Grid::from_fn(distance.w, distance.h, |x, y| {
        (distance.get(x, y) + 0.5).clamp(0.0, 1.0)
    })
}

/// Anti-aliased coverage -> outline band distance -> bevel.
pub fn bevel_mask_stroke_aa(
    coverage: &Grid<f32>,
    params: &BevelParams,
    width_px: f32,
    wrap: Wrap,
) -> Grid<Vec3> {
    let coverage = smoothed_coverage(coverage, params, wrap);
    let mut distance = stroke_distance_aa(&coverage, width_px, wrap);
    if params.softness_px > 1e-4 {
        distance = gaussian_blur(&distance, params.softness_px, wrap);
    }
    bevel_from_distance(&distance, params)
}

/// Anti-aliased coverage -> outline band distance -> height.
pub fn height_mask_stroke_aa(
    coverage: &Grid<f32>,
    params: &BevelParams,
    width_px: f32,
    wrap: Wrap,
) -> Grid<f32> {
    let coverage = smoothed_coverage(coverage, params, wrap);
    let mut distance = stroke_distance_aa(&coverage, width_px, wrap);
    if params.softness_px > 1e-4 {
        distance = gaussian_blur(&distance, params.softness_px, wrap);
    }
    height_from_distance(&distance, params)
}

/// Turn a distance field into a bevel normal field.
///
/// `smooth_px` blurs a copy of the field that only the gradient is read from:
/// an exact distance transform kinks wherever the nearest boundary pixel
/// changes, and those kinks show up as ribs along diagonal bevels. The slope
/// `t` keeps the exact distance, so the profile, its width and the silhouette
/// stay as authored while the shading direction is smooth.
pub fn bevel_from_distance(d: &Grid<f32>, params: &BevelParams) -> Grid<Vec3> {
    let radius = params.radius_px.max(1e-4);
    let direction_field = if params.smooth_px > 1e-4 {
        Some(gaussian_blur(
            d,
            params.smooth_px.min(radius * 0.5),
            Wrap::Clamp,
        ))
    } else {
        None
    };
    let gradient = direction_field.as_ref().unwrap_or(d);
    let edge_aa = params.edge_aa_px.max(0.0);

    Grid::from_fn_par(d.w, d.h, |x, y| {
        let dist = *d.get(x, y);
        let t = (dist / radius).clamp(0.0, 1.0);

        if dist <= 0.0 || t >= 1.0 {
            return Vec3::Z;
        }

        // Anti-alias the silhouette: ramp the lateral slope from 0 at the
        // boundary to full inside. A one-pixel radius otherwise jumps from a
        // 90-degree bevel normal straight to flat and stair-steps.
        let fade = if edge_aa > 1e-4 {
            let u = (dist / edge_aa).clamp(0.0, 1.0);
            u * u * (3.0 - 2.0 * u)
        } else {
            1.0
        };

        let gx = (gradient.sample(x as i32 + 1, y as i32, Wrap::Clamp)
            - gradient.sample(x as i32 - 1, y as i32, Wrap::Clamp))
            * 0.5;
        let gy = (gradient.sample(x as i32, y as i32 + 1, Wrap::Clamp)
            - gradient.sample(x as i32, y as i32 - 1, Wrap::Clamp))
            * 0.5;
        let mut g = glam::Vec2::new(gx, gy);
        let gl = g.length();
        if gl > 1e-5 {
            g /= gl;
        }

        // Match `height_to_normal` (OpenGL, Y+): n = (-dh/dx, +dh/dy) with
        // image rows increasing downward. The height rises along `g` for the
        // normal profile and falls along it when inverted, which gives the
        // signed slope direction below.
        let s = if params.invert { -1.0 } else { 1.0 };
        let g = glam::Vec2::new(-g.x, g.y) * s;
        // A bevel's lateral slope is its rise over its run: `height_px` over
        // `radius_px`. Round and Chamfer used to shade as if every bevel were
        // exactly one radius tall, so changing Height never changed the normal
        // and every shape looked equally deep. The curve profiles below already
        // include this ratio in their analytic slope.
        let rise = (params.height_px / radius).clamp(-50.0, 50.0);
        let n = match &params.profile {
            Profile::Round => Vec3::new(
                g.x * (1.0 - t) * rise * fade,
                g.y * (1.0 - t) * rise * fade,
                (1.0 - (1.0 - t).powi(2)).max(0.0).sqrt(),
            ),
            Profile::Chamfer { degrees } => {
                let theta = degrees.to_radians();
                Vec3::new(
                    g.x * theta.sin() * rise * fade,
                    g.y * theta.sin() * rise * fade,
                    theta.cos(),
                )
            }
            Profile::Linear
            | Profile::Convex
            | Profile::Concave
            | Profile::Smooth
            | Profile::Custom(_) => {
                let dt = 1.0 / 512.0;
                let f0 = params.profile.sample((t - dt).max(0.0));
                let f1 = params.profile.sample((t + dt).min(1.0));
                let slope = ((f1 - f0) / (2.0 * dt) * params.height_px / radius).clamp(-50.0, 50.0);
                Vec3::new(g.x * slope * fade, g.y * slope * fade, 1.0)
            }
        };

        if n.length_squared() > 1e-12 {
            n.normalize()
        } else {
            Vec3::Z
        }
    })
}

/// Height field implied by a distance field and profile. Used by the height
/// export and the exact shape-height path.
pub fn height_from_distance(d: &Grid<f32>, params: &BevelParams) -> Grid<f32> {
    let radius = params.radius_px.max(1e-4);
    Grid::from_fn_par(d.w, d.h, |x, y| {
        let dist = *d.get(x, y);
        let t = (dist / radius).clamp(0.0, 1.0);
        let p = params.profile.sample(t);
        let p = if params.invert { 1.0 - p } else { p };
        p * params.height_px
    })
}

/// Mask -> distance -> bevel with softness.
///
/// The bevel band always sits inside the shape; `invert` carves the shape
/// (the surface sinks inward) instead of moving the band outside, which used
/// to cancel the normal flip and make inset and outset look identical.
pub fn bevel_mask(mask: &Grid<bool>, params: &BevelParams, wrap: Wrap) -> Grid<Vec3> {
    let d = inside_distance(mask, wrap);
    let d = if params.softness_px > 1e-4 {
        gaussian_blur(&d, params.softness_px, wrap)
    } else {
        d
    };
    bevel_from_distance(&d, params)
}

/// Mask -> distance -> height.
pub fn height_mask(mask: &Grid<bool>, params: &BevelParams, wrap: Wrap) -> Grid<f32> {
    let d = inside_distance(mask, wrap);
    let d = if params.softness_px > 1e-4 {
        gaussian_blur(&d, params.softness_px, wrap)
    } else {
        d
    };
    height_from_distance(&d, params)
}

/// Anti-aliased coverage -> distance -> bevel.
pub fn bevel_mask_aa(coverage: &Grid<f32>, params: &BevelParams, wrap: Wrap) -> Grid<Vec3> {
    let coverage = smoothed_coverage(coverage, params, wrap);
    let d = inside_distance_aa(&coverage, wrap);
    let d = if params.softness_px > 1e-4 {
        gaussian_blur(&d, params.softness_px, wrap)
    } else {
        d
    };
    bevel_from_distance(&d, params)
}

/// Anti-aliased coverage -> distance -> height.
pub fn height_mask_aa(coverage: &Grid<f32>, params: &BevelParams, wrap: Wrap) -> Grid<f32> {
    let coverage = smoothed_coverage(coverage, params, wrap);
    let d = inside_distance_aa(&coverage, wrap);
    let d = if params.softness_px > 1e-4 {
        gaussian_blur(&d, params.softness_px, wrap)
    } else {
        d
    };
    height_from_distance(&d, params)
}

/// Anti-aliased coverage -> distance -> bevel with per-edge profiles.
///
/// Falls back to the uniform path when there is no edge field, so shapes
/// without per-edge overrides keep exactly the normals they had before.
pub fn bevel_mask_aa_with(
    coverage: &Grid<f32>,
    params: &BevelParams,
    edges: Option<&EdgeProfileField>,
    wrap: Wrap,
) -> Grid<Vec3> {
    let Some(edges) = edges else {
        return bevel_mask_aa(coverage, params, wrap);
    };
    let coverage = smoothed_coverage(coverage, params, wrap);
    let d = inside_distance_aa(&coverage, wrap);
    let d = if params.softness_px > 1e-4 {
        gaussian_blur(&d, params.softness_px, wrap)
    } else {
        d
    };
    bevel_from_distance_with(&d, params, Some(edges))
}

/// Anti-aliased coverage -> distance -> height with per-edge profiles.
pub fn height_mask_aa_with(
    coverage: &Grid<f32>,
    params: &BevelParams,
    edges: Option<&EdgeProfileField>,
    wrap: Wrap,
) -> Grid<f32> {
    let Some(edges) = edges else {
        return height_mask_aa(coverage, params, wrap);
    };
    let coverage = smoothed_coverage(coverage, params, wrap);
    let d = inside_distance_aa(&coverage, wrap);
    let d = if params.softness_px > 1e-4 {
        gaussian_blur(&d, params.softness_px, wrap)
    } else {
        d
    };
    height_from_distance_with(&d, params, Some(edges))
}

/// Stroke-band variant of [`bevel_mask_aa_with`].
pub fn bevel_mask_stroke_aa_with(
    coverage: &Grid<f32>,
    params: &BevelParams,
    width_px: f32,
    edges: Option<&EdgeProfileField>,
    wrap: Wrap,
) -> Grid<Vec3> {
    let Some(edges) = edges else {
        return bevel_mask_stroke_aa(coverage, params, width_px, wrap);
    };
    let coverage = smoothed_coverage(coverage, params, wrap);
    let mut distance = stroke_distance_aa(&coverage, width_px, wrap);
    if params.softness_px > 1e-4 {
        distance = gaussian_blur(&distance, params.softness_px, wrap);
    }
    bevel_from_distance_with(&distance, params, Some(edges))
}

/// Stroke-band variant of [`height_mask_aa_with`].
pub fn height_mask_stroke_aa_with(
    coverage: &Grid<f32>,
    params: &BevelParams,
    width_px: f32,
    edges: Option<&EdgeProfileField>,
    wrap: Wrap,
) -> Grid<f32> {
    let Some(edges) = edges else {
        return height_mask_stroke_aa(coverage, params, width_px, wrap);
    };
    let coverage = smoothed_coverage(coverage, params, wrap);
    let mut distance = stroke_distance_aa(&coverage, width_px, wrap);
    if params.softness_px > 1e-4 {
        distance = gaussian_blur(&distance, params.softness_px, wrap);
    }
    height_from_distance_with(&distance, params, Some(edges))
}

/// Per-edge variant of [`bevel_from_distance`].
pub fn bevel_from_distance_with(
    d: &Grid<f32>,
    params: &BevelParams,
    edges: Option<&EdgeProfileField>,
) -> Grid<Vec3> {
    let Some(edges) = edges else {
        return bevel_from_distance(d, params);
    };
    let radius = params.radius_px.max(1e-4);
    let direction_field = if params.smooth_px > 1e-4 {
        Some(gaussian_blur(
            d,
            params.smooth_px.min(radius * 0.5),
            Wrap::Clamp,
        ))
    } else {
        None
    };
    let gradient = direction_field.as_ref().unwrap_or(d);
    let edge_aa = params.edge_aa_px.max(0.0);
    let rise = (params.height_px / radius).clamp(-50.0, 50.0);

    Grid::from_fn_par(d.w, d.h, |x, y| {
        let dist = *d.get(x, y);
        let t = (dist / radius).clamp(0.0, 1.0);
        if dist <= 0.0 || t >= 1.0 {
            return Vec3::Z;
        }
        let fade = if edge_aa > 1e-4 {
            let u = (dist / edge_aa).clamp(0.0, 1.0);
            u * u * (3.0 - 2.0 * u)
        } else {
            1.0
        };
        let gx = (gradient.sample(x as i32 + 1, y as i32, Wrap::Clamp)
            - gradient.sample(x as i32 - 1, y as i32, Wrap::Clamp))
            * 0.5;
        let gy = (gradient.sample(x as i32, y as i32 + 1, Wrap::Clamp)
            - gradient.sample(x as i32, y as i32 - 1, Wrap::Clamp))
            * 0.5;
        let mut g = glam::Vec2::new(gx, gy);
        let gl = g.length();
        if gl > 1e-5 {
            g /= gl;
        }
        let s = if params.invert { -1.0 } else { 1.0 };
        let g = glam::Vec2::new(-g.x, g.y) * s;
        let slope = edges.slope(x, y, t) * rise * fade;
        let n = Vec3::new(g.x * slope, g.y * slope, 1.0);
        if n.length_squared() > 1e-12 {
            n.normalize()
        } else {
            Vec3::Z
        }
    })
}

/// Per-edge variant of [`height_from_distance`].
pub fn height_from_distance_with(
    d: &Grid<f32>,
    params: &BevelParams,
    edges: Option<&EdgeProfileField>,
) -> Grid<f32> {
    let Some(edges) = edges else {
        return height_from_distance(d, params);
    };
    let radius = params.radius_px.max(1e-4);
    Grid::from_fn_par(d.w, d.h, |x, y| {
        let dist = *d.get(x, y);
        let t = (dist / radius).clamp(0.0, 1.0);
        let p = edges.sample(x, y, t);
        let p = if params.invert { 1.0 - p } else { p };
        p * params.height_px
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_band_stays_near_the_outline() {
        // A filled square with large empty margins: an outline band must not
        // flood the empty region (text blocks exposed this as a full plate).
        let coverage = Grid::from_fn(64, 64, |x, y| {
            if (20..44).contains(&x) && (20..44).contains(&y) {
                1.0
            } else {
                0.0
            }
        });
        let band = stroke_coverage_aa(&coverage, 6.0, Wrap::Clamp);
        assert!(*band.get(32, 32) < 0.01, "the interior must stay empty");
        assert!(*band.get(2, 2) < 0.01, "far outside must stay empty");
        assert!(*band.get(20, 32) > 0.5, "the boundary must be banded");

        let params = BevelParams {
            radius_px: 2.0,
            height_px: 8.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
        };
        let heights = height_mask_stroke_aa(&coverage, &params, 6.0, Wrap::Clamp);
        assert!(*heights.get(2, 2) < 0.01, "empty pixels carry no height");
        assert!(*heights.get(20, 32) > 0.01, "the band carries height");
    }

    #[test]
    fn edt_single_seed() {
        let mut mask = Grid::new(5, 5, false);
        mask.set(2, 2, true);
        let d = distance_transform(&mask);
        assert!((*d.get(2, 2) - 0.0).abs() < 1e-5);
        assert!((*d.get(2, 3) - 1.0).abs() < 1e-5);
        assert!((*d.get(4, 2) - 2.0).abs() < 1e-5);
        assert!((*d.get(0, 0) - (8.0f32).sqrt()).abs() < 1e-5);
    }

    #[test]
    fn inside_distance_signs() {
        let mut mask = Grid::new(9, 9, false);
        for y in 2..7 {
            for x in 2..7 {
                mask.set(x, y, true);
            }
        }
        let d = inside_distance(&mask, Wrap::Clamp);
        assert!(*d.get(4, 4) > 1.0);
        assert!(*d.get(0, 0) < 0.0);
        assert!(*d.get(2, 4) > 0.0);
    }

    #[test]
    fn bevel_is_flat_in_the_center_and_unit_length() {
        let mut mask = Grid::new(64, 64, false);
        for y in 8..56 {
            for x in 8..56 {
                mask.set(x, y, true);
            }
        }
        let params = BevelParams {
            radius_px: 12.0,
            height_px: 6.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
        };
        let n = bevel_mask(&mask, &params, Wrap::Clamp);
        assert!((*n.get(32, 32) - Vec3::Z).length() < 1e-5);
        let edge = *n.get(8, 32);
        assert!(edge.length() > 0.0);
        assert!((edge.length() - 1.0).abs() < 1e-4);
        assert!(edge.x.abs() > 0.1);
        for v in n.iter() {
            assert!((v.length() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn edge_aa_fades_the_bevel_at_the_silhouette() {
        let mut mask = Grid::new(64, 64, false);
        for y in 8..56 {
            for x in 8..56 {
                mask.set(x, y, true);
            }
        }
        let mut params = BevelParams {
            radius_px: 12.0,
            height_px: 6.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
        };
        let sharp = bevel_mask(&mask, &params, Wrap::Clamp);
        params.edge_aa_px = 4.0;
        let soft = bevel_mask(&mask, &params, Wrap::Clamp);

        // One pixel inside the boundary the lateral slope is faded…
        let sharp_edge = sharp.get(9, 32).truncate().length();
        let soft_edge = soft.get(9, 32).truncate().length();
        assert!(sharp_edge > 0.2, "sharp edge slope {sharp_edge}");
        assert!(soft_edge < sharp_edge * 0.6, "{soft_edge} vs {sharp_edge}");

        // …while the interior keeps the authored slope.
        let sharp_inner = sharp.get(14, 32).truncate().length();
        let soft_inner = soft.get(14, 32).truncate().length();
        assert!((sharp_inner - soft_inner).abs() < 1e-4);
        assert!(sharp_inner > 0.1);
    }

    #[test]
    fn square_bevel_is_symmetric() {
        let mut mask = Grid::new(64, 64, false);
        for y in 8..56 {
            for x in 8..56 {
                mask.set(x, y, true);
            }
        }
        let params = BevelParams::default();
        let n = bevel_mask(&mask, &params, Wrap::Clamp);
        let left = *n.get(9, 32);
        let right = *n.get(54, 32);
        assert!((left.x + right.x).abs() < 1e-4);
        assert!((left.y - right.y).abs() < 1e-4);
    }

    #[test]
    fn invert_flips_normal_direction() {
        let mut mask = Grid::new(64, 64, false);
        for y in 8..56 {
            for x in 8..56 {
                mask.set(x, y, true);
            }
        }
        let mut params = BevelParams {
            radius_px: 12.0,
            height_px: 6.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
        };
        let raised = bevel_mask(&mask, &params, Wrap::Clamp);
        let raised_left = *raised.get(10, 32);
        params.invert = true;
        let carved = bevel_mask(&mask, &params, Wrap::Clamp);
        let carved_left = *carved.get(10, 32);
        // The band sits in the same place in both modes; inverting carves the
        // surface, so the shading flips instead of looking identical.
        assert!(
            raised_left.x < -0.1,
            "raised bevel faces left, got {raised_left}"
        );
        assert!(
            carved_left.x > 0.1,
            "carved bevel must face the other way, got {carved_left}"
        );
        assert!(raised_left.z > 0.0 && carved_left.z > 0.0);
        // Same flip along Y: a raised top edge faces up (+Y), a carved one
        // faces down.
        assert!(raised.get(32, 14).y > 0.1);
        assert!(carved.get(32, 14).y < -0.1);
    }

    #[test]
    fn height_sets_the_round_and_chamfer_slope() {
        let mut mask = Grid::new(64, 64, false);
        for y in 8..56 {
            for x in 8..56 {
                mask.set(x, y, true);
            }
        }
        let mut params = BevelParams {
            radius_px: 10.0,
            height_px: 0.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
        };
        let flat = bevel_mask(&mask, &params, Wrap::Clamp);
        assert!(
            flat.get(10, 32).x.abs() < 1e-4,
            "zero height must shade flat, got {:?}",
            flat.get(10, 32)
        );

        let slope_at = |params: &BevelParams| {
            let n = *bevel_mask(&mask, params, Wrap::Clamp).get(10, 32);
            n.x / n.z
        };

        // Round: lateral gain is (1 - t) * height / radius.
        params.height_px = 5.0;
        let shallow = slope_at(&params);
        params.height_px = 10.0;
        let deep = slope_at(&params);
        assert!(
            (deep / shallow - 2.0).abs() < 0.05,
            "doubling Height must double the slope: {shallow} -> {deep}"
        );

        // Chamfer follows the same rise-over-run rule.
        params.profile = Profile::Chamfer { degrees: 45.0 };
        params.height_px = 5.0;
        let shallow = slope_at(&params);
        params.height_px = 10.0;
        let deep = slope_at(&params);
        assert!(
            (deep / shallow - 2.0).abs() < 0.05,
            "chamfer depth must scale the slope too: {shallow} -> {deep}"
        );
    }

    #[test]
    fn bevel_normals_match_their_height_gradient() {
        let mut mask = Grid::new(64, 64, false);
        for y in 16..48 {
            for x in 16..48 {
                mask.set(x, y, true);
            }
        }
        for invert in [false, true] {
            let params = BevelParams {
                radius_px: 10.0,
                height_px: 6.0,
                profile: Profile::Round,
                invert,
                softness_px: 0.0,
                smooth_px: 0.0,
                edge_aa_px: 0.0,
            };
            let normals = bevel_mask(&mask, &params, Wrap::Clamp);
            let height = height_mask(&mask, &params, Wrap::Clamp);
            let reference = crate::height::height_to_normal(&height, 1.0, Wrap::Clamp, false);
            // Left, right, top and bottom bevels. The band is inside the
            // square in both modes.
            let probes: [(u32, u32); 4] = [(20, 32), (43, 32), (32, 20), (32, 43)];
            for (x, y) in probes {
                let n = *normals.get(x, y);
                let r = *reference.get(x, y);
                assert!(
                    n.truncate().dot(r.truncate()) > 0.0 && r.truncate().length() > 1e-3,
                    "invert={invert} ({x},{y}): bevel {n:?} vs height gradient {r:?} (n={:?} r={:?})", n, r
                );
            }
        }
    }

    #[test]
    fn height_profile_rises_inside() {
        let mut mask = Grid::new(64, 64, false);
        for y in 8..56 {
            for x in 8..56 {
                mask.set(x, y, true);
            }
        }
        let params = BevelParams {
            radius_px: 12.0,
            height_px: 6.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
        };
        let h = height_mask(&mask, &params, Wrap::Clamp);
        assert!(*h.get(8, 32) < *h.get(32, 32));
        assert!(*h.get(20, 32) > 5.0);
        assert!((*h.get(32, 32) - 6.0).abs() < 1e-5);
    }

    #[test]
    fn builtin_profiles_span_zero_to_one() {
        for profile in [
            Profile::Round,
            Profile::Linear,
            Profile::Convex,
            Profile::Concave,
            Profile::Smooth,
        ] {
            assert!((profile.sample(0.0) - 0.0).abs() < 1e-5);
            assert!((profile.sample(1.0) - 1.0).abs() < 1e-5);
            assert!(profile.sample(0.5) > 0.0);
        }
    }

    #[test]
    fn custom_lut_smooths_between_control_points() {
        let points = [(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)];
        let lut = Profile::lut_from_points(&points);
        let profile = Profile::Custom(lut);
        // A smooth curve rises faster than a straight line before the peak.
        let quarter = profile.sample(0.25);
        assert!(quarter > 0.55, "quarter={quarter}");
        assert!((profile.sample(0.0) - 0.0).abs() < 1e-4);
        assert!((profile.sample(0.5) - 1.0).abs() < 1e-3);
        assert!((profile.sample(1.0) - 0.0).abs() < 1e-4);
        for index in 0..=100 {
            let value = profile.sample(index as f32 / 100.0);
            assert!((0.0..=1.0).contains(&value), "value={value}");
        }
    }

    #[test]
    fn custom_lut_matches_control_points() {
        let points = [(0.0, 0.0), (0.25, 0.8), (1.0, 0.3)];
        let lut = Profile::lut_from_points(&points);
        assert_eq!(lut.len(), Profile::CUSTOM_LUT_SAMPLES);
        let profile = Profile::Custom(lut);
        for (t, expected) in points {
            assert!(
                (profile.sample(t) - expected).abs() < 0.01,
                "t={t}, expected {expected}, got {}",
                profile.sample(t)
            );
        }
        assert!(
            profile.sample(0.5) > 0.65 && profile.sample(0.5) < 0.8,
            "smooth midpoint was {}",
            profile.sample(0.5)
        );
    }

    #[test]
    fn numeric_profiles_generate_non_flat_normals() {
        let mut mask = Grid::new(64, 64, false);
        for y in 8..56 {
            for x in 8..56 {
                mask.set(x, y, true);
            }
        }
        for profile in [
            Profile::Linear,
            Profile::Convex,
            Profile::Concave,
            Profile::Smooth,
        ] {
            let params = BevelParams {
                radius_px: 12.0,
                height_px: 6.0,
                profile: profile.clone(),
                invert: false,
                softness_px: 0.0,
                smooth_px: 0.0,
                edge_aa_px: 0.0,
            };
            let normal = *bevel_mask(&mask, &params, Wrap::Clamp).get(10, 32);
            assert!(
                normal.truncate().length() > 0.01,
                "{profile:?} produced a flat bevel"
            );
        }
    }

    #[test]
    fn repeat_wrap_finishes_a_bevel_at_the_seam() {
        // A bar touching the right edge. With Repeat the canvas is periodic,
        // so the distance at the seam comes from the wrapped background at
        // x = 0. Clamp treats the outside as continuing the shape, which is
        // what breaks tileability.
        let mut coverage = Grid::new(32, 8, 0.0);
        for y in 0..8 {
            for x in 28..32 {
                coverage.set(x, y, 1.0);
            }
        }
        let params = BevelParams {
            radius_px: 8.0,
            height_px: 10.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
        };
        let repeat = height_mask_aa(&coverage, &params, Wrap::Repeat);
        let clamp = height_mask_aa(&coverage, &params, Wrap::Clamp);
        assert!(
            *repeat.get(31, 4) < *clamp.get(31, 4),
            "the seam must cut the bevel instead of extending it: repeat {} clamp {}",
            repeat.get(31, 4),
            clamp.get(31, 4)
        );
        // The rest of the bar is untouched: only the seam neighbour changes.
        for x in 24..28 {
            assert!(
                (*repeat.get(x, 4) - *clamp.get(x, 4)).abs() < 1e-5,
                "interior pixel {x} changed with the wrap mode"
            );
        }
    }
}
