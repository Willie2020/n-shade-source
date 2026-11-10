use glam::{Vec2, Vec3};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Edge behavior for every sampling kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Wrap {
    #[default]
    Clamp,
    Repeat,
}

impl Wrap {
    #[inline]
    pub fn coord(self, v: i32, n: u32) -> u32 {
        match self {
            Wrap::Clamp => v.clamp(0, n.saturating_sub(1) as i32) as u32,
            Wrap::Repeat => v.rem_euclid(n as i32) as u32,
        }
    }
}

/// A dense, row-major 2D buffer.
///
/// Deserialisation goes through [`GridRepr`] so a file can never produce a grid
/// whose `data.len()` disagrees with `w * h`, or one with a zero side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "GridRepr<T>", bound(deserialize = "T: Deserialize<'de>"))]
pub struct Grid<T> {
    pub w: u32,
    pub h: u32,
    pub data: Vec<T>,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct GridRepr<T> {
    w: u32,
    h: u32,
    data: Vec<T>,
}

impl<T> TryFrom<GridRepr<T>> for Grid<T> {
    type Error = String;

    fn try_from(repr: GridRepr<T>) -> Result<Self, Self::Error> {
        if repr.w == 0 || repr.h == 0 {
            return Err(format!(
                "grid dimensions {}x{} must be non-zero",
                repr.w, repr.h
            ));
        }
        // u64 so `w * h` cannot wrap on 32-bit targets.
        let expected = u64::from(repr.w) * u64::from(repr.h);
        if repr.data.len() as u64 != expected {
            return Err(format!(
                "grid is {}x{} ({expected} cells) but holds {} values",
                repr.w,
                repr.h,
                repr.data.len()
            ));
        }
        Ok(Self {
            w: repr.w,
            h: repr.h,
            data: repr.data,
        })
    }
}

impl<T> Grid<T> {
    pub fn new(w: u32, h: u32, value: T) -> Self
    where
        T: Clone,
    {
        assert!(w > 0 && h > 0, "grid dimensions must be non-zero");
        Self {
            w,
            h,
            data: vec![value; (w as usize) * (h as usize)],
        }
    }

    pub fn from_vec(w: u32, h: u32, data: Vec<T>) -> Self {
        assert!(w > 0 && h > 0, "grid dimensions must be non-zero");
        assert_eq!(
            data.len(),
            (w as usize) * (h as usize),
            "grid data length does not match dimensions"
        );
        Self { w, h, data }
    }

    pub fn from_fn<F>(w: u32, h: u32, mut f: F) -> Self
    where
        F: FnMut(u32, u32) -> T,
    {
        assert!(w > 0 && h > 0, "grid dimensions must be non-zero");
        let mut data = Vec::with_capacity((w as usize) * (h as usize));
        for y in 0..h {
            for x in 0..w {
                data.push(f(x, y));
            }
        }
        Self { w, h, data }
    }

