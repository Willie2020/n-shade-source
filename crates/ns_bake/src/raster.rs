//! UV-space rasterization, ray shading, supersampling and island padding.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use glam::{Quat, Vec2, Vec3};
use rayon::prelude::*;

use crate::bvh::Bvh;
use crate::mesh::{arbitrary_tangent, normalize_or, LowTriangle, PreparedLow, WRAP_SHIFTS};
use crate::{
    BakeError, BakeMesh, BakeMode, BakeOutput, BakeSettings, BakeSpace, BakeStats, Result,
    TriplanarOutput, TriplanarSettings,
};

const BAND_HEIGHT: u32 = 32;

pub(crate) fn render(
    low: &PreparedLow,
    high: Option<&Bvh>,
    settings: &BakeSettings,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<BakeOutput> {
    let resolution = settings.output_size();
    let anti_alias = settings.effective_aa();
    let width = resolution * anti_alias;
    let height = width;
    let mut samples = vec![0u8; width as usize * height as usize * 4];

    let band_count = height.div_ceil(BAND_HEIGHT) as usize;
    let mut bands: Vec<Vec<u32>> = (0..band_count).map(|_| Vec::new()).collect();
    for (index, triangle) in low.triangles.iter().enumerate() {
        let mut y_min = i32::MAX;
        let mut y_max = i32::MIN;
        for (shift_index, shift) in WRAP_SHIFTS.iter().enumerate() {
            if triangle.wrap_mask & (1 << shift_index) == 0 {
                continue;
            }
            let (_, y0, _, y1) =
                pixel_bbox(triangle, *shift, width, height, settings.uv_bottom_left);
            if y1 >= y0 {
                y_min = y_min.min(y0);
                y_max = y_max.max(y1);
            }
        }
        if y_max < y_min {
            continue;
        }
        let first_band = (y_min.max(0) as u32 / BAND_HEIGHT) as usize;
        let last_band = (y_max.min(height as i32 - 1) as u32 / BAND_HEIGHT) as usize;
        for band in bands.iter_mut().take(last_band + 1).skip(first_band) {
            band.push(index as u32);
        }
    }

    let chunk_size = BAND_HEIGHT as usize * width as usize * 4;
    let progress_step = (650 / band_count.max(1) as u32).max(1);
    let counts = bands
        .par_iter()
        .zip(samples.par_chunks_mut(chunk_size))
        .enumerate()
        .map(|(band_index, (band, chunk))| {
            let mut counts = [0u64; 3];
            if cancel.load(Ordering::Relaxed) {
                return counts;
            }
            let y_start = band_index as u32 * BAND_HEIGHT;
            let y_end = (y_start + BAND_HEIGHT).min(height);
            for &triangle_index in band {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let triangle = &low.triangles[triangle_index as usize];
                for (shift_index, shift) in WRAP_SHIFTS.iter().enumerate() {
                    if triangle.wrap_mask & (1 << shift_index) == 0 {
                        continue;
                    }
                    let (x0, y0, x1, y1) =
                        pixel_bbox(triangle, *shift, width, height, settings.uv_bottom_left);
                    let x_start = x0.max(0);
                    let x_end = x1.min(width as i32 - 1);
                    let row_start = y0.max(y_start as i32);
                    let row_end = y1.min(y_end as i32 - 1);
                    if x_end < x_start || row_end < row_start {
                        continue;
                    }
                    for y in row_start..=row_end {
                        for x in x_start..=x_end {
                            let u = (x as f32 + 0.5) / width as f32;
                            let v = if settings.uv_bottom_left {
                                1.0 - (y as f32 + 0.5) / height as f32
                            } else {
                                (y as f32 + 0.5) / height as f32
                            };
                            let Some(weights) = barycentric(triangle, *shift, u, v) else {
                                continue;
                            };
                            if weights.iter().any(|weight| *weight < -1e-5) {
                                continue;
                            }
                            let color = shade(low, triangle, weights, high, settings, &mut counts);
                            let offset =
                                ((y as u32 - y_start) as usize * width as usize + x as usize) * 4;
                            chunk[offset..offset + 4].copy_from_slice(&color);
                        }
                    }
                }
            }
            progress.fetch_add(progress_step, Ordering::Relaxed);
            counts
        })
        .reduce(
            || [0u64; 3],
            |left, right| [left[0] + right[0], left[1] + right[1], left[2] + right[2]],
        );

    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }
    progress.store(950, Ordering::Relaxed);

    let mut pixels = if anti_alias == 1 {
        samples
    } else {
        downsample(&samples, resolution, anti_alias, settings.space)
    };
    progress.store(980, Ordering::Relaxed);

    // Padding is authored as a float; the dilation ring is whole texels.
    let padding = settings.padding.round().clamp(0.0, 256.0) as u32;
    if padding > 0 {
        dilate(&mut pixels, resolution, padding);
    }
    fill_background(&mut pixels, settings.space);
    progress.store(1000, Ordering::Relaxed);

    let stats = BakeStats {
        resolution,
        effective_aa: anti_alias,
        tile: settings.tile.max(1001),
        low_triangles: low.triangles.len() as u32,
        high_triangles: high.map(|bvh| bvh.triangle_count() as u32).unwrap_or(0),
        rays: counts[0],
        hits: counts[1],
        misses: counts[2],
        elapsed_ms: 0.0,
    };
    Ok(BakeOutput {
        width: resolution,
        height: resolution,
        pixels,
        stats,
    })
}

