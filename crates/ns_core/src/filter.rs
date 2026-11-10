use crate::grid::{Grid, Wrap};

/// Separable Gaussian blur for scalar fields.
pub fn gaussian_blur(src: &Grid<f32>, sigma: f32, wrap: Wrap) -> Grid<f32> {
    if sigma <= 1e-4 {
        return src.clone();
    }

    let radius = (sigma * 3.0).ceil().max(1.0) as i32;
    let kernel = gaussian_kernel(sigma, radius);
    let tmp = convolve_1d(src, &kernel, radius, true, wrap);
    convolve_1d(&tmp, &kernel, radius, false, wrap)
}

fn gaussian_kernel(sigma: f32, radius: i32) -> Vec<f32> {
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut sum = 0.0;
    for i in -radius..=radius {
        let x = i as f32;
        let w = (-0.5 * (x / sigma).powi(2)).exp();
        kernel.push(w);
        sum += w;
    }
    if sum > 0.0 {
        for w in &mut kernel {
            *w /= sum;
        }
    }
    kernel
}

fn convolve_1d(
    src: &Grid<f32>,
    kernel: &[f32],
    radius: i32,
    horizontal: bool,
    wrap: Wrap,
) -> Grid<f32> {
    Grid::from_fn_par(src.w, src.h, |x, y| {
        let mut sum = 0.0;
        for (i, weight) in kernel.iter().enumerate() {
            let offset = i as i32 - radius;
            let (sx, sy) = if horizontal {
                (x as i32 + offset, y as i32)
            } else {
                (x as i32, y as i32 + offset)
            };
            sum += src.sample(sx, sy, wrap) * weight;
        }
        sum
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_image_stays_constant() {
        let g = Grid::new(32, 32, 0.7);
        let b = gaussian_blur(&g, 3.0, Wrap::Clamp);
        for v in b.iter() {
            assert!((*v - 0.7).abs() < 1e-5);
        }
    }

    #[test]
    fn impulse_spreads_symmetrically() {
        let mut g = Grid::new(33, 33, 0.0);
        g.set(16, 16, 1.0);
        let b = gaussian_blur(&g, 2.0, Wrap::Clamp);
        let center = *b.get(16, 16);
        assert!(center > 0.0 && center < 1.0);
        // A symmetric kernel makes mirrored samples equal, not neighbours.
        assert!((*b.get(15, 16) - *b.get(17, 16)).abs() < 1e-5);
        assert!((*b.get(16, 15) - *b.get(16, 17)).abs() < 1e-5);
        assert!((*b.get(10, 16) - *b.get(22, 16)).abs() < 1e-5);
    }

    #[test]
    fn wrap_blur_is_seamless() {
        let mut g = Grid::new(64, 64, 0.0);
        g.set(0, 32, 1.0);
        let b = gaussian_blur(&g, 3.0, Wrap::Repeat);
        assert!((*b.get(0, 32) - *b.get(63, 32)).abs() < 1e-3);
    }

    #[test]
    fn repeat_blur_wraps_the_far_corner_back_to_the_seed() {
        let mut g = Grid::new(16, 16, 0.0);
        g.set(0, 0, 1.0);
        let b = gaussian_blur(&g, 1.5, Wrap::Repeat);
        // (1,1) and (15,15) sit the same distance from the seed through the
        // seam, so a periodic blur must give them identical values.
        assert!((*b.get(1, 1) - *b.get(15, 15)).abs() < 1e-5);
        assert!((*b.get(0, 1) - *b.get(0, 15)).abs() < 1e-5);
        let clamped = gaussian_blur(&g, 1.5, Wrap::Clamp);
        assert!(
            *clamped.get(15, 15) < 1e-9,
            "clamping must not pull the far corner in"
        );
    }
}
