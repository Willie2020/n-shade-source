//! Low-discrepancy sampling for the v2 lightmap tracer.
//!
//! The first [`SOBOL_PAIRS`] 2D requests of a path use Owen-scrambled Sobol
//! points. Later requests (deep bounces) fall back to a splitmix64 stream, where
//! stratification no longer pays for itself. Each texel gets an independent
//! scramble, so neighbouring texels stay decorrelated.

use glam::Vec3;
use std::f32::consts::PI;

/// Dimension pairs that use Sobol points. Keep this order stable:
///   pair 0: continuation direction at the vertex
///   pair 1: sun cone sample
///   pair 2: environment next-event sample
///   pair 3: area-light rectangle sample (rotated per light index)
pub(crate) const SOBOL_PAIRS: u32 = 4;

#[inline]
pub(crate) fn hash_u32(mut x: u32) -> u32 {
    // lowbias32
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

#[inline]
fn laine_karras(mut x: u32, seed: u32) -> u32 {
    x = x.wrapping_add(seed);
    x ^= x.wrapping_mul(0x6c50_b47c);
    x ^= x.wrapping_mul(0xb82f_1e52);
    x ^= x.wrapping_mul(0xc7af_e638);
    x ^= x.wrapping_mul(0x8d22_f6e6);
    x
}

/// Nested uniform (Owen) scramble of a 32-bit fixed-point value.
#[inline]
pub(crate) fn owen_scramble(x: u32, seed: u32) -> u32 {
    laine_karras(x.reverse_bits(), seed).reverse_bits()
}

/// Second Sobol dimension (the first is the reversed index).
#[inline]
fn sobol_dim1(mut i: u32) -> u32 {
    let mut v: u32 = 1 << 31;
    let mut r: u32 = 0;
    while i != 0 {
        if i & 1 != 0 {
            r ^= v;
        }
        i >>= 1;
        v ^= v >> 1;
    }
    r
}

#[inline]
fn to_unit(x: u32) -> f32 {
    // 24 high bits -> [0, 1), exactly representable in f32.
    (x >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// Per-texel, per-sample sampler. Create one per path.
pub(crate) struct Sampler {
    seed: u32,
    texel: u32,
    index: u32,
    mask: u32,
    pair: u32,
    fallback: u64,
    low_discrepancy: bool,
}

impl Sampler {
    /// `sample_index` is the global index `0..total_samples`, so the CPU and
    /// GPU agree regardless of how the work is batched.
    pub(crate) fn new(
        seed: u32,
        texel: u32,
        sample_index: u32,
        total_samples: u32,
        low_discrepancy: bool,
    ) -> Self {
        let mask = total_samples.max(1).next_power_of_two() - 1;
        let hi = hash_u32(seed ^ hash_u32(texel)) as u64;
        let lo = hash_u32(sample_index ^ hash_u32(texel ^ 0x5bd1_e995)) as u64;
        Self {
            seed,
            texel,
            index: sample_index,
            mask,
            pair: 0,
            fallback: (hi << 32) | lo,
            low_discrepancy,
        }
    }

    #[inline]
    fn next_fallback(&mut self) -> f32 {
        self.fallback = self.fallback.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.fallback;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^= value >> 31;
        ((value >> 40) as f32) * (1.0 / 16_777_216.0)
    }

    /// Next uniform 1D value (always the fallback stream).
    #[inline]
    pub(crate) fn next_1d(&mut self) -> f32 {
        self.next_fallback()
    }

    /// Next 2D point in `[0,1)²`.
    pub(crate) fn next_2d(&mut self) -> [f32; 2] {
        let pair = self.pair;
        self.pair += 1;
        if !self.low_discrepancy || pair >= SOBOL_PAIRS {
            return [self.next_fallback(), self.next_fallback()];
        }
        let seed = hash_u32(self.seed ^ hash_u32(self.texel) ^ pair.wrapping_mul(0x9e37_79b9));
        // Shuffle the index per pair so pairs are not correlated. Distinct
        // inputs stay distinct after masking, so no sample repeats.
        let index = owen_scramble(self.index, seed ^ 0x68bc_21eb) & self.mask;
        let a = owen_scramble(index.reverse_bits(), hash_u32(seed ^ 0x02e5_be93));
        let b = owen_scramble(sobol_dim1(index), hash_u32(seed ^ 0x967a_889b));
        [to_unit(a), to_unit(b)]
    }
}

// ---------- direction mappings ----------

/// Cosine-weighted hemisphere with +Z as the normal. `pdf = cos(theta) / pi`.
#[inline]
pub(crate) fn cosine_hemisphere(u: f32, v: f32) -> Vec3 {
    let r = u.sqrt();
    let phi = 2.0 * PI * v;
    Vec3::new(r * phi.cos(), r * phi.sin(), (1.0 - u).max(0.0).sqrt())
}

/// Uniform direction inside a cone around +Z with `cos_max = cos(half_angle)`.
#[inline]
pub(crate) fn uniform_cone(u: f32, v: f32, cos_max: f32) -> Vec3 {
    let cos_theta = 1.0 - u * (1.0 - cos_max);
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    let phi = 2.0 * PI * v;
    Vec3::new(sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta)
}

/// Branchless orthonormal basis (Duff et al. 2017).
#[inline]
pub(crate) fn orthonormal_basis(n: Vec3) -> (Vec3, Vec3) {
    let s = 1.0f32.copysign(n.z);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    (
        Vec3::new(1.0 + s * n.x * n.x * a, s * b, -s * n.x),
        Vec3::new(b, s + n.y * n.y * a, -n.y),
    )
}

/// Rotate a local-frame vector (+Z = n) into world space.
#[inline]
pub(crate) fn to_world(local: Vec3, n: Vec3) -> Vec3 {
    let (tangent, bitangent) = orthonormal_basis(n);
    tangent * local.x + bitangent * local.y + n * local.z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_texel_dependent() {
        let mut first = Sampler::new(7, 42, 3, 64, true);
        let mut second = Sampler::new(7, 42, 3, 64, true);
        let mut other_texel = Sampler::new(7, 43, 3, 64, true);
        assert_eq!(first.next_2d(), second.next_2d());
        assert_ne!(first.next_2d(), other_texel.next_2d());
    }

    #[test]
    fn shuffled_indices_do_not_repeat() {
        let mut sampler = Sampler::new(1, 5, 0, 128, true);
        let mut points = Vec::new();
        for _ in 0..64 {
            points.push(sampler.next_2d());
        }
        for pair in 0..SOBOL_PAIRS as usize {
            let slice = &points[pair * 16..(pair + 1) * 16];
            for first in 0..slice.len() {
                for second in first + 1..slice.len() {
                    assert_ne!(slice[first], slice[second], "pair {pair} repeated a point");
                }
            }
        }
    }

    #[test]
    fn samples_cover_the_unit_square() {
        let mut sampler = Sampler::new(3, 11, 0, 256, true);
        let mut sum = [0.0f32; 2];
        for _ in 0..256 {
            let point = sampler.next_2d();
            sum[0] += point[0];
            sum[1] += point[1];
        }
        let mean = [sum[0] / 256.0, sum[1] / 256.0];
        assert!((mean[0] - 0.5).abs() < 0.05, "u mean {}", mean[0]);
        assert!((mean[1] - 0.5).abs() < 0.05, "v mean {}", mean[1]);
    }

    #[test]
    fn cosine_hemisphere_stays_on_the_upper_hemisphere() {
        for step in 0..32 {
            let point = cosine_hemisphere(step as f32 / 32.0, 0.37);
            assert!(point.z >= -1e-6);
            assert!((point.length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn to_world_maps_the_pole_to_the_normal() {
        let normal = Vec3::new(0.3, -0.5, 0.8).normalize();
        let world = to_world(Vec3::Z, normal);
        assert!((world - normal).length() < 1e-5);
    }

    #[test]
    fn sobol_beats_white_noise_on_a_smooth_integral() {
        // Estimate the mean of x*y over the unit square. The Sobol estimate
        // should stay closer to 0.25 than the white-noise fallback.
        let mut sobol_error = 0.0f32;
        let mut white_error = 0.0f32;
        for texel in 0..16 {
            let mut sobol = Sampler::new(1, texel, 0, 64, true);
            let mut white = Sampler::new(2, texel, 0, 64, false);
            let mut sobol_sum = 0.0f32;
            let mut white_sum = 0.0f32;
            for _ in 0..64 {
                let a = sobol.next_2d();
                sobol_sum += a[0] * a[1];
                let b = white.next_2d();
                white_sum += b[0] * b[1];
            }
            sobol_error += (sobol_sum / 64.0 - 0.25).powi(2);
            white_error += (white_sum / 64.0 - 0.25).powi(2);
        }
        assert!(
            sobol_error < white_error,
            "Sobol {sobol_error} did not beat white noise {white_error}"
        );
    }
}
