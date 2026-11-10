//! Appearance priors: roughness heuristic and metallic modes.

use std::sync::atomic::{AtomicBool, Ordering};

use ns_core::filter::gaussian_blur;
use ns_core::Grid;
use rayon::prelude::*;

use crate::geometry::GeometryOutput;
use crate::prep::DelitOutput;
use crate::types::{MetallicMode, PbrSettings, RoughnessMode};
use crate::util::{normalize_percentile, smoothstep};
use crate::{PbrError, Result};

pub fn roughness(
    delit: &DelitOutput,
    geometry: &GeometryOutput,
    cavity: Option<&Grid<f32>>,
    wear: Option<&Grid<f32>>,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<Grid<f32>> {
    check_cancel(cancel)?;
    let (lo, hi, base, _) = settings.roughness.class.range();
    if settings.roughness.mode == RoughnessMode::Constant {
        return Ok(Grid::new(
            geometry.height.w,
            geometry.height.h,
            settings.roughness.constant.clamp(0.0, 1.0),
        ));
    }

    let wrap = settings.wrap();
    let short = geometry.height.w.min(geometry.height.h) as f32;
    let scale = (short / 1024.0).max(0.25);
    let fine = gaussian_blur(&delit.luma, (2.0 * scale).max(0.5), wrap);
    let mut detail = Grid::new(delit.luma.w, delit.luma.h, 0.0);
    detail
        .data
        .par_iter_mut()
        .zip(delit.luma.data.par_iter())
        .zip(fine.data.par_iter())
        .for_each(|((out, luma), fine)| {
            *out = (luma - fine).abs();
        });
    let detail = gaussian_blur(&detail, (4.0 * scale).max(1.0), wrap);
    let mut detail = detail;
    normalize_percentile(&mut detail, 0.005, 0.995);
    check_cancel(cancel)?;

    let k_hf = settings.roughness.k_hf;
    let k_cav = if settings.roughness.use_cavity {
        settings.roughness.k_cav
    } else {
        0.0
    };
    let k_wear = if settings.roughness.use_wear {
        settings.roughness.k_wear
    } else {
        0.0
    };
    let cavity_default = Grid::new(delit.luma.w, delit.luma.h, 0.0);
    let wear_default = Grid::new(delit.luma.w, delit.luma.h, 0.0);
    let cavity = cavity.unwrap_or(&cavity_default);
    let wear = wear.unwrap_or(&wear_default);

    let mut out = Grid::new(delit.luma.w, delit.luma.h, base);
    out.data
        .par_iter_mut()
        .zip(detail.data.par_iter())
        .zip(cavity.data.par_iter())
        .zip(wear.data.par_iter())
        .for_each(|(((value, detail), cavity), wear)| {
            *value = (base + k_hf * (detail - 0.5) + k_cav * cavity + k_wear * wear).clamp(lo, hi);
        });
    check_cancel(cancel)?;
    Ok(out)
}

pub fn metallic(
    delit: &DelitOutput,
    geometry: &GeometryOutput,
    wear: Option<&Grid<f32>>,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<Grid<f32>> {
    check_cancel(cancel)?;
    match settings.metallic.mode {
        MetallicMode::None => Ok(Grid::new(delit.image.w, delit.image.h, 0.0)),
        MetallicMode::Constant => Ok(Grid::new(
            delit.image.w,
            delit.image.h,
            settings.metallic.constant.clamp(0.0, 1.0),
        )),
        MetallicMode::FromEdgeWear => Ok(wear
            .cloned()
            .unwrap_or_else(|| Grid::new(delit.image.w, delit.image.h, 0.0))),
        MetallicMode::AutoSegment => {
            let short = geometry.height.w.min(geometry.height.h) as f32;
            let scale = (short / 1024.0).max(0.25);
            let wrap = settings.wrap();
            let fine = gaussian_blur(&delit.luma, (2.0 * scale).max(0.5), wrap);
            let mut detail = Grid::new(delit.luma.w, delit.luma.h, 0.0);
            detail
                .data
                .par_iter_mut()
                .zip(delit.luma.data.par_iter())
                .zip(fine.data.par_iter())
                .for_each(|((out, luma), fine)| {
                    *out = (luma - fine).abs();
                });
            let mut detail = gaussian_blur(&detail, (4.0 * scale).max(1.0), wrap);
            normalize_percentile(&mut detail, 0.005, 0.995);

            let mut candidate = Grid::new(delit.image.w, delit.image.h, 0.0);
            candidate
                .data
                .par_iter_mut()
                .enumerate()
                .for_each(|(index, out)| {
                    let color = delit.image.data[index];
                    let max = color.max_element();
                    let min = color.min_element();
                    let saturation = if max > 1e-5 { (max - min) / max } else { 0.0 };
                    let luma = 0.2126 * color.x + 0.7152 * color.y + 0.0722 * color.z;
                    let low_saturation = 1.0 - smoothstep(0.15, 0.45, saturation);
                    let bright = smoothstep(0.02, 0.30, luma);
                    *out = low_saturation * (0.35 + 0.65 * bright) * detail.data[index];
                });
            let blurred = gaussian_blur(&candidate, (3.0 * scale).max(1.0), wrap);
            let threshold = settings.metallic.threshold.clamp(0.0, 1.0);
            let mut out = Grid::new(delit.image.w, delit.image.h, 0.0);
            out.data
                .par_iter_mut()
                .zip(blurred.data.par_iter())
                .for_each(|(out, value)| {
                    *out = smoothstep(threshold - 0.15, threshold + 0.15, *value);
                });
            check_cancel(cancel)?;
            Ok(out)
        }
    }
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
    use crate::geometry::geometry;
    use crate::pixels::PbrImage;
    use crate::prep::{delit, prepare};
    use crate::types::MetallicMode;
    use glam::Vec3;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn roughness_stays_in_class_range() {
        let source = PbrImage::new(64, 64, Vec3::splat(0.5));
        let settings = PbrSettings {
            maps: crate::types::MapToggles {
                roughness: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let cancel = AtomicBool::new(false);
        let prep = prepare(&source, &settings, &cancel).unwrap();
        let delit = delit(&prep, &settings, &cancel).unwrap();
        let geometry = geometry(&delit, &settings, &cancel).unwrap();
        let map = roughness(&delit, &geometry, None, None, &settings, &cancel).unwrap();
        let (lo, hi, _, _) = settings.roughness.class.range();
        assert!(map.data.iter().all(|value| (lo..=hi).contains(value)));
    }

    #[test]
    fn metallic_modes_are_bounded() {
        let source = PbrImage::new(32, 32, Vec3::new(0.5, 0.5, 0.55));
        let mut settings = PbrSettings::default();
        let cancel = AtomicBool::new(false);
        let prep = prepare(&source, &settings, &cancel).unwrap();
        let delit = delit(&prep, &settings, &cancel).unwrap();
        let geometry = geometry(&delit, &settings, &cancel).unwrap();

        settings.metallic.mode = MetallicMode::None;
        let map = metallic(&delit, &geometry, None, &settings, &cancel).unwrap();
        assert!(map.data.iter().all(|value| *value == 0.0));

        settings.metallic.mode = MetallicMode::Constant;
        settings.metallic.constant = 0.75;
        let map = metallic(&delit, &geometry, None, &settings, &cancel).unwrap();
        assert!(map.data.iter().all(|value| (*value - 0.75).abs() < 1e-6));

        settings.metallic.mode = MetallicMode::AutoSegment;
        let map = metallic(&delit, &geometry, None, &settings, &cancel).unwrap();
        assert!(map.data.iter().all(|value| (0.0..=1.0).contains(value)));
    }
}
