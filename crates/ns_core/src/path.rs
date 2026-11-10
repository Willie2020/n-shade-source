use std::cmp::Ordering;
use std::collections::HashMap;

use glam::{IVec2, Mat2, Vec2};
use kurbo::{BezPath, Cap, Join, PathEl, Point, Stroke, StrokeOpts};
use serde::{Deserialize, Serialize};

use crate::grid::Grid;
use crate::sdf::Profile;
use crate::text::TextSource;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ShapeSource {
    /// Lasso, magic-wand, imported mask, or any pre-rasterized coverage.
    /// The grid spans the whole document.
    Raster(Grid<f32>),
    /// Baked pixels cropped to the area they actually cover. Unlike [`Self::Raster`]
    /// this keeps the layer's bounds, so a baked layer still selects, snaps and
    /// moves like the shape it replaced.
    RasterRegion(RasterRegion),
    /// Editable Bézier path.
    Path(Path),
    /// Parametric primitive.
    Primitive(Primitive),
    /// Live text. Glyph outlines are rasterized on demand, so the bevel,
    /// boolean, knife and mask pipelines treat it like any other shape.
    Text(TextSource),
}

impl ShapeSource {
    /// Rasterize to an anti-aliased coverage grid in document pixel space.
    pub fn rasterize(&self, w: u32, h: u32) -> Grid<f32> {
        self.rasterize_region(glam::UVec2::new(w, h), glam::UVec2::ZERO, w, h)
    }

    /// Rasterize only a document-space sub-rectangle. `origin` is in document
    /// pixels; the returned grid is `w x h` and starts at that origin.
    pub fn rasterize_region(
        &self,
        doc_size: glam::UVec2,
        origin: glam::UVec2,
        w: u32,
        h: u32,
    ) -> Grid<f32> {
        match self {
            ShapeSource::Raster(raster) => {
                if raster.w == doc_size.x && raster.h == doc_size.y {
                    return Grid::from_fn(w, h, |x, y| *raster.get(origin.x + x, origin.y + y));
                }
                Grid::from_fn(w, h, |x, y| {
                    let dx = (origin.x + x) as f32 / doc_size.x.max(1) as f32 * raster.w as f32;
                    let dy = (origin.y + y) as f32 / doc_size.y.max(1) as f32 * raster.h as f32;
                    *raster.get(
                        (dx as i32).clamp(0, raster.w as i32 - 1) as u32,
                        (dy as i32).clamp(0, raster.h as i32 - 1) as u32,
                    )
                })
            }
            ShapeSource::RasterRegion(region) => Grid::from_fn(w, h, |x, y| {
                region.get_document(origin.x as i64 + x as i64, origin.y as i64 + y as i64)
            }),
            ShapeSource::Path(path) => {
                let polys = flatten_path(path);
                let offset = Vec2::new(origin.x as f32, origin.y as f32);
                rasterize_polygons(&polys, w, h, path.fill_rule, offset)
            }
            ShapeSource::Primitive(primitive) => {
                let offset = Vec2::new(origin.x as f32, origin.y as f32);
                rasterize_primitive(primitive, w, h, offset)
            }
            ShapeSource::Text(text) => {
                let offset = Vec2::new(origin.x as f32, origin.y as f32);
                text.rasterize_region(offset, w, h)
            }
        }
    }

    /// Polyline outline(s) in document pixel space, used by canvas overlays.
    pub fn outline(&self) -> Vec<Vec<Vec2>> {
        match self {
            ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) => Vec::new(),
            ShapeSource::Path(path) => flatten_path(path),
            ShapeSource::Primitive(primitive) => vec![primitive_polygon(primitive)],
            ShapeSource::Text(text) => vec![text.outline()],
        }
    }

    pub fn bounds(&self) -> Option<Bounds> {
        match self {
            ShapeSource::RasterRegion(region) => Some(region.bounds()),
            // The full-document raster has no cheap extent; its coverage would
            // have to be scanned every frame. Baked layers use a region.
            ShapeSource::Raster(_) => None,
            _ => Bounds::from_points(self.outline().iter().flat_map(|p| p.iter().copied())),
        }
    }

    /// Hit test in document space.
    pub fn contains(&self, point: Vec2) -> bool {
        match self {
            ShapeSource::Raster(raster) => {
                if point.x < 0.0 || point.y < 0.0 {
                    return false;
                }
                let x = point.x as u32;
                let y = point.y as u32;
                x < raster.w && y < raster.h && *raster.get(x, y) > 0.5
            }
            ShapeSource::RasterRegion(region) => {
                region.get_document(point.x.floor() as i64, point.y.floor() as i64) > 0.5
            }
            ShapeSource::Path(path) => {
                point_in_polygons(&flatten_path(path), point, path.fill_rule)
            }
            ShapeSource::Primitive(primitive) => {
                point_in_polygons(&[primitive_polygon(primitive)], point, FillRule::NonZero)
            }
            ShapeSource::Text(text) => text.contains(point),
        }
    }

    /// Affine edit used by canvas drag/resize: `p' = offset + p * scale`.
    pub fn transformed(&self, scale: Vec2, offset: Vec2) -> Option<Self> {
        match self {
            ShapeSource::Raster(_) => None,
            ShapeSource::RasterRegion(region) => region.transformed(scale, offset),
            ShapeSource::Path(path) => Some(ShapeSource::Path(transform_path(path, scale, offset))),
            ShapeSource::Primitive(primitive) => Some(ShapeSource::Primitive(transform_primitive(
                primitive, scale, offset,
            ))),
            // Text scales uniformly and has no flip parameter. A reflection
            // therefore bakes to an editable path, exactly like a rotated copy,
            // so the mirror is visually exact instead of a translated block.
            ShapeSource::Text(text) if scale.x * scale.y < 0.0 => {
                let mut path = text.to_path()?;
                path.apply_transform(glam::Affine2::from_scale_angle_translation(
                    scale, 0.0, offset,
                ));
                Some(ShapeSource::Path(path))
            }
            ShapeSource::Text(text) => text.transformed(scale, offset).map(ShapeSource::Text),
        }
    }

    /// Affine edit with rotation: `p' = offset + R(rotation) * (p * scale)`.
    ///
    /// Rotated copies cannot keep primitives that have no rotation field (rect,
    /// ellipse) or styled live text, so those bake to an editable path. This is
    /// the same trade-off the mirror operation makes for stars, and it keeps the
    /// copy visually exact instead of silently ignoring the rotation.
    pub fn transformed_affine(&self, scale: Vec2, rotation: f32, offset: Vec2) -> Option<Self> {
        if rotation.abs() < 1e-6 {
            return self.transformed(scale, offset);
        }
        if let ShapeSource::RasterRegion(region) = self {
            return region.transformed_affine(scale, rotation, offset);
        }
        let mut path = self.to_path()?;
        path.apply_transform(glam::Affine2::from_scale_angle_translation(
            scale, rotation, offset,
        ));
        Some(ShapeSource::Path(path))
    }

    /// Convert to an editable Bezier/path representation. Raster sources are
    /// rejected because they have no path topology to edit.
    pub fn to_path(&self) -> Option<Path> {
        match self {
            ShapeSource::Path(path) => Some(path.clone()),
            ShapeSource::Primitive(_) => Some(Path::from_polygon(&self.as_polygon()?)),
            ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) => None,
            ShapeSource::Text(text) => text.to_path(),
        }
    }

    /// Single closed polygon suitable for the GPU SDF path, if the source is
    /// representable without losing topology.
    pub fn as_polygon(&self) -> Option<Vec<Vec2>> {
        match self {
            ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) => None,
            ShapeSource::Path(path) => {
                if path.fill_rule != FillRule::NonZero
                    || path.subpaths.len() != 1
                    || !path.subpaths[0].closed
                {
                    return None;
                }
                flatten_path(path).into_iter().next()
            }
            ShapeSource::Primitive(primitive) => Some(primitive_polygon(primitive)),
            // Text has many contours and holes. The GPU gate rejects it, so the
            // CPU evaluator renders the exact glyph coverage instead.
            ShapeSource::Text(_) => None,
        }
    }
}

/// Raster coverage cropped to the pixels it actually covers.
///
/// `origin` is the document-pixel position of the top-left pixel in
/// `coverage`, so a baked layer keeps its place on the canvas and reports a
/// real selection box without storing an almost empty document-sized grid.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RasterRegion {
    pub coverage: Grid<f32>,
    pub origin: IVec2,
}

/// Sub-pixel slack when turning transformed corners into a destination grid.
const RESAMPLE_EDGE_EPSILON: f32 = 1.0e-3;

impl RasterRegion {
    pub fn new(coverage: Grid<f32>, origin: IVec2) -> Self {
        Self { coverage, origin }
    }

    /// Crop a document-sized coverage grid to its non-empty pixels.
    pub fn from_document(coverage: &Grid<f32>) -> Self {
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for (y, row) in coverage.rows().enumerate() {
            for (x, value) in row.iter().enumerate() {
                if *value > 0.0 {
                    min_x = min_x.min(x as u32);
                    min_y = min_y.min(y as u32);
                    max_x = max_x.max(x as u32);
                    max_y = max_y.max(y as u32);
                }
            }
        }
        if min_x == u32::MAX {
            return Self::new(Grid::new(1, 1, 0.0), IVec2::ZERO);
        }
        let w = max_x - min_x + 1;
        let h = max_y - min_y + 1;
        let mut data = Vec::with_capacity((w as usize) * (h as usize));
        let row_len = coverage.w as usize;
        for y in min_y..=max_y {
            let start = y as usize * row_len;
            data.extend_from_slice(
                &coverage.as_slice()[start + min_x as usize..=start + max_x as usize],
            );
        }
        Self::new(
            Grid::from_vec(w, h, data),
            IVec2::new(min_x as i32, min_y as i32),
        )
    }

    pub fn bounds(&self) -> Bounds {
        let min = self.origin.as_vec2();
        let max = min + Vec2::new(self.coverage.w as f32, self.coverage.h as f32);
        Bounds { min, max }
    }

    /// Coverage at a document pixel, or zero outside the region.
    pub fn get_document(&self, x: i64, y: i64) -> f32 {
        let local_x = x - self.origin.x as i64;
        let local_y = y - self.origin.y as i64;
        if local_x < 0
            || local_y < 0
            || local_x >= self.coverage.w as i64
            || local_y >= self.coverage.h as i64
        {
            return 0.0;
        }
        *self.coverage.get(local_x as u32, local_y as u32)
    }

