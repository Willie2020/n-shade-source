//! Linear RGB image buffers used by the single-image PBR pipeline.

use std::path::Path;

use glam::Vec3;
use rayon::prelude::*;

use crate::Result;

/// Dense linear-light RGB image. All intermediates stay in f32 until export.
#[derive(Clone, Debug, PartialEq)]
pub struct PbrImage {
    pub w: u32,
    pub h: u32,
    pub data: Vec<Vec3>,
}

impl PbrImage {
    pub fn new(w: u32, h: u32, color: Vec3) -> Self {
        assert!(w > 0 && h > 0, "image dimensions must be non-zero");
        Self {
            w,
            h,
            data: vec![color; (w as usize) * (h as usize)],
        }
    }

    pub fn from_vec(w: u32, h: u32, data: Vec<Vec3>) -> Self {
        assert!(w > 0 && h > 0, "image dimensions must be non-zero");
        assert_eq!(data.len(), (w as usize) * (h as usize));
        Self { w, h, data }
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        debug_assert!(x < self.w && y < self.h);
        (y as usize) * (self.w as usize) + (x as usize)
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> Vec3 {
        self.data[self.index(x, y)]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, value: Vec3) {
        let index = self.index(x, y);
        self.data[index] = value;
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Rec. 709 luminance in linear light.
    pub fn luma_grid(&self) -> ns_core::Grid<f32> {
        ns_core::Grid::from_vec(
            self.w,
            self.h,
            self.data
                .iter()
                .map(|color| 0.2126 * color.x + 0.7152 * color.y + 0.0722 * color.z)
                .collect(),
        )
    }

    pub fn mean_luma(&self) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data
            .iter()
            .map(|color| 0.2126 * color.x + 0.7152 * color.y + 0.0722 * color.z)
            .sum::<f32>()
            / self.data.len() as f32
    }

    /// Downsample by area averaging, or bilinearly upsample small inputs.
    pub fn resized(&self, w: u32, h: u32) -> Self {
        if w == self.w && h == self.h {
            return self.clone();
        }
        if w <= self.w && h <= self.h {
            return self.resized_area(w, h);
        }
        self.resized_bilinear(w, h)
    }

    fn resized_area(&self, dst_w: u32, dst_h: u32) -> Self {
        let src_w = self.w as f32;
        let src_h = self.h as f32;
        let inv_w = src_w / dst_w as f32;
        let inv_h = src_h / dst_h as f32;
        let spans: Vec<(u32, u32)> = (0..dst_w)
            .map(|x| {
                let start = (x as f32 * inv_w).floor() as u32;
                let end = (((x + 1) as f32 * inv_w).ceil() as u32).min(self.w);
                (start, end.max(start + 1).min(self.w))
            })
            .collect();
        let data: Vec<Vec3> = (0..dst_h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let y0 = (y as f32 * inv_h).floor() as u32;
                let y1 = (((y + 1) as f32 * inv_h).ceil() as u32).min(self.h);
                let y1 = y1.max(y0 + 1).min(self.h);
                spans.iter().map(move |&(x0, x1)| {
                    let mut sum = Vec3::ZERO;
                    let mut count = 0.0;
                    for sy in y0..y1 {
                        for sx in x0..x1 {
                            sum += self.get(sx, sy);
                            count += 1.0;
                        }
                    }
                    if count > 0.0 {
                        sum / count
                    } else {
                        Vec3::ZERO
                    }
                })
            })
            .collect();
        Self::from_vec(dst_w, dst_h, data)
    }

