//! Displacement, horizon AO, multi-scale curvature/cavity and edge wear.

use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};

use ns_core::filter::gaussian_blur;
use ns_core::{Grid, Wrap};
use rayon::prelude::*;

use crate::noise::{interleaved_gradient_noise, tileable_fbm};
use crate::types::PbrSettings;
use crate::util::{fast_blur, lerp, median, normalize_abs_percentile, sample_bilinear, smoothstep};
use crate::{PbrError, Result};

#[derive(Clone, Debug)]
pub struct DisplacementOutput {
    pub map: Grid<f32>,
    pub midlevel: f32,
    pub amount_m: f32,
}

#[derive(Clone, Debug)]
pub struct CurvatureOutput {
    /// Signed, scale-normalized curvature: positive = convex.
    pub values: Grid<f32>,
    /// Display/export map with 0.5 = flat.
    pub map: Grid<f32>,
}

pub fn displacement(
    height: &Grid<f32>,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<DisplacementOutput> {
    check_cancel(cancel)?;
    let wrap = settings.wrap();
    let short = height.w.min(height.h) as f32;
    let mut map = height.clone();
    if settings.displacement.drift_removal > 0.0 {
        let low = fast_blur(&map, (short * 0.12).max(2.0), wrap);
        let amount = settings.displacement.drift_removal.clamp(0.0, 1.0);
        for (value, low) in map.data.iter_mut().zip(low.data) {
            *value -= amount * low;
        }
    }
    let center = median(&map);
    for value in &mut map.data {
        *value = (*value - center + settings.displacement.midlevel).clamp(0.0, 1.0);
    }
    check_cancel(cancel)?;
    Ok(DisplacementOutput {
        map,
        midlevel: settings.displacement.midlevel,
        amount_m: settings.displacement.amount_m,
    })
}

/// Multi-scale curvature.
pub fn curvature(
    height: &Grid<f32>,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<CurvatureOutput> {
    check_cancel(cancel)?;
    let wrap = settings.wrap();
    let short = height.w.min(height.h) as f32;
    let scale = (short / 1024.0).max(0.25);
    let mut values = Grid::new(height.w, height.h, 0.0);

    for (sigma, weight) in settings
        .curvature
        .scales
        .iter()
        .zip(settings.curvature.weights.iter())
    {
        check_cancel(cancel)?;
        let sigma = (*sigma * scale).max(0.5);
        let blur = gaussian_blur(height, sigma, wrap);
        let laplacian = laplacian(&blur, wrap);
        let factor = -weight * sigma * sigma;
        values
            .data
            .par_iter_mut()
            .zip(laplacian.data.par_iter())
            .for_each(|(value, lap)| {
                *value += factor * *lap;
            });
    }
    normalize_abs_percentile(&mut values, 0.99);

    let mut map = Grid::new(height.w, height.h, 0.0);
    map.data
        .par_iter_mut()
        .zip(values.data.par_iter())
        .for_each(|(out, value)| {
            *out = (0.5 + settings.curvature.gain * value).clamp(0.0, 1.0);
        });
    check_cancel(cancel)?;
    Ok(CurvatureOutput { values, map })
}

/// Concave cavity mask derived from normalized curvature.
pub fn cavity(values: &Grid<f32>, settings: &PbrSettings) -> Grid<f32> {
    let mut cavity = Grid::new(values.w, values.h, 0.0);
    let lo = settings.cavity.lo;
    let hi = settings.cavity.hi;
    let invert = settings.cavity.invert;
    cavity
        .data
        .par_iter_mut()
        .zip(values.data.par_iter())
        .for_each(|(out, value)| {
            let concave = smoothstep(lo, hi, (-*value).max(0.0));
            *out = if invert { 1.0 - concave } else { concave };
        });
    cavity
}

/// Convenience wrapper used by tests and callers that want both at once.
pub fn curvature_cavity(
    height: &Grid<f32>,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<(CurvatureOutput, Grid<f32>)> {
    let curvature = curvature(height, settings, cancel)?;
    let cavity = cavity(&curvature.values, settings);
    Ok((curvature, cavity))
}

/// Horizon AO for a single radius. One thread per row; cancellation is
/// checked between rows.
#[allow(clippy::too_many_arguments)]
pub fn heightfield_ao(
    height: &Grid<f32>,
    height_scale_px: f32,
    radius_px: f32,
    directions: usize,
    steps: usize,
    strength: f32,
    wrap: Wrap,
    cancel: &AtomicBool,
) -> Result<Grid<f32>> {
    let dirs = directions.max(4);
    let steps = steps.max(2);
    let radius = radius_px.max(0.5);
    let mut out = Grid::new(height.w, height.h, 1.0);
    let width = height.w as usize;
    let result =
        out.data
            .par_chunks_mut(width)
            .enumerate()
            .try_for_each(|(y, row)| -> Result<()> {
                check_cancel(cancel)?;
                for (x, slot) in row.iter_mut().enumerate() {
                    let h0 = *height.get(x as u32, y as u32) * height_scale_px;
                    let p0 = (x as f32, y as f32);
                    let rotation = interleaved_gradient_noise(x as u32, y as u32);
                    let mut occlusion = 0.0;
                    for direction in 0..dirs {
                        let angle = (direction as f32 + rotation) / dirs as f32 * TAU;
                        let dir = (angle.cos(), angle.sin());
                        let mut best = 0.0f32;
                        for step in 1..=steps {
                            let t = step as f32 / steps as f32;
                            let r = radius * t * t;
                            let sample =
                                sample_bilinear(height, p0.0 + dir.0 * r, p0.1 + dir.1 * r, wrap)
                                    * height_scale_px;
                            let dh = sample - h0;
                            let sin_e = dh / (dh * dh + r * r).sqrt();
                            best = best.max(sin_e * (1.0 - t * t));
                        }
                        occlusion += best;
                    }
                    *slot = (1.0 - strength * occlusion / dirs as f32).clamp(0.0, 1.0);
                }
                Ok(())
            });
    result?;
    Ok(out)
}

/// Two-radius horizon AO with a depth-aware 3x3 cleanup blur.
pub fn ao_maps(
    height: &Grid<f32>,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<Grid<f32>> {
    let wrap = settings.wrap();
    let dirs = settings
        .quality
        .ao_directions()
        .unwrap_or(settings.ao.directions.max(4));
    let steps = settings.quality.ao_steps();
    let strength = settings.ao.strength.clamp(0.0, 2.0);
    if strength <= 1e-4 {
        return Ok(Grid::new(height.w, height.h, 1.0));
    }
    let small = heightfield_ao(
        height,
        settings.height.height_scale_px,
        settings.ao.small_radius_px,
        dirs,
        steps,
        strength,
        wrap,
        cancel,
    )?;
    check_cancel(cancel)?;
    let large = heightfield_ao(
        height,
        settings.height.height_scale_px,
        settings.ao.large_radius_px,
        dirs,
        steps,
        strength,
        wrap,
        cancel,
    )?;
    check_cancel(cancel)?;
    let mut combined = Grid::new(height.w, height.h, 1.0);
    combined
        .data
        .par_iter_mut()
        .zip(small.data.par_iter())
        .zip(large.data.par_iter())
        .for_each(|((out, small), large)| {
            *out = small * large;
        });
    let blurred = bilateral_blur3x3(&combined, height, settings.ao.blur_range, wrap);
    check_cancel(cancel)?;
    Ok(blurred)
}

fn bilateral_blur3x3(
    source: &Grid<f32>,
    guide: &Grid<f32>,
    range_sigma: f32,
    wrap: Wrap,
) -> Grid<f32> {
    let range_sigma = range_sigma.max(1e-4);
    Grid::from_fn_par(source.w, source.h, |x, y| {
        let center = *source.get(x, y);
        let guide_center = *guide.get(x, y);
        let mut sum = 0.0;
        let mut total = 0.0;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let sample = source.sample(x as i32 + dx, y as i32 + dy, wrap);
                let guide_sample = guide.sample(x as i32 + dx, y as i32 + dy, wrap);
                let spatial = (-(dx * dx + dy * dy) as f32 / 2.0).exp();
                let difference = guide_sample - guide_center;
                let range = (-(difference * difference) / (2.0 * range_sigma * range_sigma)).exp();
                let weight = spatial * range;
                sum += sample * weight;
                total += weight;
            }
        }
        if total > 0.0 {
            (sum / total).clamp(0.0, 1.0)
        } else {
            center
        }
    })
}

pub fn edge_wear(
    curvature: &CurvatureOutput,
    ao: &Grid<f32>,
    height: &Grid<f32>,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<Grid<f32>> {
    check_cancel(cancel)?;
    let wrap = settings.wrap();
    let short = height.w.min(height.h) as f32;
    let scale = (short / 1024.0).max(0.25);
    let filtered = if settings.edge_wear.width_px > 0.01 {
        gaussian_blur(
            &curvature.values,
            (settings.edge_wear.width_px * scale).max(0.5),
            wrap,
        )
    } else {
        curvature.values.clone()
    };
    check_cancel(cancel)?;

    let width = height.w as usize;
    let mut output = Grid::new(height.w, height.h, 0.0);
    output
        .data
        .par_chunks_mut(width)
        .enumerate()
        .try_for_each(|(y, row)| -> Result<()> {
            check_cancel(cancel)?;
            let v = (y as f32 + 0.5) / height.h as f32;
            for (x, slot) in row.iter_mut().enumerate() {
                let u = (x as f32 + 0.5) / height.w as f32;
                let convex = filtered.get(x as u32, y as u32).max(0.0);
                let edge = smoothstep(settings.edge_wear.lo, settings.edge_wear.hi, convex);
                let noise = tileable_fbm(
                    u,
                    v,
                    settings.edge_wear.noise_frequency,
                    settings.edge_wear.noise_octaves,
                    settings.edge_wear.seed,
                );
                let c = (settings.edge_wear.contrast * 0.5).clamp(0.0, 0.49);
                let breakup = smoothstep(0.5 - c, 0.5 + c, noise);
                let exposure = lerp(
                    1.0,
                    *ao.get(x as u32, y as u32),
                    settings.edge_wear.ao_influence,
                );
                let mut wear = edge * lerp(1.0, breakup, settings.edge_wear.breakup) * exposure
                    + settings.edge_wear.height_bias * height.get(x as u32, y as u32);
                if settings.edge_wear.scratch_amount > 0.0 {
                    let scratch = tileable_fbm(
                        u * 0.35,
                        v * 3.0,
                        settings.edge_wear.noise_frequency,
                        settings.edge_wear.noise_octaves,
                        settings.edge_wear.seed.wrapping_add(17),
                    );
                    wear *= lerp(
                        1.0,
                        scratch,
                        settings.edge_wear.scratch_amount.clamp(0.0, 1.0),
                    );
                }
                *slot = (wear * settings.edge_wear.intensity).clamp(0.0, 1.0);
            }
            Ok(())
        })?;
    Ok(output)
}

fn laplacian(height: &Grid<f32>, wrap: Wrap) -> Grid<f32> {
    Grid::from_fn_par(height.w, height.h, |x, y| {
        let h = *height.get(x, y);
        let left = height.sample(x as i32 - 1, y as i32, wrap);
        let right = height.sample(x as i32 + 1, y as i32, wrap);
        let up = height.sample(x as i32, y as i32 - 1, wrap);
        let down = height.sample(x as i32, y as i32 + 1, wrap);
        (left + right + up + down - 4.0 * h) * 0.25
    })
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(PbrError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Quality;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn flat_height_is_lit_and_flat() {
        let height = Grid::new(32, 32, 0.5);
        let cancel = AtomicBool::new(false);
        let ao = heightfield_ao(&height, 8.0, 8.0, 8, 6, 1.0, Wrap::Clamp, &cancel).unwrap();
        for value in ao.data {
            assert!((value - 1.0).abs() < 1e-4);
        }
        let settings = PbrSettings::default();
        let (curvature, cavity) = curvature_cavity(&height, &settings, &cancel).unwrap();
        for value in curvature.values.data {
            assert!(value.abs() < 1e-4);
        }
        for value in cavity.data {
            assert!(value.abs() < 1e-4);
        }
    }

    #[test]
    fn pit_creates_cavity() {
        let mut height = Grid::new(32, 32, 0.8);
        height.set(16, 16, 0.2);
        let settings = PbrSettings::default();
        let (_, cavity) = curvature_cavity(&height, &settings, &AtomicBool::new(false)).unwrap();
        assert!(*cavity.get(16, 16) > 0.0);
    }

    #[test]
    fn ao_quality_modes_are_finite() {
        let mut height = Grid::new(24, 24, 0.5);
        height.set(12, 12, 0.9);
        for quality in Quality::ALL {
            let settings = PbrSettings {
                quality,
                ..Default::default()
            };
            let ao = ao_maps(&height, &settings, &AtomicBool::new(false)).unwrap();
            assert!(ao.data.iter().all(|value| value.is_finite()));
        }
    }
}