    /// Bilinear sample in grid-index space, where integer coordinates sit on
    /// pixel centres. Outside the grid reads as zero.
    fn sample_index(&self, x: f32, y: f32) -> f32 {
        let w = self.coverage.w as i32;
        let h = self.coverage.h as i32;
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        let sample = |ix: i32, iy: i32| -> f32 {
            if ix < 0 || iy < 0 || ix >= w || iy >= h {
                0.0
            } else {
                *self.coverage.get(ix as u32, iy as u32)
            }
        };
        let top = sample(x0, y0) * (1.0 - fx) + sample(x0 + 1, y0) * fx;
        let bottom = sample(x0, y0 + 1) * (1.0 - fx) + sample(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    fn transformed(&self, scale: Vec2, offset: Vec2) -> Option<ShapeSource> {
        if scale == Vec2::ONE {
            // Whole-pixel moves keep the baked pixels crisp.
            let shift = IVec2::new(offset.x.round() as i32, offset.y.round() as i32);
            return Some(ShapeSource::RasterRegion(Self::new(
                self.coverage.clone(),
                self.origin + shift,
            )));
        }
        self.resampled(Mat2::from_diagonal(scale), offset)
    }

    fn transformed_affine(&self, scale: Vec2, rotation: f32, offset: Vec2) -> Option<ShapeSource> {
        let matrix = Mat2::from_angle(rotation) * Mat2::from_diagonal(scale);
        self.resampled(matrix, offset)
    }

    /// Resample through a raw affine matrix. Canvas rotation already carries an
    /// [`glam::Affine2`], so it hands the matrix over directly.
    pub fn transformed_matrix(&self, matrix: Mat2, offset: Vec2) -> Option<ShapeSource> {
        self.resampled(matrix, offset)
    }

    fn resampled(&self, matrix: Mat2, offset: Vec2) -> Option<ShapeSource> {
        let determinant = matrix.determinant();
        if determinant.abs() < 1.0e-8 {
            return None;
        }
        let bounds = self.bounds();
        let corners = [
            bounds.min,
            Vec2::new(bounds.max.x, bounds.min.y),
            Vec2::new(bounds.min.x, bounds.max.y),
            bounds.max,
        ];
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for corner in corners {
            let p = matrix * corner + offset;
            min = min.min(p);
            max = max.max(p);
        }
        let origin = IVec2::new(
            (min.x + RESAMPLE_EDGE_EPSILON).floor() as i32,
            (min.y + RESAMPLE_EDGE_EPSILON).floor() as i32,
        );
        // Rotation and scaling introduce sub-pixel float noise in the corners.
        // The epsilon keeps that noise from adding a transparent row/column.
        let w = ((max.x - RESAMPLE_EDGE_EPSILON).ceil() as i64 - origin.x as i64).max(1) as u64;
        let h = ((max.y - RESAMPLE_EDGE_EPSILON).ceil() as i64 - origin.y as i64).max(1) as u64;
        // A runaway drag should not try to allocate a gigapixel coverage grid.
        if w > 16_384 || h > 16_384 || w * h > 64 * 1024 * 1024 {
            return None;
        }
        let (w, h) = (w as u32, h as u32);
        let inverse = matrix.inverse();
        let origin_f = origin.as_vec2();
        let coverage = Grid::from_fn(w, h, |x, y| {
            let target = origin_f + Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let source = inverse * (target - offset);
            // Grid index -> document pixel centre, so undo the half-pixel
            // offset of both grids before sampling.
            self.sample_index(
                source.x - self.origin.x as f32 - 0.5,
                source.y - self.origin.y as f32 - 0.5,
            )
        });
        Some(ShapeSource::RasterRegion(Self::new(coverage, origin)))
    }
}

/// Axis-aligned bounds in document pixel space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Vec2,
    pub max: Vec2,
}

impl Bounds {
    pub fn from_points(points: impl IntoIterator<Item = Vec2>) -> Option<Self> {
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        let mut any = false;
        for p in points {
            if !p.is_finite() {
                continue;
            }
            min = min.min(p);
            max = max.max(p);
            any = true;
        }
        any.then_some(Self { min, max })
    }

