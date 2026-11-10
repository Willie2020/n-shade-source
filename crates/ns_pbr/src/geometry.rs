//! Height from luminance bands and tangent-space normals.

use std::sync::atomic::{AtomicBool, Ordering};

use glam::Vec3;
use ns_core::{Grid, Wrap};
use rayon::prelude::*;

use crate::prep::DelitOutput;
use crate::types::PbrSettings;
use crate::util::{fast_blur, normalize_percentile};
use crate::{PbrError, Result};

#[derive(Clone, Debug)]
pub struct GeometryOutput {
    pub height: Grid<f32>,
    pub normal: Grid<Vec3>,
}

pub fn geometry(
    delit: &DelitOutput,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<GeometryOutput> {
    let height = height_from_luma(&delit.luma, settings, cancel)?;
    let normal = normals_from_height(&height, settings, settings.wrap());
    Ok(GeometryOutput { height, normal })
}

/// Classical luminance-band height. This is the fallback used when no ML
/// normal backend is available (the default shipping path).
pub fn height_from_luma(
    luma: &Grid<f32>,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<Grid<f32>> {
    check_cancel(cancel)?;
    let wrap = settings.wrap();
    let short = luma.w.min(luma.h) as f32;
    let scale = (short / 1024.0).max(0.25);
    let sigmas: [f32; 6] = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
    let mut blurs = Vec::with_capacity(sigmas.len());
    for sigma in sigmas {
        check_cancel(cancel)?;
        blurs.push(fast_blur(luma, (sigma * scale).max(0.5), wrap));
    }

    let weights = settings.height.band_weights;
    let weight_sum: f32 = weights.iter().sum();
    let weight_sum = if weight_sum.abs() < 1e-5 {
        1.0
    } else {
        weight_sum
    };

    let mut height = Grid::new(luma.w, luma.h, 0.0);
    height
        .data
        .par_iter_mut()
        .enumerate()
        .for_each(|(index, value)| {
            let fine = luma.data[index] - blurs[0].data[index];
            let mut sum = weights[0] * fine;
            for band in 0..5 {
                sum += weights[band + 1] * (blurs[band].data[index] - blurs[band + 1].data[index]);
            }
            *value = sum / weight_sum;
        });
    check_cancel(cancel)?;

    normalize_percentile(&mut height, 0.005, 0.995);

    if settings.height.drift_removal > 0.0 {
        let low = fast_blur(&height, (short * 0.25).max(2.0), wrap);
        let amount = settings.height.drift_removal.clamp(0.0, 1.0);
        for (value, low) in height.data.iter_mut().zip(low.data) {
            *value -= amount * low;
        }
        normalize_percentile(&mut height, 0.005, 0.995);
    }

    if settings.height.invert {
        for value in &mut height.data {
            *value = 1.0 - *value;
        }
    }
    for value in &mut height.data {
        *value = value.clamp(0.0, 1.0);
    }
    Ok(height)
}

pub fn normals_from_height(height: &Grid<f32>, settings: &PbrSettings, wrap: Wrap) -> Grid<Vec3> {
    let strength = settings.height.height_scale_px.max(0.01) * settings.normal.strength.max(0.0);
    ns_core::height::height_to_normal(height, strength, wrap, false)
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
    use crate::pixels::PbrImage;
    use crate::prep::{delit, prepare};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn flat_input_has_flat_height_and_normals() {
        let source = PbrImage::new(64, 64, Vec3::splat(0.5));
        let settings = PbrSettings::default();
        let cancel = AtomicBool::new(false);
        let prep = prepare(&source, &settings, &cancel).unwrap();
        let delit = delit(&prep, &settings, &cancel).unwrap();
        let geometry = geometry(&delit, &settings, &cancel).unwrap();
        for value in geometry.height.data {
            assert!((value - 0.5).abs() < 1e-3);
        }
        for normal in geometry.normal.data {
            assert!((normal - Vec3::Z).length() < 1e-3);
        }
    }

    #[test]
    fn luminance_ramp_produces_unit_normals() {
        let luma = Grid::from_fn(64, 64, |x, _| x as f32 / 63.0);
        let settings = PbrSettings::default();
        let height = height_from_luma(&luma, &settings, &AtomicBool::new(false)).unwrap();
        let normals = normals_from_height(&height, &settings, Wrap::Clamp);
        let center = *normals.get(32, 32);
        assert!(center.z > 0.0);
        assert!((center.length() - 1.0).abs() < 1e-4);
    }
}