    pub fn from_fn_par<F>(w: u32, h: u32, f: F) -> Self
    where
        T: Send,
        F: Fn(u32, u32) -> T + Sync + Send,
    {
        assert!(w > 0 && h > 0, "grid dimensions must be non-zero");
        let rows: Vec<Vec<T>> = (0..h)
            .into_par_iter()
            .map(|y| (0..w).map(|x| f(x, y)).collect())
            .collect();
        let data = rows.into_iter().flatten().collect();
        Self { w, h, data }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        debug_assert!(x < self.w && y < self.h);
        (y as usize) * (self.w as usize) + (x as usize)
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> &T {
        &self.data[self.index(x, y)]
    }

    #[inline]
    pub fn get_mut(&mut self, x: u32, y: u32) -> &mut T {
        let i = self.index(x, y);
        &mut self.data[i]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, value: T) {
        let i = self.index(x, y);
        self.data[i] = value;
    }

    #[inline]
    pub fn sample(&self, x: i32, y: i32, wrap: Wrap) -> T
    where
        T: Copy,
    {
        let sx = wrap.coord(x, self.w);
        let sy = wrap.coord(y, self.h);
        *self.get(sx, sy)
    }

    pub fn map<U, F>(&self, f: F) -> Grid<U>
    where
        F: Fn(&T) -> U,
    {
        Grid {
            w: self.w,
            h: self.h,
            data: self.data.iter().map(f).collect(),
        }
    }

    pub fn resample_nearest(&self, w: u32, h: u32) -> Grid<T>
    where
        T: Copy,
    {
        assert!(w > 0 && h > 0, "grid dimensions must be non-zero");
        Grid::from_fn(w, h, |x, y| {
            let sx = ((x as f32 + 0.5) * self.w as f32 / w as f32).floor() as u32;
            let sy = ((y as f32 + 0.5) * self.h as f32 / h as f32).floor() as u32;
            *self.get(sx.min(self.w - 1), sy.min(self.h - 1))
        })
    }

    /// Area-majority resample for categorical grids such as material-ID maps.
    ///
    /// Each destination pixel takes the most frequent source value inside its
    /// footprint. The result only ever contains values that existed in the
    /// source, so two IDs meet at a hard edge instead of blending into an
    /// intermediate colour. Ties prefer the value under the destination centre,
    /// which keeps silhouettes from drifting. Upscaling matches nearest sampling
    /// because each footprint is sub-pixel.
    pub fn resample_majority(&self, w: u32, h: u32) -> Grid<T>
    where
        T: Copy + PartialEq,
    {
        assert!(w > 0 && h > 0, "grid dimensions must be non-zero");
        let scale_x = self.w as f32 / w as f32;
        let scale_y = self.h as f32 / h as f32;
        Grid::from_fn(w, h, |x, y| {
            let x0 = (x as f32 * scale_x).floor() as u32;
            let x1 = (((x + 1) as f32 * scale_x).ceil() as u32).min(self.w);
            let y0 = (y as f32 * scale_y).floor() as u32;
            let y1 = (((y + 1) as f32 * scale_y).ceil() as u32).min(self.h);
            // The pixel containing the destination centre breaks ties.
            let centre = *self.get(
                (((x as f32 + 0.5) * scale_x).floor() as u32).min(self.w - 1),
                (((y as f32 + 0.5) * scale_y).floor() as u32).min(self.h - 1),
            );
            let mut counts: Vec<(T, usize)> = Vec::new();
            for sy in y0..y1.max(y0 + 1) {
                for sx in x0..x1.max(x0 + 1) {
                    let value = *self.get(sx.min(self.w - 1), sy.min(self.h - 1));
                    match counts.iter_mut().find(|(seen, _)| *seen == value) {
                        Some((_, count)) => *count += 1,
                        None => counts.push((value, 1)),
                    }
                }
            }
            let mut best = centre;
            let mut best_count = counts
                .iter()
                .find(|(value, _)| *value == centre)
                .map(|(_, count)| *count)
                .unwrap_or(0);
            for (value, count) in counts {
                if count > best_count {
                    best = value;
                    best_count = count;
                }
            }
            best
        })
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.data.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.data.iter_mut()
    }

    pub fn rows(&self) -> impl Iterator<Item = &[T]> {
        self.data.chunks(self.w as usize)
    }

    pub fn rows_mut(&mut self) -> impl Iterator<Item = &mut [T]> {
        self.data.chunks_mut(self.w as usize)
    }

    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.data
    }
}

impl Grid<f32> {
    pub fn min_max(&self) -> (f32, f32) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &v in &self.data {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        }
        (min, max)
    }

    pub fn normalize_to_unit(&mut self) {
        let (min, max) = self.min_max();
        let range = max - min;
        if range > f32::EPSILON {
            for v in &mut self.data {
                *v = (*v - min) / range;
            }
        }
    }

