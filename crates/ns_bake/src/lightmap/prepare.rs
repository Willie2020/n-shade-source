//! UV preparation and rasterization for lightmaps.

use glam::{Vec2, Vec3};

use crate::mesh::{ensure_normals, normalize_or, WRAP_SHIFTS};
use crate::{BakeError, BakeMesh, Result};

use super::geometry::terminator_shift;
use super::{Features, LightmapSettings};

struct LightmapTriangle {
    p: [Vec3; 3],
    n: [Vec3; 3],
    uv: [Vec2; 3],
    face_normal: Vec3,
    wrap_mask: u16,
    /// Connected surface patch, used to stop the v2 denoiser and gutter fill at
    /// UV seams that are also geometry seams.
    chart: u32,
}

pub(crate) struct PreparedLightmap {
    triangles: Vec<LightmapTriangle>,
    pub(crate) extent: f32,
    /// Center of the mesh bounds in raw mesh coordinates. Scene lights are
    /// stored in normalized mesh space and use this to map back.
    pub(crate) center: Vec3,
    pub(crate) uv_bottom_left: bool,
}

impl PreparedLightmap {
    pub(crate) fn triangle_count(&self) -> usize {
        self.triangles.len()
    }
}

pub(crate) struct GBuffer {
    pub(crate) positions: Vec<Vec3>,
    pub(crate) normals: Vec<Vec3>,
    pub(crate) covered: Vec<bool>,
    /// [v2] Flat triangle normal, oriented to agree with `normals`.
    pub(crate) geo_normals: Vec<Vec3>,
    /// [v2] Connected surface patch; `u32::MAX` when uncovered.
    pub(crate) chart_ids: Vec<u32>,
    /// [v2] Texel was shaded at the closest point on a triangle outside it.
    pub(crate) edge: Vec<bool>,
}

pub(crate) fn prepare_lightmap(
    mesh: &BakeMesh,
    settings: &LightmapSettings,
) -> Result<PreparedLightmap> {
    if mesh.positions.is_empty() || mesh.indices.len() < 3 {
        return Err(BakeError::Message(
            "The low-poly mesh has no triangles".into(),
        ));
    }
    if mesh.uvs.len() != mesh.positions.len() {
        return Err(BakeError::Message(
            "The low-poly mesh has no per-vertex UV coordinates".into(),
        ));
    }

    let positions: Vec<Vec3> = mesh
        .positions
        .iter()
        .map(|position| Vec3::from_array(*position))
        .collect();
    let has_normals = mesh.normals.len() == mesh.positions.len();
    let normals = ensure_normals(mesh);
    let uvs: Vec<Vec2> = mesh.uvs.iter().map(|uv| Vec2::from_array(*uv)).collect();
    let charts = vertex_charts(&mesh.indices, positions.len());

    let mut min_uv = Vec2::splat(f32::INFINITY);
    for uv in &uvs {
        if uv.is_finite() {
            min_uv = min_uv.min(*uv);
        }
    }
    let mesh_offset = Vec2::new(
        if min_uv.x >= 1000.0 {
            min_uv.x.floor()
        } else {
            0.0
        },
        if min_uv.y >= 1000.0 {
            min_uv.y.floor()
        } else {
            0.0
        },
    );

    let target_tile = settings.tile.max(1001) as i64;
    let mut triangles = Vec::new();
    for (triangle_index, triangle) in mesh.indices.as_chunks::<3>().0.iter().enumerate() {
        if let Some(group) = settings.group {
            if mesh.triangle_group(triangle_index) != group {
                continue;
            }
        }
        let [a, b, c] = [triangle[0], triangle[1], triangle[2]];
        if a as usize >= positions.len()
            || b as usize >= positions.len()
            || c as usize >= positions.len()
        {
            continue;
        }
        let mut uv = [uvs[a as usize], uvs[b as usize], uvs[c as usize]];
        for value in &mut uv {
            *value -= mesh_offset;
        }
        if uv.iter().any(|value| !value.is_finite()) {
            continue;
        }
        let centroid = (uv[0] + uv[1] + uv[2]) / 3.0;
        let cell = Vec2::new(centroid.x.floor().max(0.0), centroid.y.floor().max(0.0));
        let tile = 1001 + cell.x as i64 + 10 * cell.y as i64;
        if tile != target_tile {
            continue;
        }
        let local = [uv[0] - cell, uv[1] - cell, uv[2] - cell];
        let wrap_mask = wrap_mask(&local);
        if wrap_mask == 0 {
            continue;
        }
        let p = [
            positions[a as usize],
            positions[b as usize],
            positions[c as usize],
        ];
        let face = (p[1] - p[0]).cross(p[2] - p[0]);
        if face.length_squared() < 1e-20 {
            continue;
        }
        let face_normal = face.normalize();
        let shading_normals = if has_normals {
            [
                normals[a as usize],
                normals[b as usize],
                normals[c as usize],
            ]
        } else {
            [face_normal; 3]
        };
        triangles.push(LightmapTriangle {
            p,
            n: shading_normals,
            uv: local,
            face_normal,
            wrap_mask,
            chart: charts[a as usize],
        });
    }
    if triangles.is_empty() {
        let group = settings
            .group
            .map(|group| format!(" for separated group {group}"))
            .unwrap_or_default();
        return Err(BakeError::Message(format!(
            "No UV triangles in tile {target_tile}{group}. The mesh may not use that UDIM tile."
        )));
    }

    let (bounds_min, bounds_max) = mesh_bounds(&positions);
    Ok(PreparedLightmap {
        triangles,
        extent: (bounds_max - bounds_min).max_element().max(1e-5),
        center: (bounds_min + bounds_max) * 0.5,
        uv_bottom_left: settings.uv_bottom_left,
    })
}

