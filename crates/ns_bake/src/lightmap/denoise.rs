//! Built-in, OpenImageDenoise and OptiX denoising.

use glam::Vec3;
use rayon::prelude::*;

use crate::oidn;

use super::output::{dilate, dilate_v2};
use super::trace::luminance;
use super::{DenoiserUsed, Features, GBuffer, LightmapDenoiser, LightmapSettings, OidnDevice};

pub(crate) fn denoise(
    radiance: &mut [Vec3],
    gbuffer: &GBuffer,
    extent: f32,
    resolution: u32,
    settings: &LightmapSettings,
) -> DenoiserUsed {
    let padding = settings.padding;
    match settings.denoiser {
        LightmapDenoiser::None => DenoiserUsed::None,
        LightmapDenoiser::Auto => {
            if optix_denoise(radiance, gbuffer, resolution, padding).is_ok() {
                DenoiserUsed::Optix
            } else if let Some(used) =
                oidn_denoise(radiance, gbuffer, resolution, settings.oidn_device, padding)
            {
                used
            } else {
                builtin_fallback(radiance, gbuffer, extent, resolution, settings);
                DenoiserUsed::Builtin
            }
        }
        LightmapDenoiser::Oidn => {
            if let Some(used) =
                oidn_denoise(radiance, gbuffer, resolution, settings.oidn_device, padding)
            {
                used
            } else {
                builtin_fallback(radiance, gbuffer, extent, resolution, settings);
                DenoiserUsed::Builtin
            }
        }
        LightmapDenoiser::Builtin => {
            builtin_fallback(radiance, gbuffer, extent, resolution, settings);
            DenoiserUsed::Builtin
        }
        LightmapDenoiser::Optix => {
            if let Err(error) = optix_denoise(radiance, gbuffer, resolution, padding) {
                eprintln!("OptiX denoiser unavailable; using built-in denoiser: {error}");
                builtin_fallback(radiance, gbuffer, extent, resolution, settings);
                DenoiserUsed::Builtin
            } else {
                DenoiserUsed::Optix
            }
        }
    }
}

fn builtin_fallback(
    radiance: &mut [Vec3],
    gbuffer: &GBuffer,
    extent: f32,
    resolution: u32,
    settings: &LightmapSettings,
) {
    builtin_denoise(
        radiance,
        &gbuffer.positions,
        &gbuffer.normals,
        &gbuffer.covered,
        extent,
        resolution,
        settings.denoise_passes,
    );
}

/// v2 built-in filter: variance-guided à-trous with chart and plane stops.
/// OIDN and OptiX keep their v1 code path; only their padded inputs switch to
/// the chart-aware dilation.
pub(crate) fn denoise_v2(
    radiance: &mut [Vec3],
    variance: &mut Vec<f32>,
    gbuffer: &GBuffer,
    extent: f32,
    resolution: u32,
    settings: &LightmapSettings,
) -> DenoiserUsed {
    let features = settings.features();
    let padding = settings.padding;
    match settings.denoiser {
        LightmapDenoiser::None => DenoiserUsed::None,
        LightmapDenoiser::Auto => {
            if optix_denoise_v2(radiance, gbuffer, resolution, padding, &features).is_ok() {
                DenoiserUsed::Optix
            } else if let Some(used) = oidn_denoise_v2(
                radiance,
                gbuffer,
                resolution,
                settings.oidn_device,
                padding,
                &features,
            ) {
                used
            } else {
                builtin_v2(radiance, variance, gbuffer, extent, resolution, settings);
                DenoiserUsed::Builtin
            }
        }
        LightmapDenoiser::Oidn => {
            if let Some(used) = oidn_denoise_v2(
                radiance,
                gbuffer,
                resolution,
                settings.oidn_device,
                padding,
                &features,
            ) {
                used
            } else {
                builtin_v2(radiance, variance, gbuffer, extent, resolution, settings);
                DenoiserUsed::Builtin
            }
        }
        LightmapDenoiser::Builtin => {
            builtin_v2(radiance, variance, gbuffer, extent, resolution, settings);
            DenoiserUsed::Builtin
        }
        LightmapDenoiser::Optix => {
            if let Err(error) = optix_denoise_v2(radiance, gbuffer, resolution, padding, &features)
            {
                eprintln!("OptiX denoiser unavailable; using built-in denoiser: {error}");
                builtin_v2(radiance, variance, gbuffer, extent, resolution, settings);
                DenoiserUsed::Builtin
            } else {
                DenoiserUsed::Optix
            }
        }
    }
}

