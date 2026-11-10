use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::filter::gaussian_blur;
use crate::grid::{Grid, Wrap};

/// Signed Laplacian of a height field. Positive = convex, negative = concave.
pub fn curvature(height: &Grid<f32>, wrap: Wrap) -> Grid<f32> {
    Grid::from_fn(height.w, height.h, |x, y| {
        let h = *height.get(x, y);
        let hx0 = height.sample(x as i32 - 1, y as i32, wrap);
        let hx1 = height.sample(x as i32 + 1, y as i32, wrap);
        let hy0 = height.sample(x as i32, y as i32 - 1, wrap);
        let hy1 = height.sample(x as i32, y as i32 + 1, wrap);
        (hx0 + hx1 + hy0 + hy1 - 4.0 * h) * 0.25
    })
}

/// Concave cavity: how far a pixel sits below its blurred neighborhood.
pub fn cavity(height: &Grid<f32>, radius_px: f32, wrap: Wrap) -> Grid<f32> {
    let blurred = gaussian_blur(height, radius_px.max(1e-3), wrap);
    Grid::from_fn(height.w, height.h, |x, y| {
        (*blurred.get(x, y) - *height.get(x, y)).max(0.0)
    })
}

/// Convex ridge mask: how far a pixel sits above its blurred neighborhood.
pub fn convexity(height: &Grid<f32>, radius_px: f32, wrap: Wrap) -> Grid<f32> {
    let blurred = gaussian_blur(height, radius_px.max(1e-3), wrap);
    Grid::from_fn(height.w, height.h, |x, y| {
        (*height.get(x, y) - *blurred.get(x, y)).max(0.0)
    })
}

/// Cheap multi-direction horizon AO from a height field.
///
/// `height_scale_px` converts the 0–1 height convention to pixel units.
pub fn ao(
    height: &Grid<f32>,
    height_scale_px: f32,
    max_radius_px: f32,
    directions: usize,
    wrap: Wrap,
) -> Grid<f32> {
    let dirs = directions.max(4);
    let radius = max_radius_px.max(1.0);
    let steps = 16i32;

    Grid::from_fn_par(height.w, height.h, |x, y| {
        let h0 = *height.get(x, y) * height_scale_px;
        let mut occlusion = 0.0;

        for d in 0..dirs {
            let angle = std::f32::consts::TAU * d as f32 / dirs as f32;
            let dir = Vec2::new(angle.cos(), angle.sin());
            let mut max_slope = 0.0f32;

            for step in 1..=steps {
                let r = radius * step as f32 / steps as f32;
                let h = height.sample(
                    (x as f32 + dir.x * r).round() as i32,
                    (y as f32 + dir.y * r).round() as i32,
                    wrap,
                ) * height_scale_px;
                let slope = (h - h0) / r.max(0.5);
                max_slope = max_slope.max(slope);
            }

            occlusion += max_slope.max(0.0).atan() / std::f32::consts::FRAC_PI_2;
        }

        (1.0 - occlusion / dirs as f32).clamp(0.0, 1.0)
    })
}

/// Binary edge mask from positive curvature.
pub fn edge_mask(height: &Grid<f32>, threshold: f32, wrap: Wrap) -> Grid<f32> {
    let c = curvature(height, wrap);
    c.map(|v| if *v > threshold { 1.0 } else { 0.0 })
}

/// Signed curvature of the evaluated normal field.
///
/// This is the divergence of the tangent-space normal XY in document space:
/// positive values are convex ridges, negative values are concave creases. It
/// matches the exported normal map (including masks and clipping), unlike a
/// curvature derived from analytical per-layer heights.
pub fn normal_curvature(normals: &Grid<Vec3>, wrap: Wrap) -> Grid<f32> {
    Grid::from_fn_par(normals.w, normals.h, |x, y| {
        let left = normals.sample(x as i32 - 1, y as i32, wrap);
        let right = normals.sample(x as i32 + 1, y as i32, wrap);
        let up = normals.sample(x as i32, y as i32 - 1, wrap);
        let down = normals.sample(x as i32, y as i32 + 1, wrap);
        // The normal field is canonical OpenGL (+Y up) while the grid rows
        // increase downward, so the vertical derivative uses up - down.
        (right.x - left.x) * 0.5 + (up.y - down.y) * 0.5
    })
}

