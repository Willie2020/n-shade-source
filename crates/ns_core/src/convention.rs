use glam::Vec3;
use serde::{Deserialize, Serialize};

/// Tangent-space normal-map convention.
///
/// Canonical internal data is OpenGL-style (Y+). DirectX maps are flipped only
/// at import/export time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GreenChannel {
    OpenGl,
    DirectX,
}

impl GreenChannel {
    #[inline]
    pub fn flips_y(self) -> bool {
        matches!(self, Self::DirectX)
    }
}

/// Decode an encoded normal (each component in `[0, 1]`) into a unit vector.
///
/// The result is always in canonical OpenGL (Y+) tangent space.
pub fn decode_normal(rgb: Vec3, convention: GreenChannel) -> Vec3 {
    let mut n = rgb * 2.0 - Vec3::ONE;
    if convention.flips_y() {
        n.y = -n.y;
    }
    // Imported maps occasionally contain invalid -Z vectors.
    n.z = n.z.max(1e-4);
    n.normalize_or_zero()
}

/// Encode a canonical normal into `[0, 1]` for the requested convention.
pub fn encode_normal(n: Vec3, convention: GreenChannel) -> Vec3 {
    let mut n = n.normalize_or_zero();
    n.z = n.z.max(0.0);
    if convention.flips_y() {
        n.y = -n.y;
    }
    n * 0.5 + Vec3::splat(0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_opengl() {
        let n = Vec3::new(0.3, -0.4, 0.8660254).normalize();
        let encoded = encode_normal(n, GreenChannel::OpenGl);
        let decoded = decode_normal(encoded, GreenChannel::OpenGl);
        assert!((decoded - n).length() < 1e-5);
    }

    #[test]
    fn directx_flips_green() {
        let n = Vec3::new(0.0, 0.95, 0.3).normalize();
        let gl = encode_normal(n, GreenChannel::OpenGl);
        let dx = encode_normal(n, GreenChannel::DirectX);
        assert!((gl.y - dx.y).abs() > 0.9);
        let decoded = decode_normal(dx, GreenChannel::DirectX);
        assert!((decoded - n).length() < 1e-5);
    }

    #[test]
    fn invalid_negative_z_is_clamped() {
        let decoded = decode_normal(Vec3::new(0.5, 0.5, 0.0), GreenChannel::OpenGl);
        assert!(decoded.z > 0.0);
        assert!((decoded.length() - 1.0).abs() < 1e-5);
    }
}