fn builtin_v2(
    radiance: &mut [Vec3],
    variance: &mut Vec<f32>,
    gbuffer: &GBuffer,
    extent: f32,
    resolution: u32,
    settings: &LightmapSettings,
) {
    let passes = settings.denoise_passes.min(6);
    if passes == 0 {
        return;
    }
    let size = resolution as usize;
    let texel_world = (extent / resolution as f32).max(1e-6);
    // The v1 spike clamp stays in front: it removes isolated fireflies that the
    // luminance term alone would still blur into a blotch.
    suppress_fireflies(
        radiance,
        &gbuffer.positions,
        &gbuffer.normals,
        &gbuffer.covered,
        texel_world,
        resolution,
    );
    if variance.len() != radiance.len() {
        variance.resize(radiance.len(), 0.0);
    }
    if variance.iter().all(|value| *value <= 0.0) {
        // No usable variance (an unusual fallback path): degrade the luminance
        // term to a relative threshold instead of dividing by zero.
        for (value, radiance) in variance.iter_mut().zip(radiance.iter()) {
            let lum = luminance(*radiance);
            *value = 0.0625 * lum * lum;
        }
    }
    atrous_v2(radiance, variance, gbuffer, size, texel_world, passes);
}

fn padded_inputs_v2(
    radiance: &[Vec3],
    gbuffer: &GBuffer,
    resolution: u32,
    padding: u32,
    features: &Features,
) -> (Vec<Vec3>, Vec<Vec3>, Vec<bool>) {
    let mut color = radiance.to_vec();
    let mut covered = gbuffer.covered.clone();
    let mut normals = gbuffer.normals.clone();
    let mut normal_coverage = gbuffer.covered.clone();
    if padding > 0 {
        if features.smooth_dilate {
            let mut color_charts = gbuffer.chart_ids.clone();
            let mut normal_charts = gbuffer.chart_ids.clone();
            dilate_v2(
                &mut color,
                &mut covered,
                &mut color_charts,
                &gbuffer.edge,
                resolution as usize,
                padding,
            );
            dilate_v2(
                &mut normals,
                &mut normal_coverage,
                &mut normal_charts,
                &gbuffer.edge,
                resolution as usize,
                padding,
            );
        } else {
            dilate(&mut color, &mut covered, resolution, padding);
            dilate(&mut normals, &mut normal_coverage, resolution, padding);
        }
    }
    (color, normals, covered)
}

fn optix_denoise_v2(
    radiance: &mut [Vec3],
    gbuffer: &GBuffer,
    resolution: u32,
    padding: u32,
    features: &Features,
) -> Result<(), String> {
    let (mut color, normals, covered) =
        padded_inputs_v2(radiance, gbuffer, resolution, padding, features);
    crate::optix::denoise(&mut color, &normals, &covered, resolution)?;
    for (index, target) in radiance.iter_mut().enumerate() {
        if gbuffer.covered[index] {
            *target = color[index];
        }
    }
    Ok(())
}