    /// Resize with an area filter when shrinking and bilinear sampling when
    /// growing. Used by export resolution options.
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
        let xs = box_weights(self.w, dst_w);
        let ys = box_weights(self.h, dst_h);
        let span = dst_w as usize;
        let mut mid = vec![0.0f32; span * self.h as usize];
        mid.par_chunks_mut(span).enumerate().for_each(|(y, row)| {
            for (dx, weights) in xs.iter().enumerate() {
                let mut sum = 0.0;
                for &(sx, weight) in weights {
                    sum += *self.get(sx, y as u32) * weight;
                }
                row[dx] = sum;
            }
        });
        let mid = &mid;
        let ys = &ys;
        let data: Vec<f32> = (0..dst_h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let weights = &ys[y as usize];
                (0..span).map(move |x| {
                    weights
                        .iter()
                        .map(|&(sy, weight)| mid[sy as usize * span + x] * weight)
                        .sum()
                })
            })
            .collect();
        Grid::from_vec(dst_w, dst_h, data)
    }

    fn resized_bilinear(&self, dst_w: u32, dst_h: u32) -> Self {
        Grid::from_fn_par(dst_w, dst_h, |x, y| {
            let (x0, y0, x1, y1, fx, fy) = bilinear_cell(self.w, self.h, dst_w, dst_h, x, y);
            let a = *self.get(x0, y0);
            let b = *self.get(x1, y0);
            let c = *self.get(x0, y1);
            let d = *self.get(x1, y1);
            let top = a + (b - a) * fx;
            let bottom = c + (d - c) * fx;
            top + (bottom - top) * fy
        })
    }
}

impl Grid<Vec3> {
    /// Resize a normal grid with area/bilinear filtering, renormalizing each
    /// resulting vector so the map stays unit length.
    ///
    /// Filtering happens in slope space (`nx/nz`, `ny/nz`) rather than on the
    /// unit vectors: averaging vectors cancels opposing slopes and flattens
    /// steep bevels into the flat surface when a map is downscaled. Averages of
    /// slopes reconstruct the filtered height field, so edges keep their tilt
    /// and the map retains detail at 512 as well as at 2K.
    pub fn resized_normalized(&self, w: u32, h: u32) -> Self {
        if w == self.w && h == self.h {
            return self.clone();
        }
        if w <= self.w && h <= self.h {
            return self.resized_area_normalized(w, h);
        }
        self.resized_bilinear_normalized(w, h)
    }

    fn resized_area_normalized(&self, dst_w: u32, dst_h: u32) -> Self {
        let xs = box_weights(self.w, dst_w);
        let ys = box_weights(self.h, dst_h);
        let span = dst_w as usize;
        let mut mid = vec![Vec2::ZERO; span * self.h as usize];
        mid.par_chunks_mut(span).enumerate().for_each(|(y, row)| {
            for (dx, weights) in xs.iter().enumerate() {
                let mut sum = Vec2::ZERO;
                for &(sx, weight) in weights {
                    sum += normal_slope(*self.get(sx, y as u32)) * weight;
                }
                row[dx] = sum;
            }
        });
        let mid = &mid;
        let ys = &ys;
        let data: Vec<Vec3> = (0..dst_h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let weights = &ys[y as usize];
                (0..span).map(move |x| {
                    let slope: Vec2 = weights
                        .iter()
                        .map(|&(sy, weight)| mid[sy as usize * span + x] * weight)
                        .sum();
                    slope_to_normal(slope)
                })
            })
            .collect();
        Grid::from_vec(dst_w, dst_h, data)
    }

    fn resized_bilinear_normalized(&self, dst_w: u32, dst_h: u32) -> Self {
        Grid::from_fn_par(dst_w, dst_h, |x, y| {
            let (x0, y0, x1, y1, fx, fy) = bilinear_cell(self.w, self.h, dst_w, dst_h, x, y);
            let a = normal_slope(*self.get(x0, y0));
            let b = normal_slope(*self.get(x1, y0));
            let c = normal_slope(*self.get(x0, y1));
            let d = normal_slope(*self.get(x1, y1));
            let top = a.lerp(b, fx);
            let bottom = c.lerp(d, fx);
            slope_to_normal(top.lerp(bottom, fy))
        })
    }
}