fn shade(
    low: &PreparedLow,
    triangle: &LowTriangle,
    weights: [f32; 3],
    high: Option<&Bvh>,
    settings: &BakeSettings,
    counts: &mut [u64; 3],
) -> [u8; 4] {
    let p = triangle.p[0] * weights[0] + triangle.p[1] * weights[1] + triangle.p[2] * weights[2];

    let mut normal =
        triangle.n[0] * weights[0] + triangle.n[1] * weights[1] + triangle.n[2] * weights[2];
    normal = normalize_or(normal, triangle.face_normal);

    let mut tangent =
        triangle.t[0] * weights[0] + triangle.t[1] * weights[1] + triangle.t[2] * weights[2];
    tangent -= normal * normal.dot(tangent);
    let tangent = normalize_or(tangent, arbitrary_tangent(normal));
    let sign = if triangle.sign[0] * weights[0]
        + triangle.sign[1] * weights[1]
        + triangle.sign[2] * weights[2]
        < 0.0
    {
        -1.0
    } else {
        1.0
    };
    let bitangent = normal.cross(tangent) * sign;

    let world_normal = match settings.mode {
        BakeMode::HighToLow => {
            let Some(high) = high else {
                return encode(Vec3::Z, settings.space);
            };
            let cage = settings.cage_offset_pct.max(0.0) * 0.01 * low.extent;
            let range = settings.ray_distance_pct.max(0.0) * 0.01 * low.extent;
            let max_distance = cage + range;
            let front = high.raycast(p + normal * cage, -normal, max_distance);
            let back = high.raycast(p - normal * cage, normal, max_distance);
            counts[0] += 2;
            match closest(front, back, cage) {
                Some(hit_normal) => {
                    counts[1] += 1;
                    hit_normal
                }
                None => {
                    counts[2] += 1;
                    normal
                }
            }
        }
        BakeMode::Bevel => bevel_normal(low, triangle, p, normal, settings),
    };

    let mapped = match settings.space {
        BakeSpace::TangentOpenGl | BakeSpace::TangentDirectX => apply_intensity(
            Vec3::new(
                world_normal.dot(tangent),
                world_normal.dot(bitangent),
                world_normal.dot(normal),
            ),
            settings.normal_intensity,
        ),
        BakeSpace::Object => world_normal,
    };
    encode(mapped, settings.space)
}

/// Scales the tangent-space slope of a baked normal and renormalises it.
///
/// `1.0` returns the input unchanged, `0.0` gives the flat (0, 0, 1) normal and
/// values above one exaggerate the relief. Applying the scale before the
/// supersampled samples are averaged keeps anti-aliased and direct bakes equal.
fn apply_intensity(normal: Vec3, intensity: f32) -> Vec3 {
    let intensity = intensity.clamp(0.0, 8.0);
    if (intensity - 1.0).abs() < 1e-4 {
        return normal;
    }
    normalize_or(
        Vec3::new(normal.x * intensity, normal.y * intensity, normal.z),
        Vec3::Z,
    )
}