fn oidn_denoise_v2(
    radiance: &mut [Vec3],
    gbuffer: &GBuffer,
    resolution: u32,
    device: OidnDevice,
    padding: u32,
    features: &Features,
) -> Option<DenoiserUsed> {
    let texel_count = resolution as usize * resolution as usize;
    if radiance.len() != texel_count {
        return None;
    }
    let (padded, normals, covered) =
        padded_inputs_v2(radiance, gbuffer, resolution, padding, features);
    let mut color = vec![0.0f32; texel_count * 3];
    let mut normal = vec![0.0f32; texel_count * 3];
    let mut albedo = vec![0.0f32; texel_count * 3];
    for index in 0..texel_count {
        if !covered[index] {
            normal[index * 3 + 1] = 1.0;
            continue;
        }
        let value = padded[index];
        color[index * 3] = value.x.max(0.0);
        color[index * 3 + 1] = value.y.max(0.0);
        color[index * 3 + 2] = value.z.max(0.0);
        let guide = normals[index];
        normal[index * 3] = guide.x;
        normal[index * 3 + 1] = guide.y;
        normal[index * 3 + 2] = guide.z;
        albedo[index * 3] = 1.0;
        albedo[index * 3 + 1] = 1.0;
        albedo[index * 3 + 2] = 1.0;
    }
    let (output, used_device) = oidn::denoise(
        &color,
        &normal,
        &albedo,
        resolution,
        resolution,
        device.preference(),
    )?;
    for index in 0..texel_count {
        if !gbuffer.covered[index] {
            continue;
        }
        radiance[index] = Vec3::new(
            output[index * 3].max(0.0),
            output[index * 3 + 1].max(0.0),
            output[index * 3 + 2].max(0.0),
        );
    }
    Some(DenoiserUsed::from_oidn_device(used_device))
}

/// Denoise a padded copy of the lightmap. Screen-space denoisers assume their
/// input is a continuous image; unfilled UV gutters and the black background
/// outside the atlas otherwise bleed into island edges.
fn padded_inputs(
    radiance: &[Vec3],
    gbuffer: &GBuffer,
    resolution: u32,
    padding: u32,
) -> (Vec<Vec3>, Vec<Vec3>, Vec<bool>) {
    let mut color = radiance.to_vec();
    let mut covered = gbuffer.covered.clone();
    let mut normals = gbuffer.normals.clone();
    let mut normal_coverage = gbuffer.covered.clone();
    if padding > 0 {
        dilate(&mut color, &mut covered, resolution, padding);
        dilate(&mut normals, &mut normal_coverage, resolution, padding);
    }
    (color, normals, covered)
}

fn optix_denoise(
    radiance: &mut [Vec3],
    gbuffer: &GBuffer,
    resolution: u32,
    padding: u32,
) -> Result<(), String> {
    let (mut color, normals, covered) = padded_inputs(radiance, gbuffer, resolution, padding);
    crate::optix::denoise(&mut color, &normals, &covered, resolution)?;
    for (index, target) in radiance.iter_mut().enumerate() {
        if gbuffer.covered[index] {
            *target = color[index];
        }
    }
    Ok(())
}

fn oidn_denoise(
    radiance: &mut [Vec3],
    gbuffer: &GBuffer,
    resolution: u32,
    device: OidnDevice,
    padding: u32,
) -> Option<DenoiserUsed> {
    let texel_count = resolution as usize * resolution as usize;
    if radiance.len() != texel_count {
        return None;
    }
    let (padded, normals, covered) = padded_inputs(radiance, gbuffer, resolution, padding);
    let mut color = vec![0.0f32; texel_count * 3];
    let mut normal = vec![0.0f32; texel_count * 3];
    let mut albedo = vec![0.0f32; texel_count * 3];
    for index in 0..texel_count {
        if !covered[index] {
            normal[index * 3 + 1] = 1.0;
            continue;
        }
        let value = padded[index];
        color[index * 3] = value.x.max(0.0);
        color[index * 3 + 1] = value.y.max(0.0);
        color[index * 3 + 2] = value.z.max(0.0);
        let guide = normals[index];
        normal[index * 3] = guide.x;
        normal[index * 3 + 1] = guide.y;
        normal[index * 3 + 2] = guide.z;
        // The lightmap material is a white diffuse surface.
        albedo[index * 3] = 1.0;
        albedo[index * 3 + 1] = 1.0;
        albedo[index * 3 + 2] = 1.0;
    }
    let (output, used_device) = oidn::denoise(
        &color,
        &normal,
        &albedo,
        resolution,
        resolution,
        device.preference(),
    )?;
    for index in 0..texel_count {
        if !gbuffer.covered[index] {
            continue;
        }
        radiance[index] = Vec3::new(
            output[index * 3].max(0.0),
            output[index * 3 + 1].max(0.0),
            output[index * 3 + 2].max(0.0),
        );
    }
    Some(DenoiserUsed::from_oidn_device(used_device))
}

