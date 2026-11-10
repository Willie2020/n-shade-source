//! High-poly acceleration structure and two-sided ray casting.

use glam::Vec3;

use crate::mesh::{ensure_normals, normalize_or};
use crate::{BakeError, BakeMesh, Result};

struct Triangle {
    indices: [u32; 3],
    min: Vec3,
    max: Vec3,
    centroid: Vec3,
    face_normal: Vec3,
}

#[derive(Clone, Copy)]
struct BvhNode {
    min: Vec3,
    max: Vec3,
    left: u32,
    right: u32,
    start: u32,
    count: u32,
}

impl Default for BvhNode {
    fn default() -> Self {
        Self {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
            left: 0,
            right: 0,
            start: 0,
            count: 0,
        }
    }
}

pub(crate) struct Bvh {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    has_normals: bool,
    triangles: Vec<Triangle>,
    nodes: Vec<BvhNode>,
    order: Vec<u32>,
}

/// Closest hit along a ray.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Hit {
    pub t: f32,
    pub point: Vec3,
    /// Interpolated vertex normal for shading.
    pub normal: Vec3,
    /// Geometric triangle normal, stable for ray offsets.
    pub face_normal: Vec3,
}

/// Flattened triangle data for the optional GPU path tracer.
#[cfg(feature = "gpu")]
pub(crate) struct GpuTriangle {
    pub p: [Vec3; 3],
    pub n: [Vec3; 3],
}

/// Flattened BVH node. Leaf nodes use `start..start + count` in the reordered
/// triangle array produced by [`Bvh::gpu_geometry`].
#[cfg(feature = "gpu")]
#[derive(Clone, Copy)]
pub(crate) struct GpuNode {
    pub min: Vec3,
    pub max: Vec3,
    pub left: u32,
    pub right: u32,
    pub start: u32,
    pub count: u32,
}

impl Bvh {
    pub(crate) fn new(mesh: BakeMesh) -> Result<Self> {
        Self::from_mesh(&mesh)
    }

    /// Build from borrowed geometry, avoiding a full mesh clone for lightmaps.
    pub(crate) fn from_mesh(mesh: &BakeMesh) -> Result<Self> {
        let positions: Vec<Vec3> = mesh
            .positions
            .iter()
            .map(|position| Vec3::from_array(*position))
            .collect();
        let normals = ensure_normals(mesh);
        let has_normals = mesh.normals.len() == mesh.positions.len();
        let mut triangles = Vec::new();
        for triangle in mesh.indices.as_chunks::<3>().0 {
            let [a, b, c] = [triangle[0], triangle[1], triangle[2]];
            if a as usize >= positions.len()
                || b as usize >= positions.len()
                || c as usize >= positions.len()
            {
                continue;
            }
            let p0 = positions[a as usize];
            let p1 = positions[b as usize];
            let p2 = positions[c as usize];
            let face = (p1 - p0).cross(p2 - p0);
            if face.length_squared() < 1e-20 {
                continue;
            }
            triangles.push(Triangle {
                indices: [a, b, c],
                min: p0.min(p1).min(p2),
                max: p0.max(p1).max(p2),
                centroid: (p0 + p1 + p2) / 3.0,
                face_normal: face.normalize(),
            });
        }
        if triangles.is_empty() {
            return Err(BakeError::Message(
                "The high-poly mesh has no valid triangles".into(),
            ));
        }

        let mut nodes = Vec::new();
        let mut order: Vec<u32> = (0..triangles.len() as u32).collect();
        build_node(&triangles, &mut nodes, &mut order, 0, 0);
        Ok(Self {
            positions,
            normals,
            has_normals,
            triangles,
            nodes,
            order,
        })
    }