/// Source texels and fractional coverage for one destination pixel, normalised
/// to sum to 1.
///
/// The old filter used `floor`/`ceil` spans with every texel counted equally,
/// which shifts and softens edges whenever the scale factor is not an integer.
/// A proper box filter weights each source texel by the part of its cell that
/// the destination pixel covers.
fn box_weights(src: u32, dst: u32) -> Vec<Vec<(u32, f32)>> {
    let src_f = src as f32;
    let scale = src_f / dst as f32;
    (0..dst)
        .map(|d| {
            let start = d as f32 * scale;
            let end = ((d + 1) as f32 * scale).min(src_f);
            let first = (start.floor() as u32).min(src - 1);
            let last = (end.ceil() as u32).clamp(first + 1, src);
            let mut weights = Vec::with_capacity((last - first) as usize);
            let mut total = 0.0;
            for index in first..last {
                let a = (index as f32).max(start);
                let b = ((index + 1) as f32).min(end);
                let weight = (b - a).max(0.0);
                if weight > 0.0 {
                    weights.push((index, weight));
                    total += weight;
                }
            }
            if total > 0.0 {
                for (_, weight) in &mut weights {
                    *weight /= total;
                }
            } else {
                weights.push((first, 1.0));
            }
            weights
        })
        .collect()
}

/// Bilinear source cell and fractions for a destination pixel.
#[allow(clippy::too_many_arguments)]
fn bilinear_cell(
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    x: u32,
    y: u32,
) -> (u32, u32, u32, u32, f32, f32) {
    let u = ((x as f32 + 0.5) / dst_w as f32 * src_w as f32 - 0.5).clamp(0.0, (src_w - 1) as f32);
    let v = ((y as f32 + 0.5) / dst_h as f32 * src_h as f32 - 0.5).clamp(0.0, (src_h - 1) as f32);
    let x0 = u.floor() as u32;
    let y0 = v.floor() as u32;
    let x1 = (x0 + 1).min(src_w - 1);
    let y1 = (y0 + 1).min(src_h - 1);
    (x0, y0, x1, y1, u - x0 as f32, v - y0 as f32)
}

/// Slopes steeper than this (about 89°) are clamped so near-tangent normals do
/// not dominate an average.
const SLOPE_LIMIT: f32 = 64.0;

fn normal_slope(normal: Vec3) -> Vec2 {
    let z = normal.z.max(1e-3);
    Vec2::new(normal.x / z, normal.y / z).clamp(Vec2::splat(-SLOPE_LIMIT), Vec2::splat(SLOPE_LIMIT))
}

fn slope_to_normal(slope: Vec2) -> Vec3 {
    Vec3::new(slope.x, slope.y, 1.0).normalize()
}

impl<T> std::ops::Index<(u32, u32)> for Grid<T> {
    type Output = T;

    fn index(&self, (x, y): (u32, u32)) -> &Self::Output {
        self.get(x, y)
    }
}

