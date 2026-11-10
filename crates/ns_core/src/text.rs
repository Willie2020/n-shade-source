//! Live text layers and font handling.
//!
//! Text is a [`ShapeSource`](crate::path::ShapeSource) variant: the glyph
//! outlines are rasterized to coverage and then go through the same bevel,
//! mask, clipping and boolean pipeline as every other shape. Nothing here
//! touches Bevy or the UI, so the CPU evaluator stays the reference.
//!
//! Fonts are either bundled (Inter, Inter SemiBold, JetBrains Mono, all SIL
//! OFL 1.1) or loaded from a `.ttf`/`.otf` file chosen by the user. A missing
//! font file silently falls back to Inter so opening a project on another
//! machine never fails; the Properties panel still reports the original font
//! name and flags it as missing.

use std::collections::HashMap;
use std::path::Path as StdPath;
use std::sync::{Arc, Mutex, OnceLock};

use glam::Vec2;
use serde::{Deserialize, Serialize};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::grid::Grid;
use crate::path::{rasterize_polygons, Bounds, FillRule, Path};
use crate::Result;

/// The three faces embedded in the executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BuiltinFont {
    Inter,
    InterSemiBold,
    JetBrainsMono,
}

impl BuiltinFont {
    pub const ALL: [Self; 3] = [Self::Inter, Self::InterSemiBold, Self::JetBrainsMono];

    pub fn label(self) -> &'static str {
        match self {
            Self::Inter => "Inter",
            Self::InterSemiBold => "Inter SemiBold",
            Self::JetBrainsMono => "JetBrains Mono",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Inter => include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
            Self::InterSemiBold => include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"),
            Self::JetBrainsMono => {
                include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf")
            }
        }
    }
}

/// Where a text layer's font comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FontSource {
    /// A face embedded in the binary.
    Builtin(BuiltinFont),
    /// A font file on disk. `name` is the family name read when the file was
    /// picked and is kept even when the file later disappears.
    File { path: String, name: String },
}

impl Default for FontSource {
    fn default() -> Self {
        Self::Builtin(BuiltinFont::Inter)
    }
}

impl FontSource {
    pub fn label(&self) -> String {
        match self {
            Self::Builtin(font) => font.label().to_string(),
            Self::File { name, .. } => name.clone(),
        }
    }

    /// True for bundled faces and for files that still exist on disk.
    pub fn is_available(&self) -> bool {
        match self {
            Self::Builtin(_) => true,
            Self::File { path, .. } => StdPath::new(path).is_file(),
        }
    }

    /// Load a font file and keep its family name for the font list.
    pub fn from_file(path: impl AsRef<StdPath>) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path)?;
        let face = Face::parse(&data, 0)
            .map_err(|err| crate::CoreError::Message(format!("invalid font file: {err}")))?;
        let name = family_name(&face)
            .or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Custom font".to_string());
        Ok(Self::File {
            path: path.to_string_lossy().into_owned(),
            name,
        })
    }
}

fn family_name(face: &Face<'_>) -> Option<String> {
    for name in face.names() {
        if name.name_id == ttf_parser::name_id::FAMILY
            || name.name_id == ttf_parser::name_id::TYPOGRAPHIC_FAMILY
        {
            if let Some(text) = name.to_string() {
                let text = text.trim();
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

/// Horizontal alignment of every line inside the text block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub const ALL: [Self; 3] = [Self::Left, Self::Center, Self::Right];

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Center => "Center",
            Self::Right => "Right",
        }
    }
}

fn default_line_height() -> f32 {
    1.2
}

/// A live, editable text run.
///
/// `anchor` is the top-left corner of the text block in document pixels. The
/// block grows right and down; alignment shifts each line inside the widest
/// line's width. `size_px` is the em size, so a font size of 100 px gives a
/// capital height of roughly 70 px.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextSource {
    pub content: String,
    #[serde(default)]
    pub font: FontSource,
    pub anchor: Vec2,
    pub size_px: f32,
    /// Extra space between glyphs, in pixels. Negative values tighten a face.
    #[serde(default)]
    pub tracking_px: f32,
    /// Baseline-to-baseline distance as a multiple of `size_px`.
    #[serde(default = "default_line_height")]
    pub line_height: f32,
    #[serde(default)]
    pub align: TextAlign,
}

