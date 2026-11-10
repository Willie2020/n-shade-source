//! Low-poly preparation: UDIM filtering, tangent frames and bevel edge groups.

use std::collections::{HashMap, HashSet, VecDeque};

use glam::{Vec2, Vec3};

use crate::{BakeError, BakeMesh, BakeMode, BakeSettings, Result};

/// All integer UV shifts a triangle may need to appear on both sides of a UV
/// seam. Index 0 is always the unshifted triangle.
pub(crate) const WRAP_SHIFTS: [Vec2; 9] = [
    Vec2::ZERO,
    Vec2::new(-1.0, 0.0),
    Vec2::new(0.0, -1.0),
    Vec2::new(-1.0, -1.0),
    Vec2::new(1.0, 0.0),
    Vec2::new(0.0, 1.0),
    Vec2::new(1.0, 1.0),
    Vec2::new(-1.0, 1.0),
    Vec2::new(1.0, -1.0),
];

/// Triangles whose face normals are within this dot product are treated as one
/// flat surface (about 1.8°) and triangulation diagonals are not beveled.
const COPLANAR_DOT: f32 = 0.9995;

pub(crate) struct EdgeSegment {
    pub a: Vec3,
    pub b: Vec3,
    pub other_normal: Vec3,
    pub min: Vec3,
    pub max: Vec3,
}

pub(crate) struct LowTriangle {
    pub p: [Vec3; 3],
    pub n: [Vec3; 3],
    pub t: [Vec3; 3],
    pub sign: [f32; 3],
    pub uv: [Vec2; 3],
    pub face_normal: Vec3,
    pub ids: [u32; 3],
    pub group: u32,
    pub wrap_mask: u16,
}

pub(crate) struct PreparedLow {
    pub triangles: Vec<LowTriangle>,
    /// Boundary edges for each coplanar face group. Only populated for bevel
    /// mode.
    pub groups: Vec<Vec<EdgeSegment>>,
    /// Area-weighted shading normal of each coplanar face group. Bevel shading
    /// uses these instead of interpolated vertex normals so both sides of a
    /// hard edge converge on the same normal.
    pub group_normals: Vec<Vec3>,
    pub extent: f32,
}

struct LowVertex {
    position: Vec3,
    normal: Vec3,
    uv: Vec2,
    tangent: Vec3,
    sign: f32,
}