#[allow(clippy::too_many_arguments)]
fn builtin_denoise(
    radiance: &mut [Vec3],
    positions: &[Vec3],
    normals: &[Vec3],
    covered: &[bool],
    extent: f32,
    resolution: u32,
    passes: u32,
) {
    let passes = passes.min(6);
    if passes == 0 {
        return;
    }
    let size = resolution as usize;
    let texel_world = (extent / resolution as f32).max(1e-6);
    suppress_fireflies(
        radiance,
        positions,
        normals,
        covered,
        texel_world,
        resolution,
    );
    let mut source = radiance.to_vec();
    let mut target = vec![Vec3::ZERO; radiance.len()];
    let mut step = 1usize;
    // B3-spline weights. A 5x5 kernel (rather than a 3x3 kernel with a
    // growing step) fills the gaps between samples, so repeated passes do not
    // leave the low-frequency grid-like blotches that a sparse kernel can
    // imprint on smooth surfaces.
    const KERNEL: [f32; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];

    for _ in 0..passes {
        let sigma = texel_world * step as f32 * 2.0;
        let inverse_sigma_squared = 1.0 / (2.0 * sigma * sigma);
        target
            .par_iter_mut()
            .enumerate()
            .for_each(|(index, output)| {
                if !covered[index] {
                    *output = Vec3::ZERO;
                    return;
                }
                let x = index % size;
                let y = index / size;
                let position = positions[index];
                let normal = normals[index];
                let mut sum = Vec3::ZERO;
                let mut weight_sum = 0.0f32;
                for offset_y in -2i32..=2 {
                    for offset_x in -2i32..=2 {
                        let sample_x = x as i32 + offset_x * step as i32;
                        let sample_y = y as i32 + offset_y * step as i32;
                        if sample_x < 0
                            || sample_y < 0
                            || sample_x >= size as i32
                            || sample_y >= size as i32
                        {
                            continue;
                        }
                        let sample_index = sample_y as usize * size + sample_x as usize;
                        if !covered[sample_index] {
                            continue;
                        }
                        let spatial = KERNEL[(offset_x + 2) as usize]
                            * KERNEL[(offset_y + 2) as usize]
                            / 256.0;
                        let dot = normal.dot(normals[sample_index]).max(0.0);
                        let dot2 = dot * dot;
                        let dot4 = dot2 * dot2;
                        let dot8 = dot4 * dot4;
                        let normal_weight = dot8;
                        if normal_weight < 1e-4 {
                            continue;
                        }
                        let distance = position.distance_squared(positions[sample_index]);
                        let position_weight = 1.0 / (1.0 + distance * inverse_sigma_squared);
                        if position_weight < 1e-4 {
                            continue;
                        }
                        let weight = spatial * normal_weight * position_weight;
                        sum += source[sample_index] * weight;
                        weight_sum += weight;
                    }
                }
                *output = if weight_sum > 1e-6 {
                    sum / weight_sum
                } else {
                    source[index]
                };
            });
        std::mem::swap(&mut source, &mut target);
        step *= 2;
    }
    radiance.copy_from_slice(&source);
}

// ---------------------------------------------------------------------------
// v2 variance-guided filter
// ---------------------------------------------------------------------------

const ATROUS_KERNEL: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
const PHI_LUM: f32 = 4.0;
const NORMAL_POWER: f32 = 32.0;