    fn resized_bilinear(&self, dst_w: u32, dst_h: u32) -> Self {
        let data: Vec<Vec3> = (0..dst_h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let v = (y as f32 + 0.5) / dst_h as f32 * self.h as f32 - 0.5;
                let y0 = v.floor().max(0.0) as u32;
                let y1 = (y0 + 1).min(self.h - 1);
                let fy = (v - v.floor()).clamp(0.0, 1.0);
                (0..dst_w).map(move |x| {
                    let u = (x as f32 + 0.5) / dst_w as f32 * self.w as f32 - 0.5;
                    let x0 = u.floor().max(0.0) as u32;
                    let x1 = (x0 + 1).min(self.w - 1);
                    let fx = (u - u.floor()).clamp(0.0, 1.0);
                    let a = self.get(x0, y0);
                    let b = self.get(x1, y0);
                    let c = self.get(x0, y1);
                    let d = self.get(x1, y1);
                    (a.lerp(b, fx)).lerp(c.lerp(d, fx), fy)
                })
            })
            .collect();
        Self::from_vec(dst_w, dst_h, data)
    }

    /// 8-bit sRGB RGBA for previews and thumbnails.
    pub fn to_srgb_rgba8(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.len() * 4];
        for (index, color) in self.data.iter().enumerate() {
            let base = index * 4;
            out[base] = encode_u8(srgb_encode(color.x));
            out[base + 1] = encode_u8(srgb_encode(color.y));
            out[base + 2] = encode_u8(srgb_encode(color.z));
            out[base + 3] = 255;
        }
        out
    }
}

pub fn load(path: &Path) -> Result<PbrImage> {
    let image = ns_core::io::open_image_limited(path)
        .map_err(|error| crate::PbrError::Message(error.to_string()))?;
    let srgb = !matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("exr") | Some("hdr") | Some("pfm")
    );
    Ok(from_dynamic(image, srgb))
}

pub fn from_dynamic(image: ::image::DynamicImage, srgb: bool) -> PbrImage {
    let rgb = image.to_rgb32f();
    let (w, h) = rgb.dimensions();
    let data: Vec<Vec3> = rgb
        .pixels()
        .map(|pixel| {
            let mut color = Vec3::new(pixel[0], pixel[1], pixel[2]);
            if srgb {
                color = Vec3::new(
                    srgb_decode(color.x),
                    srgb_decode(color.y),
                    srgb_decode(color.z),
                );
            }
            color.max(Vec3::ZERO)
        })
        .collect();
    PbrImage::from_vec(w, h, data)
}

fn encode_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

pub fn srgb_decode(value: f32) -> f32 {
    let value = value.max(0.0);
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

pub fn srgb_encode(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// Stable content hash of the decoded pixels and dimensions.
pub fn hash_image(image: &PbrImage) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&image.w.to_le_bytes());
    hasher.update(&image.h.to_le_bytes());
    let mut bytes = [0u8; 12];
    for color in &image.data {
        bytes[0..4].copy_from_slice(&color.x.to_bits().to_le_bytes());
        bytes[4..8].copy_from_slice(&color.y.to_bits().to_le_bytes());
        bytes[8..12].copy_from_slice(&color.z.to_bits().to_le_bytes());
        hasher.update(&bytes);
    }
    *hasher.finalize().as_bytes()
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn assert<T: Send + Sync>() {}
    assert::<PbrImage>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trip() {
        for value in [0.0, 0.02, 0.2, 0.5, 0.9, 1.0] {
            let encoded = srgb_encode(value);
            let decoded = srgb_decode(encoded);
            assert!((decoded - value).abs() < 1e-5);
        }
    }

    #[test]
    fn hash_changes_with_pixels() {
        let mut a = PbrImage::new(4, 4, Vec3::splat(0.5));
        let b = a.clone();
        assert_eq!(hash_image(&a), hash_image(&b));
        a.set(0, 0, Vec3::ZERO);
        assert_ne!(hash_image(&a), hash_image(&b));
    }

    #[test]
    fn area_resize_keeps_average() {
        let image = PbrImage::new(8, 8, Vec3::new(0.25, 0.5, 0.75));
        let small = image.resized(2, 2);
        assert_eq!(small.w, 2);
        assert_eq!(small.h, 2);
        for color in small.data {
            assert!((color - Vec3::new(0.25, 0.5, 0.75)).length() < 1e-6);
        }
    }
}
