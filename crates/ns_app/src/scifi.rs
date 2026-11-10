//! Optional procedural sci-fi panel generator.
//!
//! A Rust port of the standalone "Procedural Sci-Fi Texture Generator" HTML
//! tool: rule-driven panel subdivision (octagon grid / trapezoid rows),
//! rule-based greeble selection, compositional-gravity balancing, warped noise
//! weathering and a height -> normal pipeline.
//!
//! It is deliberately self-contained. It never touches the document or the
//! layer stack, it lives in its own window, and it only writes PNGs when asked,
//! so the normal editor workflow is unaffected and the output can be wired into
//! layers/exports later without reshaping anything here.

// Drawing helpers take a rectangle plus the shared context; the argument lists
// mirror the canvas API the HTML tool was written against.
#![allow(clippy::too_many_arguments)]

use glam::{UVec2, Vec2};
use ns_core::{Path, Primitive, ShapeSource};

/// Resolutions offered by the generator.
pub const SIZE_CHOICES: [u32; 3] = [512, 768, 1024];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    OctagonGrid,
    TrapezoidRows,
}

impl Layout {
    pub const ALL: [Self; 2] = [Self::OctagonGrid, Self::TrapezoidRows];

    pub fn label(self) -> &'static str {
        match self {
            Self::OctagonGrid => "Octagon grid",
            Self::TrapezoidRows => "Trapezoid rows",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symmetry {
    None,
    MirrorX,
    MirrorXy,
}

impl Symmetry {
    pub const ALL: [Self; 3] = [Self::None, Self::MirrorX, Self::MirrorXy];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::MirrorX => "Mirror X",
            Self::MirrorXy => "Mirror XY",
        }
    }

    fn mirrored_x(self) -> bool {
        !matches!(self, Self::None)
    }

    fn mirrored_y(self) -> bool {
        matches!(self, Self::MirrorXy)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScifiView {
    #[default]
    Final,
    Albedo,
    Normal,
    Height,
    Emissive,
}

impl ScifiView {
    pub const ALL: [Self; 5] = [
        Self::Final,
        Self::Albedo,
        Self::Normal,
        Self::Height,
        Self::Emissive,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Final => "Final",
            Self::Albedo => "Albedo",
            Self::Normal => "Normal",
            Self::Height => "Height",
            Self::Emissive => "Emissive",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Final => 0,
            Self::Albedo => 1,
            Self::Normal => 2,
            Self::Height => 3,
            Self::Emissive => 4,
        }
    }
}

/// One colour scheme: panel base and accent as HSL triples, glow as RGB.
pub struct Palette {
    pub name: &'static str,
    pub base: [f32; 3],
    pub accent: [f32; 3],
    pub glow: [f32; 3],
}

pub const PALETTES: [Palette; 6] = [
    Palette {
        name: "Steel Blue",
        base: [222.0, 14.0, 30.0],
        accent: [30.0, 88.0, 52.0],
        glow: [1.0, 0.635, 0.227],
    },
    Palette {
        name: "Hull Orange",
        base: [215.0, 10.0, 40.0],
        accent: [30.0, 85.0, 52.0],
        glow: [1.0, 0.702, 0.278],
    },
    Palette {
        name: "Cyan Tech",
        base: [200.0, 25.0, 30.0],
        accent: [185.0, 90.0, 55.0],
        glow: [0.208, 0.941, 1.0],
    },
    Palette {
        name: "Crimson Alert",
        base: [350.0, 18.0, 28.0],
        accent: [5.0, 85.0, 50.0],
        glow: [1.0, 0.231, 0.231],
    },
    Palette {
        name: "Bio Alien",
        base: [150.0, 20.0, 26.0],
        accent: [95.0, 70.0, 50.0],
        glow: [0.49, 1.0, 0.353],
    },
    Palette {
        name: "Ivory Station",
        base: [40.0, 8.0, 66.0],
        accent: [210.0, 70.0, 50.0],
        glow: [0.353, 0.69, 1.0],
    },
];

/// Generator settings, mirroring the HTML tool's panel.
#[derive(Clone, Debug, PartialEq)]
pub struct ScifiParams {
    pub seed: u32,
    pub palette: usize,
    pub layout: Layout,
    pub size: u32,
    pub symmetry: Symmetry,
    pub chamfer: u32,
    pub depth: u32,
    pub seam: u32,
    pub density: u32,
    pub wear: u32,
    pub glow: u32,
    pub balance: bool,
    pub light_deg: u32,
    pub normal_strength: u32,
}

impl Default for ScifiParams {
    fn default() -> Self {
        Self {
            seed: 20241,
            palette: 0,
            layout: Layout::OctagonGrid,
            size: 768,
            symmetry: Symmetry::MirrorXy,
            chamfer: 6,
            depth: 6,
            seam: 3,
            density: 6,
            wear: 6,
            glow: 6,
            balance: true,
            light_deg: 40,
            normal_strength: 5,
        }
    }
}

impl ScifiParams {
    pub fn palette(&self) -> &'static Palette {
        &PALETTES[self.palette.min(PALETTES.len() - 1)]
    }

    pub fn randomise(&mut self) {
        // A small LCG keeps randomise deterministic per seed without pulling in
        // a dependency; the button reseeds first.
        self.seed = self
            .seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223)
            % 1_000_000;
    }
}

/// Generated maps, ready to preview or save.
pub struct ScifiTexture {
    pub size: u32,
    pub panels: usize,
    /// Detail kind and how often it was used.
    pub details: Vec<(String, usize)>,
    /// Imbalance before and after the balancing pass.
    pub imbalance: (f32, f32),
    pub counterweights: usize,
    views: [Vec<u8>; 5],
}

impl ScifiTexture {
    pub fn view(&self, view: ScifiView) -> &[u8] {
        &self.views[view.index()]
    }

    pub fn summary(&self) -> String {
        let mut text = format!(
            "panels {}{}",
            self.panels,
            if self.counterweights > 0 {
                format!(" · {} counterweights", self.counterweights)
            } else {
                String::new()
            }
        );
        if !self.details.is_empty() {
            text.push_str(" · details ");
            text.push_str(
                &self
                    .details
                    .iter()
                    .map(|(name, count)| format!("{name}:{count}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        text.push_str(&format!(
            "\nimbalance {:.3} → {:.3}",
            self.imbalance.0, self.imbalance.1
        ));
        text
    }
}

// --- colour helpers -------------------------------------------------------

fn hsl(h: f32, s: f32, l: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 360.0;
    let s = (s / 100.0).clamp(0.0, 1.0);
    let l = (l / 100.0).clamp(0.0, 1.0);
    if s <= 1e-4 {
        return [l, l, l];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

fn base_color(palette: &Palette, delta: f32) -> [f32; 3] {
    hsl(palette.base[0], palette.base[1], palette.base[2] + delta)
}

/// Panel tone at an absolute lightness, in percent.
fn base_tone(palette: &Palette, lightness: f32) -> [f32; 3] {
    hsl(palette.base[0], palette.base[1], lightness)
}

fn accent_color(palette: &Palette, delta: f32) -> [f32; 3] {
    hsl(
        palette.accent[0],
        palette.accent[1],
        palette.accent[2] + delta,
    )
}

// --- rng / noise ----------------------------------------------------------

/// mulberry32, the generator the HTML tool uses.
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut t = (self.0 ^ (self.0 >> 15)).wrapping_mul(1 | self.0);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        ((t ^ (t >> 14)) as f64 / 4_294_967_296.0) as f32
    }
}

fn hash2(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(374_761_393)
        .wrapping_add((y as u32).wrapping_mul(668_265_263))
        .wrapping_add(seed.wrapping_mul(1_274_126_177));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) as f64 / 4_294_967_296.0) as f32
}

