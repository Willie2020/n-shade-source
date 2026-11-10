//! Equirectangular environments and importance sampling.

use glam::Vec3;

use crate::mesh::normalize_or;
use crate::{BakeError, Result};

use super::trace::Rng;
/// Linear equirectangular environment map, top row first.
#[derive(Clone, Debug)]
pub struct EnvironmentMap {
    pub width: u32,
    pub height: u32,
    /// Tightly packed linear RGB radiance, `width * height` entries.
    pub pixels: Vec<[f32; 3]>,
}

impl EnvironmentMap {
    pub fn new(width: u32, height: u32, pixels: Vec<[f32; 3]>) -> Result<Self> {
        if width == 0 || height == 0 || pixels.len() != width as usize * height as usize {
            return Err(BakeError::Message(
                "The HDR environment has invalid dimensions".into(),
            ));
        }
        let pixels = pixels
            .into_iter()
            .map(|pixel| [sanitize(pixel[0]), sanitize(pixel[1]), sanitize(pixel[2])])
            .collect();
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Bilinear equirectangular lookup. `direction` points from the surface
    /// toward the environment.
    pub fn radiance(&self, direction: Vec3) -> Vec3 {
        let direction = normalize_or(direction, Vec3::Y);
        let v = direction.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
        let u = direction.z.atan2(direction.x) / std::f32::consts::TAU;
        self.sample_uv(u, v)
    }

    /// Luminance-weighted average radiance over the whole sphere.
    ///
    /// Texels are weighted by their solid angle (`sin(theta)`), so small bright
    /// features near the poles do not count more than they should. This is the
    /// brightness reference for the automatic fill that keeps surfaces facing
    /// away from an environment's bright regions lit.
    pub fn average_radiance(&self) -> f32 {
        let mut weighted = 0.0f64;
        let mut weight = 0.0f64;
        for y in 0..self.height {
            let theta = (y as f64 + 0.5) * std::f64::consts::PI / self.height.max(1) as f64;
            let row_weight = theta.sin();
            if row_weight <= 0.0 {
                continue;
            }
            let mut row_sum = 0.0f64;
            for x in 0..self.width {
                let pixel = self.pixels[y as usize * self.width as usize + x as usize];
                row_sum += f64::from(luminance(Vec3::from_array(pixel)).max(0.0));
            }
            weighted += row_sum * row_weight;
            weight += f64::from(self.width) * row_weight;
        }
        if weight > 0.0 {
            (weighted / weight) as f32
        } else {
            0.0
        }
    }

    /// Bilinear lookup by normalized equirectangular coordinates. `u` wraps;
    /// `v` clamps at the poles.
    pub fn sample_uv(&self, u: f32, v: f32) -> Vec3 {
        let u = u - u.floor();
        let v = v.clamp(0.0, 1.0);
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        let x0 = x.floor() as i64;
        let y0 = y.floor() as i64;
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        let sample = |ix: i64, iy: i64| {
            let column = ix.rem_euclid(self.width as i64) as usize;
            let row = iy.clamp(0, self.height as i64 - 1) as usize;
            Vec3::from_array(self.pixels[row * self.width as usize + column])
        };
        let top = sample(x0, y0) * (1.0 - fx) + sample(x0 + 1, y0) * fx;
        let bottom = sample(x0, y0 + 1) * (1.0 - fx) + sample(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

fn sanitize(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

fn luminance(value: Vec3) -> f32 {
    value.x * 0.2126 + value.y * 0.7152 + value.z * 0.0722
}

pub(crate) struct EnvSampler {
    width: u32,
    height: u32,
    radiance: Vec<Vec3>,
    rows: Vec<f32>,
    cols: Vec<f32>,
    total: f32,
    /// v2 evaluates the same bilinear radiance for NEE and escape rays, so the
    /// MIS weights describe one estimator.
    bilinear: bool,
}

/// A box-filtered, solid-angle-weighted copy of an equirectangular map.
pub(crate) struct SampledEnvironment {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) radiance: Vec<Vec3>,
}

/// Row and column cumulative distributions used for importance sampling.
pub(crate) struct EnvironmentCdf {
    pub(crate) rows: Vec<f32>,
    pub(crate) cols: Vec<f32>,
    pub(crate) total: f32,
}

/// Downsample an environment without losing small, bright features such as the
/// sun. Each output texel is the solid-angle-weighted average of the input
/// texels that map to it; point sampling would miss or alias a small sun disk.
pub(crate) fn sample_environment(
    environment: &EnvironmentMap,
    max_width: u32,
) -> SampledEnvironment {
    let width = environment.width.clamp(1, max_width.max(1));
    let height = ((environment.height as f32 * width as f32 / environment.width as f32).round()
        as u32)
        .clamp(1, max_width.max(1));
    let count = width as usize * height as usize;
    let mut split = vec![Vec3::ZERO; count];
    let mut weights = vec![0.0f32; count];
    for y in 0..environment.height {
        let theta = (y as f32 + 0.5) * std::f32::consts::PI / environment.height as f32;
        let row_weight = theta.sin().max(0.0);
        let out_y = ((y as u64 * height as u64) / environment.height as u64) as u32;
        for x in 0..environment.width {
            let out_x = ((x as u64 * width as u64) / environment.width as u64) as u32;
            let index = out_y as usize * width as usize + out_x as usize;
            let pixel = environment.pixels[y as usize * environment.width as usize + x as usize];
            split[index] += Vec3::from_array(pixel) * row_weight;
            weights[index] += row_weight;
        }
    }
    let mut radiance = Vec::with_capacity(count);
    for (index, weight) in weights.into_iter().enumerate() {
        if weight > 1e-12 {
            radiance.push(split[index] / weight);
        } else {
            let u = ((index % width as usize) as f32 + 0.5) / width as f32;
            let v = ((index / width as usize) as f32 + 0.5) / height as f32;
            radiance.push(environment.sample_uv(u, v));
        }
    }
    SampledEnvironment {
        width,
        height,
        radiance,
    }
}

/// Build per-row and per-row-column cumulative distributions.
pub(crate) fn build_cdf(radiance: &[Vec3], width: u32, height: u32) -> EnvironmentCdf {
    let width = width.max(1);
    let height = height.max(1);
    let mut cols = vec![0.0; width as usize * height as usize];
    let mut rows = vec![0.0; height as usize];
    let mut total = 0.0f32;
    for y in 0..height {
        let theta = (y as f32 + 0.5) * std::f32::consts::PI / height as f32;
        let solid_angle = theta.sin().max(0.0)
            * (std::f32::consts::PI / height as f32)
            * (std::f32::consts::TAU / width as f32);
        let mut accumulated = 0.0;
        for x in 0..width {
            let index = y as usize * width as usize + x as usize;
            let value = radiance.get(index).copied().unwrap_or(Vec3::ZERO);
            accumulated += luminance(value).max(0.0) * solid_angle;
            cols[index] = accumulated;
        }
        total += accumulated;
        rows[y as usize] = total;
    }
    EnvironmentCdf { rows, cols, total }
}
impl EnvSampler {
    pub(crate) fn new(environment: &EnvironmentMap, bilinear: bool) -> Option<Self> {
        let sampled = sample_environment(environment, 2048);
        let cdf = build_cdf(&sampled.radiance, sampled.width, sampled.height);
        if cdf.total <= 1e-8 || !cdf.total.is_finite() {
            return None;
        }
        Some(Self {
            width: sampled.width,
            height: sampled.height,
            radiance: sampled.radiance,
            rows: cdf.rows,
            cols: cdf.cols,
            total: cdf.total,
            bilinear,
        })
    }

    /// Bilinear lookup over the downsampled radiance array. `u` wraps, `v`
    /// clamps at the poles.
    fn radiance_at(&self, direction: Vec3) -> Vec3 {
        let direction = normalize_or(direction, Vec3::Y);
        let v = direction.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
        let u = direction.z.atan2(direction.x) / std::f32::consts::TAU;
        let u = u - u.floor();
        let v = v.clamp(0.0, 1.0);
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        let x0 = x.floor() as i64;
        let y0 = y.floor() as i64;
        let tx = x - x0 as f32;
        let ty = y - y0 as f32;
        let sample = |ix: i64, iy: i64| {
            let column = ix.rem_euclid(self.width as i64) as usize;
            let row = iy.clamp(0, self.height as i64 - 1) as usize;
            self.radiance[row * self.width as usize + column]
        };
        let top = sample(x0, y0).lerp(sample(x0 + 1, y0), tx);
        let bottom = sample(x0, y0 + 1).lerp(sample(x0 + 1, y0 + 1), tx);
        top.lerp(bottom, ty)
    }

    pub(crate) fn sample(&self, rng: &mut Rng) -> (Vec3, Vec3, f32) {
        let u = [rng.next_f32(), rng.next_f32()];
        let jitter = [rng.next_f32(), rng.next_f32()];
        self.sample_uvs(u, jitter)
    }

    /// v2 sample from an explicit 2D pair, so the same Sobol point drives the
    /// CPU and GPU. `jitter` picks the position inside the chosen texel.
    pub(crate) fn sample_pair(&self, u: [f32; 2], jitter: [f32; 2]) -> (Vec3, Vec3, f32) {
        self.sample_uvs(u, jitter)
    }

    fn sample_uvs(&self, u: [f32; 2], jitter: [f32; 2]) -> (Vec3, Vec3, f32) {
        let target = (u[0].max(1e-7)) * self.total;
        let row = self
            .rows
            .partition_point(|value| *value < target)
            .min(self.height as usize - 1);
        let row_start = if row == 0 { 0.0 } else { self.rows[row - 1] };
        let row_total = (self.rows[row] - row_start).max(1e-20);
        let target = (u[1].max(1e-7)) * row_total;
        let start = row * self.width as usize;
        let end = start + self.width as usize;
        let column = self.cols[start..end]
            .partition_point(|value| *value < target)
            .min(self.width as usize - 1);
        let index = start + column;
        let u = (column as f32 + jitter[0]) / self.width as f32;
        let v = (row as f32 + jitter[1]) / self.height as f32;
        let theta = v * std::f32::consts::PI;
        let phi = u * std::f32::consts::TAU;
        let sin_theta = theta.sin();
        let direction = Vec3::new(sin_theta * phi.cos(), theta.cos(), sin_theta * phi.sin());
        let direction = direction.normalize_or_zero();
        let texel = self.radiance[index];
        let value = if self.bilinear {
            self.radiance_at(direction)
        } else {
            texel
        };
        let pdf = (luminance(texel) / self.total).max(1e-8);
        (direction, value, pdf)
    }

    pub(crate) fn eval(&self, direction: Vec3) -> (Vec3, f32) {
        let direction = normalize_or(direction, Vec3::Y);
        let v = direction.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
        let u = direction.z.atan2(direction.x) / std::f32::consts::TAU;
        let u = u - u.floor();
        let x = ((u * self.width as f32) as usize).min(self.width as usize - 1);
        let y = ((v * self.height as f32) as usize).min(self.height as usize - 1);
        let texel = self.radiance[y * self.width as usize + x];
        let value = if self.bilinear {
            self.radiance_at(direction)
        } else {
            texel
        };
        (value, (luminance(texel) / self.total).max(1e-8))
    }
}