/// Root vertex of each vertex's connected component. Importers split vertices
/// at UV seams, so a component is the smooth surface patch the artist sees.
pub(crate) fn vertex_charts(indices: &[u32], vertex_count: usize) -> Vec<u32> {
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            parent[x as usize] = parent[parent[x as usize] as usize];
            x = parent[x as usize];
        }
        x
    }
    let mut parent: Vec<u32> = (0..vertex_count as u32).collect();
    for triangle in indices.as_chunks::<3>().0 {
        if triangle.iter().any(|index| *index as usize >= vertex_count) {
            continue;
        }
        for index in 1..3 {
            let a = find(&mut parent, triangle[0]);
            let b = find(&mut parent, triangle[index]);
            if a != b {
                parent[b as usize] = a;
            }
        }
    }
    (0..vertex_count as u32)
        .map(|vertex| find(&mut parent, vertex))
        .collect()
}

fn wrap_mask(uv: &[Vec2; 3]) -> u16 {
    let min = uv[0].min(uv[1]).min(uv[2]);
    let max = uv[0].max(uv[1]).max(uv[2]);
    let mut mask = 0u16;
    for (index, shift) in WRAP_SHIFTS.iter().enumerate() {
        let low = min + *shift;
        let high = max + *shift;
        if high.x > 0.0 && low.x < 1.0 && high.y > 0.0 && low.y < 1.0 {
            mask |= 1 << index;
        }
    }
    mask
}

fn mesh_bounds(positions: &[Vec3]) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for position in positions {
        if position.is_finite() {
            min = min.min(*position);
            max = max.max(*position);
        }
    }
    if min.is_finite() && max.is_finite() {
        (min, max)
    } else {
        (Vec3::ZERO, Vec3::ZERO)
    }
}

/// v1 rasterizer: centre coverage only. Kept so `Legacy` is byte-compatible.
pub(crate) fn rasterize(prepared: &PreparedLightmap, resolution: u32) -> GBuffer {
    rasterize_with(prepared, resolution, &Features::legacy())
}