/// Project a 2D normal map onto a mesh through a transformed bounding box.
/// Mesh positions are normalized to the preview convention (bounds centred on
/// the origin, largest extent one unit) before the box transform is applied.
pub(crate) fn project_triplanar<F>(
    mesh: &BakeMesh,
    settings: &TriplanarSettings,
    sample: F,
) -> Result<TriplanarOutput>
where
    F: Fn([f32; 2]) -> [f32; 3],
{
    let resolution = settings.resolution.clamp(64, 4096);
    let bake_settings = BakeSettings {
        mode: BakeMode::Bevel,
        resolution,
        anti_alias: 1,
        space: BakeSpace::TangentOpenGl,
        tile: 1001,
        cage_offset_pct: 1.0,
        ray_distance_pct: 5.0,
        bevel_radius_pct: 2.0,
        bevel_falloff: 1.0,
        normal_intensity: 1.0,
        padding: 0.0,
        uv_bottom_left: settings.uv_bottom_left,
    };
    let low = crate::mesh::prepare_low(mesh, &bake_settings)?;
    let (mesh_center, mesh_extent) = mesh_bounds(mesh)?;
    let inv_extent = 1.0 / mesh_extent.max(1e-5);

    let center = Vec3::from_array(settings.center);
    let half = Vec3::from_array(settings.size).abs().max(Vec3::splat(1e-4)) * 0.5;
    let rotation = {
        let q = Quat::from_array(settings.rotation);
        if q.is_finite() && q.length_squared() > 1e-12 {
            q.normalize()
        } else {
            Quat::IDENTITY
        }
    };
    let rotation_inv = rotation.conjugate();
    let sharpness = settings.sharpness.clamp(0.5, 32.0);
    let feather = settings.edge_feather.max(0.0);
    let window = settings.uv_window;

    let w = resolution as usize;
    let h = w;
    let mut normals = vec![[0.0f32, 0.0, 1.0]; w * h];
    let mut coverage = vec![0.0f32; w * h];
    let mut covered = vec![false; w * h];

    for triangle in &low.triangles {
        for (shift_index, shift) in WRAP_SHIFTS.iter().enumerate() {
            if triangle.wrap_mask & (1 << shift_index) == 0 {
                continue;
            }
            let (x0, y0, x1, y1) = pixel_bbox(
                triangle,
                *shift,
                resolution,
                resolution,
                settings.uv_bottom_left,
            );
            let x_start = x0.max(0);
            let x_end = x1.min(resolution as i32 - 1);
            let y_start = y0.max(0);
            let y_end = y1.min(resolution as i32 - 1);
            if x_end < x_start || y_end < y_start {
                continue;
            }
            for y in y_start..=y_end {
                for x in x_start..=x_end {
                    let u = (x as f32 + 0.5) / resolution as f32;
                    let v = if settings.uv_bottom_left {
                        1.0 - (y as f32 + 0.5) / resolution as f32
                    } else {
                        (y as f32 + 0.5) / resolution as f32
                    };
                    if u < window[0] || v < window[1] || u > window[2] || v > window[3] {
                        continue;
                    }
                    let Some(weights) = barycentric(triangle, *shift, u, v) else {
                        continue;
                    };
                    if weights.iter().any(|weight| *weight < -1e-5) {
                        continue;
                    }
                    let p = triangle.p[0] * weights[0]
                        + triangle.p[1] * weights[1]
                        + triangle.p[2] * weights[2];
                    let normal = normalize_or(
                        triangle.n[0] * weights[0]
                            + triangle.n[1] * weights[1]
                            + triangle.n[2] * weights[2],
                        triangle.face_normal,
                    );
                    let mut tangent = triangle.t[0] * weights[0]
                        + triangle.t[1] * weights[1]
                        + triangle.t[2] * weights[2];
                    tangent -= normal * normal.dot(tangent);
                    let tangent = normalize_or(tangent, arbitrary_tangent(normal));
                    let sign = if triangle.sign[0] * weights[0]
                        + triangle.sign[1] * weights[1]
                        + triangle.sign[2] * weights[2]
                        < 0.0
                    {
                        -1.0
                    } else {
                        1.0
                    };
                    let bitangent = normal.cross(tangent) * sign;

                    let p_norm = (p - mesh_center) * inv_extent;
                    let local = rotation_inv * ((p_norm - center) / half);
                    let edge = local.abs().max_element();
                    let cov = cube_coverage(edge, feather);
                    if cov <= 0.0 {
                        continue;
                    }
                    let geom = normalize_or(rotation_inv * normal, Vec3::Z);
                    let world = triplanar_normal(local, geom, &sample, rotation, sharpness);
                    let mut mapped =
                        Vec3::new(world.dot(tangent), world.dot(bitangent), world.dot(normal));
                    // A projected normal should always face out of the mesh;
                    // grazing projections can otherwise produce negative Z.
                    mapped.z = mapped.z.max(0.02);
                    let mapped = normalize_or(mapped, Vec3::Z);
                    let index = y as usize * w + x as usize;
                    normals[index] = mapped.to_array();
                    coverage[index] = cov;
                    covered[index] = true;
                }
            }
        }
    }

    if settings.padding > 0 {
        dilate_normals(&mut normals, &mut covered, w, h, settings.padding.min(128));
    }

    Ok(TriplanarOutput {
        width: resolution,
        height: resolution,
        normals,
        coverage,
    })
}