impl TextSource {
    pub fn new(content: impl Into<String>, anchor: Vec2, size_px: f32) -> Self {
        Self {
            content: content.into(),
            font: FontSource::default(),
            anchor,
            size_px,
            tracking_px: 0.0,
            line_height: default_line_height(),
            align: TextAlign::Left,
        }
    }

    pub fn font_available(&self) -> bool {
        self.font.is_available()
    }

    /// Layout box of the whole block. This is also the selection bounds.
    pub fn bounds(&self) -> Bounds {
        let layout = self.layout();
        Bounds {
            min: self.anchor,
            max: self.anchor + Vec2::new(layout.width, layout.height),
        }
    }

    /// Closed rectangle used for the canvas selection highlight.
    pub fn outline(&self) -> Vec<Vec2> {
        let bounds = self.bounds();
        vec![
            bounds.min,
            Vec2::new(bounds.max.x, bounds.min.y),
            bounds.max,
            Vec2::new(bounds.min.x, bounds.max.y),
        ]
    }

    pub fn contains(&self, point: Vec2) -> bool {
        self.bounds().contains(point)
    }

    /// Scale and translate the block. Text scales uniformly so glyphs never
    /// stretch: horizontal-only and vertical-only handles still change the em
    /// size, just less than the dragged axis.
    pub fn transformed(&self, scale: Vec2, offset: Vec2) -> Option<Self> {
        let sx = scale.x.abs().max(1e-3);
        let sy = scale.y.abs().max(1e-3);
        let factor = (sx * sy).sqrt();
        let mut out = self.clone();
        out.anchor = offset + self.anchor * scale;
        out.size_px = (self.size_px * factor).clamp(2.0, 4096.0);
        out.tracking_px *= factor;
        Some(out)
    }

    /// Glyph outlines as editable path loops. Text keeps a live source, but
    /// booleans, knife cuts and "Convert to path" all read the same outline.
    pub fn to_path(&self) -> Option<Path> {
        let loops = self.glyph_loops();
        (!loops.is_empty()).then(|| Path::from_loops(loops, FillRule::NonZero))
    }

    /// Anti-aliased coverage inside a document-space sub-rectangle.
    pub(crate) fn rasterize_region(&self, origin: Vec2, w: u32, h: u32) -> Grid<f32> {
        let loops = self.glyph_loops();
        rasterize_polygons(&loops, w, h, FillRule::NonZero, origin)
    }

    fn layout(&self) -> TextLayout {
        let bytes = resolved_bytes(&self.font);
        match Face::parse(&bytes, 0) {
            Ok(face) => TextLayout::from_face(&face, self),
            Err(_) => TextLayout::empty(self.size_px),
        }
    }

    fn glyph_loops(&self) -> Vec<Vec<Vec2>> {
        if self.content.is_empty() {
            return Vec::new();
        }
        let bytes = resolved_bytes(&self.font);
        let Ok(face) = Face::parse(&bytes, 0) else {
            return Vec::new();
        };
        let layout = TextLayout::from_face(&face, self);
        let units = f32::from(face.units_per_em().max(1));
        let scale = sanitized_size(self.size_px) / units;
        let mut loops = Vec::new();
        for (line_index, line) in layout.lines.iter().enumerate() {
            let baseline = self.anchor.y + layout.ascent + line_index as f32 * layout.step;
            for glyph in &line.glyphs {
                let origin = Vec2::new(self.anchor.x + line.offset + glyph.x, baseline);
                let mut builder = ContourBuilder::new(origin, scale);
                face.outline_glyph(glyph.id, &mut builder);
                loops.extend(builder.finish());
            }
        }
        loops
    }
}

fn sanitized_size(size_px: f32) -> f32 {
    if size_px.is_finite() {
        size_px.clamp(2.0, 4096.0)
    } else {
        64.0
    }
}

struct PlacedGlyph {
    id: GlyphId,
    /// Pen position at the glyph origin, relative to the line start.
    x: f32,
}

struct PlacedLine {
    glyphs: Vec<PlacedGlyph>,
    width: f32,
    /// Left edge after alignment inside the block width.
    offset: f32,
}