    pub(crate) fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    /// Closest double-sided hit along `direction`, up to `max_distance`.
    pub(crate) fn raycast(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<(f32, Vec3)> {
        self.trace(origin, direction, max_distance)
            .map(|hit| (hit.t, hit.normal))
    }

    /// Closest double-sided hit with its position and normals.
    pub(crate) fn trace(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
        if self.nodes.is_empty() || max_distance <= 0.0 {
            return None;
        }
        let mut stack = [0u32; 64];
        stack[0] = 0;
        let mut top = 1usize;
        let mut best = max_distance;
        let mut best_hit: Option<(f32, f32, f32, &Triangle)> = None;

        while top > 0 {
            top -= 1;
            let node = &self.nodes[stack[top] as usize];
            if !aabb_hit(node.min, node.max, origin, direction, best) {
                continue;
            }
            if node.count > 0 {
                let start = node.start as usize;
                let end = start + node.count as usize;
                for &index in &self.order[start..end] {
                    let triangle = &self.triangles[index as usize];
                    if let Some((t, u, v)) = self.intersect(triangle, origin, direction, best) {
                        best = t;
                        best_hit = Some((t, u, v, triangle));
                    }
                }
            } else if top + 2 <= stack.len() {
                stack[top] = node.left;
                top += 1;
                stack[top] = node.right;
                top += 1;
            }
        }

        best_hit.map(|(t, u, v, triangle)| {
            let normal = if self.has_normals {
                let normal = self.normals[triangle.indices[0] as usize] * (1.0 - u - v)
                    + self.normals[triangle.indices[1] as usize] * u
                    + self.normals[triangle.indices[2] as usize] * v;
                normalize_or(normal, triangle.face_normal)
            } else {
                triangle.face_normal
            };
            Hit {
                t,
                point: origin + direction * t,
                normal,
                face_normal: triangle.face_normal,
            }
        })
    }

    /// True if any triangle blocks the ray before `max_distance`.
    pub(crate) fn occluded(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> bool {
        if self.nodes.is_empty() || max_distance <= 0.0 {
            return false;
        }
        let mut stack = [0u32; 64];
        stack[0] = 0;
        let mut top = 1usize;

        while top > 0 {
            top -= 1;
            let node = &self.nodes[stack[top] as usize];
            if !aabb_hit(node.min, node.max, origin, direction, max_distance) {
                continue;
            }
            if node.count > 0 {
                let start = node.start as usize;
                let end = start + node.count as usize;
                for &index in &self.order[start..end] {
                    let triangle = &self.triangles[index as usize];
                    if self
                        .intersect(triangle, origin, direction, max_distance)
                        .is_some()
                    {
                        return true;
                    }
                }
            } else if top + 2 <= stack.len() {
                stack[top] = node.left;
                top += 1;
                stack[top] = node.right;
                top += 1;
            }
        }
        false
    }

    /// Reorder triangles into BVH leaf order and expose flat nodes. The GPU
    /// shader can then use leaf ranges directly without an indirection buffer.
    #[cfg(feature = "gpu")]
    pub(crate) fn gpu_geometry(&self) -> (Vec<GpuTriangle>, Vec<GpuNode>) {
        let triangles = self
            .order
            .iter()
            .map(|&index| {
                let triangle = &self.triangles[index as usize];
                let normal = if self.has_normals {
                    [
                        self.normals[triangle.indices[0] as usize],
                        self.normals[triangle.indices[1] as usize],
                        self.normals[triangle.indices[2] as usize],
                    ]
                } else {
                    [triangle.face_normal; 3]
                };
                GpuTriangle {
                    p: [
                        self.positions[triangle.indices[0] as usize],
                        self.positions[triangle.indices[1] as usize],
                        self.positions[triangle.indices[2] as usize],
                    ],
                    n: normal,
                }
            })
            .collect();
        let nodes = self
            .nodes
            .iter()
            .map(|node| GpuNode {
                min: node.min,
                max: node.max,
                left: node.left,
                right: node.right,
                start: node.start,
                count: node.count,
            })
            .collect();
        (triangles, nodes)
    }

    fn intersect(
        &self,
        triangle: &Triangle,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<(f32, f32, f32)> {
        let p0 = self.positions[triangle.indices[0] as usize];
        let p1 = self.positions[triangle.indices[1] as usize];
        let p2 = self.positions[triangle.indices[2] as usize];
        let edge1 = p1 - p0;
        let edge2 = p2 - p0;
        let pvec = direction.cross(edge2);
        let determinant = edge1.dot(pvec);
        if determinant.abs() < 1e-10 {
            return None;
        }
        let inverse = 1.0 / determinant;
        let tvec = origin - p0;
        let u = tvec.dot(pvec) * inverse;
        if !(-1e-6..=1.0 + 1e-6).contains(&u) {
            return None;
        }
        let qvec = tvec.cross(edge1);
        let v = direction.dot(qvec) * inverse;
        if v < -1e-6 || u + v > 1.0 + 1e-6 {
            return None;
        }
        let t = edge2.dot(qvec) * inverse;
        if t > 1e-7 && t < max_distance {
            Some((t, u, v))
        } else {
            None
        }
    }
}

fn build_node(
    triangles: &[Triangle],
    nodes: &mut Vec<BvhNode>,
    order: &mut [u32],
    offset: u32,
    depth: u32,
) -> u32 {
    let node_index = nodes.len() as u32;
    nodes.push(BvhNode::default());

    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut centroid_min = Vec3::splat(f32::INFINITY);
    let mut centroid_max = Vec3::splat(f32::NEG_INFINITY);
    for &index in order.iter() {
        let triangle = &triangles[index as usize];
        min = min.min(triangle.min);
        max = max.max(triangle.max);
        centroid_min = centroid_min.min(triangle.centroid);
        centroid_max = centroid_max.max(triangle.centroid);
    }
    let extent = centroid_max - centroid_min;

    if order.len() <= 8 || depth >= 48 || extent.max_element() <= 1e-12 {
        nodes[node_index as usize] = BvhNode {
            min,
            max,
            start: offset,
            count: order.len() as u32,
            ..BvhNode::default()
        };
        return node_index;
    }

    let axis = if extent.x >= extent.y && extent.x >= extent.z {
        0
    } else if extent.y >= extent.z {
        1
    } else {
        2
    };
    let middle = order.len() / 2;
    order.select_nth_unstable_by(middle, |&a, &b| {
        component(triangles[a as usize].centroid, axis)
            .partial_cmp(&component(triangles[b as usize].centroid, axis))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let (left_order, right_order) = order.split_at_mut(middle);
    let left = build_node(triangles, nodes, left_order, offset, depth + 1);
    let right = build_node(
        triangles,
        nodes,
        right_order,
        offset + middle as u32,
        depth + 1,
    );
    nodes[node_index as usize] = BvhNode {
        min,
        max,
        left,
        right,
        start: 0,
        count: 0,
    };
    node_index
}

fn component(vector: Vec3, axis: usize) -> f32 {
    match axis {
        0 => vector.x,
        1 => vector.y,
        _ => vector.z,
    }
}

/// Slab test with a separate branch for axis-aligned rays, avoiding NaNs when
/// a direction component is exactly zero.
fn aabb_hit(min: Vec3, max: Vec3, origin: Vec3, direction: Vec3, max_distance: f32) -> bool {
    let mut t_min = 0.0f32;
    let mut t_max = max_distance;
    for axis in 0..3 {
        let origin_axis = component(origin, axis);
        let direction_axis = component(direction, axis);
        let low = component(min, axis);
        let high = component(max, axis);
        if direction_axis.abs() < 1e-12 {
            if origin_axis < low || origin_axis > high {
                return false;
            }
            continue;
        }
        let inverse = 1.0 / direction_axis;
        let mut t1 = (low - origin_axis) * inverse;
        let mut t2 = (high - origin_axis) * inverse;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        t_min = t_min.max(t1);
        t_max = t_max.min(t2);
        if t_min > t_max {
            return false;
        }
    }
    true
}