fn mesh_bounds(mesh: &BakeMesh) -> Result<(Vec3, f32)> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for position in &mesh.positions {
        let p = Vec3::from_array(*position);
        if !p.is_finite() {
            continue;
        }
        min = min.min(p);
        max = max.max(p);
    }
    if !min.is_finite() || !max.is_finite() {
        return Err(BakeError::Message(
            "The mesh has no finite positions".into(),
        ));
    }
    let center = (min + max) * 0.5;
    let extent = (max - min).max_element().max(1e-5);
    Ok((center, extent))
}

fn cube_coverage(edge: f32, feather: f32) -> f32 {
    if feather <= 1e-4 {
        if edge <= 1.0 {
            1.0
        } else {
            0.0
        }
    } else {
        ((1.0 + feather - edge) / feather).clamp(0.0, 1.0)
    }
}

fn triplanar_normal<F>(local: Vec3, geom: Vec3, sample: &F, rotation: Quat, sharpness: f32) -> Vec3
where
    F: Fn([f32; 2]) -> [f32; 3],
{
    let mut weights = Vec3::new(
        geom.x.abs().powf(sharpness),
        geom.y.abs().powf(sharpness),
        geom.z.abs().powf(sharpness),
    );
    let sum = weights.x + weights.y + weights.z;
    if sum <= 1e-6 {
        weights = geom.abs();
    }
    weights /= (weights.x + weights.y + weights.z).max(1e-6);

    let sign_x = if geom.x < 0.0 { -1.0 } else { 1.0 };
    let sign_y = if geom.y < 0.0 { -1.0 } else { 1.0 };
    let sign_z = if geom.z < 0.0 { -1.0 } else { 1.0 };

    let n_x = normalize_or(
        Vec3::from_array(sample([(local.z + 1.0) * 0.5, (1.0 - local.y) * 0.5])),
        Vec3::Z,
    );
    let n_y = normalize_or(
        Vec3::from_array(sample([(local.x + 1.0) * 0.5, (1.0 - local.z) * 0.5])),
        Vec3::Z,
    );
    let n_z = normalize_or(
        Vec3::from_array(sample([(local.x + 1.0) * 0.5, (1.0 - local.y) * 0.5])),
        Vec3::Z,
    );

    let local_x = Vec3::new(n_x.z * sign_x, -n_x.y, n_x.x);
    let local_y = Vec3::new(n_y.x, n_y.z * sign_y, -n_y.y);
    let local_z = Vec3::new(n_z.x, -n_z.y, n_z.z * sign_z);
    let combined = local_x * weights.x + local_y * weights.y + local_z * weights.z;
    normalize_or(rotation * combined, Vec3::Z)
}

fn dilate_normals(
    normals: &mut [[f32; 3]],
    covered: &mut [bool],
    width: usize,
    height: usize,
    passes: u32,
) {
    for _ in 0..passes {
        let mut next = covered.to_vec();
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                if covered[index] {
                    continue;
                }
                let mut sum = Vec3::ZERO;
                let mut count = 0.0f32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                            continue;
                        }
                        let neighbour = ny as usize * width + nx as usize;
                        if covered[neighbour] {
                            sum += Vec3::from_array(normals[neighbour]);
                            count += 1.0;
                        }
                    }
                }
                if count > 0.0 {
                    normals[index] = normalize_or(sum / count, Vec3::Z).to_array();
                    next[index] = true;
                }
            }
        }
        covered.copy_from_slice(&next);
    }
}

fn closest(front: Option<(f32, Vec3)>, back: Option<(f32, Vec3)>, cage: f32) -> Option<Vec3> {
    match (front, back) {
        (Some((front_t, front_normal)), Some((back_t, back_normal))) => {
            let front_distance = (cage - front_t).abs();
            let back_distance = (back_t - cage).abs();
            if front_distance <= back_distance {
                Some(front_normal)
            } else {
                Some(back_normal)
            }
        }
        (Some((_, normal)), None) | (None, Some((_, normal))) => Some(normal),
        (None, None) => None,
    }
}