/// v2 rasterizer: centre coverage with a shared-edge tolerance, then a one-texel
/// edge extension shaded at the closest point on the triangle.
pub(crate) fn rasterize_v2(
    prepared: &PreparedLightmap,
    resolution: u32,
    features: &Features,
) -> GBuffer {
    rasterize_with(prepared, resolution, features)
}

const BARY_EPS: f32 = -1.0e-5;
const EDGE_REACH: f32 = 0.75;

fn rasterize_with(prepared: &PreparedLightmap, resolution: u32, features: &Features) -> GBuffer {
    let size = resolution as usize;
    let texel_count = size * size;
    let mut gbuffer = GBuffer {
        positions: vec![Vec3::ZERO; texel_count],
        normals: vec![Vec3::Y; texel_count],
        covered: vec![false; texel_count],
        geo_normals: vec![Vec3::Y; texel_count],
        chart_ids: vec![u32::MAX; texel_count],
        edge: vec![false; texel_count],
    };
    let mut edge_distance = features
        .edge_extension
        .then(|| vec![f32::INFINITY; texel_count]);

    // Pass 1: the texel centre is inside (within the shared-edge tolerance).
    for triangle in &prepared.triangles {
        for (shift_index, shift) in WRAP_SHIFTS.iter().enumerate() {
            if triangle.wrap_mask & (1 << shift_index) == 0 {
                continue;
            }
            let (x0, y0, x1, y1) =
                pixel_bbox(triangle, *shift, resolution, prepared.uv_bottom_left, 0);
            let x_start = x0.max(0);
            let x_end = x1.min(resolution as i32 - 1);
            let y_start = y0.max(0);
            let y_end = y1.min(resolution as i32 - 1);
            if x_end < x_start || y_end < y_start {
                continue;
            }
            for y in y_start..=y_end {
                for x in x_start..=x_end {
                    let index = y as usize * size + x as usize;
                    if gbuffer.covered[index] {
                        continue;
                    }
                    let u = (x as f32 + 0.5) / resolution as f32;
                    let v = if prepared.uv_bottom_left {
                        1.0 - (y as f32 + 0.5) / resolution as f32
                    } else {
                        (y as f32 + 0.5) / resolution as f32
                    };
                    let Some(weights) = barycentric(&triangle.uv, *shift, u, v) else {
                        continue;
                    };
                    if weights.iter().any(|weight| *weight < BARY_EPS) {
                        continue;
                    }
                    shade_texel(&mut gbuffer, index, triangle, weights, false, features);
                }
            }
        }
    }

    // Pass 2: nearest triangle within 0.75 texel of an uncovered centre. It may
    // replace another edge texel when it is closer, but never an interior one.
    if let (true, Some(distance)) = (features.edge_extension, edge_distance.as_mut()) {
        for triangle in &prepared.triangles {
            for (shift_index, shift) in WRAP_SHIFTS.iter().enumerate() {
                if triangle.wrap_mask & (1 << shift_index) == 0 {
                    continue;
                }
                let (x0, y0, x1, y1) =
                    pixel_bbox(triangle, *shift, resolution, prepared.uv_bottom_left, 1);
                let x_start = x0.max(0);
                let x_end = x1.min(resolution as i32 - 1);
                let y_start = y0.max(0);
                let y_end = y1.min(resolution as i32 - 1);
                if x_end < x_start || y_end < y_start {
                    continue;
                }
                let uv = [
                    triangle.uv[0] + *shift,
                    triangle.uv[1] + *shift,
                    triangle.uv[2] + *shift,
                ];
                for y in y_start..=y_end {
                    for x in x_start..=x_end {
                        let index = y as usize * size + x as usize;
                        if gbuffer.covered[index] && !gbuffer.edge[index] {
                            continue;
                        }
                        let u = (x as f32 + 0.5) / resolution as f32;
                        let v = if prepared.uv_bottom_left {
                            1.0 - (y as f32 + 0.5) / resolution as f32
                        } else {
                            (y as f32 + 0.5) / resolution as f32
                        };
                        let point = Vec2::new(u, v);
                        let weights = closest_bary(point, uv[0], uv[1], uv[2]);
                        let closest = uv[0] * weights[0] + uv[1] * weights[1] + uv[2] * weights[2];
                        let texel = point * resolution as f32;
                        let target = closest * resolution as f32;
                        let d = (texel - target).length();
                        if d <= EDGE_REACH && d < distance[index] {
                            distance[index] = d;
                            shade_texel(&mut gbuffer, index, triangle, weights, true, features);
                        }
                    }
                }
            }
        }
    }

    gbuffer
}

