//! Output assembly and UV-island padding.

use std::collections::VecDeque;

use glam::Vec3;

use super::{
    BackendUsed, DenoiserUsed, Features, GBuffer, LightmapOutput, LightmapSettings, LightmapStats,
    PngBitDepth,
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_lightmap(
    radiance: &mut [Vec3],
    gbuffer: &GBuffer,
    resolution: u32,
    settings: &LightmapSettings,
    elapsed_ms: f32,
    covered_count: usize,
    triangle_count: usize,
    rays: u64,
    hits: u64,
    denoiser: DenoiserUsed,
    backend: BackendUsed,
) -> LightmapOutput {
    let features = settings.features();
    let texel_count = resolution as usize * resolution as usize;
    let mut coverage = gbuffer.covered.clone();
    if settings.padding > 0 {
        if features.smooth_dilate {
            let mut chart_ids = gbuffer.chart_ids.clone();
            dilate_v2(
                radiance,
                &mut coverage,
                &mut chart_ids,
                &gbuffer.edge,
                resolution as usize,
                settings.padding,
            );
        } else {
            dilate(radiance, &mut coverage, resolution, settings.padding);
        }
    }

    let mut pixels = vec![0u8; texel_count * 4];
    let mut pixels16 =
        matches!(settings.png_bit_depth, PngBitDepth::Sixteen).then(|| vec![0u16; texel_count * 4]);
    let mut hdr = vec![0.0f32; texel_count * 3];
    let mut clipped = 0u32;
    let exposure = 2.0f32.powf(settings.exposure.clamp(-8.0, 8.0));
    let rolloff = if features.tpdf_dither {
        settings.highlight_rolloff
    } else {
        0.0
    };
    for index in 0..texel_count {
        if !coverage[index] {
            continue;
        }
        let value = radiance[index].max(Vec3::ZERO) * exposure;
        // The clipping report intentionally measures before the shoulder, so it
        // still warns when rolloff is hiding an over-bright bake.
        if gbuffer.covered[index] && value.max_element() > 1.0 {
            clipped += 1;
        }
        let x = (index % resolution as usize) as u32;
        let y = (index / resolution as usize) as u32;
        let target = index * 4;
        if features.tpdf_dither {
            let [r, g, b] = encode_rgb8(value, x, y, &features, rolloff);
            pixels[target] = r;
            pixels[target + 1] = g;
            pixels[target + 2] = b;
        } else {
            // Legacy keeps the v1 Bayer dither byte for byte.
            let dither = dither_offset(x as usize, y as usize);
            pixels[target] = encode_srgb_dither(value.x, dither);
            pixels[target + 1] = encode_srgb_dither(value.y, dither);
            pixels[target + 2] = encode_srgb_dither(value.z, dither);
        }
        pixels[target + 3] = 255;
        if let Some(buffer) = &mut pixels16 {
            let [r, g, b] = encode_rgb16(value, rolloff);
            buffer[target] = r;
            buffer[target + 1] = g;
            buffer[target + 2] = b;
            buffer[target + 3] = 65535;
        }
        let target = index * 3;
        hdr[target] = radiance[index].x;
        hdr[target + 1] = radiance[index].y;
        hdr[target + 2] = radiance[index].z;
    }

    LightmapOutput {
        width: resolution,
        height: resolution,
        pixels,
        pixels16,
        hdr,
        stats: LightmapStats {
            resolution,
            lighting: settings.lighting,
            samples: settings.effective_samples(),
            bounces: settings.effective_bounces(),
            texels: covered_count as u32,
            triangles: triangle_count as u32,
            rays,
            hits,
            elapsed_ms,
            denoiser,
            backend,
            clipped,
            pipeline: settings.pipeline,
            edge_texels: if features.edge_extension {
                gbuffer.edge.iter().filter(|edge| **edge).count() as u32
            } else {
                0
            },
            charts: unique_charts(gbuffer),
        },
        backend_name: None,
    }
}

fn unique_charts(gbuffer: &GBuffer) -> u32 {
    let mut charts: Vec<u32> = gbuffer
        .chart_ids
        .iter()
        .zip(gbuffer.covered.iter())
        .filter_map(|(chart, covered)| covered.then_some(*chart))
        .collect();
    charts.sort_unstable();
    charts.dedup();
    charts.len() as u32
}

/// Round to 8-bit sRGB with a sub-LSB offset. Quantizing a smooth gradient
/// without dithering produces visible bands, which is especially obvious in
/// low-contrast lightmap falloff. v1's 4x4 Bayer matrix; kept for `Legacy`.
fn encode_srgb_dither(value: f32, dither: f32) -> u8 {
    let linear = value.clamp(0.0, 1.0);
    (srgb_encode(linear) * 255.0 + dither)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn dither_offset(x: usize, y: usize) -> f32 {
    const BAYER: [[f32; 4]; 4] = [
        [0.0, 8.0, 2.0, 10.0],
        [12.0, 4.0, 14.0, 6.0],
        [3.0, 11.0, 1.0, 9.0],
        [15.0, 7.0, 13.0, 5.0],
    ];
    BAYER[y & 3][x & 3] / 16.0 - 0.5
}

#[inline]
pub(crate) fn srgb_encode(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// Interleaved gradient noise, ported from Jimenez 2014.
#[inline]
fn ign(x: f32, y: f32) -> f32 {
    let t = 0.067_110_56 * x + 0.005_837_15 * y;
    (52.982_919 * t.fract()).fract()
}

/// Triangular noise on `(-1, 1)` with no repeating period.
#[inline]
pub(crate) fn tpdf(x: u32, y: u32) -> f32 {
    let (x, y) = (x as f32, y as f32);
    ign(x, y) + ign(x + 5.588_238, y + 5.588_238) - 1.0
}

/// Soft highlight shoulder; `r <= 0` returns the input (the clamp stays in the
/// encoder), and `r` near 0.95 starts the shoulder at 5% of the range.
pub(crate) fn rolloff(rgb: Vec3, r: f32) -> Vec3 {
    if r <= 0.0 {
        return rgb;
    }
    let knee = 1.0 - r.clamp(0.0, 0.95);
    let max = rgb.max_element();
    if max <= knee {
        return rgb;
    }
    let span = 1.0 - knee;
    let shaped = knee + span * (1.0 - (-(max - knee) / span).exp());
    rgb * (shaped / max)
}

/// sRGB 8-bit with optional TPDF dither and rolloff.
pub(crate) fn encode_rgb8(lin: Vec3, x: u32, y: u32, f: &Features, rolloff_amt: f32) -> [u8; 3] {
    let lin = if f.tpdf_dither {
        rolloff(lin, rolloff_amt)
    } else {
        lin
    };
    let noise = if f.tpdf_dither { tpdf(x, y) } else { 0.0 };
    let encode = |channel: f32| -> u8 {
        let channel = channel.clamp(0.0, 1.0);
        let encoded = srgb_encode(channel) * 255.0;
        let value = if f.tpdf_dither && channel > 0.0 && channel < 1.0 {
            encoded + noise
        } else {
            encoded
        };
        (value + 0.5).floor().clamp(0.0, 255.0) as u8
    };
    [encode(lin.x), encode(lin.y), encode(lin.z)]
}

/// sRGB 16-bit. The step is small enough that no dither is needed.
pub(crate) fn encode_rgb16(lin: Vec3, rolloff_amt: f32) -> [u16; 3] {
    let lin = rolloff(lin, rolloff_amt);
    let encode =
        |channel: f32| (srgb_encode(channel.clamp(0.0, 1.0)) * 65535.0 + 0.5).floor() as u16;
    [encode(lin.x), encode(lin.y), encode(lin.z)]
}

/// v1 padding: breadth-first 4-neighbour copy.
pub(crate) fn dilate(radiance: &mut [Vec3], covered: &mut [bool], resolution: u32, passes: u32) {
    if passes == 0 {
        return;
    }
    let size = resolution as usize;
    let texel_count = size * size;
    let mut queue = VecDeque::new();
    for index in 0..texel_count {
        if !covered[index] {
            continue;
        }
        let x = index % size;
        let y = index / size;
        for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= size as i32 || ny >= size as i32 {
                continue;
            }
            let neighbor = ny as usize * size + nx as usize;
            if !covered[neighbor] {
                queue.push_back((neighbor, index, 1u32));
            }
        }
    }

    while let Some((target, source, distance)) = queue.pop_front() {
        if covered[target] {
            continue;
        }
        radiance[target] = radiance[source];
        covered[target] = true;
        if distance >= passes {
            continue;
        }
        let x = target % size;
        let y = target / size;
        for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= size as i32 || ny >= size as i32 {
                continue;
            }
            let neighbor = ny as usize * size + nx as usize;
            if !covered[neighbor] {
                queue.push_back((neighbor, source, distance + 1));
            }
        }
    }
}

/// v2 padding: synchronous rings, 8-neighbour weighted average, one chart per
/// filled texel. Every fill of a pass is applied together, so the result does
/// not depend on traversal order and two adjacent islands never mix.
///
/// Edge-extension texels are shaded by extrapolating onto the nearest triangle.
/// They are correct where they are used for their own island's border, but they
/// are poor padding seeds: a seed on one side of a sub-texel gutter can fill the
/// gutter a ring before the interior texel on the other side is reached, which
/// spreads an extrapolated bright value beside a differently lit island. Interior
/// texels therefore seed every ring first; edge texels are only a fallback for
/// padding that interiors cannot reach at all.
pub(crate) fn dilate_v2(
    radiance: &mut [Vec3],
    covered: &mut [bool],
    chart: &mut [u32],
    edge: &[bool],
    resolution: usize,
    passes: u32,
) {
    let used = dilate_rings(radiance, covered, chart, edge, resolution, passes, true);
    let remaining = passes.saturating_sub(used);
    if remaining > 0 {
        dilate_rings(radiance, covered, chart, edge, resolution, remaining, false);
    }
}

fn dilate_rings(
    radiance: &mut [Vec3],
    covered: &mut [bool],
    chart: &mut [u32],
    edge: &[bool],
    resolution: usize,
    budget: u32,
    interior_only: bool,
) -> u32 {
    const N8: [(i32, i32, f32); 8] = [
        (-1, 0, 1.0),
        (1, 0, 1.0),
        (0, -1, 1.0),
        (0, 1, 1.0),
        (-1, -1, 0.707_106_77),
        (1, -1, 0.707_106_77),
        (-1, 1, 0.707_106_77),
        (1, 1, 0.707_106_77),
    ];
    let index = |x: i32, y: i32| -> Option<usize> {
        (x >= 0 && y >= 0 && (x as usize) < resolution && (y as usize) < resolution)
            .then(|| y as usize * resolution + x as usize)
    };
    let mut used = 0;
    for _ in 0..budget {
        let mut fills: Vec<(usize, Vec3, u32)> = Vec::new();
        for y in 0..resolution as i32 {
            for x in 0..resolution as i32 {
                let i = y as usize * resolution + x as usize;
                if covered[i] {
                    continue;
                }
                // Deterministic chart choice: the lowest ID among neighbours.
                let mut best = u32::MAX;
                for &(dx, dy, _) in &N8 {
                    if let Some(j) = index(x + dx, y + dy) {
                        if covered[j] && !(interior_only && edge[j]) {
                            best = best.min(chart[j]);
                        }
                    }
                }
                if best == u32::MAX {
                    continue;
                }
                let (mut sum, mut weight) = (Vec3::ZERO, 0.0f32);
                for &(dx, dy, w) in &N8 {
                    if let Some(j) = index(x + dx, y + dy) {
                        if covered[j] && chart[j] == best && !(interior_only && edge[j]) {
                            sum += radiance[j] * w;
                            weight += w;
                        }
                    }
                }
                if weight > 0.0 {
                    fills.push((i, sum / weight, best));
                }
            }
        }
        if fills.is_empty() {
            break;
        }
        for (i, value, id) in fills {
            radiance[i] = value;
            covered[i] = true;
            chart[i] = id;
        }
        used += 1;
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2_features() -> Features {
        Features {
            tpdf_dither: true,
            smooth_dilate: true,
            ..Features::legacy()
        }
    }

    #[test]
    fn dither_preserves_the_mean_and_stays_within_one_lsb() {
        let features = v2_features();
        for value in [0.01f32, 0.05, 0.2, 0.5, 0.8] {
            let expected = srgb_encode(value) * 255.0;
            let mut sum = 0.0f32;
            let mut count = 0.0f32;
            for y in 0..32u32 {
                for x in 0..32u32 {
                    let [r, _, _] = encode_rgb8(Vec3::splat(value), x, y, &features, 0.0);
                    assert!((f32::from(r) - expected).abs() <= 1.5 + 1e-3);
                    sum += f32::from(r);
                    count += 1.0;
                }
            }
            let bias = (sum / count - expected).abs();
            assert!(bias < 0.25, "value {value}: bias {bias}");
        }
    }

    #[test]
    fn dither_keeps_black_and_white_exact() {
        let features = v2_features();
        for y in 0..16u32 {
            for x in 0..16u32 {
                let black = encode_rgb8(Vec3::ZERO, x, y, &features, 0.0);
                let white = encode_rgb8(Vec3::ONE, x, y, &features, 0.0);
                assert_eq!(black, [0, 0, 0]);
                assert_eq!(white, [255, 255, 255]);
            }
        }
    }

    #[test]
    fn edge_extension_texels_do_not_seed_padding_before_interiors() {
        // A bright edge texel one ring from a gutter must not beat the dark
        // interior two rings away on the other side of that gutter. This is the
        // thin-bevel fringe: the edge texel is correct for its own island, but a
        // poor padding seed.
        let size = 9usize;
        let row = 4usize;
        let mut radiance = vec![Vec3::ZERO; size * size];
        let mut covered = vec![false; size * size];
        let mut chart = vec![u32::MAX; size * size];
        let mut edge = vec![false; size * size];

        let dark = row * size + 1;
        let bright_edge = row * size + 4;
        let bright_interior = row * size + 5;
        radiance[dark] = Vec3::splat(0.2);
        covered[dark] = true;
        chart[dark] = 324;
        radiance[bright_edge] = Vec3::splat(1.0);
        covered[bright_edge] = true;
        edge[bright_edge] = true;
        chart[bright_edge] = 332;
        radiance[bright_interior] = Vec3::splat(1.0);
        covered[bright_interior] = true;
        chart[bright_interior] = 332;

        dilate_v2(&mut radiance, &mut covered, &mut chart, &edge, size, 4);

        let gutter = row * size + 3;
        assert!(covered[gutter]);
        assert_eq!(chart[gutter], 324, "padding took the edge texel's chart");
        assert!((radiance[gutter].x - 0.2).abs() < 1e-6);
    }

    #[test]
    fn edge_only_islands_still_seed_padding_as_a_fallback() {
        let size = 7usize;
        let row = 3usize;
        let mut radiance = vec![Vec3::ZERO; size * size];
        let mut covered = vec![false; size * size];
        let mut chart = vec![u32::MAX; size * size];
        let mut edge = vec![false; size * size];
        let seed = row * size + 3;
        radiance[seed] = Vec3::splat(0.7);
        covered[seed] = true;
        edge[seed] = true;
        chart[seed] = 9;

        dilate_v2(&mut radiance, &mut covered, &mut chart, &edge, size, 1);

        let neighbour = row * size + 2;
        assert!(covered[neighbour]);
        assert_eq!(chart[neighbour], 9);
        assert!((radiance[neighbour].x - 0.7).abs() < 1e-6);
    }

    #[test]
    fn rolloff_is_monotonic_continuous_and_hue_preserving() {
        let mut previous = 0.0f32;
        for step in 0..=200 {
            let value = step as f32 / 100.0;
            let shaped = rolloff(Vec3::new(value, value * 0.5, value * 0.25), 0.4);
            assert!(shaped.max_element() >= previous - 1e-5);
            previous = shaped.max_element();
            if value > 1e-5 {
                let ratio = shaped.y / shaped.x;
                assert!((ratio - 0.5).abs() < 1e-4, "hue drifted at {value}");
            }
        }
        assert!((rolloff(Vec3::splat(0.6), 0.4) - Vec3::splat(0.6)).length() < 1e-5);
    }

    #[test]
    fn dilate_v2_fills_each_chart_with_its_own_radiance() {
        let size = 16usize;
        let mut radiance = vec![Vec3::ZERO; size * size];
        let mut covered = vec![false; size * size];
        let mut chart = vec![u32::MAX; size * size];
        let edge = vec![false; size * size];
        let left = 3 * size + 3;
        let right = 12 * size + 12;
        radiance[left] = Vec3::splat(0.2);
        covered[left] = true;
        chart[left] = 0;
        radiance[right] = Vec3::splat(0.8);
        covered[right] = true;
        chart[right] = 1;

        dilate_v2(&mut radiance, &mut covered, &mut chart, &edge, size, 4);
        for index in 0..size * size {
            if !covered[index] {
                continue;
            }
            let expected = match chart[index] {
                0 => 0.2f32,
                1 => 0.8,
                other => panic!("gutter texel kept an invalid chart {other}"),
            };
            assert!(
                (radiance[index].x - expected).abs() < 1e-6,
                "chart {} mixed radiance {}",
                chart[index],
                radiance[index].x
            );
        }
    }
}