impl<T> std::ops::IndexMut<(u32, u32)> for Grid<T> {
    fn index_mut(&mut self, (x, y): (u32, u32)) -> &mut Self::Output {
        self.get_mut(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialization_rejects_inconsistent_data() {
        assert!(ron::from_str::<Grid<f32>>("(w: 2, h: 2, data: [0.0, 1.0, 2.0])").is_err());
        assert!(ron::from_str::<Grid<f32>>("(w: 0, h: 4, data: [])").is_err());
        assert!(ron::from_str::<Grid<f32>>("(w: 4, h: 0, data: [])").is_err());
        assert!(ron::from_str::<Grid<f32>>("(w: 2, h: 1, data: [0.0, 1.0])").is_ok());
    }

    #[test]
    fn deserialization_round_trips_through_ron() {
        let grid = Grid::from_vec(2, 2, vec![1.0f32, 2.0, 3.0, 4.0]);
        let text = ron::ser::to_string(&grid).unwrap();
        let loaded: Grid<f32> = ron::from_str(&text).unwrap();
        assert_eq!(loaded, grid);
    }

    #[test]
    fn clamp_and_repeat_sampling() {
        let g = Grid::from_vec(3, 1, vec![10, 20, 30]);
        assert_eq!(g.sample(-1, 0, Wrap::Clamp), 10);
        assert_eq!(g.sample(3, 0, Wrap::Clamp), 30);
        assert_eq!(g.sample(-1, 0, Wrap::Repeat), 30);
        assert_eq!(g.sample(3, 0, Wrap::Repeat), 10);
    }

    #[test]
    fn nearest_resample_preserves_corners() {
        let g = Grid::from_vec(2, 2, vec![1, 2, 3, 4]);
        let r = g.resample_nearest(4, 4);
        assert_eq!(r.w, 4);
        assert_eq!(*r.get(0, 0), 1);
        assert_eq!(*r.get(3, 3), 4);
    }

    #[test]
    fn majority_resample_keeps_exact_categories() {
        // One stray pixel sits where nearest sampling would look. The 2x2
        // footprint is 3/4 blue, so the ID map must keep the majority colour
        // instead of switching the whole output pixel to the outlier.
        let blue = [0.0f32, 0.0, 1.0];
        let red = [1.0f32, 0.0, 0.0];
        let mut ids = Grid::new(4, 4, blue);
        ids.set(1, 1, red);
        let down = ids.resample_majority(2, 2);
        assert!(down.data.iter().all(|color| *color == blue));

        // Every exported value is a source category: no blended colour, even
        // when the scale factor is not a whole number.
        let mut strip = Grid::new(10, 3, blue);
        for y in 0..3 {
            for x in 4..6 {
                strip.set(x, y, red);
            }
        }
        let resized = strip.resample_majority(4, 2);
        assert!(resized
            .data
            .iter()
            .all(|color| *color == blue || *color == red));
    }

    #[test]
    fn bilinear_resize_keeps_values_and_normals_valid() {
        let grid = Grid::from_fn(4, 4, |x, y| (x + y) as f32);
        let up = grid.resized(8, 8);
        assert_eq!((up.w, up.h), (8, 8));
        assert!((*up.get(0, 0) - 0.0).abs() < 1.0);

        let down = up.resized(2, 2);
        assert_eq!((down.w, down.h), (2, 2));

        let normal = Grid::from_fn(2, 2, |_, _| Vec3::new(0.0, 0.0, 2.0));
        let resized = normal.resized_normalized(4, 4);
        assert_eq!((resized.w, resized.h), (4, 4));
        let value = *resized.get(1, 1);
        assert!((value.length() - 1.0).abs() < 1e-5);
        assert!(value.z > 0.99);
    }

    #[test]
    fn downscaling_weights_partial_source_pixels() {
        // 3 -> 2 is a 1.5x scale: destination pixel 1 covers [1.5, 3), i.e.
        // half of source 1 and all of source 2. Counting whole pixels (the old
        // floor/ceil filter) would average 0 and 1 equally and give 0.5.
        let grid = Grid::from_vec(3, 1, vec![0.0, 0.0, 1.0]);
        let down = grid.resized(2, 1);
        assert!((*down.get(0, 0) - 0.0).abs() < 1e-6);
        assert!(
            (*down.get(1, 0) - 2.0 / 3.0).abs() < 1e-6,
            "expected the exact area average, got {}",
            down.get(1, 0)
        );
    }

    #[test]
    fn downscaled_normals_average_slopes_not_vectors() {
        // One steep bevel texel and three flat texels. Averaging unit vectors
        // and renormalizing gives a slope of 0.5; the correct filtered slope is
        // the average of the input slopes (4/3 / 4 = 1/3). Slope-space filtering
        // must keep the steep tilt instead of flattening it.
        let steep = Vec3::new(0.8, 0.0, 0.6).normalize();
        let flat = Vec3::Z;
        let grid = Grid::from_vec(2, 2, vec![steep, flat, flat, flat]);
        let down = grid.resized_normalized(1, 1);
        let value = *down.get(0, 0);
        assert!((value.length() - 1.0).abs() < 1e-5);
        let slope = value.x / value.z;
        let expected = (steep.x / steep.z) / 4.0;
        assert!(
            (slope - expected).abs() < 1e-4,
            "expected slope {expected}, got {slope}"
        );
    }

    #[test]
    fn upscaled_normals_stay_unit_length_and_keep_their_tilt() {
        let flat = Vec3::Z;
        let steep = Vec3::new(0.6, 0.0, 0.8).normalize();
        let grid = Grid::from_vec(2, 1, vec![flat, steep]);
        let up = grid.resized_normalized(8, 4);
        assert_eq!((up.w, up.h), (8, 4));
        assert!(up
            .data
            .iter()
            .all(|value| (value.length() - 1.0).abs() < 1e-5));
        let middle = *up.get(4, 2);
        assert!(
            middle.x > 0.1 && middle.x < steep.x,
            "the edge interpolates between flat and steep: {middle:?}"
        );
    }
}
