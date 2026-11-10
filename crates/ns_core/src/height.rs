use std::f32::consts::TAU;

use glam::Vec3;
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::grid::{Grid, Wrap};

/// Convert a height field to tangent-space normals.
///
/// `flip_y` selects DirectX-style green (Y-). The result is canonical for the
/// caller: there is no hidden convention conversion beyond this flag.
pub fn height_to_normal(h: &Grid<f32>, strength: f32, wrap: Wrap, flip_y: bool) -> Grid<Vec3> {
    Grid::from_fn_par(h.w, h.h, |x, y| {
        let s = |dx: i32, dy: i32| h.sample(x as i32 + dx, y as i32 + dy, wrap);

        // Scharr derivatives. The /32 gives height units per pixel.
        let dx = (3.0 * (s(1, -1) - s(-1, -1))
            + 10.0 * (s(1, 0) - s(-1, 0))
            + 3.0 * (s(1, 1) - s(-1, 1)))
            / 32.0;

        let dy = (3.0 * (s(-1, 1) - s(-1, -1))
            + 10.0 * (s(0, 1) - s(0, -1))
            + 3.0 * (s(1, 1) - s(1, -1)))
            / 32.0;

        let sy = if flip_y { -strength } else { strength };
        Vec3::new(-strength * dx, sy * dy, 1.0).normalize()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_height_is_flat_normal() {
        let h = Grid::new(32, 32, 0.5);
        let n = height_to_normal(&h, 1.0, Wrap::Clamp, false);
        for v in n.iter() {
            assert!((*v - Vec3::Z).length() < 1e-5);
        }
    }

    #[test]
    fn rising_height_tilts_normal_left() {
        let h = Grid::from_fn(32, 32, |x, _| x as f32 / 31.0);
        let n = height_to_normal(&h, 4.0, Wrap::Clamp, false);
        let center = *n.get(16, 16);
        assert!(center.x < -0.1, "expected negative X, got {center:?}");
        assert!(center.y.abs() < 1e-5);
        assert!(center.z > 0.9);
        assert!((center.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn directx_flips_green_slope() {
        let h = Grid::from_fn(32, 32, |_, y| y as f32 / 31.0);
        let gl = height_to_normal(&h, 1.0, Wrap::Clamp, false);
        let dx = height_to_normal(&h, 1.0, Wrap::Clamp, true);
        let a = *gl.get(16, 16);
        let b = *dx.get(16, 16);
        assert!((a.y + b.y).abs() < 1e-5);
        assert!((a.x - b.x).abs() < 1e-5);
    }

    #[test]
    fn wrap_is_seamless() {
        let h = Grid::from_fn(64, 64, |x, y| {
            (x as f32 * std::f32::consts::TAU / 64.0).sin()
                * (y as f32 * std::f32::consts::TAU / 64.0).cos()
        });
        let n = height_to_normal(&h, 1.0, Wrap::Repeat, false);
        for y in 0..64 {
            let left = *n.get(0, y);
            let right = *n.get(63, y);
            assert!((left - right).length() < 0.2);
        }
    }

    #[test]
    fn repeat_scharr_uses_the_wrapped_neighbour() {
        let w = 64usize;
        let h = Grid::from_fn(w as u32, 4, |x, _| {
            (std::f32::consts::TAU * x as f32 / w as f32).sin()
        });
        let repeat = height_to_normal(&h, 1.0, Wrap::Repeat, false);
        let clamp = height_to_normal(&h, 1.0, Wrap::Clamp, false);
        // At x = 0 the wrapped stencil sees h(63) = -sin, while the clamped
        // stencil repeats h(0) = 0, so the periodic slope is twice as steep.
        let ratio = repeat.get(0, 2).x / clamp.get(0, 2).x;
        assert!(
            (ratio - 2.0).abs() < 0.05,
            "wrapped slope should be twice the clamped slope, got {ratio}"
        );
    }
}

/// Options for the approximate normal→height integration.
#[derive(Clone, Debug)]
pub struct NormalToHeightOptions {
    /// Periodic input can be integrated directly; clamped input is mirror-padded.
    pub wrap: Wrap,
    /// Suppress frequencies below this many cycles per pixel (0 disables).
    pub high_pass: f32,
    /// Percentile used as the output black point.
    pub percentile_low: f32,
    /// Percentile used as the output white point.
    pub percentile_high: f32,
}

impl Default for NormalToHeightOptions {
    fn default() -> Self {
        Self {
            wrap: Wrap::Clamp,
            high_pass: 0.0,
            percentile_low: 0.0005,
            percentile_high: 0.9995,
        }
    }
}

/// Integrate a normal field into an approximate height field.
///
/// Normals are treated as a gradient field and integrated with the
/// Frankot–Chellappa least-squares method in the frequency domain. The result
/// is normalized to 0–1 with percentile clipping; this is an approximation
/// because many height fields share nearly identical normals.
pub fn normal_to_height(normal: &Grid<Vec3>, options: &NormalToHeightOptions) -> Grid<f32> {
    let w = normal.w as usize;
    let h = normal.h as usize;
    if w == 0 || h == 0 {
        return Grid::new(normal.w, normal.h, 0.5);
    }

    // Mirror padding makes clamped images periodic. Mirroring the height also
    // flips the derivative along the mirrored axis, so do the same to slopes.
    let (pw, ph) = match options.wrap {
        Wrap::Repeat => (w, h),
        Wrap::Clamp => (w * 2, h * 2),
    };

    let mut p = vec![Complex::new(0.0, 0.0); pw * ph];
    let mut q = vec![Complex::new(0.0, 0.0); pw * ph];
    for y in 0..ph {
        let (sy, flip_y) = source_index(y, h, options.wrap);
        for x in 0..pw {
            let (sx, flip_x) = source_index(x, w, options.wrap);
            let n = *normal.get(sx as u32, sy as u32);
            let n = if n.is_finite() { n } else { Vec3::Z };
            let nz = n.z.max(1e-4);
            let mut dhdx = -n.x / nz;
            let mut dhdy = n.y / nz;
            if flip_x {
                dhdx = -dhdx;
            }
            if flip_y {
                dhdy = -dhdy;
            }
            let i = y * pw + x;
            p[i] = Complex::new(dhdx, 0.0);
            q[i] = Complex::new(dhdy, 0.0);
        }
    }

    let mut planner = FftPlanner::<f32>::new();
    let fwd_x = planner.plan_fft_forward(pw);
    let fwd_y = planner.plan_fft_forward(ph);
    let inv_x = planner.plan_fft_inverse(pw);
    let inv_y = planner.plan_fft_inverse(ph);
    fft_2d(&mut p, pw, ph, fwd_x.as_ref(), fwd_y.as_ref());
    fft_2d(&mut q, pw, ph, fwd_x.as_ref(), fwd_y.as_ref());

    let mut hhat = vec![Complex::new(0.0, 0.0); pw * ph];
    let cutoff = options.high_pass.max(0.0) * TAU;
    for y in 0..ph {
        let wy = omega(y, ph);
        for x in 0..pw {
            let wx = omega(x, pw);
            let denom = wx * wx + wy * wy;
            if denom <= 1e-12 {
                continue;
            }

            let i = y * pw + x;
            // H = (-i·wx·P - i·wy·Q) / (wx² + wy²)
            let mut value = Complex::new(0.0, -wx) * p[i] + Complex::new(0.0, -wy) * q[i];
            value /= denom;
            if cutoff > 0.0 {
                value *= denom / (denom + cutoff * cutoff);
            }
            hhat[i] = value;
        }
    }

    fft_2d(&mut hhat, pw, ph, inv_x.as_ref(), inv_y.as_ref());

    let inv_area = 1.0 / (pw * ph) as f32;
    let mut out = Grid::from_fn(normal.w, normal.h, |x, y| {
        hhat[y as usize * pw + x as usize].re * inv_area
    });
    normalize_percentile(&mut out, options.percentile_low, options.percentile_high);
    out
}

fn source_index(i: usize, n: usize, wrap: Wrap) -> (usize, bool) {
    match wrap {
        Wrap::Repeat => (i % n, false),
        Wrap::Clamp => {
            if i < n {
                (i, false)
            } else {
                (2 * n - 1 - i, true)
            }
        }
    }
}

/// Signed angular frequency for DFT index `k`.
fn omega(k: usize, n: usize) -> f32 {
    let k = k as i32;
    let n = n as i32;
    let signed = if k > n / 2 { k - n } else { k };
    TAU * signed as f32 / n as f32
}

fn fft_2d(
    data: &mut [Complex<f32>],
    w: usize,
    h: usize,
    row_fft: &dyn Fft<f32>,
    col_fft: &dyn Fft<f32>,
) {
    for row in data.chunks_exact_mut(w) {
        row_fft.process(row);
    }

    let mut column = vec![Complex::new(0.0, 0.0); h];
    for x in 0..w {
        for y in 0..h {
            column[y] = data[y * w + x];
        }
        col_fft.process(&mut column);
        for y in 0..h {
            data[y * w + x] = column[y];
        }
    }
}

fn normalize_percentile(height: &mut Grid<f32>, low: f32, high: f32) {
    let mut values: Vec<f32> = height
        .data
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if values.is_empty() {
        height.data.fill(0.5);
        return;
    }
    values.sort_by(|a, b| a.total_cmp(b));

    let last = values.len() - 1;
    let lo = values[((last as f32) * low.clamp(0.0, 1.0)).round() as usize];
    let hi = values[((last as f32) * high.clamp(0.0, 1.0)).round() as usize];
    if hi - lo < 1e-6 {
        height.data.fill(0.5);
        return;
    }
    for v in &mut height.data {
        *v = if v.is_finite() {
            ((*v - lo) / (hi - lo)).clamp(0.0, 1.0)
        } else {
            0.5
        };
    }
}

#[cfg(test)]
mod normal_to_height_tests {
    use super::*;

    fn correlation(a: &Grid<f32>, b: &Grid<f32>) -> f32 {
        let n = a.data.len() as f32;
        let ma = a.data.iter().sum::<f32>() / n;
        let mb = b.data.iter().sum::<f32>() / n;
        let mut cov = 0.0;
        let mut va = 0.0;
        let mut vb = 0.0;
        for (x, y) in a.data.iter().zip(b.data.iter()) {
            let da = x - ma;
            let db = y - mb;
            cov += da * db;
            va += da * da;
            vb += db * db;
        }
        if va < 1e-12 || vb < 1e-12 {
            return 0.0;
        }
        cov / (va.sqrt() * vb.sqrt())
    }

    #[test]
    fn flat_normals_integrate_to_flat_height() {
        let normals = Grid::new(16, 16, Vec3::Z);
        let height = normal_to_height(&normals, &NormalToHeightOptions::default());
        for v in height.iter() {
            assert!((*v - 0.5).abs() < 1e-5);
        }
    }

    #[test]
    fn periodic_round_trip_recovers_smooth_height() {
        let n = 64usize;
        let source = Grid::from_fn(n as u32, n as u32, |x, y| {
            let fx = x as f32 / n as f32 * TAU;
            let fy = y as f32 / n as f32 * TAU;
            fx.sin() * fy.cos()
        });
        let normals = height_to_normal(&source, 2.0, Wrap::Repeat, false);
        let options = NormalToHeightOptions {
            wrap: Wrap::Repeat,
            percentile_low: 0.0,
            percentile_high: 1.0,
            ..Default::default()
        };
        let recovered = normal_to_height(&normals, &options);
        let corr = correlation(&source, &recovered);
        assert!(corr > 0.99, "correlation was {corr}");
    }

    #[test]
    fn clamped_round_trip_recovers_gaussian_bump() {
        let source = Grid::from_fn(48, 48, |x, y| {
            let dx = (x as f32 - 24.0) / 9.0;
            let dy = (y as f32 - 24.0) / 9.0;
            (-(dx * dx + dy * dy)).exp()
        });
        let normals = height_to_normal(&source, 1.5, Wrap::Clamp, false);
        let options = NormalToHeightOptions {
            wrap: Wrap::Clamp,
            percentile_low: 0.0,
            percentile_high: 1.0,
            ..Default::default()
        };
        let recovered = normal_to_height(&normals, &options);
        let corr = correlation(&source, &recovered);
        assert!(corr > 0.95, "correlation was {corr}");
    }
}