fn bevel_normal(
    low: &PreparedLow,
    triangle: &LowTriangle,
    p: Vec3,
    vertex_normal: Vec3,
    settings: &BakeSettings,
) -> Vec3 {
    let Some(edges) = low.groups.get(triangle.group as usize) else {
        return vertex_normal;
    };
    let group_normal = low
        .group_normals
        .get(triangle.group as usize)
        .copied()
        .unwrap_or(triangle.face_normal);
    let radius_pct = settings.bevel_radius_pct.clamp(0.01, 2.0);
    let radius = (radius_pct * 0.01 * low.extent).max(1e-6);
    let falloff = settings.bevel_falloff.max(0.05);
    let mut influence = 0.0f32;
    let mut tangential_sum = Vec3::ZERO;
    for edge in edges {
        if p.x < edge.min.x - radius
            || p.x > edge.max.x + radius
            || p.y < edge.min.y - radius
            || p.y > edge.max.y + radius
            || p.z < edge.min.z - radius
            || p.z > edge.max.z + radius
        {
            continue;
        }
        let distance = point_segment_distance(p, edge.a, edge.b);
        if distance >= radius {
            continue;
        }
        let weight = bevel_weight(1.0 - distance / radius, falloff);
        influence = influence.max(weight);
        let tangential = edge.other_normal - group_normal * group_normal.dot(edge.other_normal);
        tangential_sum += tangential * weight;
    }
    if influence <= 0.0 {
        return vertex_normal;
    }
    // Inside the bevel, move the base from the interpolated vertex normal to
    // the flat group normal. At the shared edge this makes both faces produce
    // the same bisector normal instead of each adding a tilt to an already
    // averaged smooth normal. Away from the edge the original shading normal
    // is preserved.
    let base = normalize_or(vertex_normal.lerp(group_normal, influence), group_normal);
    normalize_or(base + tangential_sum, base)
}

/// Smooth bevel profile. `t` is 0 at the outer edge of the bevel and 1 at the
/// geometric edge. `falloff` biases the profile: lower values spread the
/// transition, higher values concentrate it near the edge. Smoothstep keeps
/// both ends C1 so the bevel leaves neither a ring at its start nor a crease
/// at the edge.
pub(crate) fn bevel_weight(t: f32, falloff: f32) -> f32 {
    let t = t.clamp(0.0, 1.0).powf(falloff.max(0.05));
    t * t * (3.0 - 2.0 * t)
}

fn point_segment_distance(point: Vec3, a: Vec3, b: Vec3) -> f32 {
    let segment = b - a;
    let length_squared = segment.length_squared();
    if length_squared < 1e-20 {
        return (point - a).length();
    }
    let t = ((point - a).dot(segment) / length_squared).clamp(0.0, 1.0);
    (point - (a + segment * t)).length()
}