struct TextLayout {
    lines: Vec<PlacedLine>,
    width: f32,
    height: f32,
    ascent: f32,
    step: f32,
}

impl TextLayout {
    fn empty(size: f32) -> Self {
        let size = sanitized_size(size);
        Self {
            lines: Vec::new(),
            width: size * 0.15,
            height: size * 0.5,
            ascent: size * 0.8,
            step: size * default_line_height(),
        }
    }

    fn from_face(face: &Face<'_>, text: &TextSource) -> Self {
        let size = sanitized_size(text.size_px);
        let units = f32::from(face.units_per_em().max(1));
        let scale = size / units;

        let mut ascent = f32::from(face.ascender()) * scale;
        let mut descent = f32::from(face.descender()) * scale;
        if !ascent.is_finite() || ascent <= 0.0 {
            ascent = size * 0.8;
        }
        if !descent.is_finite() || descent >= 0.0 {
            descent = -size * 0.2;
        }

        let step = size * text.line_height.clamp(0.4, 4.0);
        let mut lines = Vec::new();
        let mut widest = 0.0_f32;

        // `split` keeps one empty line for an empty string, so the caret and
        // selection box still exist before the first character is typed.
        for raw in text.content.split('\n') {
            let line = raw.trim_end_matches('\r');
            let mut glyphs = Vec::new();
            let mut pen = 0.0_f32;
            let mut previous: Option<GlyphId> = None;
            let space = face
                .glyph_index(' ')
                .and_then(|id| face.glyph_hor_advance(id))
                .map(|advance| f32::from(advance) * scale)
                .unwrap_or(size * 0.3);

            for (index, ch) in line.chars().enumerate() {
                if index > 0 {
                    pen += text.tracking_px;
                }
                if ch == '\t' {
                    pen += space * 4.0;
                    previous = None;
                    continue;
                }
                let Some(id) = face.glyph_index(ch) else {
                    pen += space;
                    previous = None;
                    continue;
                };
                if let Some(left) = previous {
                    pen += kerning(face, left, id) * scale;
                }
                glyphs.push(PlacedGlyph { id, x: pen });
                pen += f32::from(face.glyph_hor_advance(id).unwrap_or(0)) * scale;
                previous = Some(id);
            }

            widest = widest.max(pen);
            lines.push(PlacedLine {
                glyphs,
                width: pen,
                offset: 0.0,
            });
        }

        // A block always keeps a small width so empty text remains clickable.
        let width = widest.max(size * 0.15);
        for line in &mut lines {
            line.offset = match text.align {
                TextAlign::Left => 0.0,
                TextAlign::Center => (width - line.width) * 0.5,
                TextAlign::Right => width - line.width,
            };
        }

        let count = lines.len().max(1) as f32;
        let height = ((count - 1.0) * step + ascent - descent).max(size * 0.5);
        Self {
            lines,
            width,
            height,
            ascent,
            step,
        }
    }
}

fn kerning(face: &Face<'_>, left: GlyphId, right: GlyphId) -> f32 {
    let Some(table) = face.tables().kern.as_ref() else {
        return 0.0;
    };
    for subtable in table.subtables {
        if subtable.horizontal && !subtable.variable && !subtable.has_cross_stream {
            if let Some(value) = subtable.glyphs_kerning(left, right) {
                return f32::from(value);
            }
        }
    }
    0.0
}

/// Collects one glyph's contours in document space. Font coordinates are
/// y-up, so the builder flips them around the baseline.
struct ContourBuilder {
    origin: Vec2,
    scale: f32,
    contours: Vec<Vec<Vec2>>,
    current: Vec<Vec2>,
    last: (f32, f32),
}

impl ContourBuilder {
    fn new(origin: Vec2, scale: f32) -> Self {
        Self {
            origin,
            scale,
            contours: Vec::new(),
            current: Vec::new(),
            last: (0.0, 0.0),
        }
    }

    fn map(&self, x: f32, y: f32) -> Vec2 {
        Vec2::new(
            self.origin.x + x * self.scale,
            self.origin.y - y * self.scale,
        )
    }