fn value_noise(x: f32, y: f32, seed: u32, period: u32) -> f32 {
    let xi = x.floor();
    let yi = y.floor();
    let fx = x - xi;
    let fy = y - yi;
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);
    let wrap = |v: f32| -> i32 {
        let p = period.max(1) as i32;
        (((v as i32) % p) + p) % p
    };
    let p = hash2(wrap(xi), wrap(yi), seed);
    let q = hash2(wrap(xi + 1.0), wrap(yi), seed);
    let t = hash2(wrap(xi), wrap(yi + 1.0), seed);
    let u = hash2(wrap(xi + 1.0), wrap(yi + 1.0), seed);
    p + (q - p) * sx + (t - p) * sy + (p - q - t + u) * sx * sy
}

fn fbm(x: f32, y: f32, seed: u32, period: u32) -> f32 {
    let mut total = 0.0;
    let mut amp = 0.5;
    let (mut x, mut y, mut period) = (x, y, period);
    for octave in 0..4 {
        total += amp * value_noise(x, y, seed + octave * 7, period);
        x *= 2.0;
        y *= 2.0;
        period *= 2;
        amp *= 0.5;
    }
    total
}

// --- software canvas ------------------------------------------------------

/// Albedo + height + emissive buffers that all drawing targets share.
///
/// Shapes are rasterized with the editor's own anti-aliased rasterizer, so the
/// result has the same edge quality as the normal-map pipeline.
struct Canvas {
    size: u32,
    albedo: Vec<[f32; 3]>,
    height: Vec<f32>,
    glow: Vec<[f32; 3]>,
    clip: Vec<f32>,
    clip_stack: Vec<Vec<f32>>,
    clipped: bool,
}

impl Canvas {
    fn new(size: u32, palette: &Palette) -> Self {
        let size = size.max(64);
        let background = base_color(palette, -28.0);
        Self {
            size,
            albedo: vec![background; (size * size) as usize],
            height: vec![0.14; (size * size) as usize],
            glow: vec![[0.0; 3]; (size * size) as usize],
            clip: vec![1.0; (size * size) as usize],
            clip_stack: Vec::new(),
            clipped: false,
        }
    }

    /// Intersect the active clip with a polygon; nestable.
    fn start_clip(&mut self, poly: &[Vec2]) {
        self.clip_stack.push(std::mem::take(&mut self.clip));
        self.clip = vec![0.0; (self.size * self.size) as usize];
        self.clipped = true;
        self.fill_clip(poly);
    }

    fn fill_clip(&mut self, poly: &[Vec2]) {
        if poly.len() < 3 {
            return;
        }
        let source = ShapeSource::Path(Path::from_polygon(poly));
        let Some((ox, oy, w, h)) = self.raster_bounds(&source) else {
            return;
        };
        let coverage = source.rasterize_region(UVec2::splat(self.size), UVec2::new(ox, oy), w, h);
        let stride = self.size;
        for y in 0..h {
            for x in 0..w {
                let index = (oy + y) as usize * stride as usize + (ox + x) as usize;
                self.clip[index] = coverage.data[(y * w + x) as usize];
            }
        }
    }

    fn end_clip(&mut self) {
        self.clip = self
            .clip_stack
            .pop()
            .unwrap_or_else(|| vec![1.0; (self.size * self.size) as usize]);
        self.clipped = !self.clip_stack.is_empty();
    }

    /// Integer bounding box of a shape, clamped to the canvas.
    fn raster_bounds(&self, source: &ShapeSource) -> Option<(u32, u32, u32, u32)> {
        let bounds = source.bounds()?;
        let x0 = bounds.min.x.floor().clamp(0.0, self.size as f32) as u32;
        let y0 = bounds.min.y.floor().clamp(0.0, self.size as f32) as u32;
        let x1 = (bounds.max.x.ceil() + 1.0).clamp(0.0, self.size as f32) as u32;
        let y1 = (bounds.max.y.ceil() + 1.0).clamp(0.0, self.size as f32) as u32;
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
    }

    fn fill(
        &mut self,
        source: &ShapeSource,
        color: Option<[f32; 3]>,
        height: Option<f32>,
        glow: Option<[f32; 3]>,
    ) {
        if color.is_none() && height.is_none() && glow.is_none() {
            return;
        }
        let Some((ox, oy, w, h)) = self.raster_bounds(source) else {
            return;
        };
        let coverage = source.rasterize_region(UVec2::splat(self.size), UVec2::new(ox, oy), w, h);
        let stride = self.size as usize;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let index = (oy as usize + y) * stride + ox as usize + x;
                let mut amount = coverage.data[y * w as usize + x];
                if self.clipped {
                    amount *= self.clip[index];
                }
                if amount <= 0.0005 {
                    continue;
                }
                let amount = amount.min(1.0);
                if let Some(color) = color {
                    for (dst, src) in self.albedo[index].iter_mut().zip(color) {
                        *dst += (src - *dst) * amount;
                    }
                }
                if let Some(height) = height {
                    let pixel = &mut self.height[index];
                    *pixel += (height - *pixel) * amount;
                }
                if let Some(glow) = glow {
                    for (dst, src) in self.glow[index].iter_mut().zip(glow) {
                        *dst += (src - *dst) * amount;
                    }
                }
            }
        }
    }

    fn rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Option<[f32; 3]>,
        height: Option<f32>,
        glow: Option<[f32; 3]>,
    ) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let source = ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::new(x + w * 0.5, y + h * 0.5),
            size: Vec2::new(w, h),
            corner_radius: 0.0,
        });
        self.fill(&source, color, height, glow);
    }

    fn polygon(&mut self, points: &[Vec2], color: [f32; 3], height: f32, glow: Option<[f32; 3]>) {
        if points.len() < 3 {
            return;
        }
        let source = ShapeSource::Path(Path::from_polygon(points));
        self.fill(&source, Some(color), Some(height), glow);
    }

    fn disc(
        &mut self,
        center: Vec2,
        radius: f32,
        color: [f32; 3],
        height: f32,
        glow: Option<[f32; 3]>,
    ) {
        if radius <= 0.0 {
            return;
        }
        let source = ShapeSource::Primitive(Primitive::Ellipse {
            center,
            radii: Vec2::splat(radius),
        });
        self.fill(&source, Some(color), Some(height), glow);
    }

    /// Thin polyline used for wear scratches; albedo only.
    fn scratch(&mut self, points: &[Vec2], width: f32, value: f32, alpha: f32) {
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let delta = b - a;
            let length = delta.length();
            if length < 1e-3 {
                continue;
            }
            let normal = Vec2::new(-delta.y, delta.x) / length * (width * 0.5).max(0.35);
            let quad = [a + normal, b + normal, b - normal, a - normal];
            let source = ShapeSource::Path(Path::from_polygon(&quad));
            let Some((ox, oy, w, h)) = self.raster_bounds(&source) else {
                continue;
            };
            let coverage =
                source.rasterize_region(UVec2::splat(self.size), UVec2::new(ox, oy), w, h);
            let stride = self.size as usize;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let index = (oy as usize + y) * stride + ox as usize + x;
                    let amount = coverage.data[y * w as usize + x] * alpha;
                    if amount <= 0.0005 {
                        continue;
                    }
                    for dst in &mut self.albedo[index] {
                        *dst += (value - *dst) * amount;
                    }
                }
            }
        }
    }
}