/// 3x3 box filter over the variance of covered texels. The pre-filtered value
/// keeps a single noisy texel from shutting the luminance term down entirely.
fn prefilter_variance(variance: &[f32], covered: &[bool], resolution: usize) -> Vec<f32> {
    let mut output = variance.to_vec();
    output
        .par_chunks_mut(resolution)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, target) in row.iter_mut().enumerate() {
                let index = y * resolution + x;
                if !covered[index] {
                    continue;
                }
                let mut sum = 0.0f32;
                let mut count = 0u32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let sx = x as i32 + dx;
                        let sy = y as i32 + dy;
                        if sx < 0 || sy < 0 || sx >= resolution as i32 || sy >= resolution as i32 {
                            continue;
                        }
                        let sample = sy as usize * resolution + sx as usize;
                        if covered[sample] {
                            sum += variance[sample];
                            count += 1;
                        }
                    }
                }
                if count > 0 {
                    *target = sum / count as f32;
                }
            }
        });
    output
}

/// Chart-aware, variance-guided à-trous filter.
///
/// Weights stop at chart, normal and surface-plane boundaries, and the plane
/// sigma is fixed rather than growing with the step, so neighbouring islands in
/// the same plane cannot bleed into each other.
fn atrous_v2(
    radiance: &mut [Vec3],
    variance: &mut [f32],
    gbuffer: &GBuffer,
    resolution: usize,
    texel_world: f32,
    passes: u32,
) {
    let sigma_plane = 2.0 * texel_world.max(1e-6);
    let mut out_r = radiance.to_vec();
    let mut out_v = variance.to_vec();

    for pass in 0..passes {
        let step = 1i32 << pass;
        let variance_prefiltered = prefilter_variance(variance, &gbuffer.covered, resolution);

        out_r
            .par_chunks_mut(resolution)
            .zip(out_v.par_chunks_mut(resolution))
            .enumerate()
            .for_each(|(y, (row_r, row_v))| {
                for x in 0..resolution {
                    let index = y * resolution + x;
                    if !gbuffer.covered[index] {
                        row_r[x] = radiance[index];
                        row_v[x] = variance[index];
                        continue;
                    }
                    let position = gbuffer.positions[index];
                    let normal = gbuffer.normals[index];
                    let chart = gbuffer.chart_ids[index];
                    let luminance_center = luminance(radiance[index]);
                    let sigma_l = PHI_LUM * variance_prefiltered[index].max(0.0).sqrt() + 1.0e-4;
                    let (mut weight_sum, mut color_sum, mut variance_sum) =
                        (0.0f32, Vec3::ZERO, 0.0f32);

                    for dy in -2i32..=2 {
                        for dx in -2i32..=2 {
                            let sample_x = x as i32 + dx * step;
                            let sample_y = y as i32 + dy * step;
                            if sample_x < 0
                                || sample_y < 0
                                || sample_x >= resolution as i32
                                || sample_y >= resolution as i32
                            {
                                continue;
                            }
                            let sample = sample_y as usize * resolution + sample_x as usize;
                            if !gbuffer.covered[sample] || gbuffer.chart_ids[sample] != chart {
                                continue;
                            }
                            let kernel =
                                ATROUS_KERNEL[(dy + 2) as usize] * ATROUS_KERNEL[(dx + 2) as usize];
                            let normal_weight = normal
                                .dot(gbuffer.normals[sample])
                                .max(0.0)
                                .powf(NORMAL_POWER);
                            let plane_distance = normal.dot(gbuffer.positions[sample] - position);
                            let plane_weight = (-(plane_distance * plane_distance)
                                / (2.0 * sigma_plane * sigma_plane))
                                .exp();
                            let luminance_weight =
                                (-(luminance_center - luminance(radiance[sample])).abs() / sigma_l)
                                    .exp();
                            let weight = kernel * normal_weight * plane_weight * luminance_weight;
                            weight_sum += weight;
                            color_sum += radiance[sample] * weight;
                            variance_sum += weight * weight * variance[sample];
                        }
                    }
                    if weight_sum > 1e-12 {
                        row_r[x] = color_sum / weight_sum;
                        row_v[x] = variance_sum / (weight_sum * weight_sum);
                    } else {
                        row_r[x] = radiance[index];
                        row_v[x] = variance[index];
                    }
                }
            });

        radiance.copy_from_slice(&out_r);
        variance.copy_from_slice(&out_v);
    }
}