pub(crate) fn prepare_low(mesh: &BakeMesh, settings: &BakeSettings) -> Result<PreparedLow> {
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
    let normals = ensure_normals(mesh);
    let uvs: Vec<Vec2> = mesh.uvs.iter().map(|uv| Vec2::from_array(*uv)).collect();

    // Blender-style UDIMs encode the tile number in U (sometimes V). Normalize
    // that absolute offset away so the selected tile becomes local 0–1 UVs.
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
    let mut vertices: Vec<LowVertex> = Vec::new();
    let mut remap: HashMap<(u32, i32, i32), u32> = HashMap::new();
    let mut indices: Vec<u32> = Vec::new();

    for triangle in mesh.indices.as_chunks::<3>().0 {
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
        let key_cell = (cell.x as i32, cell.y as i32);
        let mut remapped = [0u32; 3];
        for (corner, index) in [a, b, c].into_iter().enumerate() {
            let key = (index, key_cell.0, key_cell.1);
            let vertex = *remap.entry(key).or_insert_with(|| {
                let id = vertices.len() as u32;
                vertices.push(LowVertex {
                    position: positions[index as usize],
                    normal: normals[index as usize],
                    uv: local[corner],
                    tangent: Vec3::ZERO,
                    sign: 1.0,
                });
                id
            });
            remapped[corner] = vertex;
        }
        indices.extend_from_slice(&remapped);
    }

    if indices.is_empty() {
        return Err(BakeError::Message(format!(
            "No UV triangles in tile {target_tile}. The mesh may not use that UDIM tile."
        )));
    }

    // Area-weighted tangents from the filtered (tile-local) UV layout.
    let mut tangent_sum = vec![Vec3::ZERO; vertices.len()];
    let mut bitangent_sum = vec![Vec3::ZERO; vertices.len()];
    for triangle in indices.as_chunks::<3>().0 {
        let [i0, i1, i2] = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let p0 = vertices[i0].position;
        let p1 = vertices[i1].position;
        let p2 = vertices[i2].position;
        let uv0 = vertices[i0].uv;
        let uv1 = vertices[i1].uv;
        let uv2 = vertices[i2].uv;
        let dp1 = p1 - p0;
        let dp2 = p2 - p0;
        let duv1 = uv1 - uv0;
        let duv2 = uv2 - uv0;
        let determinant = duv1.x * duv2.y - duv2.x * duv1.y;
        if determinant.abs() < 1e-12 {
            continue;
        }
        let inverse = 1.0 / determinant;
        let tangent = (dp1 * duv2.y - dp2 * duv1.y) * inverse;
        let bitangent = (dp2 * duv1.x - dp1 * duv2.x) * inverse;
        for index in [i0, i1, i2] {
            tangent_sum[index] += tangent;
            bitangent_sum[index] += bitangent;
        }
    }
    for (index, vertex) in vertices.iter_mut().enumerate() {
        let normal = normalize_or(vertex.normal, Vec3::Y);
        let tangent = tangent_sum[index] - normal * normal.dot(tangent_sum[index]);
        vertex.tangent = normalize_or(tangent, arbitrary_tangent(normal));
        vertex.sign = if normal.cross(vertex.tangent).dot(bitangent_sum[index]) < 0.0 {
            -1.0
        } else {
            1.0
        };
        vertex.normal = normal;
    }

    let extent = mesh_extent(&positions);
    let mut triangles = Vec::with_capacity(indices.len() / 3);
    for triangle in indices.as_chunks::<3>().0 {
        let [i0, i1, i2] = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let p = [
            vertices[i0].position,
            vertices[i1].position,
            vertices[i2].position,
        ];
        let face = (p[1] - p[0]).cross(p[2] - p[0]);
        if face.length_squared() < 1e-20 {
            continue;
        }
        let uv = [vertices[i0].uv, vertices[i1].uv, vertices[i2].uv];
        let wrap_mask = wrap_mask(&uv);
        if wrap_mask == 0 {
            continue;
        }
        triangles.push(LowTriangle {
            p,
            n: [
                vertices[i0].normal,
                vertices[i1].normal,
                vertices[i2].normal,
            ],
            t: [
                vertices[i0].tangent,
                vertices[i1].tangent,
                vertices[i2].tangent,
            ],
            sign: [vertices[i0].sign, vertices[i1].sign, vertices[i2].sign],
            uv,
            face_normal: face.normalize(),
            ids: [triangle[0], triangle[1], triangle[2]],
            group: 0,
            wrap_mask,
        });
    }

    if triangles.is_empty() {
        return Err(BakeError::Message(
            "The low-poly UV layout has no rasterizable triangles".into(),
        ));
    }

    let (groups, group_normals) = if settings.mode == BakeMode::Bevel {
        build_bevel_groups(&mut triangles, &vertices, extent)
    } else {
        (Vec::new(), Vec::new())
    };

    Ok(PreparedLow {
        triangles,
        groups,
        group_normals,
        extent,
    })
}