    fn flush(&mut self) {
        if self.current.len() >= 2 {
            self.contours.push(std::mem::take(&mut self.current));
        } else {
            self.current.clear();
        }
    }

    fn push_quad(&mut self, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
        let a = Vec2::new(p0.0, p0.1);
        let b = Vec2::new(p1.0, p1.1);
        let c = Vec2::new(p2.0, p2.1);
        let steps = curve_steps((b - a).length() + (c - b).length(), self.scale, 2);
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let one = 1.0 - t;
            let p = a * (one * one) + b * (2.0 * one * t) + c * (t * t);
            self.current.push(self.map(p.x, p.y));
        }
    }

    fn push_cubic(&mut self, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) {
        let a = Vec2::new(p0.0, p0.1);
        let b = Vec2::new(p1.0, p1.1);
        let c = Vec2::new(p2.0, p2.1);
        let d = Vec2::new(p3.0, p3.1);
        let polyline = (b - a).length() + (c - b).length() + (d - c).length();
        let steps = curve_steps(polyline, self.scale, 3);
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let one = 1.0 - t;
            let p = a * (one * one * one)
                + b * (3.0 * one * one * t)
                + c * (3.0 * one * t * t)
                + d * (t * t * t);
            self.current.push(self.map(p.x, p.y));
        }
    }

    fn finish(mut self) -> Vec<Vec<Vec2>> {
        self.flush();
        self.contours
    }
}

impl OutlineBuilder for ContourBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.current.push(self.map(x, y));
        self.last = (x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.current.push(self.map(x, y));
        self.last = (x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let start = self.last;
        self.push_quad(start, (x1, y1), (x, y));
        self.last = (x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let start = self.last;
        self.push_cubic(start, (x1, y1), (x2, y2), (x, y));
        self.last = (x, y);
    }

    fn close(&mut self) {
        self.flush();
    }
}

fn curve_steps(polyline_len: f32, scale: f32, minimum: usize) -> usize {
    let doc_len = polyline_len * scale.abs().max(1e-4);
    ((doc_len / 1.25).ceil() as usize).clamp(minimum, 96)
}

fn builtin_bytes(font: BuiltinFont) -> Arc<Vec<u8>> {
    static INTER: OnceLock<Arc<Vec<u8>>> = OnceLock::new();
    static INTER_SEMIBOLD: OnceLock<Arc<Vec<u8>>> = OnceLock::new();
    static JETBRAINS: OnceLock<Arc<Vec<u8>>> = OnceLock::new();
    match font {
        BuiltinFont::Inter => {
            Arc::clone(INTER.get_or_init(|| Arc::new(BuiltinFont::Inter.bytes().to_vec())))
        }
        BuiltinFont::InterSemiBold => Arc::clone(
            INTER_SEMIBOLD.get_or_init(|| Arc::new(BuiltinFont::InterSemiBold.bytes().to_vec())),
        ),
        BuiltinFont::JetBrainsMono => Arc::clone(
            JETBRAINS.get_or_init(|| Arc::new(BuiltinFont::JetBrainsMono.bytes().to_vec())),
        ),
    }
}

type FileFontCache = Mutex<HashMap<String, Option<Arc<Vec<u8>>>>>;

fn file_bytes(path: &str) -> Option<Arc<Vec<u8>>> {
    static CACHE: OnceLock<FileFontCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some(bytes) = cache.get(path) {
            return bytes.clone();
        }
    }
    let bytes = std::fs::read(path).ok().map(Arc::new);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(path.to_string(), bytes.clone());
    }
    bytes
}