/// Encodes signed curvature into a 0.5-centred grayscale map (flat = 0.5).
/// The map is scaled to its strongest edge so the exported contrast is useful
/// regardless of document size or bevel width.
pub fn encode_curvature(curvature: &Grid<f32>) -> Grid<f32> {
    let scale = curvature_scale(curvature);
    let inv = 0.5 / scale;
    curvature.map(|value| (0.5 + value * inv).clamp(0.0, 1.0))
}

/// Convex edge-wear mask: white on outward ridges, black elsewhere.
pub fn edge_wear(curvature: &Grid<f32>) -> Grid<f32> {
    edge_wear_with(curvature, &EdgeWearParams::default())
}

/// Tunable edge-wear mask. Curvature is normalized to the strongest edge, so
/// the parameters are resolution independent.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct EdgeWearParams {
    /// Normalized curvature where wear starts, 0..1.
    pub threshold: f32,
    /// Width of the ramp above the threshold.
    pub softness: f32,
    /// Output multiplier, 0..1 keeps the mask usable as a blend amount.
    pub strength: f32,
}

impl Default for EdgeWearParams {
    fn default() -> Self {
        Self {
            threshold: 0.12,
            softness: 0.78,
            strength: 1.0,
        }
    }
}

pub fn edge_wear_with(curvature: &Grid<f32>, params: &EdgeWearParams) -> Grid<f32> {
    let scale = curvature_scale(curvature);
    let high = params.threshold + params.softness.max(1e-3);
    let strength = params.strength.clamp(0.0, 1.0);
    curvature.map(|value| {
        (smoothstep(params.threshold, high, (value / scale).clamp(0.0, 1.0)) * strength)
            .clamp(0.0, 1.0)
    })
}

/// Edge-detect mask: white on convex and concave creases.
pub fn edge_detect(curvature: &Grid<f32>) -> Grid<f32> {
    edge_detect_with(curvature, &EdgeDetectParams::default())
}

/// Tunable edge-detect mask; `threshold` and `softness` act on the absolute
/// normalized curvature, so convex and concave creases are treated alike.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct EdgeDetectParams {
    pub threshold: f32,
    pub softness: f32,
    pub strength: f32,
}

impl Default for EdgeDetectParams {
    fn default() -> Self {
        Self {
            threshold: 0.08,
            softness: 0.27,
            strength: 1.0,
        }
    }
}

pub fn edge_detect_with(curvature: &Grid<f32>, params: &EdgeDetectParams) -> Grid<f32> {
    let scale = curvature_scale(curvature);
    let high = params.threshold + params.softness.max(1e-3);
    let strength = params.strength.clamp(0.0, 1.0);
    curvature.map(|value| {
        (smoothstep(
            params.threshold,
            high,
            (value.abs() / scale).clamp(0.0, 1.0),
        ) * strength)
            .clamp(0.0, 1.0)
    })
}