/// Points `distance` inside a polygon, per edge offset.
fn inset(polygon: &[Vec2], distance: f32) -> Vec<Vec2> {
    let mut points: Vec<Vec2> = Vec::with_capacity(polygon.len());
    for point in polygon {
        if points
            .last()
            .is_some_and(|last| last.distance(*point) <= 0.5)
        {
            continue;
        }
        points.push(*point);
    }
    if points.len() > 1 && points[0].distance(*points.last().unwrap()) <= 0.5 {
        points.pop();
    }
    if points.len() < 3 {
        return points;
    }
    let area: f32 = (0..points.len())
        .map(|index| {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            a.x * b.y - b.x * a.y
        })
        .sum();
    let sign = if area > 0.0 { 1.0 } else { -1.0 };
    let edges: Vec<(Vec2, Vec2)> = (0..points.len())
        .map(|index| {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            let delta = b - a;
            let length = delta.length().max(1e-5);
            (
                Vec2::new(
                    a.x - sign * delta.y / length * distance,
                    a.y + sign * delta.x / length * distance,
                ),
                delta / length,
            )
        })
        .collect();
    let count = points.len();
    (0..count)
        .map(|index| {
            let (point, direction) = edges[index];
            let (before, before_dir) = edges[(index + count - 1) % count];
            let det = before_dir.x * direction.y - before_dir.y * direction.x;
            if det.abs() < 1e-6 {
                return point;
            }
            let t = ((point.x - before.x) * direction.y - (point.y - before.y) * direction.x) / det;
            before + before_dir * t
        })
        .collect()
}

/// Chamfer every vertex of a polygon by its own distance.
fn chamfer(polygon: &[Vec2], distances: &[f32]) -> Vec<Vec2> {
    let mut out = Vec::with_capacity(polygon.len() * 2);
    let count = polygon.len();
    for index in 0..count {
        let point = polygon[index];
        let before = polygon[(index + count - 1) % count];
        let after = polygon[(index + 1) % count];
        let len_before = before.distance(point).max(1e-5);
        let len_after = after.distance(point).max(1e-5);
        let amount = distances
            .get(index)
            .copied()
            .unwrap_or(0.0)
            .min(len_before * 0.5)
            .min(len_after * 0.5);
        if amount < 1.0 {
            out.push(point);
            continue;
        }
        out.push(point + (before - point) / len_before * amount);
        out.push(point + (after - point) / len_after * amount);
    }
    out
}

fn bounds_of(points: &[Vec2]) -> (f32, f32, f32, f32) {
    let mut min = Vec2::splat(f32::MAX);
    let mut max = Vec2::splat(f32::MIN);
    for point in points {
        min = min.min(*point);
        max = max.max(*point);
    }
    (min.x, min.y, max.x, max.y)
}