/// Group coplanar triangles into flat surfaces and collect the boundary edges
/// of each group. A boundary edge carries the normal of the adjacent surface,
/// which is the normal the bevel blends toward.
fn build_bevel_groups(
    triangles: &mut [LowTriangle],
    vertices: &[LowVertex],
    extent: f32,
) -> (Vec<Vec<EdgeSegment>>, Vec<Vec3>) {
    let eps = (extent * 1e-5).max(1e-7);
    let mut weld_lookup: HashMap<[i64; 3], u32> = HashMap::new();
    let mut welded = Vec::with_capacity(vertices.len());
    for vertex in vertices {
        let key = [
            (vertex.position.x / eps).round() as i64,
            (vertex.position.y / eps).round() as i64,
            (vertex.position.z / eps).round() as i64,
        ];
        let next = weld_lookup.len() as u32;
        let id = *weld_lookup.entry(key).or_insert(next);
        welded.push(id);
    }

    let face_normals: Vec<Vec3> = triangles.iter().map(|tri| tri.face_normal).collect();
    let mut edge_faces: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (index, triangle) in triangles.iter().enumerate() {
        for edge in 0..3 {
            let key = edge_key(triangle, edge, &welded);
            edge_faces.entry(key).or_default().push(index as u32);
        }
    }

    let mut group_of = vec![u32::MAX; triangles.len()];
    let mut group_count = 0usize;
    for start in 0..triangles.len() {
        if group_of[start] != u32::MAX {
            continue;
        }
        let group = group_count as u32;
        group_count += 1;
        group_of[start] = group;
        let mut queue = VecDeque::new();
        queue.push_back(start as u32);
        while let Some(current) = queue.pop_front() {
            let current_index = current as usize;
            for edge in 0..3 {
                let key = edge_key(&triangles[current_index], edge, &welded);
                let Some(faces) = edge_faces.get(&key) else {
                    continue;
                };
                for &other in faces {
                    if other == current {
                        continue;
                    }
                    let other_index = other as usize;
                    if group_of[other_index] != u32::MAX {
                        continue;
                    }
                    if face_normals[current_index].dot(face_normals[other_index]) >= COPLANAR_DOT {
                        group_of[other_index] = group;
                        queue.push_back(other);
                    }
                }
            }
        }
    }

    let mut group_edges: Vec<Vec<EdgeSegment>> = (0..group_count).map(|_| Vec::new()).collect();
    let mut group_normals = vec![Vec3::ZERO; group_count];
    for (index, triangle) in triangles.iter().enumerate() {
        let area = (triangle.p[1] - triangle.p[0])
            .cross(triangle.p[2] - triangle.p[0])
            .length();
        group_normals[group_of[index] as usize] += triangle.face_normal * area;
    }
    for normal in &mut group_normals {
        *normal = normalize_or(*normal, Vec3::Y);
    }

    let mut seen: Vec<HashSet<(u32, u32)>> = (0..group_count).map(|_| HashSet::new()).collect();
    for (index, triangle) in triangles.iter_mut().enumerate() {
        let group = group_of[index];
        triangle.group = group;
        let group_index = group as usize;
        for edge in 0..3 {
            let key = edge_key(triangle, edge, &welded);
            let Some(faces) = edge_faces.get(&key) else {
                continue;
            };
            let Some(&other) = faces.iter().find(|&&face| group_of[face as usize] != group) else {
                continue;
            };
            if !seen[group_index].insert(key) {
                continue;
            }
            let a = triangle.p[edge];
            let b = triangle.p[(edge + 1) % 3];
            group_edges[group_index].push(EdgeSegment {
                a,
                b,
                other_normal: group_normals[group_of[other as usize] as usize],
                min: a.min(b),
                max: a.max(b),
            });
        }
    }
    (group_edges, group_normals)
}

fn edge_key(triangle: &LowTriangle, edge: usize, welded: &[u32]) -> (u32, u32) {
    let a = welded[triangle.ids[edge] as usize];
    let b = welded[triangle.ids[(edge + 1) % 3] as usize];
    (a.min(b), a.max(b))
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

pub(crate) fn ensure_normals(mesh: &BakeMesh) -> Vec<Vec3> {
    if mesh.normals.len() == mesh.positions.len() {
        return mesh
            .normals
            .iter()
            .map(|normal| normalize_or(Vec3::from_array(*normal), Vec3::Y))
            .collect();
    }
    let mut sums = vec![Vec3::ZERO; mesh.positions.len()];
    for triangle in mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = [triangle[0], triangle[1], triangle[2]];
        let (Some(&pa), Some(&pb), Some(&pc)) = (
            mesh.positions.get(a as usize),
            mesh.positions.get(b as usize),
            mesh.positions.get(c as usize),
        ) else {
            continue;
        };
        let normal = (Vec3::from_array(pb) - Vec3::from_array(pa))
            .cross(Vec3::from_array(pc) - Vec3::from_array(pa));
        for index in [a, b, c] {
            sums[index as usize] += normal;
        }
    }
    sums.into_iter()
        .map(|normal| normalize_or(normal, Vec3::Y))
        .collect()
}

fn mesh_extent(positions: &[Vec3]) -> f32 {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for position in positions {
        if position.is_finite() {
            min = min.min(*position);
            max = max.max(*position);
        }
    }
    if min.is_finite() && max.is_finite() {
        (max - min).max_element().max(1e-5)
    } else {
        1.0
    }
}

pub(crate) fn normalize_or(value: Vec3, fallback: Vec3) -> Vec3 {
    if value.length_squared() > 1e-20 {
        value.normalize()
    } else {
        fallback
    }
}

pub(crate) fn arbitrary_tangent(normal: Vec3) -> Vec3 {
    let axis = if normal.x.abs() < 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    normalize_or(axis - normal * normal.dot(axis), Vec3::Z)
}
