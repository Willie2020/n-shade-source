//! Shared numeric helpers for the PBR stages.

use ns_core::filter::gaussian_blur;
use ns_core::{Grid, Wrap};

#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if (edge1 - edge0).abs() < 1e-8 {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn percentile(values: &[f32], p: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return 0.0;
    }
    sorted.sort_by(|a, b| a.total_cmp(b));
    let index = ((sorted.len() - 1) as f32 * p.clamp(0.0, 1.0)).round() as usize;
    sorted[index]
}

pub fn normalize_percentile(grid: &mut Grid<f32>, low: f32, high: f32) {
    let lo = percentile(&grid.data, low);
    let hi = percentile(&grid.data, high);
    if hi - lo < 1e-6 {
        grid.data.fill(0.5);
        return;
    }
    for value in &mut grid.data {
        *value = if value.is_finite() {
            ((*value - lo) / (hi - lo)).clamp(0.0, 1.0)
        } else {
            0.5
        };
    }
}

pub fn normalize_abs_percentile(grid: &mut Grid<f32>, p: f32) {
    let scale = percentile(
        &grid
            .data
            .iter()
            .map(|value| value.abs())
            .collect::<Vec<_>>(),
        p,
    )
    .max(1e-6);
    for value in &mut grid.data {
        *value = (*value / scale).clamp(-1.0, 1.0);
    }
}

pub fn mean(grid: &Grid<f32>) -> f32 {
    if grid.data.is_empty() {
        return 0.0;
    }
    grid.data.iter().sum::<f32>() / grid.data.len() as f32
}

pub fn median(grid: &Grid<f32>) -> f32 {
    percentile(&grid.data, 0.5)
}

/// Gaussian blur for low-frequency kernels. Large sigmas are evaluated on a
/// downsampled copy and interpolated back; the result is visually identical
/// for illumination/height drift but avoids hundreds of taps per pixel.
pub fn fast_blur(grid: &Grid<f32>, sigma: f32, wrap: Wrap) -> Grid<f32> {
    if sigma < 4.0 || grid.w.min(grid.h) < 128 {
        return gaussian_blur(grid, sigma, wrap);
    }
    let factor = ((sigma / 4.0).round() as u32).clamp(2, 8);
    if factor <= 1 {
        return gaussian_blur(grid, sigma, wrap);
    }
    let small_w = (grid.w / factor).max(1);
    let small_h = (grid.h / factor).max(1);
    let small = downsample_area(grid, small_w, small_h);
    let blurred = gaussian_blur(&small, (sigma / factor as f32).max(0.5), wrap);
    let scale_x = grid.w as f32 / small_w as f32;
    let scale_y = grid.h as f32 / small_h as f32;
    Grid::from_fn_par(grid.w, grid.h, |x, y| {
        let u = (x as f32 + 0.5) / scale_x - 0.5;
        let v = (y as f32 + 0.5) / scale_y - 0.5;
        sample_bilinear(&blurred, u, v, wrap)
    })
}

fn downsample_area(grid: &Grid<f32>, dst_w: u32, dst_h: u32) -> Grid<f32> {
    let scale_x = grid.w as f32 / dst_w as f32;
    let scale_y = grid.h as f32 / dst_h as f32;
    Grid::from_fn_par(dst_w, dst_h, |x, y| {
        let x0 = (x as f32 * scale_x).floor() as u32;
        let x1 = (((x + 1) as f32 * scale_x).ceil() as u32).min(grid.w);
        let y0 = (y as f32 * scale_y).floor() as u32;
        let y1 = (((y + 1) as f32 * scale_y).ceil() as u32).min(grid.h);
        let mut sum = 0.0;
        let mut count = 0.0;
        for sy in y0..y1.max(y0 + 1) {
            for sx in x0..x1.max(x0 + 1) {
                sum += *grid.get(sx.min(grid.w - 1), sy.min(grid.h - 1));
                count += 1.0;
            }
        }
        if count > 0.0 {
            sum / count
        } else {
            0.0
        }
    })
}

/// Bilinear sample with the requested edge behavior.
pub fn sample_bilinear(grid: &Grid<f32>, x: f32, y: f32, wrap: ns_core::Wrap) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let x0 = x0 as i32;
    let y0 = y0 as i32;
    let a = grid.sample(x0, y0, wrap);
    let b = grid.sample(x0 + 1, y0, wrap);
    let c = grid.sample(x0, y0 + 1, wrap);
    let d = grid.sample(x0 + 1, y0 + 1, wrap);
    let top = a + (b - a) * fx;
    let bottom = c + (d - c) * fx;
    top + (bottom - top) * fy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_and_smoothstep() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.5), 3.0);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
    }

    #[test]
    fn bilinear_hits_pixel_centers() {
        let grid = Grid::from_vec(2, 2, vec![0.0, 1.0, 2.0, 3.0]);
        assert!((sample_bilinear(&grid, 0.0, 0.0, ns_core::Wrap::Clamp) - 0.0).abs() < 1e-6);
        assert!((sample_bilinear(&grid, 1.0, 1.0, ns_core::Wrap::Clamp) - 3.0).abs() < 1e-6);
        assert!((sample_bilinear(&grid, 0.5, 0.5, ns_core::Wrap::Clamp) - 1.5).abs() < 1e-6);
    }
}
