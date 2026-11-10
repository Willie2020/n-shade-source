//! Input preparation and lighting flattening (Prep + Delit).

use std::sync::atomic::{AtomicBool, Ordering};

use glam::Vec3;
use ns_core::{Grid, Wrap};
use rayon::prelude::*;

use crate::pixels::{srgb_decode, PbrImage};
use crate::types::PbrSettings;
use crate::util::{fast_blur, mean, percentile};
use crate::{PbrError, Result};

#[derive(Clone, Debug)]
pub struct PrepOutput {
    pub image: PbrImage,
    pub luma: Grid<f32>,
}

impl PrepOutput {
    pub fn width(&self) -> u32 {
        self.image.w
    }

    pub fn height(&self) -> u32 {
        self.image.h
    }
}

#[derive(Clone, Debug)]
pub struct DelitOutput {
    pub image: PbrImage,
    pub luma: Grid<f32>,
}

/// Decode-level preparation: resize to the working resolution and optionally
/// denoise. Periodicity is handled by passing [`Wrap::Repeat`] to every later
/// kernel rather than by altering the image itself.
pub fn prepare(
    source: &PbrImage,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<PrepOutput> {
    check_cancel(cancel)?;
    let (width, height) = settings.quality.working_size(source.w, source.h);
    let mut image = source.resized(width, height);
    check_cancel(cancel)?;
    if settings.denoise {
        image = bilateral_denoise(&image, settings.seamless);
    }
    check_cancel(cancel)?;
    let luma = image.luma_grid();
    Ok(PrepOutput { image, luma })
}

/// Flatten low-frequency lighting and recover a plausible base colour.
pub fn delit(
    prep: &PrepOutput,
    settings: &PbrSettings,
    cancel: &AtomicBool,
) -> Result<DelitOutput> {
    check_cancel(cancel)?;
    let wrap = settings.wrap();
    let short = prep.image.w.min(prep.image.h) as f32;
    let sigma = (short * settings.delit.sigma_fraction).max(0.75);
    let low = fast_blur(&prep.luma, sigma, wrap);
    check_cancel(cancel)?;
    let mean_low = mean(&low).max(1e-5);

    let mut image = PbrImage::new(prep.image.w, prep.image.h, Vec3::ZERO);
    let data: Vec<Vec3> = prep
        .image
        .data
        .par_iter()
        .enumerate()
        .map(|(index, color)| {
            let low_value = low.data[index].max(1e-5);
            (*color / low_value * mean_low).min(Vec3::splat(8.0))
        })
        .collect();
    image.data = data;

    // Soft-clamp baked speculars without flattening the whole image.
    let highlight =
        percentile(&image.luma_grid().data, settings.delit.highlight_percentile).max(1e-4);
    for color in &mut image.data {
        *color = Vec3::new(
            soft_clamp(color.x, highlight),
            soft_clamp(color.y, highlight),
            soft_clamp(color.z, highlight),
        )
        .clamp(Vec3::ZERO, Vec3::splat(1.0));
    }
    check_cancel(cancel)?;
    let luma = image.luma_grid();
    Ok(DelitOutput { image, luma })
}

fn soft_clamp(value: f32, knee: f32) -> f32 {
    if value <= knee {
        value
    } else {
        let excess = value - knee;
        knee + excess / (1.0 + excess / knee.max(1e-4))
    }
}

fn bilateral_denoise(image: &PbrImage, seamless: bool) -> PbrImage {
    const RADIUS: i32 = 2;
    const SPACE_SIGMA: f32 = 1.5;
    const RANGE_SIGMA: f32 = 0.06;
    let wrap = if seamless { Wrap::Repeat } else { Wrap::Clamp };
    let luma = image.luma_grid();
    let spatial: Vec<(i32, i32, f32)> = (-RADIUS..=RADIUS)
        .flat_map(|dy| {
            (-RADIUS..=RADIUS).map(move |dx| {
                let weight =
                    (-(dx * dx + dy * dy) as f32 / (2.0 * SPACE_SIGMA * SPACE_SIGMA)).exp();
                (dx, dy, weight)
            })
        })
        .collect();

    let data: Vec<Vec3> = (0..image.h)
        .into_par_iter()
        .map(|y| {
            (0..image.w)
                .map(|x| {
                    let center = luma.sample(x as i32, y as i32, wrap);
                    let mut sum = Vec3::ZERO;
                    let mut total = 0.0;
                    for &(dx, dy, space_weight) in &spatial {
                        let sx = wrap.coord(x as i32 + dx, image.w);
                        let sy = wrap.coord(y as i32 + dy, image.h);
                        let sample = image.get(sx, sy);
                        let sample_luma = 0.2126 * sample.x + 0.7152 * sample.y + 0.0722 * sample.z;
                        let difference = sample_luma - center;
                        let range_weight =
                            (-(difference * difference) / (2.0 * RANGE_SIGMA * RANGE_SIGMA)).exp();
                        let weight = space_weight * range_weight;
                        sum += sample * weight;
                        total += weight;
                    }
                    if total > 0.0 {
                        sum / total
                    } else {
                        image.get(x, y)
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flatten()
        .collect();
    PbrImage::from_vec(image.w, image.h, data)
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(PbrError::Cancelled)
    } else {
        Ok(())
    }
}

/// Utility for tests and for callers that already have sRGB values.
pub fn decode_srgb(color: [f32; 3]) -> Vec3 {
    Vec3::new(
        srgb_decode(color[0]),
        srgb_decode(color[1]),
        srgb_decode(color[2]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn constant_image_delights_to_constant() {
        let source = PbrImage::new(64, 64, Vec3::splat(0.3));
        let settings = PbrSettings::default();
        let cancel = AtomicBool::new(false);
        let prep = prepare(&source, &settings, &cancel).unwrap();
        let output = delit(&prep, &settings, &cancel).unwrap();
        for color in output.image.data {
            assert!((color.x - 0.3).abs() < 1e-3);
        }
    }

    #[test]
    fn prepare_respects_quality_size() {
        let source = PbrImage::new(2048, 1024, Vec3::splat(0.5));
        let settings = PbrSettings {
            quality: crate::Quality::Draft,
            ..Default::default()
        };
        let prep = prepare(&source, &settings, &AtomicBool::new(false)).unwrap();
        assert_eq!((prep.image.w, prep.image.h), (512, 256));
    }
}