    pub fn center(self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    pub fn size(self) -> Vec2 {
        self.max - self.min
    }

    pub fn contains(self, p: Vec2) -> bool {
        p.x >= self.min.x && p.y >= self.min.y && p.x <= self.max.x && p.y <= self.max.y
    }
}

fn point_in_polygons(polygons: &[Vec<Vec2>], point: Vec2, fill_rule: FillRule) -> bool {
    let winding = winding_number(polygons, point);
    match fill_rule {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => winding & 1 != 0,
    }
}

/// Signed winding number of `point` against closed polylines.
fn winding_number(polygons: &[Vec<Vec2>], point: Vec2) -> i32 {
    let mut winding = 0i32;
    for poly in polygons {
        if poly.len() < 3 {
            continue;
        }
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            if a.y <= point.y {
                if b.y > point.y && cross(b - a, point - a) > 0.0 {
                    winding += 1;
                }
            } else if b.y <= point.y && cross(b - a, point - a) < 0.0 {
                winding -= 1;
            }
        }
    }
    winding
}

fn cross(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

fn primitive_polygon(primitive: &Primitive) -> Vec<Vec2> {
    match primitive {
        Primitive::Rect {
            center,
            size,
            corner_radius,
        } => rounded_rect_polygon(*center, *size, *corner_radius, 8),
        Primitive::Ellipse { center, radii } => {
            let segments = 64;
            (0..segments)
                .map(|i| {
                    let a = std::f32::consts::TAU * i as f32 / segments as f32;
                    *center + Vec2::new(a.cos(), a.sin()) * *radii
                })
                .collect()
        }
        Primitive::Polygon { points } => points.clone(),
        Primitive::Star {
            center,
            inner_radius,
            outer_radius,
            points,
            rotation,
        } => {
            if *points < 3 {
                return Vec::new();
            }
            let mut poly = Vec::with_capacity((*points * 2) as usize);
            let step = std::f32::consts::TAU / (*points as f32 * 2.0);
            for i in 0..(*points * 2) {
                let r = if i % 2 == 0 {
                    *outer_radius
                } else {
                    *inner_radius
                };
                let a = *rotation + step * i as f32 - std::f32::consts::FRAC_PI_2;
                poly.push(*center + Vec2::new(a.cos(), a.sin()) * r);
            }
            poly
        }
    }
}

fn rounded_rect_polygon(center: Vec2, size: Vec2, radius: f32, segments: u32) -> Vec<Vec2> {
    let half = size * 0.5;
    let r = radius.max(0.0).min(half.x.abs()).min(half.y.abs());
    if r <= 1e-4 {
        return vec![
            center + Vec2::new(-half.x, -half.y),
            center + Vec2::new(half.x, -half.y),
            center + Vec2::new(half.x, half.y),
            center + Vec2::new(-half.x, half.y),
        ];
    }

    let mut out = Vec::with_capacity((segments as usize + 1) * 4);
    let corners = [
        (
            Vec2::new(half.x - r, -half.y + r),
            -std::f32::consts::FRAC_PI_2,
        ),
        (Vec2::new(half.x - r, half.y - r), 0.0),
        (
            Vec2::new(-half.x + r, half.y - r),
            std::f32::consts::FRAC_PI_2,
        ),
        (Vec2::new(-half.x + r, -half.y + r), std::f32::consts::PI),
    ];
    for (corner_center, start) in corners {
        for s in 0..=segments {
            let a = start + std::f32::consts::FRAC_PI_2 * s as f32 / segments as f32;
            out.push(center + corner_center + Vec2::new(a.cos(), a.sin()) * r);
        }
    }
    out
}

fn transform_path(path: &Path, scale: Vec2, offset: Vec2) -> Path {
    let mut out = path.clone();
    for subpath in &mut out.subpaths {
        for node in &mut subpath.nodes {
            node.pos = offset + node.pos * scale;
            node.handle_in *= scale;
            node.handle_out *= scale;
        }
    }
    out
}

fn transform_primitive(primitive: &Primitive, scale: Vec2, offset: Vec2) -> Primitive {
    let s = Vec2::new(scale.x.abs(), scale.y.abs());
    match primitive {
        Primitive::Rect {
            center,
            size,
            corner_radius,
        } => Primitive::Rect {
            center: offset + *center * scale,
            size: *size * s,
            corner_radius: *corner_radius * s.min_element(),
        },
        Primitive::Ellipse { center, radii } => Primitive::Ellipse {
            center: offset + *center * scale,
            radii: *radii * s,
        },
        Primitive::Polygon { points } => Primitive::Polygon {
            points: points.iter().map(|p| offset + *p * scale).collect(),
        },
        Primitive::Star {
            center,
            inner_radius,
            outer_radius,
            points,
            rotation,
        } => Primitive::Star {
            center: offset + *center * scale,
            inner_radius: *inner_radius * s.min_element(),
            outer_radius: *outer_radius * s.min_element(),
            points: *points,
            rotation: *rotation,
        },
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Path {
    pub subpaths: Vec<SubPath>,
    pub fill_rule: FillRule,
}

/// One flattened line piece of a path, tagged with its source edge.
#[derive(Clone, Copy, Debug)]
pub struct EdgeSegment {
    pub a: Vec2,
    pub b: Vec2,
    pub edge: u16,
}

impl Path {
    pub fn new(fill_rule: FillRule) -> Self {
        Self {
            subpaths: Vec::new(),
            fill_rule,
        }
    }

    /// Apply an affine transform to every node and Bézier handle.
    pub fn apply_transform(&mut self, transform: glam::Affine2) {
        for subpath in &mut self.subpaths {
            for node in &mut subpath.nodes {
                node.pos = transform.transform_point2(node.pos);
                node.handle_in = transform.transform_vector2(node.handle_in);
                node.handle_out = transform.transform_vector2(node.handle_out);
            }
        }
    }

    /// Expand a stroke into a filled outline band.
    ///
    /// Round joins and caps are used. The result uses the even-odd fill rule
    /// so open subpaths become one loop per stroke and closed subpaths keep a
    /// hole. Width is in document pixels.
    pub fn stroke_outline(&self, width: f32) -> Option<Path> {
        if width <= 0.05 || self.subpaths.is_empty() {
            return None;
        }
        let bez = to_kurbo_path(self);
        let style = Stroke {
            width: width as f64,
            join: Join::Round,
            miter_limit: 4.0,
            start_cap: Cap::Round,
            end_cap: Cap::Round,
            dash_pattern: Default::default(),
            dash_offset: 0.0,
        };
        let outline = kurbo::stroke(bez.iter(), &style, &StrokeOpts::default(), 0.05);
        let path = from_kurbo_path(&outline);
        (!path.subpaths.is_empty()).then_some(path)
    }

    /// Approximate bounds from the flattened outline.
    pub fn bounds(&self) -> Option<Bounds> {
        Bounds::from_points(
            flatten_path(self)
                .iter()
                .flat_map(|polyline| polyline.iter().copied()),
        )
    }

    pub fn node_count(&self) -> usize {
        self.subpaths
            .iter()
            .map(|subpath| subpath.nodes.len())
            .sum()
    }

    /// Number of bevel edges in one subpath: one per segment.
    pub fn edge_count(&self, subpath: usize) -> usize {
        self.subpaths.get(subpath).map_or(0, |subpath| {
            let count = subpath.nodes.len();
            if count < 2 {
                0
            } else if subpath.closed {
                count
            } else {
                count - 1
            }
        })
    }

    /// Profile override for one edge, or `None` when it follows the shape.
    /// The override lives on the node that starts the edge.
    pub fn edge_profile(&self, subpath: usize, edge: usize) -> Option<&Profile> {
        if edge >= self.edge_count(subpath) {
            return None;
        }
        self.subpaths.get(subpath)?.nodes.get(edge)?.bevel.as_ref()
    }

    pub fn set_edge_profile(&mut self, subpath: usize, edge: usize, profile: Option<Profile>) {
        if edge >= self.edge_count(subpath) {
            return;
        }
        if let Some(node) = self
            .subpaths
            .get_mut(subpath)
            .and_then(|subpath| subpath.nodes.get_mut(edge))
        {
            node.bevel = profile;
        }
    }

    pub fn has_edge_profiles(&self) -> bool {
        self.subpaths
            .iter()
            .any(|subpath| subpath.nodes.iter().any(|node| node.bevel.is_some()))
    }

    /// Flatten the path into line pieces tagged with the edge they belong to.
    ///
    /// Sampling matches [`flatten_subpath`], so the tagged pieces follow the
    /// same boundary the coverage rasterizer uses. `profiles` is indexed by
    /// the same edge id; `None` means the edge follows the shape profile.
    pub fn flatten_edges(&self) -> (Vec<EdgeSegment>, Vec<Option<Profile>>) {
        const STEPS: usize = 24;
        let mut segments = Vec::new();
        let mut profiles = Vec::new();
        let mut next_edge = 0u32;
        for subpath in &self.subpaths {
            let count = subpath.nodes.len();
            if count < 2 {
                continue;
            }
            let edges = if subpath.closed { count } else { count - 1 };
            for index in 0..edges {
                profiles.push(subpath.nodes[index].bevel.clone());
                if next_edge > u16::MAX as u32 {
                    continue;
                }
                let a = &subpath.nodes[index];
                let b = &subpath.nodes[(index + 1) % count];
                let (p0, p1, p2, p3) = (a.pos, a.pos + a.handle_out, b.pos + b.handle_in, b.pos);
                let mut previous = p0;
                for step in 1..=STEPS {
                    let point = cubic(p0, p1, p2, p3, step as f32 / STEPS as f32);
                    if point.distance(previous) > 1e-4 {
                        segments.push(EdgeSegment {
                            a: previous,
                            b: point,
                            edge: next_edge as u16,
                        });
                    }
                    previous = point;
                }
                next_edge += 1;
            }
        }
        (segments, profiles)
    }

    pub fn contains(&self, point: Vec2) -> bool {
        point_in_polygons(&flatten_path(self), point, self.fill_rule)
    }

    /// Build a closed, corner-node path from a polygon.
    pub fn from_polygon(points: &[Vec2]) -> Self {
        Self {
            subpaths: vec![SubPath {
                nodes: points.iter().map(|point| Node::corner(*point)).collect(),
                closed: true,
            }],
            fill_rule: FillRule::NonZero,
        }
    }

    /// Build a filled path from closed polylines.
    ///
    /// Boolean results arrive as corner polylines; collinear points left over
    /// from splitting the outlines are dropped so the result stays node-light
    /// and immediately editable.
    pub fn from_loops(loops: Vec<Vec<Vec2>>, fill_rule: FillRule) -> Self {
        let mut path = Path::new(fill_rule);
        for points in loops {
            let simplified = simplify_closed(&points, 0.02);
            if simplified.len() < 3 {
                continue;
            }
            path.subpaths.push(SubPath {
                nodes: simplified.into_iter().map(Node::corner).collect(),
                closed: true,
            });
        }
        path
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubPath {
    pub nodes: Vec<Node>,
    pub closed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathBoolean {
    /// Union: merge the outlines; internal overlaps disappear.
    Add,
    /// Remove the second operand from the first.
    Subtract,
    /// Keep only the overlap.
    Intersect,
    /// Keep the non-overlapping parts (symmetric difference).
    Xor,
}

/// Boolean-combine two filled paths in the vector domain.
///
/// Both paths are flattened, the outlines are intersected and re-stitched as
/// real geometry, and the result is returned as an editable path. No raster
/// step is involved: the boolean happens on the vectors, so beveling the
/// result bevels the combined outline.
///
/// Use `a` as the base for [`PathBoolean::Subtract`].
pub fn boolean_paths(a: &Path, b: &Path, operation: PathBoolean) -> Option<Path> {
    let operands = [VectorOperand::from_path(a), VectorOperand::from_path(b)];
    let loops = boolean_loops(&operands, &[operation]);
    (!loops.is_empty()).then(|| Path::from_loops(loops, FillRule::EvenOdd))
}

/// One filled operand for the vector boolean engine.
///
/// Loops are closed polylines in document space. `fill_rule` decides which
/// areas are filled, exactly like the rasterizer treats the source path.
#[derive(Clone, Debug)]
pub struct VectorOperand {
    pub loops: Vec<Vec<Vec2>>,
    pub fill_rule: FillRule,
}

impl VectorOperand {
    pub fn new(loops: Vec<Vec<Vec2>>, fill_rule: FillRule) -> Self {
        Self { loops, fill_rule }
    }

    /// Flatten an editable path into boolean operands.
    pub fn from_path(path: &Path) -> Self {
        Self {
            loops: flatten_path_tolerance(path, VECTOR_BOOLEAN_TOLERANCE),
            fill_rule: path.fill_rule,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.loops.iter().all(|points| points.len() < 3)
    }

    /// True when `point` lies inside the operand's filled area.
    pub fn contains(&self, point: Vec2) -> bool {
        point_in_polygons(&self.loops, point, self.fill_rule)
    }
}

/// Split a boolean result into its connected pieces.
///
/// The boolean engine keeps the filled region on the left of every loop, so an
/// outer boundary has positive signed area and a hole has negative area. Each
/// piece is one outer boundary plus the holes that sit directly inside it; an
/// island inside a hole starts its own piece. This is what turns one sliced
/// shape into separate layers.
pub fn split_loops(loops: &[Vec<Vec2>]) -> Vec<Vec<Vec<Vec2>>> {
    let mut pieces: Vec<(f32, Vec<Vec<Vec2>>)> = Vec::new();
    let mut holes: Vec<&Vec<Vec2>> = Vec::new();
    for points in loops {
        if points.len() < 3 {
            continue;
        }
        let area = signed_area(points);
        if area > 0.0 {
            pieces.push((area, vec![points.clone()]));
        } else if area < 0.0 {
            holes.push(points);
        }
    }

    for hole in holes {
        // Attach the hole to the smallest outer boundary that contains it.
        let mut owner: Option<usize> = None;
        for (index, (area, _)) in pieces.iter().enumerate() {
            let outer = std::slice::from_ref(&pieces[index].1[0]);
            if !point_in_polygons(outer, hole[0], FillRule::EvenOdd) {
                continue;
            }
            if owner.is_none_or(|best| area.abs() < pieces[best].0.abs()) {
                owner = Some(index);
            }
        }
        if let Some(index) = owner {
            pieces[index].1.push(hole.clone());
        }
    }

    pieces.into_iter().map(|(_, loops)| loops).collect()
}

/// Signed area of a closed polygon; positive keeps the interior on the left of
/// the loop direction, matching [`stitch_edges`].
fn signed_area(points: &[Vec2]) -> f32 {
    let mut sum = 0.0;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

/// Curve flattening tolerance, in document pixels, for boolean geometry.
///
/// Booleans work on polylines, so this is the accuracy of every curved edge
/// that takes part in an operation. It is finer than a raster pixel so the
/// re-rasterized result stays smooth.
pub const VECTOR_BOOLEAN_TOLERANCE: f32 = 0.15;

/// Apply `ops` to `operands` in stack order and return the resulting closed
/// loops (even-odd fill). `ops[i]` combines the accumulated result with
/// `operands[i + 1]`.
///
/// Every operand keeps its position in the expression even when it has no
/// geometry: an empty operand simply covers nothing, which is what a hidden or
/// degenerate shape should contribute.
pub fn boolean_loops(operands: &[VectorOperand], ops: &[PathBoolean]) -> Vec<Vec<Vec2>> {
    let refs: Vec<&VectorOperand> = operands.iter().collect();
    let Some(first) = refs.first() else {
        return Vec::new();
    };
    if refs.len() == 1 {
        return first.loops.clone();
    }
    let mut kept = Vec::new();
    for edge in arrangement(&refs) {
        let left = combine_masks(edge.left, ops);
        let right = combine_masks(edge.right, ops);
        if left == right {
            continue;
        }
        if left {
            kept.push((edge.a, edge.b));
        } else {
            kept.push((edge.b, edge.a));
        }
    }
    stitch_edges(&kept)
}

/// Partition the union of `operands` into the distinct regions defined by
/// their overlaps.
///
/// Every region is returned as its own loop set: with two shapes that is
/// A-only, the overlap, and B-only. Regions that are not covered by any
/// operand are dropped.
pub fn divide_loops(operands: &[VectorOperand]) -> Vec<Vec<Vec<Vec2>>> {
    if operands.is_empty() {
        return Vec::new();
    }
    let refs: Vec<&VectorOperand> = operands.iter().collect();
    let mut regions: HashMap<u64, Vec<(Vec2, Vec2)>> = HashMap::new();
    for edge in arrangement(&refs) {
        if edge.left == edge.right {
            continue;
        }
        // Each side of the edge is a boundary of the region it covers, oriented
        // so that the region stays on the left of the edge.
        regions.entry(edge.left).or_default().push((edge.a, edge.b));
        regions
            .entry(edge.right)
            .or_default()
            .push((edge.b, edge.a));
    }
    let mut masks: Vec<u64> = regions.keys().copied().filter(|mask| *mask != 0).collect();
    masks.sort_unstable();
    let mut out = Vec::new();
    for mask in masks {
        let Some(edges) = regions.remove(&mask) else {
            continue;
        };
        let loops = stitch_edges(&edges);
        if !loops.is_empty() {
            out.push(loops);
        }
    }
    out
}

/// Trace a coverage grid back to an editable path.
///
/// `scale` and `shift` describe how the grid relates to document space:
/// `document = grid_point / scale - shift`, the same transform used by
/// [`boolean_paths`]. The result uses corner nodes so traced outlines remain
/// valid even when a contour changes shape.
pub fn coverage_to_path(
    coverage: &Grid<f32>,
    scale: f32,
    shift: Vec2,
    fill_rule: FillRule,
) -> Option<Path> {
    // A contour that runs along the grid border needs an outside cell to
    // close against. Pad by one transparent pixel and remove the padding from
    // the traced coordinates so compounds that reach the document edge still
    // produce editable paths.
    let pad = 1u32;
    let w = coverage.w + pad * 2;
    let h = coverage.h + pad * 2;
    let padded = Grid::from_fn(w, h, |x, y| {
        if x < pad || y < pad || x >= coverage.w + pad || y >= coverage.h + pad {
            0.0
        } else {
            *coverage.get(x - pad, y - pad)
        }
    });
    let loops = trace_mask(&padded);
    let mut path = Path::new(fill_rule);
    for points in loops {
        let simplified = simplify_closed(&points, 0.75);
        if simplified.len() < 3 {
            continue;
        }
        let nodes = simplified
            .into_iter()
            .map(|point| Node::corner((point - Vec2::splat(pad as f32)) / scale - shift))
            .collect();
        path.subpaths.push(SubPath {
            nodes,
            closed: true,
        });
    }
    (!path.subpaths.is_empty()).then_some(path)
}

/// Operand masks are packed into a `u64`, so an operation sees at most this
/// many operands; extra shapes fall back to being ignored.
const ARRANGEMENT_MASKS: usize = 64;

/// How many operands one boolean call can classify at once.
pub const MAX_BOOLEAN_OPERANDS: usize = ARRANGEMENT_MASKS;

struct RawSegment {
    a: Vec2,
    b: Vec2,
    operand: usize,
}

/// One unique boundary edge of the operand arrangement.
struct ArrangementEdge {
    a: Vec2,
    b: Vec2,
    /// Operand coverage mask at the midpoint shifted left of `a -> b`.
    left: u64,
    /// Operand coverage mask at the midpoint shifted right of `a -> b`.
    right: u64,
    /// Operands whose own outline this edge lies on.
    owners: Vec<usize>,
}

/// Split every operand outline at its intersections with the other operands
/// and report the unique sub-edges with the operand coverage of both sides.
///
/// This is the shared core of [`boolean_loops`] and [`divide_loops`]: once the
/// arrangement is known, a boolean is the set of sub-edges whose two sides
/// disagree about the operation's result. Overlapping boundaries between two
/// operands collapse into a single sub-edge, so a union of identical shapes
/// keeps exactly one outline instead of two cancelling ones.
///
/// Coverage is sampled per operand: an operand whose own outline is the edge
/// only has one filled side, which is sampled a hair off the edge. Any other
/// operand is classified at the midpoint itself, so a boundary that runs
/// tangent to another shape cannot be misread.
fn arrangement(operands: &[&VectorOperand]) -> Vec<ArrangementEdge> {
    // Endpoints are quantized and sorted so a boundary shared by two operands
    // maps to one key no matter which operand produced it first.
    type EdgeKey = ((i64, i64), (i64, i64));
    let mut segments: Vec<RawSegment> = Vec::new();
    for (operand, source) in operands.iter().enumerate().take(ARRANGEMENT_MASKS) {
        for points in &source.loops {
            if points.len() < 3 {
                continue;
            }
            for index in 0..points.len() {
                let a = points[index];
                let b = points[(index + 1) % points.len()];
                if (b - a).length_squared() <= 1e-10 {
                    continue;
                }
                segments.push(RawSegment { a, b, operand });
            }
        }
    }
    if segments.is_empty() {
        return Vec::new();
    }

    let mut splits: Vec<Vec<(f32, Vec2)>> = vec![Vec::new(); segments.len()];
    let bounds: Vec<(Vec2, Vec2)> = segments
        .iter()
        .map(|segment| (segment.a.min(segment.b), segment.a.max(segment.b)))
        .collect();
    for i in 0..segments.len() {
        for j in (i + 1)..segments.len() {
            // Self-intersections are split too: a stroked outline folds over
            // itself at sharp corners, and an unfolded crossing would leave
            // unmatched vertices that break loop stitching.
            let (min_i, max_i) = bounds[i];
            let (min_j, max_j) = bounds[j];
            if max_i.x < min_j.x || max_j.x < min_i.x || max_i.y < min_j.y || max_j.y < min_i.y {
                continue;
            }
            let (earlier, later) = splits.split_at_mut(j);
            split_intersection(&segments[i], &segments[j], &mut earlier[i], &mut later[0]);
        }
    }

    let mut seen: HashMap<EdgeKey, usize> = HashMap::new();
    let mut edges: Vec<ArrangementEdge> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let mut params = std::mem::take(&mut splits[index]);
        params.push((0.0, segment.a));
        params.push((1.0, segment.b));
        params.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
        params.dedup_by(|a, b| (a.0 - b.0).abs() <= 1e-6);
        for pair in params.windows(2) {
            let (start, a) = pair[0];
            let (end, b) = pair[1];
            if end - start <= 1e-6 {
                continue;
            }
            let key = if point_key(a) <= point_key(b) {
                (point_key(a), point_key(b))
            } else {
                (point_key(b), point_key(a))
            };
            if seen.contains_key(&key) {
                if let Some(&index) = seen.get(&key) {
                    if !edges[index].owners.contains(&segment.operand) {
                        edges[index].owners.push(segment.operand);
                    }
                }
                continue;
            }
            seen.insert(key, edges.len());
            edges.push(ArrangementEdge {
                a,
                b,
                left: 0,
                right: 0,
                owners: vec![segment.operand],
            });
        }
    }

    for edge in &mut edges {
        let delta = edge.b - edge.a;
        let length = delta.length();
        if length <= 1e-6 {
            continue;
        }
        let normal = Vec2::new(-delta.y, delta.x) / length;
        let middle = (edge.a + edge.b) * 0.5;
        // Just far enough to leave the boundary, close enough that a thin
        // feature of the same operand still tests on the correct side.
        let offset = (length * 0.05).clamp(1e-3, 0.05);
        let mut left = 0u64;
        let mut right = 0u64;
        for (index, operand) in operands.iter().enumerate().take(ARRANGEMENT_MASKS) {
            let bit = 1 << index;
            if edge.owners.contains(&index) {
                if operand.contains(middle + normal * offset) {
                    left |= bit;
                }
                if operand.contains(middle - normal * offset) {
                    right |= bit;
                }
            } else if operand.contains(middle) {
                // Not on this operand's boundary, so both sides agree.
                left |= bit;
                right |= bit;
            }
        }
        edge.left = left;
        edge.right = right;
    }
    edges
}

/// Add the points where two segments cross to both split lists.
///
/// The crossing is stored as one shared coordinate on both sides, and snapped
/// to a segment endpoint when the crossing is a T-junction. Splitting two
/// outlines at independently computed points leaves sub-milli-pixel gaps that
/// break loop stitching, so both sides always receive the exact same vertex.
fn split_intersection(
    a: &RawSegment,
    b: &RawSegment,
    a_params: &mut Vec<(f32, Vec2)>,
    b_params: &mut Vec<(f32, Vec2)>,
) {
    let r = a.b - a.a;
    let s = b.b - b.a;
    let denominator = cross(r, s);
    let offset = b.a - a.a;
    let r_length = r.length();
    let s_length = s.length();
    if denominator.abs() <= 1e-7 * r_length * s_length {
        // Parallel: only collinear segments need extra split points, so shared
        // boundaries line up exactly instead of drifting apart.
        if cross(offset, r).abs() > 1e-4 * r_length {
            return;
        }
        let r_norm = r.dot(r).max(1e-12);
        for point in [b.a, b.b] {
            let t = (point - a.a).dot(r) / r_norm;
            if t > 1e-5 && t < 1.0 - 1e-5 {
                a_params.push((t, point));
            }
        }
        let s_norm = s.dot(s).max(1e-12);
        for point in [a.a, a.b] {
            let t = (point - b.a).dot(s) / s_norm;
            if t > 1e-5 && t < 1.0 - 1e-5 {
                b_params.push((t, point));
            }
        }
        return;
    }
    let t = cross(offset, s) / denominator;
    let u = cross(offset, r) / denominator;
    // A T-junction keeps the endpoint that lies on the other outline so both
    // sides share a vertex exactly.
    let point = if u <= 1e-5 {
        b.a
    } else if u >= 1.0 - 1e-5 {
        b.b
    } else if t <= 1e-5 {
        a.a
    } else if t >= 1.0 - 1e-5 {
        a.b
    } else {
        a.a + r * t
    };
    if t > 1e-5 && t < 1.0 - 1e-5 {
        a_params.push((t, point));
    }
    if u > 1e-5 && u < 1.0 - 1e-5 {
        b_params.push((u, point));
    }
}

/// Fold the operand coverage mask through one operation per operand, starting
/// at the bottom-most operand.
fn combine_masks(mask: u64, ops: &[PathBoolean]) -> bool {
    let mut value = mask & 1 != 0;
    for (index, op) in ops.iter().enumerate() {
        if index + 1 >= ARRANGEMENT_MASKS {
            break;
        }
        let operand = mask & (1 << (index + 1)) != 0;
        value = match op {
            PathBoolean::Add => value || operand,
            PathBoolean::Subtract => value && !operand,
            PathBoolean::Intersect => value && operand,
            PathBoolean::Xor => value != operand,
        };
    }
    value
}

/// Stitch directed boundary edges into closed loops.
///
/// Every edge keeps its region on the left, so each edge has exactly one
/// successor: the outgoing edge just clockwise of the reversed incoming
/// direction. That turns the edge soup into a set of disjoint cycles, which
/// keeps loops consistent at T-junctions and around holes. Chains that run
/// into a dead end (no successor) are dropped instead of consuming the edges
/// of a valid loop.
fn stitch_edges(edges: &[(Vec2, Vec2)]) -> Vec<Vec<Vec2>> {
    const TAU: f32 = std::f32::consts::TAU;
    let mut outgoing: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (index, (a, _)) in edges.iter().enumerate() {
        outgoing.entry(point_key(*a)).or_default().push(index);
    }

    let mut successor = vec![usize::MAX; edges.len()];
    for (index, (a, b)) in edges.iter().enumerate() {
        let incoming = (b - a).normalize_or_zero();
        let reversed = incoming.y.atan2(incoming.x) + std::f32::consts::PI;
        let mut best: Option<(usize, f32)> = None;
        if let Some(candidates) = outgoing.get(&point_key(*b)) {
            for &candidate in candidates {
                if candidate == index {
                    continue;
                }
                let direction = (edges[candidate].1 - edges[candidate].0).normalize_or_zero();
                let angle = direction.y.atan2(direction.x);
                // Turning back along the incoming edge is the last resort, so a
                // zero turn sorts to the end of the candidate list.
                let mut turn = (reversed - angle).rem_euclid(TAU);
                if turn < 1e-6 {
                    turn = TAU;
                }
                if best.is_none_or(|(_, best_turn)| turn < best_turn) {
                    best = Some((candidate, turn));
                }
            }
        }
        successor[index] = match best {
            Some((candidate, _)) => candidate,
            None => usize::MAX,
        };
    }

    let mut visited = vec![false; edges.len()];
    let mut loops = Vec::new();
    for start in 0..edges.len() {
        if visited[start] {
            continue;
        }
        let mut order: Vec<usize> = Vec::new();
        let mut position: HashMap<usize, usize> = HashMap::new();
        let mut current = start;
        let cycle_at = loop {
            if current == usize::MAX {
                break None;
            }
            if let Some(&offset) = position.get(&current) {
                break Some(offset);
            }
            position.insert(current, order.len());
            order.push(current);
            current = successor[current];
            if order.len() > edges.len() {
                break None;
            }
        };
        for &index in &order {
            visited[index] = true;
        }
        if let Some(offset) = cycle_at {
            let cycle = &order[offset..];
            if cycle.len() >= 3 {
                loops.push(cycle.iter().map(|&index| edges[index].0).collect());
            }
        }
    }
    loops
}

fn to_kurbo_path(path: &Path) -> BezPath {
    let mut out = BezPath::new();
    for subpath in &path.subpaths {
        if subpath.nodes.len() < 2 {
            continue;
        }
        out.move_to(point(subpath.nodes[0].pos));
        let count = subpath.nodes.len();
        let segments = if subpath.closed { count } else { count - 1 };
        for i in 0..segments {
            let a = &subpath.nodes[i];
            let b = &subpath.nodes[(i + 1) % count];
            if a.handle_out.length_squared() < 1e-8 && b.handle_in.length_squared() < 1e-8 {
                out.line_to(point(b.pos));
            } else {
                out.curve_to(
                    point(a.pos + a.handle_out),
                    point(b.pos + b.handle_in),
                    point(b.pos),
                );
            }
        }
        if subpath.closed {
            out.close_path();
        }
    }
    out
}

fn point(value: Vec2) -> Point {
    Point::new(value.x as f64, value.y as f64)
}

fn vec2(value: Point) -> Vec2 {
    Vec2::new(value.x as f32, value.y as f32)
}

fn from_kurbo_path(path: &BezPath) -> Path {
    let mut out = Path::new(FillRule::EvenOdd);
    let mut current: Option<SubPath> = None;
    let mut current_pos = Vec2::ZERO;

    for element in path.iter() {
        match element {
            PathEl::MoveTo(p) => {
                if let Some(subpath) = current.take() {
                    push_subpath(&mut out, subpath);
                }
                let mut subpath = SubPath::new(false);
                subpath.nodes.push(Node::corner(vec2(p)));
                current = Some(subpath);
                current_pos = vec2(p);
            }
            PathEl::LineTo(p) => {
                let pos = vec2(p);
                if let Some(subpath) = &mut current {
                    if subpath
                        .nodes
                        .last()
                        .is_none_or(|node| (node.pos - pos).length() > 1e-4)
                    {
                        subpath.nodes.push(Node::corner(pos));
                    }
                    current_pos = pos;
                }
            }
            PathEl::QuadTo(control, end) => {
                let p0 = current_pos;
                let p1 = vec2(control);
                let p2 = vec2(end);
                let c1 = p0 + (p1 - p0) * (2.0 / 3.0);
                let c2 = p2 + (p1 - p2) * (2.0 / 3.0);
                push_curve(&mut current, c1, c2, p2);
                current_pos = p2;
            }
            PathEl::CurveTo(c1, c2, end) => {
                push_curve(&mut current, vec2(c1), vec2(c2), vec2(end));
                current_pos = vec2(end);
            }
            PathEl::ClosePath => {
                if let Some(mut subpath) = current.take() {
                    subpath.closed = true;
                    if subpath.nodes.len() > 2 {
                        let last = subpath.nodes.last().cloned();
                        let first = subpath.nodes.first().cloned();
                        if let (Some(last), Some(first)) = (last, first) {
                            if (last.pos - first.pos).length() < 1e-4 {
                                subpath.nodes.pop();
                                if let Some(first) = subpath.nodes.first_mut() {
                                    first.handle_in = last.handle_in;
                                }
                            }
                        }
                    }
                    push_subpath(&mut out, subpath);
                }
            }
        }
    }
    if let Some(subpath) = current.take() {
        push_subpath(&mut out, subpath);
    }
    out
}

fn push_subpath(out: &mut Path, subpath: SubPath) {
    if subpath.nodes.len() >= 2 {
        out.subpaths.push(subpath);
    }
}

fn push_curve(current: &mut Option<SubPath>, c1: Vec2, c2: Vec2, end: Vec2) {
    let Some(subpath) = current else {
        return;
    };
    if let Some(previous) = subpath.nodes.last_mut() {
        let start = previous.pos;
        if (start - end).length() > 1e-4 || (c1 - start).length() > 1e-4 {
            previous.handle_out = c1 - start;
            subpath.nodes.push(Node {
                pos: end,
                handle_in: c2 - end,
                handle_out: Vec2::ZERO,
                kind: NodeKind::Smooth,
                bevel: None,
            });
        }
    }
}

fn trace_mask(mask: &Grid<f32>) -> Vec<Vec<Vec2>> {
    let w = mask.w;
    let h = mask.h;
    let inside = |x: i32, y: i32| {
        x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h && *mask.get(x as u32, y as u32) >= 0.5
    };
    let mut segments: Vec<[Vec2; 2]> = Vec::new();
    for y in 0..h.saturating_sub(1) {
        for x in 0..w.saturating_sub(1) {
            let x = x as i32;
            let y = y as i32;
            let tl = inside(x, y);
            let tr = inside(x + 1, y);
            let br = inside(x + 1, y + 1);
            let bl = inside(x, y + 1);
            let index =
                u8::from(tl) | (u8::from(tr) << 1) | (u8::from(br) << 2) | (u8::from(bl) << 3);
            if index == 0 || index == 15 {
                continue;
            }
            let top = Vec2::new(x as f32 + 0.5, y as f32);
            let right = Vec2::new(x as f32 + 1.0, y as f32 + 0.5);
            let bottom = Vec2::new(x as f32 + 0.5, y as f32 + 1.0);
            let left = Vec2::new(x as f32, y as f32 + 0.5);
            let center = (*mask.get(x as u32, y as u32)
                + *mask.get((x + 1) as u32, y as u32)
                + *mask.get((x + 1) as u32, (y + 1) as u32)
                + *mask.get(x as u32, (y + 1) as u32))
                * 0.25
                >= 0.5;
            match index {
                1 => segments.push([left, top]),
                2 => segments.push([top, right]),
                3 => segments.push([left, right]),
                4 => segments.push([right, bottom]),
                5 => {
                    if center {
                        segments.push([top, right]);
                        segments.push([bottom, left]);
                    } else {
                        segments.push([left, top]);
                        segments.push([right, bottom]);
                    }
                }
                6 => segments.push([top, bottom]),
                7 => segments.push([left, bottom]),
                8 => segments.push([bottom, left]),
                9 => segments.push([top, bottom]),
                10 => {
                    if center {
                        segments.push([left, top]);
                        segments.push([right, bottom]);
                    } else {
                        segments.push([top, right]);
                        segments.push([bottom, left]);
                    }
                }
                11 => segments.push([right, bottom]),
                12 => segments.push([right, left]),
                13 => segments.push([top, right]),
                14 => segments.push([left, top]),
                _ => {}
            }
        }
    }
    stitch_segments(&segments)
}

fn point_key(point: Vec2) -> (i64, i64) {
    (
        (point.x * 1000.0).round() as i64,
        (point.y * 1000.0).round() as i64,
    )
}

fn stitch_segments(segments: &[[Vec2; 2]]) -> Vec<Vec<Vec2>> {
    let mut adjacency: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (index, segment) in segments.iter().enumerate() {
        adjacency
            .entry(point_key(segment[0]))
            .or_default()
            .push(index);
        adjacency
            .entry(point_key(segment[1]))
            .or_default()
            .push(index);
    }
    let mut used = vec![false; segments.len()];
    let mut loops = Vec::new();
    for start_index in 0..segments.len() {
        if used[start_index] {
            continue;
        }
        used[start_index] = true;
        let [start, second] = segments[start_index];
        let start_key = point_key(start);
        let mut points = vec![start, second];
        let mut current = second;
        while point_key(current) != start_key {
            let key = point_key(current);
            let candidates = adjacency.get(&key);
            let next_index = candidates.and_then(|candidates| {
                let mut best: Option<(usize, f32)> = None;
                let incoming = current - points[points.len() - 2];
                for candidate in candidates {
                    if used[*candidate] {
                        continue;
                    }
                    let [a, b] = segments[*candidate];
                    let other = if point_key(a) == key { b } else { a };
                    let outgoing = other - current;
                    let cross = incoming.x * outgoing.y - incoming.y * outgoing.x;
                    let dot = incoming.dot(outgoing);
                    let turn = cross.atan2(dot).abs();
                    if best.is_none_or(|(_, angle)| turn < angle) {
                        best = Some((*candidate, turn));
                    }
                }
                best.map(|(index, _)| index)
            });
            let Some(next_index) = next_index else {
                break;
            };
            used[next_index] = true;
            let [a, b] = segments[next_index];
            current = if point_key(a) == key { b } else { a };
            points.push(current);
            if points.len() > segments.len() + 2 {
                break;
            }
        }
        if points.len() >= 4 && point_key(*points.last().unwrap()) == start_key {
            points.pop();
            loops.push(points);
        }
    }
    loops
}

/// Simplify a closed polyline with Ramer–Douglas–Peucker at `epsilon`
/// document pixels.
///
/// Lasso drafts use this so a freehand region stays light enough for the
/// boolean engine; the result is still a closed loop without a duplicated
/// last point.
pub fn simplify_closed_polyline(points: &[Vec2], epsilon: f32) -> Vec<Vec2> {
    simplify_closed(points, epsilon.max(1e-3))
}

fn simplify_closed(points: &[Vec2], epsilon: f32) -> Vec<Vec2> {
    let mut cleaned = Vec::with_capacity(points.len());
    for &point in points {
        if cleaned
            .last()
            .is_none_or(|last: &Vec2| (*last - point).length() > 1e-4)
        {
            cleaned.push(point);
        }
    }
    if cleaned.len() > 1 && (cleaned[0] - *cleaned.last().unwrap()).length() < 1e-4 {
        cleaned.pop();
    }
    if cleaned.len() < 3 {
        return cleaned;
    }
    let mut open = cleaned.clone();
    open.push(cleaned[0]);
    let mut simplified = Vec::new();
    rdp(&open, epsilon, &mut simplified);
    if simplified.len() > 1 && (simplified[0] - *simplified.last().unwrap()).length() < 1e-4 {
        simplified.pop();
    }
    simplified
}

fn rdp(points: &[Vec2], epsilon: f32, out: &mut Vec<Vec2>) {
    if points.len() < 2 {
        return;
    }
    out.push(points[0]);
    rdp_inner(points, epsilon, out);
    if let Some(last) = points.last() {
        out.push(*last);
    }
}

fn rdp_inner(points: &[Vec2], epsilon: f32, out: &mut Vec<Vec2>) {
    if points.len() < 3 {
        return;
    }
    let start = points[0];
    let end = *points.last().unwrap();
    let line = end - start;
    let length = line.length();
    let mut farthest = 0.0;
    let mut index = 0;
    for (i, point) in points.iter().enumerate().take(points.len() - 1).skip(1) {
        let distance = if length < 1e-5 {
            (*point - start).length()
        } else {
            ((*point - start).perp_dot(line) / length).abs()
        };
        if distance > farthest {
            farthest = distance;
            index = i;
        }
    }
    if farthest > epsilon {
        rdp_inner(&points[..=index], epsilon, out);
        out.push(points[index]);
        rdp_inner(&points[index..], epsilon, out);
    }
}

impl SubPath {
    pub fn new(closed: bool) -> Self {
        Self {
            nodes: Vec::new(),
            closed,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub pos: Vec2,
    /// Relative to `pos`; zero means no handle.
    pub handle_in: Vec2,
    /// Relative to `pos`; zero means no handle.
    pub handle_out: Vec2,
    pub kind: NodeKind,
    /// Optional bevel profile for the edge that starts at this node. `None`
    /// keeps the shape's profile, so existing paths load unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bevel: Option<Profile>,
}

impl Node {
    pub fn corner(pos: Vec2) -> Self {
        Self {
            pos,
            handle_in: Vec2::ZERO,
            handle_out: Vec2::ZERO,
            kind: NodeKind::Corner,
            bevel: None,
        }
    }

    pub fn smooth(pos: Vec2, handle_in: Vec2, handle_out: Vec2) -> Self {
        Self {
            pos,
            handle_in,
            handle_out,
            kind: NodeKind::Smooth,
            bevel: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Corner,
    Smooth,
    Symmetric,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Primitive {
    Rect {
        center: Vec2,
        size: Vec2,
        corner_radius: f32,
    },
    Ellipse {
        center: Vec2,
        radii: Vec2,
    },
    Polygon {
        points: Vec<Vec2>,
    },
    Star {
        center: Vec2,
        inner_radius: f32,
        outer_radius: f32,
        points: u32,
        rotation: f32,
    },
}

/// Flatten every subpath into a polyline in document pixel space.
pub fn flatten_path(path: &Path) -> Vec<Vec<Vec2>> {
    path.subpaths.iter().filter_map(flatten_subpath).collect()
}

/// Flatten every subpath together with its closed flag.
///
/// [`flatten_path`] drops the flag, which is enough for fills but not for a
/// sweep: a closed guide has to be sampled back to its first point. Subpaths
/// with fewer than two nodes are skipped, exactly as in the fill path.
pub fn flatten_path_subpaths(path: &Path) -> Vec<(Vec<Vec2>, bool)> {
    path.subpaths
        .iter()
        .filter_map(|subpath| flatten_subpath(subpath).map(|points| (points, subpath.closed)))
        .collect()
}

/// Closed circle outline with a fixed angular sampling, used for a knife
/// stroke that collapses to a single point.
pub fn circle_outline(center: Vec2, radius: f32) -> Vec<Vec2> {
    const SEGMENTS: usize = 24;
    let radius = radius.max(1e-3);
    (0..SEGMENTS)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / SEGMENTS as f32;
            center + Vec2::from_angle(angle) * radius
        })
        .collect()
}

/// Angular step of a stroke arc: one segment per 15 degrees.
const ARC_STEP: f32 = std::f32::consts::PI / 12.0;

/// Append the points of an arc, excluding its start and including its end.
fn push_arc(out: &mut Vec<Vec2>, center: Vec2, radius: f32, from: f32, sweep: f32) {
    let steps = ((sweep.abs() / ARC_STEP).ceil() as usize).max(1);
    for step in 1..=steps {
        let angle = from + sweep * step as f32 / steps as f32;
        out.push(center + Vec2::from_angle(angle) * radius);
    }
}

/// Meeting point of the line through `p` with direction `d` and the line
/// through `q` with direction `e`.
fn line_meet(p: Vec2, d: Vec2, q: Vec2, e: Vec2) -> Option<Vec2> {
    let denom = cross(d, e);
    (denom.abs() > 1e-5).then(|| p + d * (cross(q - p, e) / denom))
}

/// One side of a stroked polyline, offset by `half`.
///
/// The boundary runs along the offset edges, follows a round arc where the
/// side sits outside a turn and meets at the offset crossing where it sits
/// inside one. Very sharp inside corners are beveled instead of spiked so the
/// side never folds past the line.
fn offset_side(points: &[Vec2], half: f32) -> Vec<Vec2> {
    let dirs: Vec<Vec2> = points
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).normalize())
        .collect();
    let normals: Vec<Vec2> = dirs.iter().map(|dir| Vec2::new(-dir.y, dir.x)).collect();
    let mut side = vec![points[0] + normals[0] * half];
    for index in 1..points.len() - 1 {
        let (before, after) = (dirs[index - 1], dirs[index]);
        let (start_normal, end_normal) = (normals[index - 1], normals[index]);
        let start = points[index] + start_normal * half;
        let end = points[index] + end_normal * half;
        if cross(before, after) < 0.0 {
            // Outside of the turn: the boundary wraps the round joint.
            side.push(start);
            let from = start_normal.y.atan2(start_normal.x);
            let sweep = cross(start_normal, end_normal).atan2(start_normal.dot(end_normal));
            push_arc(&mut side, points[index], half, from, sweep);
        } else if let Some(meet) = line_meet(start, before, end, after)
            .filter(|meet| meet.distance(points[index]) <= half * 2.0)
        {
            // Inside of the turn: both offset edges reach the same point.
            side.push(meet);
        } else {
            side.push(start);
            side.push(end);
        }
    }
    side.push(points[points.len() - 1] + normals[normals.len() - 1] * half);
    side
}

/// Drop repeated points, including a last point that repeats the first.
fn dedupe_ring(points: &mut Vec<Vec2>) {
    points.dedup_by(|a, b| a.distance(*b) < 1e-4);
    while points.len() > 1 && points[0].distance(points[points.len() - 1]) < 1e-4 {
        points.pop();
    }
}

/// Closed outline of a stroked open polyline: offset edges, round joins and
/// round caps, as a single loop.
///
/// A generic stroke expander lets the inside of a sharp corner cross itself,
/// and a crossing only a fraction of a pixel wide is where boolean
/// classification breaks down. Building the band edge by edge avoids those
/// crossings, so a knife line always yields a usable outline.
pub fn stroke_polyline_outline(points: &[Vec2], width: f32) -> Option<Vec<Vec2>> {
    let half = width * 0.5;
    if half < 0.01 {
        return None;
    }
    let mut line: Vec<Vec2> = Vec::with_capacity(points.len());
    for point in points {
        if line.last().is_none_or(|last| last.distance(*point) > 1e-4) {
            line.push(*point);
        }
    }
    if line.len() < 2 {
        // A single point still cuts a dot.
        return line.first().map(|point| circle_outline(*point, half));
    }
    let mut outline = offset_side(&line, half);
    let cap = |a: Vec2, b: Vec2| {
        let dir = (b - a).normalize();
        Vec2::new(-dir.y, dir.x)
    };
    // Round cap past the last point, then the far side walked backwards.
    let end_normal = cap(line[line.len() - 2], line[line.len() - 1]);
    push_arc(
        &mut outline,
        line[line.len() - 1],
        half,
        end_normal.y.atan2(end_normal.x),
        -std::f32::consts::PI,
    );
    let mut reversed = line.clone();
    reversed.reverse();
    outline.extend(offset_side(&reversed, half));
    let start_normal = cap(line[0], line[1]);
    push_arc(
        &mut outline,
        line[0],
        half,
        (-start_normal).y.atan2(-start_normal.x),
        -std::f32::consts::PI,
    );
    dedupe_ring(&mut outline);
    if outline.len() < 3 {
        return None;
    }
    if signed_area(&outline) < 0.0 {
        outline.reverse();
    }
    Some(outline)
}

/// Flatten every subpath into a polyline, keeping whether it is closed.
pub fn flatten_path_tolerance_loops(path: &Path, tolerance: f32) -> Vec<(Vec<Vec2>, bool)> {
    let tolerance = tolerance.max(1e-3);
    path.subpaths
        .iter()
        .filter_map(|subpath| {
            flatten_subpath_tolerance(subpath, tolerance).map(|points| (points, subpath.closed))
        })
        .collect()
}

/// Flatten every subpath with an adaptive curve tolerance.
///
/// `tolerance` is the maximum deviation, in document pixels, between a curve
/// and the polyline that replaces it.
pub fn flatten_path_tolerance(path: &Path, tolerance: f32) -> Vec<Vec<Vec2>> {
    flatten_path_tolerance_loops(path, tolerance)
        .into_iter()
        .map(|(points, _)| points)
        .collect()
}

fn flatten_subpath_tolerance(subpath: &SubPath, tolerance: f32) -> Option<Vec<Vec2>> {
    let count = subpath.nodes.len();
    if count < 2 {
        return None;
    }
    let segments = if subpath.closed { count } else { count - 1 };
    let mut out = Vec::new();
    for index in 0..segments {
        let a = &subpath.nodes[index];
        let b = &subpath.nodes[(index + 1) % count];
        out.push(a.pos);
        if a.handle_out.length_squared() < 1e-8 && b.handle_in.length_squared() < 1e-8 {
            continue;
        }
        flatten_cubic(
            a.pos,
            a.pos + a.handle_out,
            b.pos + b.handle_in,
            b.pos,
            tolerance,
            0,
            &mut out,
        );
    }
    if !subpath.closed {
        out.push(subpath.nodes[count - 1].pos);
    }
    Some(out)
}

/// Subdivide a cubic until every piece is within `tolerance` of a line.
fn flatten_cubic(
    p0: Vec2,
    p1: Vec2,
    p2: Vec2,
    p3: Vec2,
    tolerance: f32,
    depth: u32,
    out: &mut Vec<Vec2>,
) {
    if depth >= 12 || cubic_is_flat(p0, p1, p2, p3, tolerance) {
        return;
    }
    let p01 = (p0 + p1) * 0.5;
    let p12 = (p1 + p2) * 0.5;
    let p23 = (p2 + p3) * 0.5;
    let p012 = (p01 + p12) * 0.5;
    let p123 = (p12 + p23) * 0.5;
    let middle = (p012 + p123) * 0.5;
    flatten_cubic(p0, p01, p012, middle, tolerance, depth + 1, out);
    out.push(middle);
    flatten_cubic(middle, p123, p23, p3, tolerance, depth + 1, out);
}

fn cubic_is_flat(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, tolerance: f32) -> bool {
    let chord = p3 - p0;
    let length = chord.length();
    let (first, second) = if length < 1e-4 {
        ((p1 - p0).length(), (p2 - p0).length())
    } else {
        (
            ((p1 - p0).perp_dot(chord) / length).abs(),
            ((p2 - p0).perp_dot(chord) / length).abs(),
        )
    };
    first <= tolerance && second <= tolerance
}

fn flatten_subpath(subpath: &SubPath) -> Option<Vec<Vec2>> {
    let n = subpath.nodes.len();
    if n < 2 {
        return None;
    }

    let segment_count = if subpath.closed { n } else { n - 1 };
    let steps = 24;
    let mut out = Vec::with_capacity(segment_count * steps + 1);

    for i in 0..segment_count {
        let a = &subpath.nodes[i];
        let b = &subpath.nodes[(i + 1) % n];
        let p0 = a.pos;
        let p1 = a.pos + a.handle_out;
        let p2 = b.pos + b.handle_in;
        let p3 = b.pos;

        for s in 0..steps {
            let t = s as f32 / steps as f32;
            out.push(cubic(p0, p1, p2, p3, t));
        }
    }

    if !subpath.closed {
        out.push(subpath.nodes[n - 1].pos);
    }

    Some(out)
}

fn cubic(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let u = 1.0 - t;
    p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t)
}

fn rasterize_primitive(primitive: &Primitive, w: u32, h: u32, origin: Vec2) -> Grid<f32> {
    match primitive {
        Primitive::Polygon { points } => rasterize_polygons(
            std::slice::from_ref(points),
            w,
            h,
            FillRule::NonZero,
            origin,
        ),
        Primitive::Star {
            center,
            inner_radius,
            outer_radius,
            points,
            rotation,
        } => {
            if *points < 3 {
                return Grid::new(w, h, 0.0);
            }
            let mut poly = Vec::with_capacity((*points * 2) as usize);
            let step = std::f32::consts::TAU / (*points as f32 * 2.0);
            for i in 0..(*points * 2) {
                let r = if i % 2 == 0 {
                    *outer_radius
                } else {
                    *inner_radius
                };
                let a = *rotation + step * i as f32 - std::f32::consts::FRAC_PI_2;
                poly.push(*center + Vec2::new(a.cos(), a.sin()) * r);
            }
            rasterize_polygons(&[poly], w, h, FillRule::NonZero, origin)
        }
        _ => Grid::from_fn(w, h, |x, y| {
            let p = origin + Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let d = match primitive {
                Primitive::Rect {
                    center,
                    size,
                    corner_radius,
                } => {
                    let half = *size * 0.5;
                    let r = corner_radius.min(half.x.abs()).min(half.y.abs()).max(0.0);
                    sd_rounded_box(p - *center, half, r)
                }
                Primitive::Ellipse { center, radii } => {
                    let radii = Vec2::new(radii.x.max(1e-4), radii.y.max(1e-4));
                    let q = (p - *center) / radii;
                    (q.length() - 1.0) * radii.min_element()
                }
                _ => f32::INFINITY,
            };
            (0.5 - d).clamp(0.0, 1.0)
        }),
    }
}

fn sd_rounded_box(p: Vec2, half: Vec2, radius: f32) -> f32 {
    let q = p.abs() - half + Vec2::splat(radius);
    q.max(Vec2::ZERO).length() + q.x.max(q.y).min(0.0) - radius
}

pub(crate) fn rasterize_polygons(
    polygons: &[Vec<Vec2>],
    w: u32,
    h: u32,
    fill_rule: FillRule,
    origin: Vec2,
) -> Grid<f32> {
    let mut coverage = vec![0.0f32; (w as usize) * (h as usize)];
    let samples = 4.0f32;
    let weight = 1.0 / samples;

    for y in 0..h {
        for s in 0..4 {
            let sy = origin.y + y as f32 + (s as f32 + 0.5) / samples;
            let mut crossings: Vec<(f32, i32)> = Vec::new();

            for poly in polygons {
                if poly.len() < 3 {
                    continue;
                }
                for i in 0..poly.len() {
                    let a = poly[i];
                    let b = poly[(i + 1) % poly.len()];
                    if (a.y <= sy) != (b.y <= sy) {
                        let t = (sy - a.y) / (b.y - a.y);
                        let x = a.x + t * (b.x - a.x);
                        let dir = if b.y > a.y { 1 } else { -1 };
                        crossings.push((x, dir));
                    }
                }
            }

            crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));

            match fill_rule {
                FillRule::EvenOdd => {
                    let mut i = 0;
                    while i + 1 < crossings.len() {
                        fill_span(
                            &mut coverage,
                            w,
                            y,
                            crossings[i].0 - origin.x,
                            crossings[i + 1].0 - origin.x,
                            weight,
                        );
                        i += 2;
                    }
                }
                FillRule::NonZero => {
                    let mut winding = 0;
                    let mut prev_x = None;
                    for (x, dir) in crossings {
                        if let Some(px) = prev_x {
                            if winding != 0 {
                                fill_span(&mut coverage, w, y, px - origin.x, x - origin.x, weight);
                            }
                        }
                        winding += dir;
                        prev_x = Some(x);
                    }
                }
            }
        }
    }

    Grid::from_fn(w, h, |x, y| {
        coverage[(y as usize) * (w as usize) + x as usize].clamp(0.0, 1.0)
    })
}

fn fill_span(coverage: &mut [f32], w: u32, y: u32, x0: f32, x1: f32, weight: f32) {
    let x0 = x0.max(0.0);
    let x1 = x1.min(w as f32);
    if x1 <= x0 {
        return;
    }

    let start = x0.floor() as u32;
    let end = x1.ceil() as u32;
    for px in start..end.min(w) {
        let left = (px as f32).max(x0);
        let right = ((px + 1) as f32).min(x1);
        let overlap = (right - left).max(0.0);
        coverage[(y as usize) * (w as usize) + px as usize] += overlap * weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_path(size: f32) -> Path {
        let mut p = Path::new(FillRule::NonZero);
        let mut sp = SubPath::new(true);
        sp.nodes.push(Node::corner(Vec2::new(8.0, 8.0)));
        sp.nodes.push(Node::corner(Vec2::new(size - 8.0, 8.0)));
        sp.nodes
            .push(Node::corner(Vec2::new(size - 8.0, size - 8.0)));
        sp.nodes.push(Node::corner(Vec2::new(8.0, size - 8.0)));
        p.subpaths.push(sp);
        p
    }

    fn rect_path(min: Vec2, max: Vec2) -> Path {
        Path::from_polygon(&[min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)])
    }

    #[test]
    fn path_booleans_produce_union_intersection_and_difference() {
        let a = rect_path(Vec2::new(10.0, 10.0), Vec2::new(60.0, 60.0));
        let b = rect_path(Vec2::new(40.0, 10.0), Vec2::new(90.0, 60.0));

        let union = boolean_paths(&a, &b, PathBoolean::Add).expect("union");
        assert!(union.contains(Vec2::new(20.0, 30.0)));
        assert!(union.contains(Vec2::new(50.0, 30.0)));
        assert!(union.contains(Vec2::new(80.0, 30.0)));

        let intersection = boolean_paths(&a, &b, PathBoolean::Intersect).expect("intersection");
        assert!(!intersection.contains(Vec2::new(20.0, 30.0)));
        assert!(intersection.contains(Vec2::new(50.0, 30.0)));
        assert!(!intersection.contains(Vec2::new(80.0, 30.0)));

        let difference = boolean_paths(&a, &b, PathBoolean::Subtract).expect("difference");
        assert!(difference.contains(Vec2::new(20.0, 30.0)));
        assert!(!difference.contains(Vec2::new(50.0, 30.0)));
        assert!(!difference.contains(Vec2::new(80.0, 30.0)));

        let xor = boolean_paths(&a, &b, PathBoolean::Xor).expect("xor");
        assert!(xor.contains(Vec2::new(20.0, 30.0)));
        assert!(!xor.contains(Vec2::new(50.0, 30.0)));
        assert!(xor.contains(Vec2::new(80.0, 30.0)));
    }

    #[test]
    fn vector_boolean_keeps_a_single_outline_for_coincident_shapes() {
        let a = rect_path(Vec2::new(12.0, 12.0), Vec2::new(52.0, 52.0));
        let b = rect_path(Vec2::new(12.0, 12.0), Vec2::new(52.0, 52.0));

        let union = boolean_paths(&a, &b, PathBoolean::Add).expect("union");
        assert_eq!(union.subpaths.len(), 1, "coincident boundaries collapse");
        assert!(union.contains(Vec2::new(32.0, 32.0)));
        assert!(!union.contains(Vec2::new(10.0, 32.0)));

        let intersection = boolean_paths(&a, &b, PathBoolean::Intersect).expect("intersection");
        assert!(intersection.contains(Vec2::new(32.0, 32.0)));
        assert!(!intersection.contains(Vec2::new(10.0, 32.0)));

        assert!(boolean_paths(&a, &b, PathBoolean::Subtract).is_none());
        assert!(boolean_paths(&a, &b, PathBoolean::Xor).is_none());
    }

    #[test]
    fn vector_boolean_result_is_rasterized_from_the_combined_outline() {
        let a = rect_path(Vec2::new(10.0, 10.0), Vec2::new(60.0, 60.0));
        let b = rect_path(Vec2::new(40.0, 10.0), Vec2::new(90.0, 60.0));
        let union = boolean_paths(&a, &b, PathBoolean::Add).expect("union");
        let coverage = ShapeSource::Path(union).rasterize(100, 70);
        assert!(*coverage.get(20, 35) > 0.99);
        assert!(*coverage.get(50, 35) > 0.99, "no seam across the overlap");
        assert!(*coverage.get(80, 35) > 0.99);
        assert!(*coverage.get(35, 5) < 0.01);
    }

    #[test]
    fn divide_partitions_the_union_into_overlap_regions() {
        let a = VectorOperand::from_path(&rect_path(Vec2::new(10.0, 10.0), Vec2::new(60.0, 60.0)));
        let b = VectorOperand::from_path(&rect_path(Vec2::new(40.0, 10.0), Vec2::new(90.0, 60.0)));
        let regions = divide_loops(&[a, b]);
        assert_eq!(regions.len(), 3, "A-only, overlap and B-only");
        let sampled: Vec<Vec2> = vec![
            Vec2::new(20.0, 30.0),
            Vec2::new(50.0, 30.0),
            Vec2::new(80.0, 30.0),
        ];
        for point in sampled {
            let hits = regions
                .iter()
                .filter(|region| point_in_polygons(region, point, FillRule::EvenOdd))
                .count();
            assert_eq!(hits, 1, "every point belongs to exactly one region");
        }
    }

    #[test]
    fn subtract_leaves_a_hole_with_even_odd_loops() {
        let outer = rect_path(Vec2::new(10.0, 10.0), Vec2::new(90.0, 90.0));
        let inner = rect_path(Vec2::new(35.0, 35.0), Vec2::new(65.0, 65.0));
        let result = boolean_paths(&outer, &inner, PathBoolean::Subtract).expect("subtract");
        assert_eq!(result.subpaths.len(), 2, "outer outline plus the hole");
        assert!(!result.contains(Vec2::new(50.0, 50.0)));
        assert!(result.contains(Vec2::new(20.0, 50.0)));
    }

    /// True when the open segments cross away from their endpoints.
    fn segments_cross(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> bool {
        let (d1, d2) = (a1 - a0, b1 - b0);
        let denom = cross(d1, d2);
        if denom.abs() < 1e-6 {
            return false;
        }
        let t = cross(b0 - a0, d2) / denom;
        let u = cross(b0 - a0, d1) / denom;
        (1e-4..1.0 - 1e-4).contains(&t) && (1e-4..1.0 - 1e-4).contains(&u)
    }

    /// Zigzag knife line used by the stroke band tests.
    fn zigzag() -> Vec<Vec2> {
        vec![
            Vec2::new(10.0, 200.0),
            Vec2::new(90.0, 120.0),
            Vec2::new(160.0, 170.0),
        ]
    }

    #[test]
    fn stroke_band_covers_the_line_with_round_ends() {
        let band = stroke_polyline_outline(&zigzag(), 6.0).expect("band");
        let inside =
            |point: Vec2| point_in_polygons(std::slice::from_ref(&band), point, FillRule::NonZero);
        // Along the line, a hair to either side of it.
        assert!(inside(Vec2::new(90.0, 120.0)));
        let mid = Vec2::new(50.0, 160.0);
        let along = (Vec2::new(90.0, 120.0) - Vec2::new(10.0, 200.0)).normalize();
        let normal = Vec2::new(-along.y, along.x);
        assert!(inside(mid + normal * 2.5));
        assert!(inside(mid - normal * 2.5));
        assert!(!inside(mid + normal * 4.0));
        // The round cap reaches past the first point, the closing edge of a
        // filled open path would not.
        assert!(inside(Vec2::new(10.0, 200.0) - along * 2.0));
        assert!(!inside(Vec2::new(10.0, 200.0) - along * 4.0));
    }

    #[test]
    fn stroke_band_is_a_simple_loop_at_sharp_corners() {
        let band = stroke_polyline_outline(&zigzag(), 6.0).expect("band");
        // A stroke expander crosses itself on the inside of a sharp corner,
        // and such a crossing cannot be classified by a boolean. The band is
        // therefore built as one loop that never crosses.
        for a in 0..band.len() {
            let a0 = band[a];
            let a1 = band[(a + 1) % band.len()];
            for b in a + 2..band.len() {
                if (b + 1) % band.len() == a {
                    continue;
                }
                let b0 = band[b];
                let b1 = band[(b + 1) % band.len()];
                assert!(
                    !segments_cross(a0, a1, b0, b1),
                    "edges {a} and {b} cross: {a0:?}->{a1:?} vs {b0:?}->{b1:?}"
                );
            }
        }
    }

    #[test]
    fn subtract_a_stroke_band_splits_a_shape_in_two() {
        let panel =
            VectorOperand::from_path(&rect_path(Vec2::new(0.0, 0.0), Vec2::new(200.0, 160.0)));
        let band = VectorOperand::new(
            vec![stroke_polyline_outline(&zigzag(), 8.0).expect("band")],
            FillRule::NonZero,
        );
        let loops = boolean_loops(&[panel, band], &[PathBoolean::Subtract]);
        let pieces = split_loops(&loops);
        assert_eq!(pieces.len(), 2, "the line crosses the panel end to end");
        let covers = |pieces: &[Vec<Vec<Vec2>>], point: Vec2| {
            pieces
                .iter()
                .filter(|loops| point_in_polygons(loops, point, FillRule::EvenOdd))
                .count()
        };
        // Above the line, below it, and inside the cut.
        assert_eq!(covers(&pieces, Vec2::new(100.0, 60.0)), 1);
        assert_eq!(covers(&pieces, Vec2::new(100.0, 150.0)), 1);
        assert_eq!(covers(&pieces, Vec2::new(125.0, 145.0)), 0);
    }

    #[test]
    fn stroke_band_keeps_the_whole_line_width_through_a_sharp_corner() {
        let band = stroke_polyline_outline(&zigzag(), 6.0).expect("band");
        let inside =
            |point: Vec2| point_in_polygons(std::slice::from_ref(&band), point, FillRule::NonZero);
        // Two and a half pixels off the centreline on both sides of the
        // corner still belongs to the band.
        let corner = Vec2::new(90.0, 120.0);
        let incoming = (corner - Vec2::new(10.0, 200.0)).normalize();
        let outgoing = (Vec2::new(160.0, 170.0) - corner).normalize();
        let outward = (incoming - outgoing).normalize();
        assert!(inside(corner + outward * 2.5));
        assert!(!inside(corner + outward * 4.0));
    }

    #[test]
    fn stroke_outline_keeps_the_center_hole() {
        let square = rect_path(Vec2::new(20.0, 20.0), Vec2::new(80.0, 80.0));
        let outline = square.stroke_outline(10.0).expect("stroke outline");
        assert!(outline.contains(Vec2::new(20.0, 50.0)));
        assert!(outline.contains(Vec2::new(16.0, 50.0)));
        assert!(!outline.contains(Vec2::new(50.0, 50.0)));
    }

    #[test]
    fn open_path_stroke_has_round_caps() {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(10.0, 20.0)));
        subpath.nodes.push(Node::corner(Vec2::new(50.0, 20.0)));
        path.subpaths.push(subpath);
        let outline = path.stroke_outline(10.0).expect("open stroke");
        assert!(outline.contains(Vec2::new(30.0, 20.0)));
        assert!(outline.contains(Vec2::new(50.0, 22.0)));
        assert!(!outline.contains(Vec2::new(30.0, 35.0)));
    }

    #[test]
    fn primitive_converts_to_editable_path() {
        let source = ShapeSource::Primitive(Primitive::Ellipse {
            center: Vec2::new(32.0, 32.0),
            radii: Vec2::new(20.0, 10.0),
        });
        let path = source.to_path().expect("primitive path");
        assert_eq!(path.subpaths.len(), 1);
        assert!(path.subpaths[0].closed);
        assert!(path.node_count() > 8);
    }

    #[test]
    fn square_path_fills_center() {
        let source = ShapeSource::Path(square_path(64.0));
        let mask = source.rasterize(64, 64);
        assert!(*mask.get(32, 32) > 0.99);
        assert!(*mask.get(2, 2) < 0.01);
        assert!(*mask.get(8, 32) > 0.1);
    }

    #[test]
    fn primitive_rect_matches_expected_coverage() {
        let source = ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::new(32.0, 32.0),
            size: Vec2::new(32.0, 32.0),
            corner_radius: 4.0,
        });
        let mask = source.rasterize(64, 64);
        assert!(*mask.get(32, 32) > 0.99);
        assert!(*mask.get(10, 10) < 0.01);
    }

    #[test]
    fn shape_bounds_hit_test_and_transform() {
        let source = ShapeSource::Primitive(Primitive::Ellipse {
            center: Vec2::new(50.0, 40.0),
            radii: Vec2::new(20.0, 10.0),
        });
        let bounds = source.bounds().unwrap();
        assert!((bounds.min - Vec2::new(30.0, 30.0)).length() < 1e-3);
        assert!((bounds.max - Vec2::new(70.0, 50.0)).length() < 1e-3);
        assert!((bounds.center() - Vec2::new(50.0, 40.0)).length() < 1e-3);
        assert!(source.contains(Vec2::new(50.0, 40.0)));
        assert!(!source.contains(Vec2::new(80.0, 40.0)));

        let moved = source
            .transformed(Vec2::ONE, Vec2::new(10.0, -5.0))
            .unwrap();
        let moved_bounds = moved.bounds().unwrap();
        assert!((moved_bounds.center() - Vec2::new(60.0, 35.0)).length() < 1e-3);

        let scaled = source.transformed(Vec2::new(2.0, 1.0), Vec2::ZERO).unwrap();
        let scaled_bounds = scaled.bounds().unwrap();
        assert!((scaled_bounds.size() - Vec2::new(80.0, 20.0)).length() < 1e-2);
    }

    #[test]
    fn rounded_rect_polygon_converts_to_single_closed_shape() {
        let source = ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::new(32.0, 32.0),
            size: Vec2::new(30.0, 20.0),
            corner_radius: 4.0,
        });
        let poly = source.as_polygon().unwrap();
        assert!(poly.len() > 8);
        let bounds = Bounds::from_points(poly.iter().copied()).unwrap();
        assert!((bounds.size() - Vec2::new(30.0, 20.0)).length() < 0.2);
        assert!(source.contains(Vec2::new(32.0, 32.0)));
        assert!(!source.contains(Vec2::new(60.0, 60.0)));
    }

    #[test]
    fn path_as_polygon_requires_single_closed_subpath() {
        let closed = ShapeSource::Path(square_path(64.0));
        assert!(closed.as_polygon().is_some());

        let mut open = Path::new(FillRule::NonZero);
        let mut sp = SubPath::new(false);
        sp.nodes.push(Node::corner(Vec2::new(0.0, 0.0)));
        sp.nodes.push(Node::corner(Vec2::new(10.0, 0.0)));
        open.subpaths.push(sp);
        assert!(ShapeSource::Path(open).as_polygon().is_none());
    }

    #[test]
    fn even_odd_and_nonzero_differ_for_overlap() {
        let mut p = Path::new(FillRule::EvenOdd);
        for offset in [0.0f32, 8.0] {
            let mut sp = SubPath::new(true);
            let o = offset;
            sp.nodes.push(Node::corner(Vec2::new(8.0 + o, 8.0)));
            sp.nodes.push(Node::corner(Vec2::new(40.0 + o, 8.0)));
            sp.nodes.push(Node::corner(Vec2::new(40.0 + o, 40.0)));
            sp.nodes.push(Node::corner(Vec2::new(8.0 + o, 40.0)));
            p.subpaths.push(sp);
        }
        let even = ShapeSource::Path(p.clone()).rasterize(64, 64);
        let mut p2 = p.clone();
        p2.fill_rule = FillRule::NonZero;
        let nonzero = ShapeSource::Path(p2).rasterize(64, 64);
        assert!(*even.get(24, 24) < 0.1);
        assert!(*nonzero.get(24, 24) > 0.9);
    }

    #[test]
    fn bezier_flattening_stays_in_bounds() {
        let mut p = Path::new(FillRule::NonZero);
        let mut sp = SubPath::new(false);
        let mut a = Node::smooth(Vec2::new(10.0, 10.0), Vec2::ZERO, Vec2::new(20.0, -20.0));
        a.kind = NodeKind::Smooth;
        sp.nodes.push(a);
        sp.nodes.push(Node::corner(Vec2::new(50.0, 10.0)));
        p.subpaths.push(sp);
        let polys = flatten_path(&p);
        assert_eq!(polys.len(), 1);
        for pt in &polys[0] {
            assert!(pt.x >= 9.0 && pt.x <= 51.0);
            assert!(pt.y >= -11.0 && pt.y <= 11.0);
        }
    }

    #[test]
    fn transformed_affine_rotates_a_rect_about_its_centre() {
        let source = ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::new(10.0, 0.0),
            size: Vec2::splat(4.0),
            corner_radius: 0.0,
        });
        // 90 degrees about the origin: (10, 0) -> (0, 10).
        let rotated = source
            .transformed_affine(Vec2::ONE, std::f32::consts::FRAC_PI_2, Vec2::ZERO)
            .expect("rect rotates");
        let bounds = rotated.bounds().expect("rotated rect has bounds");
        assert!((bounds.center() - Vec2::new(0.0, 10.0)).length() < 1e-3);
        assert!(matches!(rotated, ShapeSource::Path(_)));
    }

    #[test]
    fn transformed_affine_without_rotation_keeps_the_primitive() {
        let source = ShapeSource::Primitive(Primitive::Ellipse {
            center: Vec2::new(10.0, 10.0),
            radii: Vec2::new(4.0, 2.0),
        });
        let moved = source
            .transformed_affine(Vec2::splat(2.0), 0.0, Vec2::new(5.0, 5.0))
            .expect("ellipse moves");
        let ShapeSource::Primitive(Primitive::Ellipse { center, radii }) = moved else {
            panic!("rotation-free edits stay parametric");
        };
        assert!((center - Vec2::new(25.0, 25.0)).length() < 1e-4);
        assert!((radii - Vec2::new(8.0, 4.0)).length() < 1e-4);
    }

    #[test]
    fn a_baked_region_keeps_its_place_and_rotates_with_the_canvas() {
        let mut coverage = Grid::new(4, 2, 0.0);
        for y in 0..2 {
            for x in 0..2 {
                coverage.set(x, y, 1.0);
            }
        }
        let source = ShapeSource::RasterRegion(RasterRegion::new(coverage, IVec2::new(10, 10)));
        assert_eq!(
            source.bounds(),
            Some(Bounds {
                min: Vec2::new(10.0, 10.0),
                max: Vec2::new(14.0, 12.0),
            })
        );

        let moved = source
            .transformed(Vec2::ONE, Vec2::new(5.0, -3.0))
            .expect("baked pixels move");
        let ShapeSource::RasterRegion(moved) = moved else {
            panic!("translation keeps the region")
        };
        assert_eq!(moved.origin, IVec2::new(15, 7));
        assert_eq!(moved.get_document(15, 7), 1.0);
        assert_eq!(moved.get_document(17, 7), 0.0);

        // 90 degrees about the origin maps x 10..14, y 10..12 to a 2-wide,
        // 4-tall box at negative x.
        let rotated = source
            .transformed_affine(Vec2::ONE, std::f32::consts::FRAC_PI_2, Vec2::ZERO)
            .expect("baked pixels rotate");
        let bounds = rotated.bounds().expect("rotated region has bounds");
        assert!((bounds.size().x - 2.0).abs() < 1e-3, "{bounds:?}");
        assert!((bounds.size().y - 4.0).abs() < 1e-3, "{bounds:?}");

        let text = ron::to_string(&source).expect("serialize baked region");
        let loaded: ShapeSource = ron::from_str(&text).expect("deserialize baked region");
        assert!(matches!(loaded, ShapeSource::RasterRegion(_)));
    }

    #[test]
    fn edge_bevel_profiles_round_trip_and_old_paths_still_load() {
        let mut path =
            Path::from_polygon(&[Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)]);
        path.set_edge_profile(0, 1, Some(Profile::Smooth));
        assert!(path.has_edge_profiles());
        assert_eq!(path.edge_profile(0, 1), Some(&Profile::Smooth));
        assert!(path.edge_profile(0, 0).is_none());

        let text = ron::to_string(&path).expect("serialize path with edge profiles");
        let loaded: Path = ron::from_str(&text).expect("deserialize path with edge profiles");
        assert_eq!(loaded.edge_profile(0, 1), Some(&Profile::Smooth));

        // A project saved before the field existed has no `bevel` key.
        let old: Path = ron::from_str(
            "(subpaths: [(nodes: [(pos: (0.0, 0.0), handle_in: (0.0, 0.0), handle_out: (0.0, 0.0), kind: Corner), (pos: (1.0, 0.0), handle_in: (0.0, 0.0), handle_out: (0.0, 0.0), kind: Corner)], closed: false)], fill_rule: NonZero)",
        )
        .expect("old path loads without edge profiles");
        assert!(!old.has_edge_profiles());
    }
}