fn curvature_scale(curvature: &Grid<f32>) -> f32 {
    curvature
        .iter()
        .fold(0.0f32, |scale, value| scale.max(value.abs()))
        .max(1e-6)
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_height_has_no_curvature_or_cavity() {
        let h = Grid::new(32, 32, 0.5);
        let c = curvature(&h, Wrap::Clamp);
        let cav = cavity(&h, 3.0, Wrap::Clamp);
        for v in c.iter() {
            assert!(v.abs() < 1e-6);
        }
        for v in cav.iter() {
            assert!(v.abs() < 1e-6);
        }
    }

    #[test]
    fn pit_has_cavity() {
        let mut h = Grid::new(32, 32, 1.0);
        h.set(16, 16, 0.0);
        let cav = cavity(&h, 2.0, Wrap::Clamp);
        assert!(*cav.get(16, 16) > 0.0);
    }

    #[test]
    fn flat_height_is_fully_lit() {
        let h = Grid::new(16, 16, 0.5);
        let a = ao(&h, 100.0, 8.0, 8, Wrap::Clamp);
        for v in a.iter() {
            assert!((*v - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn flat_normals_have_no_curvature_or_edges() {
        let normals = Grid::new(16, 16, Vec3::Z);
        let c = normal_curvature(&normals, Wrap::Clamp);
        let encoded = encode_curvature(&c);
        let wear = edge_wear(&c);
        let edges = edge_detect(&c);
        for (value, center) in c.iter().zip(encoded.iter()) {
            assert!(value.abs() < 1e-6);
            assert!((*center - 0.5).abs() < 1e-6);
        }
        assert!(wear.iter().all(|v| *v < 1e-6));
        assert!(edges.iter().all(|v| *v < 1e-6));
    }

    #[test]
    fn convex_normal_ridge_produces_edge_wear() {
        // A horizontal ridge: normal X swings from negative to positive, while
        // normal Y points up on the upper row and down on the lower row.
        let mut normals = Grid::new(32, 32, Vec3::Z);
        for y in 0..32 {
            for x in 0..32 {
                let dx = (x as f32 - 16.0) / 8.0;
                let dy = -(y as f32 - 16.0) / 8.0;
                let n = Vec3::new(dx * 0.12, dy * 0.12, 1.0).normalize();
                normals.set(x, y, n);
            }
        }
        let c = normal_curvature(&normals, Wrap::Clamp);
        let wear = edge_wear(&c);
        let edges = edge_detect(&c);
        assert!(c.iter().any(|v| *v > 1e-4));
        assert!(wear.iter().any(|v| *v > 0.1));
        assert!(edges.iter().any(|v| *v > 0.1));
    }

    #[test]
    fn edge_wear_parameters_scale_and_threshold() {
        let mut c = Grid::new(8, 8, 0.0);
        c.set(4, 4, 1.0);
        c.set(2, 2, 0.5);
        let full = edge_wear_with(&c, &EdgeWearParams::default());
        let half = edge_wear_with(
            &c,
            &EdgeWearParams {
                strength: 0.5,
                ..Default::default()
            },
        );
        assert!((*half.get(4, 4) - *full.get(4, 4) * 0.5).abs() < 1e-5);

        // Raising the threshold above the normalized peak removes the mask.
        let none = edge_wear_with(
            &c,
            &EdgeWearParams {
                threshold: 1.0,
                ..Default::default()
            },
        );
        assert!(none.iter().all(|v| *v < 1e-5));

        // A narrower ramp makes the mid-value pixel fully worn sooner.
        let wide = *edge_wear_with(&c, &EdgeWearParams::default()).get(2, 2);
        let narrow = *edge_wear_with(
            &c,
            &EdgeWearParams {
                softness: 0.1,
                ..Default::default()
            },
        )
        .get(2, 2);
        assert!(narrow > wide, "narrow ramp {narrow} vs wide {wide}");
    }

    #[test]
    fn edge_detect_parameters_scale_both_signs() {
        let mut c = Grid::new(8, 8, 0.0);
        c.set(4, 4, 1.0);
        c.set(2, 2, -0.5);
        let full = edge_detect_with(&c, &EdgeDetectParams::default());
        // Concave creases are detected with the same strength as convex ones.
        let convex = *full.get(4, 4);
        let mut flipped = Grid::new(8, 8, 0.0);
        flipped.set(4, 4, -1.0);
        let concave = *edge_detect_with(&flipped, &EdgeDetectParams::default()).get(4, 4);
        assert!((convex - concave).abs() < 1e-5);

        let half = edge_detect_with(
            &c,
            &EdgeDetectParams {
                strength: 0.5,
                ..Default::default()
            },
        );
        assert!((*half.get(4, 4) - convex * 0.5).abs() < 1e-5);
    }
}
