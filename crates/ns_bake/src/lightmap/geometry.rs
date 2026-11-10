//! Scale-aware ray geometry for the v2 tracer.
//!
//! A smooth-shaded low-poly mesh has two normals per texel: the interpolated
//! shading normal and the flat triangle's geometric normal. Ray origins and
//! offsets use the geometric one, while `n · l` keeps the shading normal so the
//! bake matches how the engine shades the mesh.

use glam::Vec3;

/// Scale-aware ray epsilon. `extent` is the raw mesh extent from
/// [`super::prepare_lightmap`]. The second term covers float error far from the
/// origin.
#[inline]
pub(crate) fn ray_eps(extent: f32, point: Vec3) -> f32 {
    extent * 1.0e-5 + point.abs().max_element() * 4.0e-6
}

/// Offset along the geometric normal, never the interpolated shading normal.
#[inline]
pub(crate) fn offset_origin(point: Vec3, geometric_normal: Vec3, eps: f32) -> Vec3 {
    point + geometric_normal * eps
}

/// Face normal oriented toward the side the path arrived from.
#[inline]
pub(crate) fn orient_face_normal(face: Vec3, toward: Vec3) -> Vec3 {
    if face.dot(toward) < 0.0 {
        -face
    } else {
        face
    }
}

/// Shadow-terminator offset (Hanika 2021). Moves the shading point toward the
/// curved surface implied by the vertex normals. For a flat-shaded triangle
/// every `dot(v_i - p, n_i)` is zero, so the shift is zero.
#[inline]
pub(crate) fn terminator_shift(
    point: Vec3,
    weights: [f32; 3],
    vertices: [Vec3; 3],
    normals: [Vec3; 3],
) -> Vec3 {
    let mut offset = Vec3::ZERO;
    for index in 0..3 {
        let distance = (vertices[index] - point).dot(normals[index]).max(0.0);
        offset += weights[index] * distance * normals[index];
    }
    point + offset
}

/// Convenience for callers that only have a direction and a triangle normal.
#[inline]
pub(crate) fn face_normal_toward(face: Vec3, ray_direction: Vec3) -> Vec3 {
    orient_face_normal(face, -ray_direction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_eps_scales_with_mesh_extent() {
        let small = ray_eps(1.0, Vec3::ZERO);
        let large = ray_eps(1000.0, Vec3::ZERO);
        assert!(large > small * 100.0);
    }

    #[test]
    fn ray_eps_grows_far_from_the_origin() {
        assert!(ray_eps(1.0, Vec3::splat(10_000.0)) > ray_eps(1.0, Vec3::ZERO));
    }

    #[test]
    fn face_normal_points_toward_the_arrival_side() {
        let face = Vec3::Z;
        assert_eq!(face_normal_toward(face, -Vec3::Z), Vec3::Z);
        assert_eq!(face_normal_toward(face, Vec3::Z), -Vec3::Z);
    }

    #[test]
    fn terminator_shift_is_zero_for_flat_triangles() {
        let vertices = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let normal = Vec3::Z;
        let point = Vec3::new(0.25, 0.25, 0.0);
        let shifted = terminator_shift(point, [0.5, 0.25, 0.25], vertices, [normal; 3]);
        assert!((shifted - point).length() < 1e-6);
    }

    #[test]
    fn terminator_shift_moves_up_for_curved_normals() {
        let vertices = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let normals = [
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.3, 0.0, 0.95).normalize(),
            Vec3::new(0.0, 0.3, 0.95).normalize(),
        ];
        let point = Vec3::new(0.1, 0.1, 0.0);
        let shifted = terminator_shift(point, [0.8, 0.1, 0.1], vertices, normals);
        assert!(shifted.z > point.z + 1e-5);
    }
}