fn shade_texel(
    gbuffer: &mut GBuffer,
    index: usize,
    triangle: &LightmapTriangle,
    weights: [f32; 3],
    is_edge: bool,
    features: &Features,
) {
    let position =
        triangle.p[0] * weights[0] + triangle.p[1] * weights[1] + triangle.p[2] * weights[2];
    let mut normal =
        triangle.n[0] * weights[0] + triangle.n[1] * weights[1] + triangle.n[2] * weights[2];
    normal = normalize_or(normal, triangle.face_normal);
    let mut geometric = triangle.face_normal;
    if geometric.dot(normal) < 0.0 {
        geometric = -geometric;
    }
    let position = if features.terminator_fix {
        terminator_shift(position, weights, triangle.p, triangle.n)
    } else {
        position
    };
    gbuffer.positions[index] = position;
    gbuffer.normals[index] = normal;
    gbuffer.geo_normals[index] = geometric;
    gbuffer.chart_ids[index] = triangle.chart;
    gbuffer.covered[index] = true;
    gbuffer.edge[index] = is_edge;
}

/// Closest point on a triangle in UV space, returned as barycentrics.
///
/// This is the Ericson/Real-Time Collision Detection formulation; it covers the
/// three vertex regions, the three edge regions and the interior.
pub(crate) fn closest_bary(point: Vec2, a: Vec2, b: Vec2, c: Vec2) -> [f32; 3] {
    let ab = b - a;
    let ac = c - a;
    let ap = point - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return [1.0, 0.0, 0.0];
    }
    let bp = point - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return [0.0, 1.0, 0.0];
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return [1.0 - v, v, 0.0];
    }
    let cp = point - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return [0.0, 0.0, 1.0];
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return [1.0 - w, 0.0, w];
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return [0.0, 1.0 - w, w];
    }
    let inverse = 1.0 / (va + vb + vc);
    let v = vb * inverse;
    let w = vc * inverse;
    [1.0 - v - w, v, w]
}

fn pixel_bbox(
    triangle: &LightmapTriangle,
    shift: Vec2,
    resolution: u32,
    bottom_left: bool,
    grow: i32,
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
    let size = resolution as f32;
    let x0 = (min_u * size - 0.5).floor() as i32 - 1 - grow;
    let x1 = (max_u * size - 0.5).ceil() as i32 + 1 + grow;
    let (y0, y1) = if bottom_left {
        (
            ((1.0 - max_v) * size - 0.5).floor() as i32 - 1 - grow,
            ((1.0 - min_v) * size - 0.5).ceil() as i32 + 1 + grow,
        )
    } else {
        (
            (min_v * size - 0.5).floor() as i32 - 1 - grow,
            (max_v * size - 0.5).ceil() as i32 + 1 + grow,
        )
    };
    (x0, y0, x1, y1)
}