fn pick(rng: &mut Rng, weights: &[(&'static str, f32)]) -> &'static str {
    let total: f32 = weights.iter().map(|(_, weight)| *weight).sum();
    let mut roll = rng.next() * total;
    for (name, weight) in weights {
        if roll <= *weight {
            return name;
        }
        roll -= *weight;
    }
    weights.last().map(|(name, _)| *name).unwrap_or("plain")
}

/// Accumulated visual mass used by the balancing pass.
struct MassPoint {
    x: f32,
    y: f32,
    mass: f32,
}

fn regular_polygon(center: Vec2, radius: f32, sides: u32, rotation: f32) -> Vec<Vec2> {
    let sides = sides.max(3);
    (0..sides)
        .map(|index| {
            let angle = rotation + index as f32 * std::f32::consts::TAU / sides as f32;
            center + Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

// --- detail vocabulary ----------------------------------------------------

fn vents(
    c: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    p: &Palette,
    u: f32,
    rng: &mut Rng,
) -> f32 {
    let horizontal = w >= h;
    let pitch = (5.0 + rng.next() * 3.0) * u;
    let length = if horizontal { h } else { w };
    let slots = (length / pitch).floor().max(0.0) as u32;
    c.rect(x, y, w, h, Some(base_color(p, -14.0)), Some(0.18), None);
    for index in 0..slots {
        let offset = index as f32 * pitch + pitch * 0.25;
        if horizontal {
            c.rect(
                x + 2.0 * u,
                y + offset,
                w - 4.0 * u,
                pitch * 0.5,
                Some(base_color(p, 4.0)),
                Some(0.66),
                None,
            );
        } else {
            c.rect(
                x + offset,
                y + 2.0 * u,
                pitch * 0.5,
                h - 4.0 * u,
                Some(base_color(p, 4.0)),
                Some(0.66),
                None,
            );
        }
    }
    0.0
}

fn hex_detail(
    c: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    p: &Palette,
    u: f32,
    rng: &mut Rng,
) -> f32 {
    let radius = (5.0 + rng.next() * 5.0) * u;
    let dx = radius * 1.73;
    let dy = radius * 1.5;
    let outline = vec![
        Vec2::new(x, y),
        Vec2::new(x + w, y),
        Vec2::new(x + w, y + h),
        Vec2::new(x, y + h),
    ];
    c.start_clip(&outline);
    let rows = (h / dy).floor() as i32 + 2;
    let cols = (w / dx).floor() as i32 + 2;
    for row in -1..rows {
        for col in -1..cols {
            let stagger = if row & 1 == 1 { dx * 0.5 } else { 0.0 };
            let center = Vec2::new(x + col as f32 * dx + stagger, y + row as f32 * dy);
            let hexagon = regular_polygon(center, radius * 0.88, 6, std::f32::consts::FRAC_PI_6);
            let shade = base_color(p, rng.next() * 8.0 - 3.0);
            let height = 0.6 + rng.next() * 0.1;
            c.polygon(&hexagon, shade, height, None);
        }
    }
    c.end_clip();
    0.0
}

fn circuit(
    c: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    p: &Palette,
    u: f32,
    rng: &mut Rng,
) -> f32 {
    let traces = 3 + (rng.next() * w.max(h) / (10.0 * u)) as u32;
    let glowing = rng.next() < 0.5;
    let outline = vec![
        Vec2::new(x, y),
        Vec2::new(x + w, y),
        Vec2::new(x + w, y + h),
        Vec2::new(x, y + h),
    ];
    c.start_clip(&outline);
    for _ in 0..traces {
        let mut px = x + rng.next() * w;
        let mut py = y + rng.next() * h;
        let mut direction = (rng.next() * 8.0) as i32;
        let mut points = vec![Vec2::new(px, py)];
        let steps = 3 + (rng.next() * 6.0) as u32;
        for _ in 0..steps {
            let length = (6.0 + rng.next() * 20.0) * u;
            let angle = direction as f32 * std::f32::consts::PI / 4.0;
            px += angle.cos() * length;
            py += angle.sin() * length;
            points.push(Vec2::new(px, py));
            direction += if rng.next() < 0.5 { 1 } else { -1 };
        }
        let lit = glowing && rng.next() < 0.4;
        let color = if lit {
            accent_color(p, 0.0)
        } else {
            base_color(p, 14.0)
        };
        c.scratch(&points, 1.5 * u, 0.0, 0.0);
        // Traces are albedo + height, optionally emissive.
        for pair in points.windows(2) {
            let delta = pair[1] - pair[0];
            let length = delta.length().max(1e-3);
            let normal = Vec2::new(-delta.y, delta.x) / length * (0.75 * u);
            let quad = [
                pair[0] + normal,
                pair[1] + normal,
                pair[1] - normal,
                pair[0] - normal,
            ];
            c.polygon(&quad, color, 0.62, lit.then_some(p.glow));
        }
        let end = Vec2::new(px, py);
        c.disc(
            end,
            2.2 * u,
            accent_color(p, -10.0),
            0.7,
            lit.then_some(p.glow),
        );
    }
    c.end_clip();
    if glowing {
        w * h * 0.2
    } else {
        0.0
    }
}

fn strip(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, p: &Palette, u: f32) -> f32 {
    let horizontal = w >= h;
    let thickness = w.min(h) * 0.3;
    let (sx, sy, sw, sh) = if horizontal {
        (
            x + 2.0 * u,
            y + (h - thickness) * 0.5,
            w - 4.0 * u,
            thickness,
        )
    } else {
        (
            x + (w - thickness) * 0.5,
            y + 2.0 * u,
            thickness,
            h - 4.0 * u,
        )
    };
    c.rect(
        sx,
        sy,
        sw,
        sh,
        Some(accent_color(p, 0.0)),
        Some(0.4),
        Some(p.glow),
    );
    sw * sh * 0.5
}

fn barcode(
    c: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    p: &Palette,
    u: f32,
    rng: &mut Rng,
) -> f32 {
    let horizontal = w >= h;
    let length = if horizontal { w } else { h };
    let mut offset = 2.0 * u;
    while offset < length - 4.0 * u {
        let width = (1.0 + rng.next() * 3.0) * u;
        if horizontal {
            c.rect(
                x + offset,
                y + h * 0.2,
                width,
                h * 0.6,
                Some(base_color(p, 18.0)),
                Some(0.6),
                None,
            );
        } else {
            c.rect(
                x + w * 0.2,
                y + offset,
                w * 0.6,
                width,
                Some(base_color(p, 18.0)),
                Some(0.6),
                None,
            );
        }
        offset += width + (1.0 + rng.next() * 3.0) * u;
    }
    0.0
}

fn ribs(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, p: &Palette, u: f32, rng: &mut Rng) -> f32 {
    let horizontal = w >= h;
    let pitch = (4.0 + rng.next() * 4.0) * u;
    let length = if horizontal { h } else { w };
    let count = (length / pitch).floor().max(0.0) as u32;
    for index in 0..count {
        let offset = index as f32 * pitch;
        if horizontal {
            c.rect(
                x,
                y + offset,
                w,
                pitch * 0.35,
                Some(base_color(p, -12.0)),
                Some(0.28),
                None,
            );
        } else {
            c.rect(
                x + offset,
                y,
                pitch * 0.35,
                h,
                Some(base_color(p, -12.0)),
                Some(0.28),
                None,
            );
        }
    }
    0.0
}

fn hazard(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, p: &Palette, u: f32) -> f32 {
    c.rect(x, y, w, h, Some(base_color(p, -18.0)), Some(0.5), None);
    let outline = vec![
        Vec2::new(x, y),
        Vec2::new(x + w, y),
        Vec2::new(x + w, y + h),
        Vec2::new(x, y + h),
    ];
    c.start_clip(&outline);
    let pitch = 7.0 * u;
    let mut offset = -h;
    while offset < w + h {
        let stripe = vec![
            Vec2::new(x + offset, y + h),
            Vec2::new(x + offset + pitch, y + h),
            Vec2::new(x + offset + pitch + h, y),
            Vec2::new(x + offset + h, y),
        ];
        c.polygon(&stripe, accent_color(p, 0.0), 0.56, None);
        offset += pitch * 2.0;
    }
    c.end_clip();
    w * h * 0.1
}

fn bolts(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, p: &Palette, u: f32) -> f32 {
    let margin = 3.0 * u;
    for corner in [
        Vec2::new(x + margin, y + margin),
        Vec2::new(x + w - margin, y + margin),
        Vec2::new(x + margin, y + h - margin),
        Vec2::new(x + w - margin, y + h - margin),
    ] {
        c.disc(corner, 1.7 * u, base_color(p, 10.0), 0.95, None);
        c.disc(corner, 0.7 * u, base_color(p, -15.0), 0.7, None);
    }
    0.0
}

fn port(
    c: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    p: &Palette,
    _u: f32,
    rng: &mut Rng,
) -> f32 {
    let radius = w.min(h) * 0.38;
    let center = Vec2::new(x + w * 0.5, y + h * 0.5);
    let glowing = rng.next() < 0.6;
    c.disc(center, radius, base_color(p, -16.0), 0.3, None);
    c.disc(center, radius * 0.82, base_color(p, 6.0), 0.75, None);
    c.disc(center, radius * 0.6, base_color(p, -20.0), 0.35, None);
    if glowing {
        c.disc(
            center,
            radius * 0.32,
            accent_color(p, 0.0),
            0.5,
            Some(p.glow),
        );
        radius * radius * 4.0
    } else {
        let nut = regular_polygon(center, radius * 0.3, 8, 0.4);
        c.polygon(&nut, base_color(p, 10.0), 0.85, None);
        0.0
    }
}

fn greeble(
    c: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    p: &Palette,
    u: f32,
    rng: &mut Rng,
    depth: u32,
) -> f32 {
    if depth > 2 || w < 9.0 * u || h < 9.0 * u {
        return 0.0;
    }
    let cols = 2 + (rng.next() * 2.0) as u32;
    let rows = 1 + (rng.next() * 2.0) as u32;
    let cell_w = w / cols as f32;
    let cell_h = h / rows as f32;
    for row in 0..rows {
        for col in 0..cols {
            if rng.next() < 0.18 {
                continue;
            }
            let bx = x + col as f32 * cell_w + 1.2 * u;
            let by = y + row as f32 * cell_h + 1.2 * u;
            let bw = cell_w - 2.4 * u;
            let bh = cell_h - 2.4 * u;
            let light = p.base[2] + (rng.next() * 16.0 - 6.0);
            let height = 0.56 + depth as f32 * 0.1 + rng.next() * 0.08;
            c.rect(
                bx,
                by,
                bw,
                bh,
                Some(base_tone(p, light)),
                Some(height),
                None,
            );
            c.rect(
                bx,
                by,
                bw,
                u,
                Some(base_tone(p, light + 12.0)),
                Some(height + 0.04),
                None,
            );
            c.rect(
                bx,
                by + bh - u,
                bw,
                u,
                Some(base_tone(p, light - 12.0)),
                Some(height - 0.05),
                None,
            );
            greeble(
                c,
                bx + 2.0 * u,
                by + 2.0 * u,
                bw - 4.0 * u,
                bh - 4.0 * u,
                p,
                u,
                rng,
                depth + 1,
            );
        }
    }
    0.0
}

// --- panel layout ---------------------------------------------------------

fn lines(rng: &mut Rng, depth: u32, total: f32, mirrored: bool) -> Vec<f32> {
    // Degenerate panel: one zero-width cell is the safest shape for callers.
    if !total.is_finite() || total <= f32::EPSILON {
        return vec![0.0, 0.0];
    }
    const MAX_LINES: usize = 4096;
    let step = total / (depth as f32 / 2.0 + 1.5);
    let mut positions = vec![0.0f32];
    if !mirrored {
        for _ in 0..MAX_LINES {
            let next = positions[positions.len() - 1] + step * (0.6 + 0.9 * rng.next());
            positions.push(next);
            if total - next <= step * 0.9 {
                break;
            }
        }
        let scale = total / *positions.last().unwrap();
        positions.iter().map(|value| value * scale).collect()
    } else {
        let centre = if rng.next() < 0.65 {
            total * (0.18 + 0.2 * rng.next())
        } else {
            0.0
        };
        let half = (total - centre) / 2.0;
        for _ in 0..MAX_LINES {
            let next = positions[positions.len() - 1] + step * (0.6 + 0.9 * rng.next());
            positions.push(next);
            if half - next <= step * 0.9 {
                break;
            }
        }
        let scale = half / *positions.last().unwrap();
        let half_lines: Vec<f32> = positions.iter().map(|value| value * scale).collect();
        let mut out = half_lines.clone();
        if centre > 0.0 {
            out.extend(half_lines.iter().rev().map(|value| total - value));
        } else {
            out.extend(
                half_lines[..half_lines.len() - 1]
                    .iter()
                    .rev()
                    .map(|value| total - value),
            );
        }
        out
    }
}

fn panel_polygons(params: &ScifiParams, rng: &mut Rng, size: f32, u: f32) -> Vec<Vec<Vec2>> {
    let sym = params.symmetry;
    let cs = params.chamfer as f32 / 10.0;
    let mut polys: Vec<Vec<Vec2>> = Vec::new();
    if params.layout == Layout::OctagonGrid {
        let xs = lines(rng, params.depth, size, sym.mirrored_x());
        let ys = lines(rng, params.depth, size, sym.mirrored_y());
        let nx = xs.len() - 1;
        let ny = ys.len() - 1;
        let mut chamfers = vec![vec![0.0f32; ny]; nx];
        for i in 0..nx {
            for j in 0..ny {
                let left = if i > 0 {
                    xs[i] - xs[i - 1]
                } else {
                    xs[nx] - xs[nx - 1]
                };
                let bottom = if j > 0 {
                    ys[j] - ys[j - 1]
                } else {
                    ys[ny] - ys[ny - 1]
                };
                let m = (xs[i + 1] - xs[i])
                    .min(left)
                    .min(ys[j + 1] - ys[j])
                    .min(bottom);
                chamfers[i][j] = if rng.next() < 0.08 {
                    0.0
                } else {
                    m * 0.46 * (1.0f32).min((0.55 + 0.45 * rng.next()) * cs * 1.5)
                };
            }
        }
        for i in 0..nx {
            for j in 0..ny {
                let (i2, j2) = ((i + 1) % nx, (j + 1) % ny);
                let quad = [
                    Vec2::new(xs[i], ys[j]),
                    Vec2::new(xs[i + 1], ys[j]),
                    Vec2::new(xs[i + 1], ys[j + 1]),
                    Vec2::new(xs[i], ys[j + 1]),
                ];
                let distances = [
                    chamfers[i][j],
                    chamfers[i2][j],
                    chamfers[i2][j2],
                    chamfers[i][j2],
                ];
                polys.push(chamfer(&quad, &distances));
            }
        }
        for i in 0..=nx {
            for j in 0..=ny {
                let amount = chamfers[i % nx][j % ny];
                if amount > 8.0 * u {
                    let (vx, vy) = (xs[i], ys[j]);
                    polys.push(vec![
                        Vec2::new(vx - amount, vy),
                        Vec2::new(vx, vy - amount),
                        Vec2::new(vx + amount, vy),
                        Vec2::new(vx, vy + amount),
                    ]);
                }
            }
        }
    } else {
        let ys = lines(rng, params.depth, size, sym.mirrored_y());
        for j in 0..ys.len() - 1 {
            let xs = lines(rng, params.depth, size, sym.mirrored_x());
            let n = xs.len() - 1;
            let (y0, y1) = (ys[j], ys[j + 1]);
            let row_height = y1 - y0;
            let min_width = (0..n).map(|k| xs[k + 1] - xs[k]).fold(f32::MAX, f32::min);
            let slant = (min_width * 0.4).min(row_height * 0.5)
                * (0.3 + 0.7 * rng.next())
                * (1.0f32).min(cs * 1.4);
            let mut offsets: Vec<f32> = (0..=n)
                .map(|k| {
                    if k == 0 || k == n || (sym.mirrored_x() && 2 * k == n) {
                        0.0
                    } else {
                        let side = if sym.mirrored_x() && 2 * k > n {
                            -1.0
                        } else {
                            1.0
                        };
                        side * if k % 2 == 1 { 1.0 } else { -1.0 } * slant
                    }
                })
                .collect();
            if sym.mirrored_x() {
                for k in 0..=n {
                    if 2 * k > n {
                        offsets[k] = -offsets[n - k];
                    }
                }
            }
            for k in 0..n {
                let width = xs[k + 1] - xs[k];
                let d = width.min(row_height) * (0.08 + 0.3 * rng.next()) * cs * 1.6;
                let quad = [
                    Vec2::new(xs[k] + offsets[k], y0),
                    Vec2::new(xs[k + 1] + offsets[k + 1], y0),
                    Vec2::new(xs[k + 1] - offsets[k + 1], y1),
                    Vec2::new(xs[k] - offsets[k], y1),
                ];
                polys.push(chamfer(&quad, &[d, d, d, d]));
            }
        }
    }
    polys
}

/// One reported detail kind and how often it was used.
type DetailCounts = Vec<(String, usize)>;

fn count_detail(counts: &mut DetailCounts, name: &str) {
    if let Some(entry) = counts.iter_mut().find(|(key, _)| key == name) {
        entry.1 += 1;
    } else {
        counts.push((name.to_string(), 1));
    }
}

/// Decorate one panel: seam, bevel rim, bolts and a detail inside.
fn cell(
    canvas: &mut Canvas,
    polygon: &[Vec2],
    params: &ScifiParams,
    palette: &Palette,
    u: f32,
    rng: &mut Rng,
    masses: &mut Vec<MassPoint>,
    counts: &mut DetailCounts,
) {
    let (x0, y0, x1, y1) = bounds_of(polygon);
    let smallest = (x1 - x0).min(y1 - y0);
    let seam = params.seam as f32 * u;
    if smallest < seam * 3.0 + 8.0 * u {
        return;
    }
    let tone = palette.base[2] + rng.next() * 8.0 - 4.0;
    let height = 0.5 + rng.next() * 0.06;
    let center = Vec2::new((x0 + x1) * 0.5, (y0 + y1) * 0.5);

    canvas.polygon(
        &inset(polygon, seam),
        base_tone(palette, tone - 8.0),
        height - 0.1,
        None,
    );
    canvas.polygon(
        &inset(polygon, seam + 3.0 * u),
        base_tone(palette, tone - 18.0),
        0.3,
        None,
    );
    let inner_seam = seam + 5.0 * u;
    if smallest < inner_seam * 2.0 + 6.0 * u {
        return;
    }
    canvas.polygon(
        &inset(polygon, inner_seam),
        base_tone(palette, tone),
        height,
        None,
    );

    if smallest < 32.0 * u {
        count_detail(counts, "port");
        let m = smallest * 0.8;
        let mass = port(
            canvas,
            center.x - m * 0.4,
            center.y - m * 0.4,
            m,
            m,
            palette,
            u,
            rng,
        );
        if mass > 0.0 {
            masses.push(MassPoint {
                x: center.x,
                y: center.y,
                mass: mass * params.glow as f32 / 6.0,
            });
        }
        return;
    }

    let mut depth = inner_seam;
    if smallest > 36.0 * u && rng.next() < 0.75 {
        let bevel = inner_seam + 2.0 * u + smallest * 0.08;
        canvas.polygon(
            &inset(polygon, bevel),
            base_tone(palette, tone - 20.0),
            0.34,
            None,
        );
        depth = bevel + 2.5 * u;
        canvas.polygon(
            &inset(polygon, depth),
            base_tone(palette, tone + 3.0),
            height + 0.06,
            None,
        );
    }

    if smallest > 60.0 * u {
        let rim = inset(polygon, depth + 4.0 * u);
        if rim.len() <= 10 {
            for point in rim {
                let direction = center - point;
                let length = direction.length().max(1.0);
                let stud = point + direction / length * 3.0 * u;
                canvas.disc(stud, 2.5 * u, base_color(palette, 9.0), 0.92, None);
                let hexagon = regular_polygon(stud, 1.0 * u, 6, 0.0);
                canvas.polygon(&hexagon, base_color(palette, -16.0), 0.7, None);
            }
        }
    }

    let inner = inset(polygon, depth + smallest * 0.12);
    let (a0, b0, a1, b1) = bounds_of(&inner);
    let (width, height_inner) = (a1 - a0, b1 - b0);
    if width < 14.0 * u || height_inner < 14.0 * u {
        return;
    }
    let aspect = width.max(height_inner) / width.min(height_inner).max(1e-3);
    let density = params.density as f32 / 6.0;
    let kind = if aspect > 3.2 {
        pick(
            rng,
            &[
                ("strip", 3.0),
                ("barcode", 2.0),
                ("ribs", 2.0),
                ("hazard", 2.5),
                ("vents", 1.5),
            ],
        )
    } else if width.min(height_inner) < 42.0 * u {
        pick(
            rng,
            &[
                ("port", 2.0),
                ("barcode", 1.5),
                ("circuit", 1.0),
                ("plain", 2.0),
                ("vents", 1.0),
            ],
        )
    } else {
        pick(
            rng,
            &[
                ("vents", 2.5 * density),
                ("hex", 1.2),
                ("circuit", 2.5 * density),
                ("greeble", 2.0 * density),
                ("ribs", 1.0),
                ("port", 1.0),
                ("bolts", 0.6),
                ("plain", 3.0 / density.max(0.1)),
                ("hazard", 0.7),
            ],
        )
    };
    count_detail(counts, kind);
    if kind == "plain" {
        return;
    }
    canvas.start_clip(&inset(polygon, depth + 2.0 * u));
    let mass = match kind {
        "vents" => vents(canvas, a0, b0, width, height_inner, palette, u, rng),
        "hex" => hex_detail(canvas, a0, b0, width, height_inner, palette, u, rng),
        "circuit" => circuit(canvas, a0, b0, width, height_inner, palette, u, rng),
        "strip" => strip(canvas, a0, b0, width, height_inner, palette, u),
        "barcode" => barcode(canvas, a0, b0, width, height_inner, palette, u, rng),
        "ribs" => ribs(canvas, a0, b0, width, height_inner, palette, u, rng),
        "hazard" => hazard(canvas, a0, b0, width, height_inner, palette, u),
        "port" => port(canvas, a0, b0, width, height_inner, palette, u, rng),
        "greeble" => greeble(canvas, a0, b0, width, height_inner, palette, u, rng, 0),
        "bolts" => bolts(canvas, a0, b0, width, height_inner, palette, u),
        _ => 0.0,
    };
    canvas.end_clip();
    if mass > 0.0 {
        masses.push(MassPoint {
            x: center.x,
            y: center.y,
            mass: mass * params.glow as f32 / 6.0,
        });
    }
}

/// Net visual mass and its imbalance around the canvas centre.
fn net_mass(masses: &[MassPoint], size: f32) -> (f32, f32, f32, f32) {
    let centre = size * 0.5;
    let (mut x, mut y, mut mass) = (0.0, 0.0, 0.0);
    for point in masses {
        x += point.mass * (point.x - centre);
        y += point.mass * (point.y - centre);
        mass += point.mass;
    }
    let imbalance = if mass > 1e-6 {
        (x * x + y * y).sqrt() / (mass * centre)
    } else {
        0.0
    };
    (x, y, mass, imbalance)
}

/// Add glowing beacons that pull the composition back towards equilibrium.
fn balance(
    canvas: &mut Canvas,
    palette: &Palette,
    u: f32,
    rng: &mut Rng,
    polys: &[Vec<Vec2>],
    masses: &mut Vec<MassPoint>,
) -> usize {
    let size = canvas.size as f32;
    let centre = size * 0.5;
    let levels: Vec<(f32, f32, f32)> = polys
        .iter()
        .map(|polygon| {
            let (x0, y0, x1, y1) = bounds_of(polygon);
            ((x0 + x1) * 0.5, (y0 + y1) * 0.5, (x1 - x0).min(y1 - y0))
        })
        .filter(|level| level.2 > 40.0 * u)
        .collect();
    if levels.is_empty() || masses.is_empty() {
        return 0;
    }
    let attempts = (levels.len() as f32 * 0.25).ceil() as usize;
    let mut added = 0;
    for _ in 0..attempts {
        let (nx, ny, mass, imbalance) = net_mass(masses, size);
        if imbalance <= 0.03 {
            break;
        }
        let contribution = mass / masses.len() as f32 * 0.7;
        let mut best: Option<(f32, f32, f32)> = None;
        let mut best_imbalance = f32::INFINITY;
        for _ in 0..30 {
            let level = levels[(rng.next() * levels.len() as f32) as usize];
            let ex = nx + contribution * (level.0 - centre);
            let ey = ny + contribution * (level.1 - centre);
            let candidate = (ex * ex + ey * ey).sqrt();
            if candidate < best_imbalance {
                best_imbalance = candidate;
                best = Some(level);
            }
        }
        let Some(level) = best else {
            break;
        };
        if best_imbalance >= (nx * nx + ny * ny).sqrt() {
            break;
        }
        let radius = level.2 * 0.07 + 2.0 * u;
        let centre_point = Vec2::new(level.0, level.1);
        canvas.disc(
            centre_point,
            radius * 1.6,
            base_color(palette, -18.0),
            0.3,
            None,
        );
        canvas.disc(
            centre_point,
            radius * 1.3,
            base_color(palette, 8.0),
            0.8,
            None,
        );
        canvas.disc(
            centre_point,
            radius * 0.8,
            accent_color(palette, 0.0),
            0.6,
            Some(palette.glow),
        );
        masses.push(MassPoint {
            x: level.0,
            y: level.1,
            mass: contribution,
        });
        added += 1;
    }
    added
}

fn put(rgba: &mut [u8], index: usize, color: [f32; 3]) {
    let offset = index * 4;
    for (dst, value) in rgba[offset..offset + 3].iter_mut().zip(color) {
        *dst = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    rgba[offset + 3] = 255;
}

/// Separable box blur with wrapped edges, used for the emissive halo.
fn blur_rgb(source: &[[f32; 3]], size: u32, radius: u32) -> Vec<[f32; 3]> {
    let n = size as usize;
    if radius == 0 {
        return source.to_vec();
    }
    let radius = radius as usize;
    let mut horizontal = vec![[0.0f32; 3]; n * n];
    for y in 0..n {
        for x in 0..n {
            let mut sum = [0.0f32; 3];
            for offset in -(radius as i32)..=(radius as i32) {
                let index = ((x as i32 + offset).rem_euclid(n as i32)) as usize;
                let pixel = source[y * n + index];
                for channel in 0..3 {
                    sum[channel] += pixel[channel];
                }
            }
            let count = (radius * 2 + 1) as f32;
            horizontal[y * n + x] = [sum[0] / count, sum[1] / count, sum[2] / count];
        }
    }
    let mut out = vec![[0.0f32; 3]; n * n];
    for y in 0..n {
        for x in 0..n {
            let mut sum = [0.0f32; 3];
            for offset in -(radius as i32)..=(radius as i32) {
                let index = ((y as i32 + offset).rem_euclid(n as i32)) as usize;
                let pixel = horizontal[index * n + x];
                for channel in 0..3 {
                    sum[channel] += pixel[channel];
                }
            }
            let count = (radius * 2 + 1) as f32;
            out[y * n + x] = [sum[0] / count, sum[1] / count, sum[2] / count];
        }
    }
    out
}

/// Height field -> the five output maps.
fn shade(params: &ScifiParams, canvas: &Canvas) -> [Vec<u8>; 5] {
    let size = canvas.size as usize;
    let count = size * size;
    let height = &canvas.height;
    let wear_strength = params.wear as f32 / 10.0;
    let frequency = 4.0 / canvas.size as f32;
    let seed = params.seed;
    let strength = params.normal_strength as f32 * 2.2;
    let light = params.light_deg as f32 * std::f32::consts::PI / 180.0;
    let mut l = [light.cos() * 0.6, light.sin() * 0.6, 0.55];
    let l_len = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
    for value in &mut l {
        *value /= l_len;
    }
    let mut half = [l[0], l[1], l[2] + 1.0];
    let h_len = (half[0] * half[0] + half[1] * half[1] + half[2] * half[2]).sqrt();
    for value in &mut half {
        *value /= h_len;
    }
    let halo = blur_rgb(
        &canvas.glow,
        canvas.size,
        (3.0 * canvas.size as f32 / 512.0).round() as u32,
    );
    let mut views = [
        vec![0u8; count * 4],
        vec![0u8; count * 4],
        vec![0u8; count * 4],
        vec![0u8; count * 4],
        vec![0u8; count * 4],
    ];
    for y in 0..size {
        for x in 0..size {
            let index = y * size + x;
            let left = height[y * size + (x + size - 1) % size];
            let right = height[y * size + (x + 1) % size];
            let up = height[((y + size - 1) % size) * size + x];
            let down = height[((y + 1) % size) * size + x];
            let nx = left - right;
            let ny = up - down;
            let laplacian = (left + right + up + down - 4.0 * height[index]).abs();
            let ring = (height[y * size + (x + 4) % size]
                + height[y * size + (x + size - 4) % size]
                + height[((y + 4) % size) * size + x]
                + height[((y + size - 4) % size) * size + x])
                / 4.0;
            let ao = (1.0 - (ring - height[index]).max(0.0) * 3.0).clamp(0.0, 1.0);
            let wx = x as f32 * frequency;
            let wy = y as f32 * frequency;
            let warp = fbm(wx, wy, seed + 9, 4);
            let warped = fbm(wx + 3.0 * warp, wy + 3.0 * warp, seed + 3, 4);
            let wear = ((warped - 0.35) * 2.6).clamp(0.0, 1.0)
                * wear_strength
                * (0.4 + (laplacian * 14.0).min(1.0) * 1.6);

            let ex = nx * strength;
            let ey = ny * strength;
            let inv = 1.0 / (ex * ex + ey * ey + 1.0).sqrt();
            let (sx, sy, sz) = (ex * inv, ey * inv, inv);
            let lit = (sx * l[0] + sy * l[1] + sz * l[2]).max(0.0);
            let spec = (sx * half[0] + sy * half[1] + sz * half[2])
                .max(0.0)
                .powf(28.0)
                * (0.45 - 0.3 * wear);
            let ambient = 0.45 + 0.55 * ao * (1.0 - wear * 0.3);
            let mix = (wear * 0.8).min(1.0);
            let wear_color = [0.5, 0.4, 0.3];
            let albedo = canvas.albedo[index];
            let mut lit_albedo = [0.0f32; 3];
            for channel in 0..3 {
                lit_albedo[channel] = albedo[channel] * (1.0 - mix)
                    + wear_color[channel] * (0.6 + 0.4 * (1.0 - ao)) * mix;
            }
            let diffuse = 0.22 + 0.95 * lit * ambient;
            let sharp = canvas.glow[index];
            let soft = halo[index];
            let mut glow = [0.0f32; 3];
            for channel in 0..3 {
                glow[channel] = (sharp[channel] + soft[channel] * 0.9).min(1.0);
            }
            let mut final_color = [0.0f32; 3];
            for channel in 0..3 {
                final_color[channel] = lit_albedo[channel] * diffuse + spec + glow[channel] * 1.176;
            }
            put(&mut views[0], index, final_color);
            put(&mut views[1], index, lit_albedo);
            put(
                &mut views[2],
                index,
                [sx * 0.5 + 0.5, 0.5 - sy * 0.5, sz * 0.5 + 0.5],
            );
            let gray = height[index];
            put(&mut views[3], index, [gray, gray, gray]);
            put(&mut views[4], index, glow);
        }
    }
    views
}

/// Mirror the left half onto the right half, pixel-accurate.
fn mirror_x(canvas: &mut Canvas) {
    let n = canvas.size as usize;
    for y in 0..n {
        for x in 0..n / 2 {
            let source = y * n + x;
            let target = y * n + (n - 1 - x);
            canvas.albedo[target] = canvas.albedo[source];
            canvas.height[target] = canvas.height[source];
            canvas.glow[target] = canvas.glow[source];
        }
    }
}

/// Mirror the top half onto the bottom half, pixel-accurate.
fn mirror_y(canvas: &mut Canvas) {
    let n = canvas.size as usize;
    for y in 0..n / 2 {
        for x in 0..n {
            let source = y * n + x;
            let target = (n - 1 - y) * n + x;
            canvas.albedo[target] = canvas.albedo[source];
            canvas.height[target] = canvas.height[source];
            canvas.glow[target] = canvas.glow[source];
        }
    }
}

/// Run the whole generator.
pub fn generate(params: &ScifiParams) -> ScifiTexture {
    let size = params.size.clamp(96, 4096);
    let n = size as f32;
    let u = n / 512.0;
    let palette = params.palette();
    let mut rng = Rng::new(params.seed);
    let mut canvas = Canvas::new(size, palette);
    let mut masses: Vec<MassPoint> = Vec::new();
    let mut counts: DetailCounts = Vec::new();

    let polys = panel_polygons(params, &mut rng, n, u);
    let mirrored = params.symmetry.mirrored_x() || params.symmetry.mirrored_y();
    if mirrored {
        // Only the mirror quadrant is drawn; the rest is reflected afterwards.
        let quadrant = vec![
            Vec2::ZERO,
            Vec2::new(n * 0.5, 0.0),
            Vec2::new(n * 0.5, n * 0.5),
            Vec2::new(0.0, n * 0.5),
        ];
        canvas.start_clip(&quadrant);
    }
    for polygon in &polys {
        cell(
            &mut canvas,
            polygon,
            params,
            palette,
            u,
            &mut rng,
            &mut masses,
            &mut counts,
        );
    }
    if mirrored {
        canvas.end_clip();
    }

    let before = net_mass(&masses, n).3;
    let counterweights = if params.balance && !params.symmetry.mirrored_x() {
        balance(&mut canvas, palette, u, &mut rng, &polys, &mut masses)
    } else {
        0
    };
    if params.symmetry.mirrored_x() {
        mirror_x(&mut canvas);
    }
    if params.symmetry.mirrored_y() {
        mirror_y(&mut canvas);
    }

    for _ in 0..params.wear * 35 {
        let x = rng.next() * n;
        let y = rng.next() * n;
        let angle = rng.next() * std::f32::consts::TAU;
        let length = (8.0 + rng.next() * 50.0) * u;
        let alpha = 0.05 + rng.next() * 0.15;
        canvas.scratch(
            &[
                Vec2::new(x, y),
                Vec2::new(x + angle.cos() * length, y + angle.sin() * length),
            ],
            0.6 * u,
            1.0,
            alpha,
        );
    }

    let after = net_mass(&masses, n).3;
    let views = shade(params, &canvas);
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ScifiTexture {
        size,
        panels: polys.len(),
        details: counts,
        imbalance: (before, after),
        counterweights,
        views,
    }
}

/// Write one generated view as an 8-bit PNG.
pub fn save_png(path: &std::path::Path, size: u32, rgba: &[u8]) -> Result<(), String> {
    let Some(image) = image::RgbaImage::from_raw(size, size, rgba.to_vec()) else {
        return Err("view buffer does not match its size".into());
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image.save(path).map_err(|error| error.to_string())
}

/// Window state for the optional generator.
///
/// Lives in `EditorState` so the menu can open the window, and holds nothing
/// but its own parameters and preview texture: opening it can never change the
/// document.
#[derive(Default)]
pub struct ScifiPanel {
    pub open: bool,
    pub params: ScifiParams,
    pub texture: Option<ScifiTexture>,
    /// Uploaded preview texture and the view it was built from.
    pub texture_handle: Option<bevy_egui::egui::TextureHandle>,
    pub texture_view: Option<ScifiView>,
    pub view: ScifiView,
    /// When a parameter last changed; the window regenerates once the user
    /// stops dragging, so sliders stay responsive.
    pub pending: Option<std::time::Instant>,
    pub status: String,
}

impl ScifiPanel {
    /// Note a parameter change; the window debounces the regeneration.
    pub fn mark_dirty(&mut self) {
        self.pending = Some(std::time::Instant::now());
    }

    /// Regenerate every map from the current parameters.
    pub fn regenerate(&mut self) {
        let started = std::time::Instant::now();
        self.texture = Some(generate(&self.params));
        self.texture_handle = None;
        self.texture_view = None;
        self.pending = None;
        self.status = format!("generated in {} ms", started.elapsed().as_millis());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> ScifiParams {
        ScifiParams {
            size: 192,
            ..Default::default()
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let first = generate(&small());
        let second = generate(&small());
        for view in ScifiView::ALL {
            assert_eq!(first.view(view), second.view(view), "{:?}", view);
        }
    }

    #[test]
    fn changing_the_seed_changes_the_texture() {
        let first = generate(&small());
        let mut params = small();
        params.seed = 4711;
        let second = generate(&params);
        assert_ne!(
            first.view(ScifiView::Albedo),
            second.view(ScifiView::Albedo)
        );
    }

    #[test]
    fn every_view_is_opaque_and_sized() {
        let texture = generate(&small());
        for view in ScifiView::ALL {
            let data = texture.view(view);
            assert_eq!(data.len(), (texture.size * texture.size * 4) as usize);
            assert!(data.chunks(4).all(|pixel| pixel[3] == 255));
        }
        assert!(texture.panels > 4, "the layout produces panels");
    }

    #[test]
    fn normals_are_unit_length_and_varied() {
        let texture = generate(&small());
        let data = texture.view(ScifiView::Normal);
        let mut distinct = 0usize;
        let mut previous: Option<[u8; 3]> = None;
        for pixel in data.chunks(4) {
            let x = pixel[0] as f32 / 255.0 * 2.0 - 1.0;
            let y = pixel[1] as f32 / 255.0 * 2.0 - 1.0;
            let z = pixel[2] as f32 / 255.0 * 2.0 - 1.0;
            let length = (x * x + y * y + z * z).sqrt();
            assert!((length - 1.0).abs() < 0.05, "normal length {length}");
            let rgb = [pixel[0], pixel[1], pixel[2]];
            if previous != Some(rgb) {
                distinct += 1;
            }
            previous = Some(rgb);
        }
        assert!(distinct > 100, "the normal map has structure: {distinct}");
    }

    #[test]
    fn all_layouts_palettes_and_symmetries_generate() {
        for layout in Layout::ALL {
            for symmetry in Symmetry::ALL {
                let mut params = small();
                params.layout = layout;
                params.symmetry = symmetry;
                params.palette = 3;
                params.depth = 4;
                let texture = generate(&params);
                assert!(texture.panels > 0, "{layout:?} / {symmetry:?}");
            }
        }
    }

    #[test]
    fn mirroring_makes_the_texture_tile_symmetric() {
        let mut params = small();
        params.symmetry = Symmetry::MirrorXy;
        params.balance = false;
        // Wear scratches are drawn after mirroring on purpose, like the HTML
        // tool, so they would break pixel symmetry; disable them here.
        params.wear = 0;
        let texture = generate(&params);
        let data = texture.view(ScifiView::Albedo);
        let n = texture.size as usize;
        let pixel = |x: usize, y: usize| {
            let offset = (y * n + x) * 4;
            [data[offset], data[offset + 1], data[offset + 2]]
        };
        for y in (0..n).step_by(7) {
            for x in (0..n / 2).step_by(7) {
                assert_eq!(pixel(x, y), pixel(n - 1 - x, y), "mirror x at {x},{y}");
                assert_eq!(pixel(x, y), pixel(x, n - 1 - y), "mirror y at {x},{y}");
            }
        }
    }
}