fn encode(normal: Vec3, space: BakeSpace) -> [u8; 4] {
    let normal = normalize_or(normal, Vec3::Z);
    let y = if space == BakeSpace::TangentDirectX {
        -normal.y
    } else {
        normal.y
    };
    let channel = |value: f32| ((value * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
    [channel(normal.x), channel(y), channel(normal.z), 255]
}

fn decode(pixel: [u8; 4], space: BakeSpace) -> Vec3 {
    let mut normal = Vec3::new(
        pixel[0] as f32 / 127.5 - 1.0,
        pixel[1] as f32 / 127.5 - 1.0,
        pixel[2] as f32 / 127.5 - 1.0,
    );
    if space == BakeSpace::TangentDirectX {
        normal.y = -normal.y;
    }
    normalize_or(normal, Vec3::Z)
}

fn downsample(samples: &[u8], resolution: u32, anti_alias: u32, space: BakeSpace) -> Vec<u8> {
    let width = resolution * anti_alias;
    let mut output = vec![0u8; resolution as usize * resolution as usize * 4];
    for y in 0..resolution {
        for x in 0..resolution {
            let mut sum = Vec3::ZERO;
            let mut count = 0u32;
            for sub_y in 0..anti_alias {
                for sub_x in 0..anti_alias {
                    let source =
                        (((y * anti_alias + sub_y) * width + x * anti_alias + sub_x) * 4) as usize;
                    if samples[source + 3] == 0 {
                        continue;
                    }
                    sum += decode(
                        [
                            samples[source],
                            samples[source + 1],
                            samples[source + 2],
                            samples[source + 3],
                        ],
                        space,
                    );
                    count += 1;
                }
            }
            if count > 0 {
                let normal = normalize_or(sum, Vec3::Z);
                let encoded = encode(normal, space);
                let target = ((y * resolution + x) * 4) as usize;
                output[target..target + 4].copy_from_slice(&encoded);
            }
        }
    }
    output
}

/// Multi-source BFS that copies the nearest baked texel into empty texels. Only
/// pixels within `passes` of an island are visited.
fn dilate(pixels: &mut [u8], size: u32, passes: u32) {
    if passes == 0 {
        return;
    }
    let pixel_count = size as usize * size as usize;
    let mut filled = vec![false; pixel_count];
    for index in 0..pixel_count {
        filled[index] = pixels[index * 4 + 3] > 0;
    }

    let mut queue = VecDeque::new();
    for index in 0..pixel_count {
        if !filled[index] {
            continue;
        }
        let x = index % size as usize;
        let y = index / size as usize;
        for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= size as i32 || ny >= size as i32 {
                continue;
            }
            let neighbor = ny as usize * size as usize + nx as usize;
            if !filled[neighbor] {
                queue.push_back((neighbor, index, 1u32));
            }
        }
    }

    while let Some((target, source, distance)) = queue.pop_front() {
        if filled[target] {
            continue;
        }
        let color = [
            pixels[source * 4],
            pixels[source * 4 + 1],
            pixels[source * 4 + 2],
            pixels[source * 4 + 3],
        ];
        pixels[target * 4..target * 4 + 4].copy_from_slice(&color);
        filled[target] = true;
        if distance >= passes {
            continue;
        }
        let x = target % size as usize;
        let y = target / size as usize;
        for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= size as i32 || ny >= size as i32 {
                continue;
            }
            let neighbor = ny as usize * size as usize + nx as usize;
            if !filled[neighbor] {
                queue.push_back((neighbor, source, distance + 1));
            }
        }
    }
}

fn fill_background(pixels: &mut [u8], space: BakeSpace) {
    let flat = encode(Vec3::Z, space);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        if pixel[3] == 0 {
            pixel.copy_from_slice(&flat);
        }
    }
}

fn pixel_bbox(
    triangle: &LowTriangle,
    shift: Vec2,
    width: u32,
    height: u32,
    bottom_left: bool,
) -> (i32, i32, i32, i32) {
    let uv = [
        triangle.uv[0] + shift,
        triangle.uv[1] + shift,
        triangle.uv[2] + shift,
    ];
    let min_u = uv[0].x.min(uv[1].x).min(uv[2].x);
    let max_u = uv[0].x.max(uv[1].x).max(uv[2].x);
    let min_v = uv[0].y.min(uv[1].y).min(uv[2].y);
    let max_v = uv[0].y.max(uv[1].y).max(uv[2].y);
    let x0 = (min_u * width as f32 - 0.5).floor() as i32 - 1;
    let x1 = (max_u * width as f32 - 0.5).ceil() as i32 + 1;
    let (y0, y1) = if bottom_left {
        (
            ((1.0 - max_v) * height as f32 - 0.5).floor() as i32 - 1,
            ((1.0 - min_v) * height as f32 - 0.5).ceil() as i32 + 1,
        )
    } else {
        (
            (min_v * height as f32 - 0.5).floor() as i32 - 1,
            (max_v * height as f32 - 0.5).ceil() as i32 + 1,
        )
    };
    (x0, y0, x1, y1)
}

fn barycentric(triangle: &LowTriangle, shift: Vec2, u: f32, v: f32) -> Option<[f32; 3]> {
    let a = triangle.uv[0] + shift;
    let b = triangle.uv[1] + shift;
    let c = triangle.uv[2] + shift;
    let point = Vec2::new(u, v);
    let edge0 = b - a;
    let edge1 = c - a;
    let relative = point - a;
    let determinant = edge0.x * edge1.y - edge1.x * edge0.y;
    if determinant.abs() < 1e-12 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let weight_b = (relative.x * edge1.y - edge1.x * relative.y) * inverse;
    let weight_c = (edge0.x * relative.y - relative.x * edge0.y) * inverse;
    Some([1.0 - weight_b - weight_c, weight_b, weight_c])
}