fn barycentric(uv: &[Vec2; 3], shift: Vec2, u: f32, v: f32) -> Option<[f32; 3]> {
    let a = uv[0] + shift;
    let b = uv[1] + shift;
    let c = uv[2] + shift;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BakeMesh;

    fn quad() -> BakeMesh {
        BakeMesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            indices: vec![0, 1, 2, 0, 2, 3],
            ..Default::default()
        }
    }

    fn small_triangle() -> BakeMesh {
        BakeMesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![[0.25, 0.25], [0.75, 0.25], [0.25, 0.75]],
            indices: vec![0, 1, 2],
            ..Default::default()
        }
    }

    #[test]
    fn group_filter_rasterizes_only_that_group() {
        let mut mesh = quad();
        mesh.triangle_groups = vec![0, 1];

        let settings = LightmapSettings {
            group: Some(0),
            ..Default::default()
        };
        assert_eq!(
            prepare_lightmap(&mesh, &settings)
                .expect("group 0 has one triangle")
                .triangle_count(),
            1
        );

        let settings = LightmapSettings {
            group: Some(1),
            ..Default::default()
        };
        assert_eq!(
            prepare_lightmap(&mesh, &settings)
                .expect("group 1 has one triangle")
                .triangle_count(),
            1
        );

        let settings = LightmapSettings {
            group: Some(2),
            ..Default::default()
        };
        let error = prepare_lightmap(&mesh, &settings)
            .err()
            .expect("group 2 is empty");
        assert!(error.to_string().contains("separated group 2"));
    }

    #[test]
    fn closest_bary_matches_known_points() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(1.0, 0.0);
        let c = Vec2::new(0.0, 1.0);
        assert_eq!(closest_bary(a, a, b, c), [1.0, 0.0, 0.0]);
        assert_eq!(closest_bary(b, a, b, c), [0.0, 1.0, 0.0]);
        let inside = closest_bary(Vec2::new(0.25, 0.25), a, b, c);
        assert!((inside[1] - 0.25).abs() < 1e-5);
        assert!((inside[2] - 0.25).abs() < 1e-5);
        let edge = closest_bary(Vec2::new(0.5, 0.5), a, b, c);
        assert!((edge[1] - 0.5).abs() < 1e-5);
        assert!((edge[2] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn charts_split_where_vertices_are_not_shared() {
        assert_eq!(vertex_charts(&[0, 1, 2], 3), vec![0, 0, 0]);
        let split = vertex_charts(&[0, 1, 2, 3, 4, 5], 6);
        assert_eq!(split.len(), 6);
        assert_ne!(split[0], split[3], "disconnected triangles stay apart");
        let joined = vertex_charts(&[0, 1, 2, 0, 2, 3], 4);
        assert!(joined.iter().all(|chart| *chart == joined[0]));
    }

    #[test]
    fn v2_has_no_pinholes_along_shared_edges() {
        let prepared = prepare_lightmap(&quad(), &LightmapSettings::default()).expect("prepared");
        let features = LightmapSettings::default().features();
        let gbuffer = rasterize_v2(&prepared, 32, &features);
        for y in 1..31 {
            for x in 1..31 {
                assert!(
                    gbuffer.covered[y * 32 + x],
                    "pinhole at {x},{y} on the shared diagonal"
                );
            }
        }
    }

    #[test]
    fn edge_extension_covers_texels_outside_the_triangle() {
        let prepared =
            prepare_lightmap(&small_triangle(), &LightmapSettings::default()).expect("prepared");
        let legacy = rasterize(&prepared, 32);
        let v2 = rasterize_v2(&prepared, 32, &LightmapSettings::default().features());
        let legacy_count = legacy.covered.iter().filter(|covered| **covered).count();
        let v2_count = v2.covered.iter().filter(|covered| **covered).count();
        assert!(v2_count > legacy_count, "{v2_count} vs {legacy_count}");
        assert!(v2.edge.iter().any(|edge| *edge));
        assert!(legacy.edge.iter().all(|edge| !*edge));
        for (index, covered) in legacy.covered.iter().enumerate() {
            assert!(
                !*covered || v2.covered[index],
                "edge extension dropped a texel"
            );
        }
    }

    #[test]
    fn geometric_normals_agree_with_shading_normals() {
        let prepared = prepare_lightmap(&quad(), &LightmapSettings::default()).expect("prepared");
        let gbuffer = rasterize_v2(&prepared, 8, &LightmapSettings::default().features());
        for index in 0..gbuffer.covered.len() {
            if gbuffer.covered[index] {
                assert!(gbuffer.geo_normals[index].dot(gbuffer.normals[index]) > 0.0);
            }
        }
    }
}