/// Clamp isolated bright texels before filtering.
///
/// A single very bright path can survive several filter passes as a visible
/// blotch. Median filters remove those spikes but also erase genuine thin
/// highlights, so this only clamps a texel when *every* neighbour on the same
/// surface is at least four times darker. A legitimate highlight or shadow
/// edge keeps at least one similarly bright neighbour and is left untouched.
fn suppress_fireflies(
    radiance: &mut [Vec3],
    positions: &[Vec3],
    normals: &[Vec3],
    covered: &[bool],
    texel_world: f32,
    resolution: u32,
) {
    let size = resolution as usize;
    if size < 3 {
        return;
    }
    let source = radiance.to_vec();
    let max_distance_squared = (texel_world * 2.0) * (texel_world * 2.0);
    radiance
        .par_iter_mut()
        .enumerate()
        .for_each(|(index, output)| {
            *output = source[index];
            if !covered[index] {
                return;
            }
            let center_luminance = luminance(source[index]);
            if center_luminance <= 1e-6 {
                return;
            }
            let x = index % size;
            let y = index / size;
            let position = positions[index];
            let normal = normals[index];
            let mut max_neighbor = 0.0f32;
            let mut neighbors = 0u32;
            for offset_y in -1i32..=1 {
                for offset_x in -1i32..=1 {
                    if offset_x == 0 && offset_y == 0 {
                        continue;
                    }
                    let sample_x = x as i32 + offset_x;
                    let sample_y = y as i32 + offset_y;
                    if sample_x < 0
                        || sample_y < 0
                        || sample_x >= size as i32
                        || sample_y >= size as i32
                    {
                        continue;
                    }
                    let sample_index = sample_y as usize * size + sample_x as usize;
                    if !covered[sample_index]
                        || normal.dot(normals[sample_index]) < 0.9
                        || position.distance_squared(positions[sample_index]) > max_distance_squared
                    {
                        continue;
                    }
                    max_neighbor = max_neighbor.max(luminance(source[sample_index]));
                    neighbors += 1;
                }
            }
            if neighbors < 5 {
                return;
            }
            let threshold = (max_neighbor * 4.0).max(0.5);
            if center_luminance > threshold {
                *output = source[index] * (threshold / center_luminance);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(size: usize, spacing: f32) -> (Vec<Vec3>, Vec<Vec3>, Vec<bool>) {
        let mut positions = Vec::with_capacity(size * size);
        for y in 0..size {
            for x in 0..size {
                positions.push(Vec3::new(x as f32 * spacing, y as f32 * spacing, 0.0));
            }
        }
        let normals = vec![Vec3::Z; size * size];
        let covered = vec![true; size * size];
        (positions, normals, covered)
    }

    #[test]
    fn isolated_fireflies_are_suppressed() {
        let size = 24;
        let spacing = 0.1;
        let (positions, normals, covered) = plane(size, spacing);
        let mut radiance = vec![Vec3::splat(0.2); size * size];
        let center = (size / 2) * size + size / 2;
        radiance[center] = Vec3::splat(40.0);
        builtin_denoise(
            &mut radiance,
            &positions,
            &normals,
            &covered,
            size as f32 * spacing,
            size as u32,
            2,
        );
        assert!(
            radiance[center].x < 1.0,
            "firefly survived denoising: {}",
            radiance[center].x
        );
    }

    #[test]
    fn smooth_gradients_are_preserved() {
        let size = 24;
        let spacing = 0.1;
        let (positions, normals, covered) = plane(size, spacing);
        let mut radiance: Vec<Vec3> = (0..size * size)
            .map(|index| Vec3::splat(0.2 + (index % size) as f32))
            .collect();
        let center = (size / 2) * size + size / 2;
        let before = radiance[center].x;
        builtin_denoise(
            &mut radiance,
            &positions,
            &normals,
            &covered,
            size as f32 * spacing,
            size as u32,
            2,
        );
        assert!(
            (radiance[center].x - before).abs() < 0.01,
            "gradient changed from {before} to {}",
            radiance[center].x
        );
    }

    fn gbuffer(
        positions: Vec<Vec3>,
        normals: Vec<Vec3>,
        covered: Vec<bool>,
        chart_ids: Vec<u32>,
    ) -> GBuffer {
        let texel_count = positions.len();
        GBuffer {
            geo_normals: normals.clone(),
            positions,
            normals,
            covered,
            chart_ids,
            edge: vec![false; texel_count],
        }
    }

    #[test]
    fn v2_does_not_leak_across_charts() {
        let size = 24;
        let (positions, normals, covered) = plane(size, 0.1);
        let charts: Vec<u32> = (0..size * size)
            .map(|index| u32::from(index % size >= size / 2))
            .collect();
        let mut radiance: Vec<Vec3> = (0..size * size)
            .map(|index| Vec3::splat(if index % size < size / 2 { 0.1 } else { 0.9 }))
            .collect();
        let mut variance = vec![1.0e-6f32; size * size];
        let gb = gbuffer(positions, normals, covered, charts);
        atrous_v2(&mut radiance, &mut variance, &gb, size, 0.1, 2);
        for (index, value) in radiance.iter().enumerate() {
            let value = value.x;
            if index % size < size / 2 {
                assert!(value < 0.2, "left island leaked to {value}");
            } else {
                assert!(value > 0.8, "right island leaked to {value}");
            }
        }
    }

    #[test]
    fn v2_preserves_a_smooth_gradient() {
        let size = 24;
        let (positions, normals, covered) = plane(size, 0.1);
        let charts = vec![0u32; size * size];
        let mut radiance: Vec<Vec3> = (0..size * size)
            .map(|index| Vec3::splat(0.2 + (index % size) as f32 * 0.01))
            .collect();
        let mut variance = vec![1.0e-8f32; size * size];
        let before = radiance[(size / 2) * size + size / 2].x;
        let gb = gbuffer(positions, normals, covered, charts);
        atrous_v2(&mut radiance, &mut variance, &gb, size, 0.1, 2);
        let after = radiance[(size / 2) * size + size / 2].x;
        assert!(
            (after - before).abs() < 0.01,
            "gradient changed {before} -> {after}"
        );
    }

    #[test]
    fn v2_reduces_noise_variance() {
        let size = 24;
        let (positions, normals, covered) = plane(size, 0.1);
        let charts = vec![0u32; size * size];
        let mut radiance: Vec<Vec3> = (0..size * size)
            .map(|index| {
                Vec3::splat(if (index / size + index % size) % 2 == 0 {
                    0.4
                } else {
                    0.6
                })
            })
            .collect();
        let mut variance = vec![0.01f32; size * size];
        let spread = |values: &[Vec3]| {
            let mean = values.iter().map(|value| value.x).sum::<f32>() / values.len() as f32;
            (values
                .iter()
                .map(|value| (value.x - mean).powi(2))
                .sum::<f32>()
                / values.len() as f32)
                .sqrt()
        };
        let before = spread(&radiance);
        let gb = gbuffer(positions, normals, covered, charts);
        atrous_v2(&mut radiance, &mut variance, &gb, size, 0.1, 3);
        let after = spread(&radiance);
        assert!(after < before * 0.8, "noise spread {before} -> {after}");
    }

    #[test]
    fn v2_keeps_a_hard_shadow_edge() {
        let size = 24;
        let (positions, normals, covered) = plane(size, 0.1);
        let charts = vec![0u32; size * size];
        let mut radiance: Vec<Vec3> = (0..size * size)
            .map(|index| Vec3::splat(if index % size < size / 2 { 0.1 } else { 0.9 }))
            .collect();
        let mut variance = vec![1.0e-9f32; size * size];
        let gb = gbuffer(positions, normals, covered, charts);
        atrous_v2(&mut radiance, &mut variance, &gb, size, 0.1, 2);
        for y in 0..size {
            assert!(radiance[y * size + size / 2 - 1].x < 0.2);
            assert!(radiance[y * size + size / 2].x > 0.8);
        }
    }
}