/// Font data used for layout and rasterization. A missing external file falls
/// back to Inter so evaluation never fails.
pub fn resolved_bytes(source: &FontSource) -> Arc<Vec<u8>> {
    match source {
        FontSource::Builtin(font) => builtin_bytes(*font),
        FontSource::File { path, .. } => {
            file_bytes(path).unwrap_or_else(|| builtin_bytes(BuiltinFont::Inter))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::{Layer, LayerKind};
    use crate::sdf::Profile;
    use glam::UVec2;

    fn text(content: &str) -> TextSource {
        TextSource::new(content, Vec2::new(100.0, 100.0), 96.0)
    }

    #[test]
    fn layout_bounds_grow_with_content_and_size() {
        let narrow = text("I");
        let wide = text("N-SHADE");
        assert!(wide.bounds().size().x > narrow.bounds().size().x);
        let mut big = wide.clone();
        big.size_px = 192.0;
        assert!(big.bounds().size().x > wide.bounds().size().x * 1.7);
    }

    #[test]
    fn tracking_widens_the_block() {
        let mut tracked = text("TEXT");
        let plain = tracked.bounds().size().x;
        tracked.tracking_px = 20.0;
        assert!((tracked.bounds().size().x - plain - 60.0).abs() < 0.5);
    }

    #[test]
    fn newlines_add_lines_and_height() {
        let mut lines = text("ONE\nTWO\nTHREE");
        let single = text("ONE");
        assert!(lines.bounds().size().y > single.bounds().size().y * 2.4);
        lines.align = TextAlign::Right;
        assert!(lines.bounds().size().x >= single.bounds().size().x);
    }

    #[test]
    fn alignment_keeps_the_block_and_shifts_lines() {
        let mut multiline = text("WWWW\nI");
        let width = multiline.bounds().size().x;
        multiline.align = TextAlign::Center;
        assert!((multiline.bounds().size().x - width).abs() < 1e-3);
        let loops = multiline.glyph_loops();
        assert!(!loops.is_empty());
    }

    #[test]
    fn glyph_coverage_renders_and_keeps_holes() {
        let source = TextSource::new("O", Vec2::new(20.0, 20.0), 180.0);
        let origin = Vec2::ZERO;
        let coverage = source.rasterize_region(origin, 240, 240);
        let covered = coverage.iter().filter(|value| **value > 0.5).count();
        assert!(covered > 500, "expected glyph coverage, got {covered}");
        // The counter of an O stays empty; sample near the visual centre.
        let bounds = source.bounds();
        let centre = bounds.center();
        assert!(coverage.get(centre.x as u32, centre.y as u32) < &0.05);
    }

    #[test]
    fn text_layer_bevels_and_round_trips() {
        let mut doc = crate::Document::new(UVec2::splat(256));
        let mut layer = Layer::shape(
            "N-SHADE",
            crate::ShapeSource::Text(TextSource::new("N", Vec2::new(40.0, 40.0), 160.0)),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 6.0;
            shape.height_px = 10.0;
            shape.profile = Profile::Round;
        }
        doc.layers.push(layer);
        let normals = doc.evaluate();
        assert!(normals
            .iter()
            .any(|normal| (normal.length() - 1.0).abs() < 1e-4 && normal.z < 0.999));

        let ron = ron::ser::to_string(&doc).unwrap();
        let loaded: crate::Document = ron::from_str(&ron).unwrap();
        let crate::LayerKind::Shape(shape) = &loaded.layers[0].kind else {
            panic!("text layer lost its shape kind");
        };
        let crate::ShapeSource::Text(text) = &shape.source else {
            panic!("text source did not round-trip");
        };
        assert_eq!(text.content, "N");
        assert_eq!(text.size_px, 160.0);
    }

    #[test]
    fn missing_font_file_falls_back_instead_of_failing() {
        let mut source = text("ABC");
        source.font = FontSource::File {
            path: "C:/definitely/missing/font.ttf".into(),
            name: "Missing".into(),
        };
        assert!(!source.font_available());
        assert!(!source.glyph_loops().is_empty());
        assert!(source.bounds().size().x > 1.0);
    }

    #[test]
    fn external_font_file_loads_and_renders() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/Inter-Regular.ttf"
        );
        let font = FontSource::from_file(path).unwrap();
        assert!(font.is_available());
        let mut source = text("File font");
        source.font = font;
        assert!(!source.glyph_loops().is_empty());
        assert!(source.bounds().size().x > 10.0);
    }

    #[test]
    fn transform_scales_size_and_moves_anchor() {
        let source = text("Aa");
        let moved = source
            .transformed(Vec2::splat(2.0), Vec2::new(10.0, 20.0))
            .unwrap();
        assert!((moved.size_px - 192.0).abs() < 0.01);
        assert_eq!(moved.anchor, Vec2::new(210.0, 220.0));
    }
}
