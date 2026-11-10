//! Runtime brush-tip library: imported alpha brushes and normal-map brushes.

use std::path::Path;

use glam::Vec3;
use ns_core::brush::BrushTip;
use ns_core::Grid;

/// Longest side of an imported tip. Larger images are resized so a stroke does
/// not resample millions of texels per dab.
const MAX_TIP_SIDE: u32 = 512;

pub struct BrushLibrary {
    pub tips: Vec<BrushTip>,
    selected: usize,
}

impl Default for BrushLibrary {
    fn default() -> Self {
        Self {
            tips: vec![BrushTip::round()],
            selected: 0,
        }
    }
}

impl BrushLibrary {
    pub fn selected(&self) -> Option<&BrushTip> {
        self.tips.get(self.selected)
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn select(&mut self, index: usize) {
        if index < self.tips.len() {
            self.selected = index;
        }
    }

    pub fn push(&mut self, mut tip: BrushTip) {
        // Keep names distinguishable in the picker without touching the file.
        if self.tips.iter().any(|existing| existing.name == tip.name) {
            let base = tip.name.clone();
            let mut suffix = 2;
            loop {
                let candidate = format!("{base} {suffix}");
                if !self.tips.iter().any(|existing| existing.name == candidate) {
                    tip.name = candidate;
                    break;
                }
                suffix += 1;
            }
        }
        self.tips.push(tip);
        self.selected = self.tips.len() - 1;
    }

    pub fn is_normal(&self) -> bool {
        self.selected().is_some_and(BrushTip::is_normal)
    }

    /// Remove the selected tip. The built-in round tip at index 0 is permanent,
    /// so a library is never empty.
    pub fn remove_selected(&mut self) -> bool {
        if self.selected == 0 || self.tips.len() <= 1 {
            return false;
        }
        self.tips.remove(self.selected);
        self.selected = self.selected.min(self.tips.len() - 1);
        true
    }
}

/// Load a grayscale/alpha brush. The alpha channel wins when the image has one;
/// otherwise luminance is used so white-on-black brush sheets work directly.
pub fn load_alpha_tip(path: &Path) -> Result<BrushTip, String> {
    let rgba = load_rgba(path)?;
    let (w, h) = rgba.dimensions();
    let has_alpha = rgba.pixels().any(|pixel| pixel[3] < 250);
    let mut alpha = Grid::new(w, h, 0.0f32);
    let mut peak = 0.0f32;
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let value = if has_alpha {
            pixel[3] as f32 / 255.0
        } else {
            let r = pixel[0] as f32 / 255.0;
            let g = pixel[1] as f32 / 255.0;
            let b = pixel[2] as f32 / 255.0;
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        peak = peak.max(value);
        alpha.set(x, y, value.clamp(0.0, 1.0));
    }
    if peak <= 0.01 {
        return Err("The image has no visible pixels to use as a brush".into());
    }
    Ok(BrushTip::from_alpha(tip_name(path), alpha))
}

/// Load a tangent-space normal map as a stamp. Coverage comes from the image's
/// alpha when present, and is gated by the normal slope so a flat background
/// does not produce a square dab.
pub fn load_normal_tip(path: &Path, flip_green: bool) -> Result<BrushTip, String> {
    let rgba = load_rgba(path)?;
    let (w, h) = rgba.dimensions();
    let has_alpha = rgba.pixels().any(|pixel| pixel[3] < 250);
    let mut normal = Grid::new(w, h, Vec3::Z);
    let mut coverage = Grid::new(w, h, 1.0f32);
    let mut z_sum = 0.0f32;
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let nx = pixel[0] as f32 / 127.5 - 1.0;
        let mut ny = pixel[1] as f32 / 127.5 - 1.0;
        let nz = pixel[2] as f32 / 127.5 - 1.0;
        if flip_green {
            ny = -ny;
        }
        let mut n = Vec3::new(nx, ny, nz);
        if n.length_squared() > 1e-6 {
            n = n.normalize();
        } else {
            n = Vec3::Z;
        }
        z_sum += n.z;
        normal.set(x, y, n);
        if has_alpha {
            coverage.set(x, y, pixel[3] as f32 / 255.0);
        }
    }
    let mean_z = z_sum / (w as f32 * h as f32).max(1.0);
    if mean_z < 0.25 {
        return Err(
            "The image does not look like a tangent-space normal map (blue channel is too low)"
                .into(),
        );
    }
    Ok(BrushTip::from_normal(
        tip_name(path),
        normal,
        Some(coverage),
    ))
}

fn tip_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Brush")
        .to_string()
}

fn load_rgba(path: &Path) -> Result<image::RgbaImage, String> {
    let image = ns_core::io::open_image_limited(path)
        .map_err(|error| format!("Could not load brush {}: {error}", path.display()))?;
    let rgba = image.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return Err("The brush image has no pixels".into());
    }
    let longest = w.max(h);
    if longest <= MAX_TIP_SIDE {
        return Ok(rgba);
    }
    let scale = MAX_TIP_SIDE as f32 / longest as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    Ok(image::imageops::resize(
        &rgba,
        nw,
        nh,
        image::imageops::FilterType::Triangle,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_names_get_a_suffix() {
        let mut library = BrushLibrary::default();
        let alpha = Grid::new(2, 2, 1.0);
        library.push(BrushTip::from_alpha("speckle", alpha.clone()));
        library.push(BrushTip::from_alpha("speckle", alpha));
        assert_eq!(library.tips[1].name, "speckle");
        assert_eq!(library.tips[2].name, "speckle 2");
        assert_eq!(library.selected_index(), 2);
    }

    #[test]
    fn a_normal_tip_rejects_a_flat_colour_image() {
        let normal = Grid::new(4, 4, Vec3::Z);
        // The loader's guard lives in load_normal_tip; BrushTip itself accepts
        // a flat map but produces zero coverage.
        let tip = BrushTip::from_normal("flat", normal, None);
        let alpha = tip.alpha.unwrap();
        assert!(alpha.data.iter().all(|value| *value <= 1e-4));
    }
}
