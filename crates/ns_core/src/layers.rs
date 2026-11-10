use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use glam::{IVec2, UVec2, Vec2, Vec3};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::blend::{blend, slope, subtract_normals, BlendMode, BooleanOp};
use crate::cable::CableMeta;
use crate::convention::GreenChannel;
use crate::filter::gaussian_blur;
use crate::grid::{Grid, Wrap};
use crate::height::height_to_normal;
use crate::mask::{clip_coverage, smart_mask, static_mask, BaseSurface, MaskSettings};
use crate::path::{
    boolean_loops, coverage_to_path, divide_loops, flatten_path, flatten_path_tolerance,
    flatten_path_tolerance_loops, split_loops, stroke_polyline_outline, Bounds, FillRule, Path,
    PathBoolean, RasterRegion, ShapeSource, VectorOperand, VECTOR_BOOLEAN_TOLERANCE,
};
use crate::sdf::{
    bevel_mask_aa, bevel_mask_aa_with, bevel_mask_stroke_aa_with, height_mask_aa,
    height_mask_aa_with, height_mask_stroke_aa_with, stroke_coverage_aa, BevelParams,
    EdgeProfileField, Profile,
};
use crate::CancelToken;

pub type LayerId = Uuid;

fn is_cancelled(token: Option<&CancelToken>) -> bool {
    token.is_some_and(CancelToken::is_cancelled)
}

/// Default cap for interactive CPU caches. Larger stacks stream instead.
pub const DEFAULT_EVAL_CACHE_BUDGET: u64 = 256 * 1024 * 1024;

/// Largest accepted document side and pixel count. Evaluation allocates several
/// full-size f32 grids per layer, so these are memory limits, not UI limits.
pub const MAX_DOC_SIDE: u32 = 16_384;
pub const MAX_DOC_PIXELS: u64 = 16_384 * 16_384;
/// Group nesting and total layer count. Evaluation, the layer panel and the
/// serialiser all recurse over the tree.
pub const MAX_LAYER_DEPTH: usize = 64;
pub const MAX_LAYER_COUNT: usize = 10_000;

/// UDIM tile every document starts on. Layers without their own tile tag are
/// shared by all tiles, so this only selects which tile the canvas shows.
pub const BASE_UDIM_TILE: u32 = 1001;

fn default_active_tile() -> u32 {
    BASE_UDIM_TILE
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub size: UVec2,
    pub wrap: Wrap,
    /// External normal-map convention: used to encode `normal.png` and to
    /// decode imported normal maps. Internal data is always canonical OpenGL.
    pub green: GreenChannel,
    /// Converts 0–1 height to pixel units for derived maps.
    pub height_scale_px: f32,
    /// Bottom to top.
    pub layers: Vec<Layer>,
    /// UDIM tile the evaluators and the 2D canvas show. Layers tagged for a
    /// different tile are skipped, so one document can author every tile of a
    /// mesh without blending their maps together. Runtime-only: which tile a
    /// user was looking at should not make a project dirty.
    #[serde(default = "default_active_tile", skip_serializing)]
    pub active_tile: u32,
}

impl Document {
    pub fn new(size: UVec2) -> Self {
        Self {
            size,
            wrap: Wrap::Clamp,
            green: GreenChannel::OpenGl,
            height_scale_px: 64.0,
            layers: Vec::new(),
            active_tile: BASE_UDIM_TILE,
        }
    }

    /// True when the layer contributes to the document's active tile.
    pub fn layer_is_active(&self, layer: &Layer) -> bool {
        layer.applies_to_tile(self.active_tile)
    }

    fn eval_tile(&self, cache: &EvalCache) -> u32 {
        cache.tile.unwrap_or(self.active_tile)
    }

    /// Every UDIM tile a layer is tagged for, including tiles nested in groups.
    /// Returns tiles in sorted order; a document with only shared layers has no
    /// tagged tiles.
    pub fn layer_tiles(&self) -> Vec<u32> {
        fn visit(layers: &[Layer], tiles: &mut std::collections::BTreeSet<u32>) {
            for layer in layers {
                if let Some(tile) = layer.tile {
                    tiles.insert(tile);
                }
                if let Some(children) = layer.children() {
                    visit(children, tiles);
                }
            }
        }
        let mut tiles = std::collections::BTreeSet::new();
        visit(&self.layers, &mut tiles);
        tiles.into_iter().collect()
    }

    /// A new project: one neutral base layer and nothing else. The editor
    /// starts here so a session does not carry demo artwork into every map.
    pub fn new_with_base(size: UVec2) -> Self {
        let mut doc = Self::new(size);
        doc.layers.push(Layer::flat("base"));
        doc
    }

    /// Convert pre-crop full-document raster shapes into cropped regions.
    ///
    /// Older projects stored baked pixels as a document-sized [`ShapeSource::Raster`]
    /// with no position of its own, so those layers had no bounds and could not
    /// be selected or moved. Cropping keeps every pixel in the same document
    /// position and gives the layer its real extent. Idempotent.
    pub fn migrate_raster_sources(&mut self) {
        fn visit(layers: &mut [Layer]) {
            for layer in layers {
                match &mut layer.kind {
                    LayerKind::Shape(shape) => {
                        if let ShapeSource::Raster(coverage) = &shape.source {
                            shape.source =
                                ShapeSource::RasterRegion(RasterRegion::from_document(coverage));
                        }
                    }
                    LayerKind::Group { children } => visit(children),
                    _ => {}
                }
            }
        }
        visit(&mut self.layers);
    }

    /// Check the invariants the evaluators rely on. Called after every load;
    /// cheap (one tree walk, no pixel access).
    pub fn validate(&self) -> crate::Result<()> {
        let (w, h) = (self.size.x, self.size.y);
        if w == 0
            || h == 0
            || w > MAX_DOC_SIDE
            || h > MAX_DOC_SIDE
            || u64::from(w) * u64::from(h) > MAX_DOC_PIXELS
        {
            return Err(crate::CoreError::InvalidDimensions(w, h));
        }
        if !self.height_scale_px.is_finite() {
            return Err(crate::CoreError::Message(
                "height_scale_px is not a finite number".into(),
            ));
        }

        fn visit(
            layers: &[Layer],
            size: UVec2,
            depth: usize,
            count: &mut usize,
        ) -> crate::Result<()> {
            if depth > MAX_LAYER_DEPTH {
                return Err(crate::CoreError::Message(format!(
                    "layer groups are nested deeper than {MAX_LAYER_DEPTH} levels"
                )));
            }
            for layer in layers {
                *count += 1;
                if *count > MAX_LAYER_COUNT {
                    return Err(crate::CoreError::Message(format!(
                        "project has more than {MAX_LAYER_COUNT} layers"
                    )));
                }
                // Masks are indexed at document coordinates (see the normal and
                // height blends), so a mismatched mask reads out of bounds.
                if let Some(mask) = &layer.mask {
                    if mask.w != size.x || mask.h != size.y {
                        return Err(crate::CoreError::Message(format!(
                            "layer '{}' has a {}x{} mask in a {}x{} document",
                            layer.name, mask.w, mask.h, size.x, size.y
                        )));
                    }
                }
                if let LayerKind::Group { children } = &layer.kind {
                    visit(children, size, depth + 1, count)?;
                }
            }
            Ok(())
        }

        let mut count = 0usize;
        visit(&self.layers, self.size, 0, &mut count)
    }

    /// Resolve a path of child indices from the root layer list.
    pub fn layer_by_path(&self, path: &[usize]) -> Option<&Layer> {
        let (last, parents) = path.split_last()?;
        let mut layers: &[Layer] = &self.layers;
        for index in parents {
            layers = layers.get(*index)?.children()?;
        }
        layers.get(*last)
    }

    pub fn layer_by_path_mut(&mut self, path: &[usize]) -> Option<&mut Layer> {
        let (last, parents) = path.split_last()?;
        let mut layers = &mut self.layers;
        for index in parents {
            layers = layers.get_mut(*index)?.children_mut()?;
        }
        layers.get_mut(*last)
    }

    fn parent_layers_mut(&mut self, parent_path: &[usize]) -> Option<&mut Vec<Layer>> {
        let mut layers = &mut self.layers;
        for index in parent_path {
            layers = layers.get_mut(*index)?.children_mut()?;
        }
        Some(layers)
    }

    pub fn find_layer(&self, id: LayerId) -> Option<&Layer> {
        fn visit(layers: &[Layer], id: LayerId) -> Option<&Layer> {
            for layer in layers {
                if layer.id == id {
                    return Some(layer);
                }
                if let Some(children) = layer.children() {
                    if let Some(found) = visit(children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        visit(&self.layers, id)
    }

    pub fn find_layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        fn visit(layers: &mut [Layer], id: LayerId) -> Option<&mut Layer> {
            for layer in layers {
                if layer.id == id {
                    return Some(layer);
                }
                if let Some(children) = layer.children_mut() {
                    if let Some(found) = visit(children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        visit(&mut self.layers, id)
    }

    /// True when the layer exists and its content is locked.
    pub fn is_locked(&self, id: LayerId) -> bool {
        self.find_layer(id).is_some_and(|layer| layer.locked)
    }

    /// True when the layer at `path` is locked. Paths are what the UI holds.
    pub fn path_is_locked(&self, path: &[usize]) -> bool {
        self.layer_by_path(path).is_some_and(|layer| layer.locked)
    }

    pub fn find_path(&self, id: LayerId) -> Option<Vec<usize>> {
        fn visit(layers: &[Layer], id: LayerId, path: &mut Vec<usize>) -> Option<Vec<usize>> {
            for (index, layer) in layers.iter().enumerate() {
                path.push(index);
                if layer.id == id {
                    return Some(path.clone());
                }
                if let Some(children) = layer.children() {
                    if let Some(found) = visit(children, id, path) {
                        return Some(found);
                    }
                }
                path.pop();
            }
            None
        }
        visit(&self.layers, id, &mut Vec::new())
    }

    pub fn layer_count(&self) -> usize {
        fn visit(layers: &[Layer]) -> usize {
            layers
                .iter()
                .map(|layer| 1 + layer.children().map(visit).unwrap_or(0))
                .sum()
        }
        visit(&self.layers)
    }

    pub fn visible_layer_count(&self) -> usize {
        fn visit(layers: &[Layer]) -> usize {
            layers
                .iter()
                .map(|layer| {
                    usize::from(layer.visible)
                        + if layer.visible {
                            layer.children().map(visit).unwrap_or(0)
                        } else {
                            0
                        }
                })
                .sum()
        }
        visit(&self.layers)
    }

    pub fn all_layer_ids(&self) -> HashSet<LayerId> {
        fn visit(layers: &[Layer], out: &mut HashSet<LayerId>) {
            for layer in layers {
                out.insert(layer.id);
                if let Some(children) = layer.children() {
                    visit(children, out);
                }
            }
        }
        let mut out = HashSet::new();
        visit(&self.layers, &mut out);
        out
    }

    /// Build a flat id -> layer lookup for coverage and revision queries.
    fn layer_index(&self) -> HashMap<LayerId, &Layer> {
        fn visit<'a>(layers: &'a [Layer], out: &mut HashMap<LayerId, &'a Layer>) {
            for layer in layers {
                out.insert(layer.id, layer);
                if let Some(children) = layer.children() {
                    visit(children, out);
                }
            }
        }
        let mut out = HashMap::new();
        visit(&self.layers, &mut out);
        out
    }

    /// True when `candidate` sits inside `ancestor` (including the same layer).
    pub fn is_related(&self, ancestor: LayerId, candidate: LayerId) -> bool {
        let Some(ancestor_path) = self.find_path(ancestor) else {
            return false;
        };
        let Some(candidate_path) = self.find_path(candidate) else {
            return false;
        };
        candidate_path.starts_with(&ancestor_path) || ancestor_path.starts_with(&candidate_path)
    }

    /// True when `ancestor` is a strict ancestor of `descendant`.
    pub fn is_ancestor(&self, ancestor: LayerId, descendant: LayerId) -> bool {
        let (Some(ancestor_path), Some(descendant_path)) =
            (self.find_path(ancestor), self.find_path(descendant))
        else {
            return false;
        };
        descendant_path.len() > ancestor_path.len() && descendant_path.starts_with(&ancestor_path)
    }

    /// True when `operand` can be added to `target`'s boolean expression.
    ///
    /// The target itself, an ancestor group (which would recurse through the
    /// target), and duplicates are rejected.
    pub fn can_add_boolean_operand(&self, target: LayerId, operand: LayerId) -> bool {
        if target == operand {
            return false;
        }
        let Some(target_layer) = self.find_layer(target) else {
            return false;
        };
        if self.find_layer(operand).is_none() || self.is_ancestor(operand, target) {
            return false;
        }
        !target_layer
            .boolean_operands
            .iter()
            .any(|existing| existing.layer == operand)
    }

    /// Mask currently hiding `id`, for canvas overlays: the painted mask with
    /// its adjustments (invert, levels, feather, density, static generators)
    /// combined with the clipping source (invert, offset, feather).
    ///
    /// Smart generators are not included because they depend on the surface
    /// below; the overlay stays cheap enough to rebuild while painting.
    pub fn mask_preview(&self, id: LayerId) -> Option<Grid<f32>> {
        let layer = self.find_layer(id)?;
        let settings = &layer.mask_settings;
        let mut mask = static_mask(layer.mask.as_ref(), settings, self.size, self.wrap);
        if let Some(mask_id) = layer.mask_layer {
            let index = self.layer_index();
            if let Some(source) = index.get(&mask_id) {
                let mut stack = HashSet::new();
                let (_, coverage) = self.coverage_of(source, &index, &mut stack, self.active_tile);
                let clip = clip_coverage(
                    &coverage,
                    settings.invert_clip,
                    settings.clip_offset_px,
                    settings.clip_feather_px,
                    self.wrap,
                );
                mask = Some(match mask {
                    Some(mask) => multiply_grids(&mask, &clip),
                    None => clip,
                });
            }
        }
        mask
    }

    /// True when `id` is referenced as the clipping mask of another layer.
    pub fn is_mask_source(&self, id: LayerId) -> bool {
        fn visit(layers: &[Layer], id: LayerId) -> bool {
            layers.iter().any(|layer| {
                layer.mask_layer == Some(id)
                    || layer.children().is_some_and(|children| visit(children, id))
            })
        }
        visit(&self.layers, id)
    }

    /// Layers and groups that can safely be used as a clipping mask for `id`.
    pub fn mask_candidates(&self, id: LayerId) -> Vec<(LayerId, String, usize)> {
        fn visit(
            layers: &[Layer],
            doc: &Document,
            id: LayerId,
            depth: usize,
            out: &mut Vec<(LayerId, String, usize)>,
        ) {
            for layer in layers {
                if !doc.is_related(id, layer.id) {
                    out.push((layer.id, layer.name.clone(), depth));
                }
                if let Some(children) = layer.children() {
                    visit(children, doc, id, depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        visit(&self.layers, self, id, 0, &mut out);
        out
    }

    pub fn remove_at(&mut self, path: &[usize]) -> Option<Layer> {
        let (last, parents) = path.split_last()?;
        let layers = self.parent_layers_mut(parents)?;
        if *last >= layers.len() {
            return None;
        }
        Some(layers.remove(*last))
    }

    pub fn insert_at(&mut self, parent_path: &[usize], index: usize, layer: Layer) -> bool {
        let Some(layers) = self.parent_layers_mut(parent_path) else {
            return false;
        };
        let index = index.min(layers.len());
        layers.insert(index, layer);
        true
    }

    /// Wrap the layer at `path` in a group and return the new group's id.
    pub fn group_at(&mut self, path: &[usize]) -> Option<LayerId> {
        let layer = self.remove_at(path)?;
        let group = Layer::group("Group", vec![layer]);
        let id = group.id;
        let index = *path.last()?;
        self.insert_at(&path[..path.len() - 1], index, group);
        Some(id)
    }

    /// Wrap one or more sibling layers in a group and return the group's path.
    /// Paths from other parents, duplicate paths and invalid paths are ignored.
    pub fn group_many_at(&mut self, paths: &[Vec<usize>]) -> Option<Vec<usize>> {
        let primary = paths.first()?;
        if primary.is_empty() {
            return None;
        }
        let parent = primary[..primary.len() - 1].to_vec();
        let depth = primary.len();
        let mut unique: Vec<Vec<usize>> = Vec::new();
        for path in paths {
            if path.len() == depth
                && path[..depth - 1] == parent[..]
                && !unique.contains(path)
                && self.layer_by_path(path).is_some()
            {
                unique.push(path.clone());
            }
        }
        if unique.is_empty() {
            return None;
        }
        if unique.len() == 1 {
            let path = unique.pop()?;
            self.group_at(&path)?;
            return Some(path);
        }
        unique.sort_by_key(|path| path[depth - 1]);
        let mut removed = Vec::new();
        for path in unique.iter().rev() {
            if let Some(layer) = self.remove_at(path) {
                removed.push(layer);
            }
        }
        removed.reverse();
        let index = unique[0][depth - 1];
        let group = Layer::group("Group", removed);
        self.insert_at(&parent, index, group);
        let mut group_path = parent;
        group_path.push(index);
        Some(group_path)
    }

    /// Replace the group at `path` with its children and return the path of
    /// the first child, if the group had any.
    pub fn ungroup_at(&mut self, path: &[usize]) -> Option<Vec<usize>> {
        let group = self.remove_at(path)?;
        let LayerKind::Group { children } = group.kind else {
            self.insert_at(&path[..path.len() - 1], *path.last()?, group);
            return None;
        };
        let parent = &path[..path.len() - 1];
        let index = *path.last()?;
        let count = children.len();
        for (offset, child) in children.into_iter().enumerate() {
            self.insert_at(parent, index + offset, child);
        }
        if count == 0 {
            None
        } else {
            let mut first = parent.to_vec();
            first.push(index);
            Some(first)
        }
    }

    /// Replace a boolean group with one editable path layer. The combined
    /// vector result becomes the new shape, the bottom visible operand keeps
    /// supplying the bevel style, and the group's opacity, blend, mask and
    /// clipping settings are copied onto the result. Divide groups should use
    /// [`Self::release_divide_compound`] so the cuts are preserved.
    pub fn flatten_vector_compound(&mut self, path: &[usize]) -> Option<Vec<usize>> {
        let group = self.layer_by_path(path)?;
        let LayerKind::Group { children } = &group.kind else {
            return None;
        };
        let op = group.vector_op?;
        if op.is_divide() || op.is_cut() {
            // Cut groups stay live: their result is a height surface, not one
            // combined outline that could become a single shape.
            return None;
        }
        let base = compound_base_shape(children)?.clone();
        let index = self.layer_index();
        let mut stack = HashSet::new();
        let result = self
            .compound_regions(children, op, &index, &mut stack, self.active_tile)
            .into_iter()
            .next()?;
        let name = if group.name.is_empty() {
            "Boolean".to_string()
        } else {
            group.name.clone()
        };
        let flattened = compound_result_layer(self, group, &base, ShapeSource::Path(result), name);

        let index = *path.last()?;
        self.remove_at(path)?;
        let parent = &path[..path.len() - 1];
        self.insert_at(parent, index, flattened)
            .then(|| path.to_vec())
    }

    /// Commit a cut boolean group without rasterizing it.
    ///
    /// A cut changes the base's height surface, so it cannot collapse to one
    /// outline. Instead the group becomes an ordinary editable group: the base
    /// keeps its style and every cutter keeps its shape, with the equivalent
    /// per-layer cut blend applied. The normal tree evaluator uses the same
    /// `composite_cut` math, so the pocket, groove or seam is preserved while
    /// the live boolean disappears.
    pub fn flatten_cut_compound(&mut self, path: &[usize]) -> Option<Vec<usize>> {
        let group = self.layer_by_path_mut(path)?;
        let LayerKind::Group { children } = &mut group.kind else {
            return None;
        };
        let mode = group.vector_op?.cut_blend_mode()?;
        let mut seen_base = false;
        for child in children.iter_mut() {
            if !child.paints() || !child.has_shape_geometry() {
                continue;
            }
            if !seen_base {
                seen_base = true;
                continue;
            }
            child.blend = mode;
            child.touch();
        }
        if !seen_base {
            return None;
        }
        group.vector_op = None;
        group.touch();
        Some(path.to_vec())
    }

    /// Merge a group into one baked normal layer.
    ///
    /// The group's children are evaluated over a flat base exactly as the stack
    /// renders them, live booleans and per-layer blends included, then the group
    /// is replaced by one baked normal shape. The shape's RasterRegion provides
    /// the movable extent and its [`BakedNormal`] keeps the exact normals, so
    /// the result still selects, snaps, nudges and drags like the shapes it
    /// replaced. The replacement keeps the group's identity, name, blend,
    /// opacity, mask, clipping and tile, so the rendered normal map does not
    /// change; only the children, their bevels and their editable geometry are
    /// gone. This is the group form of "Bake to pixels".
    pub fn bake_group_to_pixels(&mut self, path: &[usize]) -> Option<Vec<usize>> {
        let group = self.layer_by_path(path)?;
        let LayerKind::Group { .. } = &group.kind else {
            return None;
        };
        let id = group.id;
        let name = if group.name.is_empty() {
            "Baked group".to_string()
        } else {
            group.name.clone()
        };
        let revision = group.revision;
        let mask_revision = group.mask_revision;
        // The group's visible relief is exactly where the contribution leaves
        // flat normals/zero height. Using that as the movable silhouette keeps
        // the baked layer's bounds on the artwork (a flat base layer that fills
        // the canvas must not make the selection frame huge) and also excludes
        // hidden mask sources automatically.
        let (normal, height) = self.evaluate_layer_contribution(id)?;
        let coverage = Grid::from_fn(normal.w, normal.h, |x, y| {
            let normal = *normal.get(x, y);
            let height = *height.get(x, y);
            if normal.z < 0.999 || height.abs() > 1.0e-3 {
                1.0
            } else {
                0.0
            }
        });
        let region = RasterRegion::from_document(&coverage);
        let baked_origin = region.origin;
        let mut merged = Layer::shape(name, ShapeSource::RasterRegion(region));
        if let LayerKind::Shape(shape) = &mut merged.kind {
            shape.baked_normal = Some(BakedNormal {
                normal,
                origin: baked_origin,
            });
        }
        // Reusing the id keeps every mask and boolean-operand reference that
        // pointed at the group working after the swap.
        merged.id = id;
        if let Some(group) = self.find_layer(id) {
            merged.visible = group.visible;
            merged.locked = group.locked;
            merged.tile = group.tile;
            merged.opacity = group.opacity;
            merged.blend = group.blend;
            merged.mask = group.mask.clone();
            merged.mask_layer = group.mask_layer;
            merged.mask_settings = group.mask_settings.clone();
            merged.boolean_operands = group.boolean_operands.clone();
            merged.hidden_as_mask = group.hidden_as_mask;
            merged.id_color = group.id_color;
        }
        // Start above the old revision so a cache keyed by the group's id
        // recomputes instead of returning the group's surface.
        merged.revision = revision;
        merged.mask_revision = mask_revision;
        merged.touch();
        merged.touch_mask();

        let index = *path.last()?;
        self.remove_at(path)?;
        let parent = &path[..path.len() - 1];
        merged.mask_layer = merged
            .mask_layer
            .filter(|mask| self.find_layer(*mask).is_some());
        self.insert_at(parent, index, merged).then(|| path.to_vec())
    }

    /// Turn a Divide group into one editable layer per overlap island.
    ///
    /// Every island is already a real vector region, so the split keeps the
    /// exact outlines and the bevel creases that Divide shows.
    pub fn release_divide_compound(&mut self, path: &[usize]) -> Option<Vec<Vec<usize>>> {
        let group = self.layer_by_path(path)?;
        let LayerKind::Group { children } = &group.kind else {
            return None;
        };
        let op = group.vector_op?;
        if !op.is_divide() {
            return None;
        }
        let base = compound_base_shape(children)?.clone();
        let index = self.layer_index();
        let mut stack = HashSet::new();
        let regions = self.compound_regions(children, op, &index, &mut stack, self.active_tile);
        // A region can be made of several disconnected islands (a line that
        // cuts a shape in two leaves both halves in the same region), so split
        // it into connected pieces to get one layer per island.
        let pieces: Vec<Path> = regions
            .into_iter()
            .flat_map(|region| {
                split_loops(&flatten_path(&region))
                    .into_iter()
                    .map(|loops| Path::from_loops(loops, FillRule::EvenOdd))
            })
            .collect();
        if pieces.len() < 2 {
            return None;
        }
        let base_name = if group.name.is_empty() {
            "Divide".to_string()
        } else {
            group.name.clone()
        };
        let layers: Vec<_> = pieces
            .into_iter()
            .enumerate()
            .map(|(island, region)| {
                let name = format!("{} island {}", base_name, island + 1);
                let mut layer =
                    compound_result_layer(self, group, &base, ShapeSource::Path(region), name);
                // The group was split into independent islands, so boolean
                // operands and clipping that referenced its children no longer
                // make sense on each piece.
                layer.boolean_operands.clear();
                layer
            })
            .collect();

        let insert_index = *path.last()?;
        self.remove_at(path)?;
        let parent = &path[..path.len() - 1];
        let mut inserted = Vec::with_capacity(layers.len());
        for (offset, layer) in layers.into_iter().enumerate() {
            if !self.insert_at(parent, insert_index + offset, layer) {
                break;
            }
            let mut new_path = parent.to_vec();
            new_path.push(insert_index + offset);
            inserted.push(new_path);
        }
        (!inserted.is_empty()).then_some(inserted)
    }

    /// Cut every visible shape (or shape group) behind `cutter_path` with the
    /// cutter's outline.
    ///
    /// The cutter is a vector shape, usually a stroked line: its outline band
    /// is subtracted from each target, and every resulting connected piece
    /// becomes its own editable layer that inherits the target's bevel style
    /// and compositing settings. The cutter is consumed so the line does not
    /// contribute a normal of its own; undo brings it back.
    pub fn cut_layers_below(&mut self, cutter_path: &[usize]) -> Option<CutOutcome> {
        let cutter = self.layer_by_path(cutter_path)?;
        let LayerKind::Shape(cutter_shape) = &cutter.kind else {
            return None;
        };
        let cutter_operand = cutter_shape.cut_operand(self.size)?;
        let cutter_id = cutter.id;
        let index = *cutter_path.last()?;
        let parent = cutter_path[..cutter_path.len() - 1].to_vec();
        let targets: Vec<Vec<usize>> = (0..index)
            .map(|offset| {
                let mut path = parent.clone();
                path.push(offset);
                path
            })
            .collect();

        let outcome = self.trim_layers_with_loops_excluding(
            &targets,
            &cutter_operand.loops,
            Some(cutter_id),
        )?;
        // Targets sit below the cutter, so their new paths survive its removal;
        // a fully trimmed target shifts the count, so re-find the cutter.
        let cutter_path = self.find_path(cutter_id)?;
        self.remove_at(&cutter_path)?;
        Some(outcome)
    }

    /// Trim explicit layers with a set of closed loops.
    ///
    /// Used by the knife and by the lasso's geometric mode. Targets may be
    /// shapes or groups with vector content; a selected parent hides its
    /// descendants from the list because the parent's pieces replace the whole
    /// subtree. Hidden layers, layers without vector geometry and targets that
    /// do not overlap the loops are skipped. A target fully covered by the
    /// loops is removed; every other target is replaced in place by its
    /// remaining pieces, which inherit its bevel, masks and compositing.
    pub fn trim_layers_with_loops(
        &mut self,
        target_paths: &[Vec<usize>],
        loops: &[Vec<Vec2>],
    ) -> Option<CutOutcome> {
        self.trim_layers_with_loops_excluding(target_paths, loops, None)
    }

    fn trim_layers_with_loops_excluding(
        &mut self,
        target_paths: &[Vec<usize>],
        loops: &[Vec<Vec2>],
        exclude: Option<LayerId>,
    ) -> Option<CutOutcome> {
        let cutter = VectorOperand::new(loops.to_vec(), FillRule::NonZero);
        if cutter.is_empty() || loops_area(loops) < 0.5 {
            return None;
        }
        let cutter_bounds = Bounds::from_points(
            cutter
                .loops
                .iter()
                .flat_map(|points| points.iter().copied()),
        )?;

        // Keep the shallowest targets: a selected group already covers its
        // children, and invalid paths are dropped here.
        let targets = self.cut_targets(target_paths);
        if targets.is_empty() {
            return None;
        }

        let index_map = self.layer_index();
        let mut stack = HashSet::new();
        // Pass 1: decide which targets change and with which operand. Nothing
        // is mutated while `index_map` borrows the document.
        let mut changing: Vec<(Vec<usize>, ShapeLayer, VectorOperand)> = Vec::new();
        for path in &targets {
            let Some(target) = self.layer_by_path(path) else {
                continue;
            };
            if !target.paints() {
                continue;
            }
            let Some(style) = cut_style(target) else {
                continue;
            };
            let Some(operand) = self.operand_loops(target, &index_map, &mut stack) else {
                continue;
            };
            let Some(bounds) = Bounds::from_points(
                operand
                    .loops
                    .iter()
                    .flat_map(|points| points.iter().copied()),
            ) else {
                continue;
            };
            if !bounds_overlap(bounds, cutter_bounds) {
                continue;
            }
            changing.push((path.clone(), style, operand));
        }
        if changing.is_empty() {
            return None;
        }

        // Layers that will disappear: clipping references into them must not
        // survive on the pieces.
        let mut excluded: HashSet<LayerId> = changing
            .iter()
            .filter_map(|(path, _, _)| self.layer_by_path(path).map(|layer| layer.id))
            .collect();
        if let Some(id) = exclude {
            excluded.insert(id);
        }

        let mut replacements: Vec<(Vec<usize>, Vec<Layer>)> = Vec::new();
        for (path, style, operand) in changing {
            let Some(target) = self.layer_by_path(&path) else {
                continue;
            };
            let result = boolean_loops(&[operand, cutter.clone()], &[PathBoolean::Subtract]);
            let pieces = split_loops(&result);
            let base_name = if target.name.is_empty() {
                "Piece".to_string()
            } else {
                target.name.clone()
            };
            let count = pieces.len();
            let layers: Vec<Layer> = pieces
                .into_iter()
                .enumerate()
                .map(|(island, component)| {
                    let name = if count == 1 {
                        base_name.clone()
                    } else {
                        format!("{base_name} {}/{count}", island + 1)
                    };
                    cut_piece_layer(self, target, &style, component, name, &excluded)
                })
                .collect();
            replacements.push((path, layers));
        }
        if replacements.is_empty() {
            return None;
        }

        // Replace from the highest sibling down so lower indices stay valid.
        let (targets_changed, paths) = self.apply_cut_replacements(replacements)?;
        Some(CutOutcome {
            paths,
            targets: targets_changed,
        })
    }

    /// Split each target into the part inside `loops` and the part outside,
    /// keeping both as editable layers.
    ///
    /// This is the lasso's right-button gesture: unlike a trim, which removes
    /// the region, the region itself and the remainder both survive. Targets
    /// that the region misses, or covers entirely, are left alone; non-vector
    /// layers have no outline to split and are skipped.
    pub fn divide_layers_with_loops(
        &mut self,
        target_paths: &[Vec<usize>],
        loops: &[Vec<Vec2>],
    ) -> Option<CutOutcome> {
        let cutter = VectorOperand::new(loops.to_vec(), FillRule::NonZero);
        if cutter.is_empty() || loops_area(loops) < 0.5 {
            return None;
        }
        let cutter_bounds = Bounds::from_points(
            cutter
                .loops
                .iter()
                .flat_map(|points| points.iter().copied()),
        )?;
        let targets = self.cut_targets(target_paths);
        if targets.is_empty() {
            return None;
        }

        let index_map = self.layer_index();
        let mut stack = HashSet::new();
        let mut changing: Vec<(Vec<usize>, ShapeLayer, VectorOperand)> = Vec::new();
        for path in &targets {
            let Some(target) = self.layer_by_path(path) else {
                continue;
            };
            if !target.paints() {
                continue;
            }
            let Some(style) = cut_style(target) else {
                continue;
            };
            let Some(operand) = self.operand_loops(target, &index_map, &mut stack) else {
                continue;
            };
            let Some(bounds) = Bounds::from_points(
                operand
                    .loops
                    .iter()
                    .flat_map(|points| points.iter().copied()),
            ) else {
                continue;
            };
            if !bounds_overlap(bounds, cutter_bounds) {
                continue;
            }
            changing.push((path.clone(), style, operand));
        }
        if changing.is_empty() {
            return None;
        }

        let excluded: HashSet<LayerId> = changing
            .iter()
            .filter_map(|(path, _, _)| self.layer_by_path(path).map(|layer| layer.id))
            .collect();

        let mut replacements: Vec<(Vec<usize>, Vec<Layer>)> = Vec::new();
        for (path, style, operand) in changing {
            let Some(target) = self.layer_by_path(&path) else {
                continue;
            };
            let outside =
                boolean_loops(&[operand.clone(), cutter.clone()], &[PathBoolean::Subtract]);
            let inside = boolean_loops(&[operand, cutter.clone()], &[PathBoolean::Intersect]);
            // A region that misses the shape or swallows all of it is not a
            // split: leave that target exactly as it was.
            if outside.is_empty() || inside.is_empty() {
                continue;
            }
            let base_name = if target.name.is_empty() {
                "Piece".to_string()
            } else {
                target.name.clone()
            };
            // The remainder goes underneath the region's piece.
            let mut pieces = split_loops(&outside);
            pieces.extend(split_loops(&inside));
            if pieces.len() < 2 {
                continue;
            }
            let count = pieces.len();
            let layers: Vec<Layer> = pieces
                .into_iter()
                .enumerate()
                .map(|(island, component)| {
                    let name = format!("{base_name} {}/{count}", island + 1);
                    cut_piece_layer(self, target, &style, component, name, &excluded)
                })
                .collect();
            replacements.push((path, layers));
        }
        if replacements.is_empty() {
            return None;
        }

        let (targets_changed, paths) = self.apply_cut_replacements(replacements)?;
        Some(CutOutcome {
            paths,
            targets: targets_changed,
        })
    }

    /// Unique, valid targets for a region cut, shallowest first and in
    /// document order. A selected parent already covers its children.
    fn cut_targets(&self, target_paths: &[Vec<usize>]) -> Vec<Vec<usize>> {
        let mut targets: Vec<Vec<usize>> = Vec::new();
        for path in target_paths {
            if path.is_empty() || self.layer_by_path(path).is_none() {
                continue;
            }
            if targets.iter().any(|kept| kept == path) {
                continue;
            }
            if targets
                .iter()
                .any(|kept| path.len() > kept.len() && path.starts_with(kept))
            {
                continue;
            }
            targets.retain(|kept| !(kept.len() > path.len() && kept.starts_with(path)));
            targets.push(path.clone());
        }
        targets.sort();
        targets
    }

    /// Replace each target with its pieces, highest sibling first, and return
    /// the number of changed targets plus the paths of the created layers.
    fn apply_cut_replacements(
        &mut self,
        mut replacements: Vec<(Vec<usize>, Vec<Layer>)>,
    ) -> Option<(usize, Vec<Vec<usize>>)> {
        replacements.sort_by(|a, b| b.0.cmp(&a.0));
        let mut targets_changed = 0;
        let mut created: Vec<LayerId> = Vec::new();
        for (path, layers) in replacements {
            let parent = path[..path.len() - 1].to_vec();
            let index = *path.last()?;
            if self.remove_at(&path).is_none() {
                continue;
            }
            for (step, layer) in layers.into_iter().enumerate() {
                created.push(layer.id);
                self.insert_at(&parent, index + step, layer);
            }
            targets_changed += 1;
        }

        let paths: Vec<Vec<usize>> = created
            .iter()
            .filter_map(|id| self.find_path(*id))
            .collect();
        Some((targets_changed, paths))
    }

    /// Move a layer within its parent. `delta` is -1 (down) or +1 (up).
    pub fn move_at(&mut self, path: &[usize], delta: isize) -> bool {
        let (last, parents) = match path.split_last() {
            Some(parts) => parts,
            None => return false,
        };
        let Some(layers) = self.parent_layers_mut(parents) else {
            return false;
        };
        let target = *last as isize + delta;
        if target < 0 || target >= layers.len() as isize {
            return false;
        }
        layers.swap(*last, target as usize);
        true
    }

    /// True when the stack needs the recursive (groups, clipping masks,
    /// boolean coverage) evaluator instead of the flat streaming path.
    pub fn requires_tree(&self) -> bool {
        self.layers.iter().any(Layer::requires_tree)
    }

    /// True when any layer (including nested group children) uses a boolean
    /// blend. Boolean and cut composition need height fields to decide what
    /// happens to the surface below, even when the caller only asked for
    /// normals.
    fn has_boolean_ops(&self) -> bool {
        fn visit(layers: &[Layer]) -> bool {
            layers.iter().any(|layer| {
                layer.blend.uses_surface()
                    || layer.vector_op.is_some_and(VectorOp::is_cut)
                    || !layer.boolean_operands.is_empty()
                    || matches!(&layer.kind, LayerKind::Group { children } if visit(children))
            })
        }
        visit(&self.layers)
    }

    /// Rough upper bound for caching every visible layer's normal + height +
    /// coverage. Tree results add one coverage grid per cached layer.
    pub fn estimated_cache_bytes(&self) -> u64 {
        let visible = self.visible_layer_count() as u64;
        self.size.x as u64 * self.size.y as u64 * 20 * visible
    }

    /// Rough heap footprint of the document, dominated by full-resolution
    /// grids (painted masks, imported normals, photo and painted height).
    ///
    /// Undo snapshots use this to stay inside a memory budget; small
    /// allocations such as names and vector slack are approximated by the
    /// struct sizes and are not the deciding factor.
    pub fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .layers
                .iter()
                .map(Layer::estimated_bytes)
                .sum::<usize>()
    }

    /// Cached evaluation when the estimated cache fits the budget, otherwise a
    /// streaming evaluation with constant working memory.
    pub fn evaluate_auto(&self, cache: &mut EvalCache, budget_bytes: u64) -> Grid<Vec3> {
        if self.estimated_cache_bytes() > budget_bytes {
            self.evaluate()
        } else {
            self.evaluate_with(cache)
        }
    }

    pub fn evaluate_height_auto(&self, cache: &mut EvalCache, budget_bytes: u64) -> Grid<f32> {
        if self.estimated_cache_bytes() > budget_bytes {
            self.evaluate_height()
        } else {
            self.evaluate_height_with(cache)
        }
    }

    /// Cancellable variant of [`Document::evaluate_auto`].
    pub fn evaluate_auto_cancellable(
        &self,
        cache: &mut EvalCache,
        budget_bytes: u64,
        token: &CancelToken,
    ) -> Option<Grid<Vec3>> {
        if self.estimated_cache_bytes() > budget_bytes {
            self.evaluate_cancellable(token)
        } else {
            self.evaluate_with_cancellable(cache, token)
        }
    }

    /// Cancellable variant of [`Document::evaluate_height_auto`].
    pub fn evaluate_height_auto_cancellable(
        &self,
        cache: &mut EvalCache,
        budget_bytes: u64,
        token: &CancelToken,
    ) -> Option<Grid<f32>> {
        if self.estimated_cache_bytes() > budget_bytes {
            self.evaluate_height_cancellable(token)
        } else {
            self.evaluate_height_with_cancellable(cache, token)
        }
    }

    /// True when a visible layer is an imported normal map.
    ///
    /// Imported normals have no analytic height, so a direct height export
    /// would omit them. The export's Auto height source uses this to decide
    /// when integration is still required.
    pub fn has_imported_normals(&self) -> bool {
        self.has_imported_normals_for_tile(self.active_tile)
    }

    /// Imported-normal check for one UDIM tile, without changing the document.
    pub fn has_imported_normals_for_tile(&self, tile: u32) -> bool {
        fn visit(layers: &[Layer], tile: u32) -> bool {
            layers
                .iter()
                .filter(|layer| layer.visible && layer.applies_to_tile(tile))
                .any(|layer| {
                    matches!(
                        layer.kind,
                        LayerKind::ImportedNormal { .. } | LayerKind::NormalPaint { .. }
                    ) || layer
                        .children()
                        .is_some_and(|children| visit(children, tile))
                })
        }
        visit(&self.layers, tile)
    }

    /// Evaluate the layer stack to a normal field.
    ///
    /// Streaming: layer normals are released after compositing. Shape layers
    /// are evaluated only inside their expanded bounding box.
    /// Evaluate one layer or group in isolation, ignoring the rest of the
    /// stack. Used by 3D projection previews and any operation that needs the
    /// source normal map of a single editable subtree.
    pub fn evaluate_layer_normal(&self, id: LayerId) -> Option<Grid<Vec3>> {
        let mut layer = self.find_layer(id)?.clone();
        layer.visible = true;
        layer.tile = None;
        let isolated = Document {
            size: self.size,
            wrap: self.wrap,
            green: self.green,
            height_scale_px: self.height_scale_px,
            layers: vec![layer],
            active_tile: self.active_tile,
        };
        Some(isolated.evaluate())
    }

    /// Normal and height contribution of one layer with its mask, opacity and
    /// blend neutralized.
    ///
    /// The GPU hybrid path uses this to precompute layers the shader cannot
    /// evaluate analytically (raster sources, smoothed outlines, text, photo
    /// and painted layers), then composites the result with the layer's own
    /// mask and blend parameters in the shader.
    pub fn evaluate_layer_contribution(&self, id: LayerId) -> Option<(Grid<Vec3>, Grid<f32>)> {
        let mut layer = self.find_layer(id)?.clone();
        layer.visible = true;
        layer.hidden_as_mask = false;
        layer.opacity = 1.0;
        layer.blend = BlendMode::Replace;
        layer.mask = None;
        layer.mask_layer = None;
        layer.mask_settings = MaskSettings::default();
        layer.tile = None;
        let isolated = Document {
            size: self.size,
            wrap: self.wrap,
            green: self.green,
            height_scale_px: self.height_scale_px,
            layers: vec![layer],
            active_tile: self.active_tile,
        };
        let normal = isolated.evaluate();
        let height = isolated.evaluate_height();
        Some((normal, height))
    }

    pub fn evaluate(&self) -> Grid<Vec3> {
        self.evaluate_inner(None)
            .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, Vec3::Z))
    }

    /// Cancellable variant of [`Document::evaluate`]. Returns `None` when the
    /// token trips before the stack finishes.
    pub fn evaluate_cancellable(&self, token: &CancelToken) -> Option<Grid<Vec3>> {
        self.evaluate_inner(Some(token))
    }

    fn evaluate_inner(&self, token: Option<&CancelToken>) -> Option<Grid<Vec3>> {
        if is_cancelled(token) {
            return None;
        }
        if self.requires_tree() {
            let result = self
                .evaluate_tree(true, false, self.active_tile)
                .0
                .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, Vec3::Z));
            return (!is_cancelled(token)).then_some(result);
        }
        let mut acc = Grid::new(self.size.x, self.size.y, Vec3::Z);
        for layer in &self.layers {
            if is_cancelled(token) {
                return None;
            }
            if !layer.paints() || !self.layer_is_active(layer) {
                continue;
            }
            let mask = self.flat_mask(layer, BaseSurface::Normal(&acc));
            if let Some((origin, region_n)) = layer.normal_region(self.size, self.wrap) {
                self.composite_normal_region(&mut acc, layer, mask.as_ref(), &region_n, origin);
            } else {
                let layer_n = layer.eval(self.size, self.wrap);
                acc = self.composite_normal(acc, layer, mask.as_ref(), &layer_n);
            }
        }
        Some(acc)
    }

    /// Effective painted + procedural mask for the flat (non-tree) path,
    /// evaluated against the surface composited so far. `None` reveals all.
    fn flat_mask(&self, layer: &Layer, base: BaseSurface<'_>) -> Option<Grid<f32>> {
        let settings = &layer.mask_settings;
        let static_part = static_mask(layer.mask.as_ref(), settings, self.size, self.wrap);
        let smart_part = smart_mask(settings, &base, self.wrap);
        match (static_part, smart_part) {
            (Some(a), Some(b)) => Some(multiply_grids(&a, &b)),
            (a, b) => a.or(b),
        }
    }

    /// Evaluate using a cache; layers whose `revision` is unchanged are not
    /// re-evaluated. Call [`Layer::touch`] after mutating a layer's parameters.
    pub fn evaluate_with(&self, cache: &mut EvalCache) -> Grid<Vec3> {
        self.evaluate_with_inner(cache, None)
            .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, Vec3::Z))
    }

    /// Cancellable variant of [`Document::evaluate_with`].
    pub fn evaluate_with_cancellable(
        &self,
        cache: &mut EvalCache,
        token: &CancelToken,
    ) -> Option<Grid<Vec3>> {
        self.evaluate_with_inner(cache, Some(token))
    }

    fn evaluate_with_inner(
        &self,
        cache: &mut EvalCache,
        token: Option<&CancelToken>,
    ) -> Option<Grid<Vec3>> {
        cache.prepare(self.size, self.wrap, self);
        if is_cancelled(token) {
            return None;
        }
        if self.requires_tree() {
            let result = self
                .evaluate_tree_cached(cache, true, false)
                .0
                .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, Vec3::Z));
            return (!is_cancelled(token)).then_some(result);
        }
        let mut acc = Grid::new(self.size.x, self.size.y, Vec3::Z);

        for layer in &self.layers {
            if is_cancelled(token) {
                return None;
            }
            if !layer.paints() || !layer.applies_to_tile(self.eval_tile(cache)) {
                continue;
            }

            let revision = layer.content_revision();
            let entry = cache
                .entries
                .entry(layer.id)
                .or_insert_with(|| CachedLayer::new(revision));
            if entry.revision != revision {
                entry.revision = revision;
                entry.normal = None;
                entry.height = None;
            }
            if entry.normal.is_none() {
                entry.normal = Some(match layer.normal_region(self.size, self.wrap) {
                    Some((origin, grid)) => CachedNormal::Region { origin, grid },
                    None => CachedNormal::Full(layer.eval(self.size, self.wrap)),
                });
            }
            let mask = self.flat_mask(layer, BaseSurface::Normal(&acc));
            match entry.normal.as_ref().expect("normal was just filled") {
                CachedNormal::Full(grid) => {
                    acc = self.composite_normal(acc, layer, mask.as_ref(), grid);
                }
                CachedNormal::Region { origin, grid } => {
                    self.composite_normal_region(&mut acc, layer, mask.as_ref(), grid, *origin);
                }
            }
        }

        Some(acc)
    }

    fn composite_normal(
        &self,
        acc: Grid<Vec3>,
        layer: &Layer,
        mask: Option<&Grid<f32>>,
        layer_n: &Grid<Vec3>,
    ) -> Grid<Vec3> {
        let w = self.size.x;
        let h = self.size.y;
        Grid::from_fn_par(w, h, |x, y| {
            let m = mask.map(|m| m.get(x, y).clamp(0.0, 1.0)).unwrap_or(1.0);
            let opacity = (layer.opacity * m).clamp(0.0, 1.0);
            blend(*acc.get(x, y), *layer_n.get(x, y), layer.blend, opacity)
        })
    }

    fn composite_normal_region(
        &self,
        acc: &mut Grid<Vec3>,
        layer: &Layer,
        mask: Option<&Grid<f32>>,
        region: &Grid<Vec3>,
        origin: UVec2,
    ) {
        let w = self.size.x as usize;
        let rw = region.w as usize;
        let x0 = origin.x as usize;
        let y0 = origin.y as usize;
        let opacity = layer.opacity;
        let blend_mode = layer.blend;
        acc.data
            .par_chunks_mut(w)
            .enumerate()
            .skip(y0)
            .take(region.h as usize)
            .for_each(|(y, row)| {
                let src_y = y - y0;
                let src = &region.data[src_y * rw..src_y * rw + rw];
                for (x, dst) in row[x0..x0 + rw].iter_mut().enumerate() {
                    let m = mask
                        .map(|m| m.get((x0 + x) as u32, y as u32).clamp(0.0, 1.0))
                        .unwrap_or(1.0);
                    let o = (opacity * m).clamp(0.0, 1.0);
                    *dst = blend(*dst, src[x], blend_mode, o);
                }
            });
    }

    /// Evaluate the height-producing layers. Normal-only layers are skipped;
    /// use [`crate::height::normal_to_height`] when integrating a normal map.
    ///
    /// Streaming: per-layer height grids are released after compositing.
    pub fn evaluate_height(&self) -> Grid<f32> {
        self.evaluate_height_inner(None)
            .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, 0.0))
    }

    /// Cancellable variant of [`Document::evaluate_height`].
    pub fn evaluate_height_cancellable(&self, token: &CancelToken) -> Option<Grid<f32>> {
        self.evaluate_height_inner(Some(token))
    }

    fn evaluate_height_inner(&self, token: Option<&CancelToken>) -> Option<Grid<f32>> {
        if is_cancelled(token) {
            return None;
        }
        if self.requires_tree() {
            let result = self
                .evaluate_tree(false, true, self.active_tile)
                .1
                .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, 0.0));
            return (!is_cancelled(token)).then_some(result);
        }
        let mut acc = Grid::new(self.size.x, self.size.y, 0.0f32);
        for layer in &self.layers {
            if is_cancelled(token) {
                return None;
            }
            if !layer.paints() || !self.layer_is_active(layer) {
                continue;
            }
            let mask = self.flat_mask(layer, BaseSurface::Height(&acc, self.height_scale_px));
            if let Some((origin, region_h)) = layer.height_region(self.size, self.wrap) {
                self.composite_height_region(&mut acc, layer, mask.as_ref(), &region_h, origin);
            } else {
                let Some(layer_h) = layer.eval_height(self.size, self.wrap) else {
                    continue;
                };
                acc = self.composite_height(acc, layer, mask.as_ref(), &layer_h);
            }
        }
        Some(acc)
    }

    /// Cached variant of [`Document::evaluate_height`].
    pub fn evaluate_height_with(&self, cache: &mut EvalCache) -> Grid<f32> {
        self.evaluate_height_with_inner(cache, None)
            .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, 0.0))
    }

    /// Cancellable variant of [`Document::evaluate_height_with`].
    pub fn evaluate_height_with_cancellable(
        &self,
        cache: &mut EvalCache,
        token: &CancelToken,
    ) -> Option<Grid<f32>> {
        self.evaluate_height_with_inner(cache, Some(token))
    }

    fn evaluate_height_with_inner(
        &self,
        cache: &mut EvalCache,
        token: Option<&CancelToken>,
    ) -> Option<Grid<f32>> {
        cache.prepare(self.size, self.wrap, self);
        if is_cancelled(token) {
            return None;
        }
        if self.requires_tree() {
            let result = self
                .evaluate_tree_cached(cache, false, true)
                .1
                .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, 0.0));
            return (!is_cancelled(token)).then_some(result);
        }
        let mut acc = Grid::new(self.size.x, self.size.y, 0.0f32);

        for layer in &self.layers {
            if is_cancelled(token) {
                return None;
            }
            if !layer.paints() || !layer.applies_to_tile(self.eval_tile(cache)) {
                continue;
            }

            let revision = layer.content_revision();
            let entry = cache
                .entries
                .entry(layer.id)
                .or_insert_with(|| CachedLayer::new(revision));
            if entry.revision != revision {
                entry.revision = revision;
                entry.normal = None;
                entry.height = None;
            }
            if entry.height.is_none() {
                entry.height = Some(match layer.height_region(self.size, self.wrap) {
                    Some((origin, grid)) => Some(CachedHeight::Region { origin, grid }),
                    None => layer
                        .eval_height(self.size, self.wrap)
                        .map(CachedHeight::Full),
                });
            }
            let Some(cached) = entry.height.as_ref().and_then(|height| height.as_ref()) else {
                continue;
            };
            let mask = self.flat_mask(layer, BaseSurface::Height(&acc, self.height_scale_px));
            match cached {
                CachedHeight::Full(grid) => {
                    acc = self.composite_height(acc, layer, mask.as_ref(), grid);
                }
                CachedHeight::Region { origin, grid } => {
                    self.composite_height_region(&mut acc, layer, mask.as_ref(), grid, *origin);
                }
            }
        }

        Some(acc)
    }

    /// Material-ID map: the most specific layer or group colour per pixel.
    ///
    /// Coverage follows the same geometry, painted masks and clipping as the
    /// normal map, but ignores opacity, blend modes and smart masks: an ID
    /// describes which material a texel belongs to, not how strongly it is
    /// blended. Painted normal and height layers only cover the texels they
    /// actually painted, so a brush layer cannot flood the whole map. Each pixel
    /// takes the topmost layer whose coverage is at least 0.5, so neighbouring
    /// IDs meet at the same silhouette as the bevel with no colour bleeding
    /// between them.
    ///
    /// Children of a plain group paint after their group, so a child's colour
    /// overrides the group colour where both are set. A live vector compound is
    /// one surface: only its own colour applies.
    pub fn evaluate_id_map(&self, background: [f32; 3]) -> Grid<[f32; 3]> {
        self.evaluate_id_map_for_tile(background, self.active_tile)
    }

    /// Material-ID map for one UDIM tile, without changing the document.
    pub fn evaluate_id_map_for_tile(&self, background: [f32; 3], tile: u32) -> Grid<[f32; 3]> {
        let mut out = Grid::new(self.size.x, self.size.y, background);
        let index = self.layer_index();
        let mut stack = HashSet::new();
        self.paint_id_layers(&self.layers, &index, &mut stack, tile, &mut out);
        out
    }

    fn paint_id_layers(
        &self,
        layers: &[Layer],
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
        out: &mut Grid<[f32; 3]>,
    ) {
        for layer in layers {
            if !layer.paints() || !layer.applies_to_tile(tile) {
                continue;
            }
            if let Some(color) = layer.id_color {
                let coverage = self.id_coverage(layer, index, stack, tile);
                for (dst, alpha) in out.data.iter_mut().zip(coverage.data.iter()) {
                    if *alpha >= 0.5 {
                        *dst = color;
                    }
                }
            }
            if let LayerKind::Group { children } = &layer.kind {
                if layer.vector_op.is_none() {
                    self.paint_id_layers(children, index, stack, tile, out);
                }
            }
        }
    }

    /// Coverage used by the ID map: geometry x painted mask x clipping, with
    /// opacity and smart masks deliberately left out.
    fn id_coverage(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Grid<f32> {
        if !stack.insert(layer.id) {
            return Grid::new(self.size.x, self.size.y, 1.0f32);
        }
        let geometric = self.id_geometric_coverage(layer, index, stack, tile);
        let weight = self.id_mask_weight(layer, index, stack, tile);
        stack.remove(&layer.id);
        multiply_grids(&geometric, &weight)
    }

    fn id_geometric_coverage(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Grid<f32> {
        match &layer.kind {
            LayerKind::Group { children } => {
                if let Some(op) = layer.vector_op {
                    return self.compound_coverage(children, op, index, stack, tile);
                }
                let mut coverage = Grid::new(self.size.x, self.size.y, 0.0f32);
                for child in children {
                    if !child.paints() || !child.applies_to_tile(tile) {
                        continue;
                    }
                    let child_coverage = self.id_coverage(child, index, stack, tile);
                    for (dst, src) in coverage.data.iter_mut().zip(child_coverage.data.iter()) {
                        *dst = dst.max(*src);
                    }
                }
                coverage
            }
            LayerKind::Shape(shape) => shape.mask_coverage(self.size, self.wrap),
            // Painted layers cover the whole canvas as far as the normal stack
            // is concerned, but an ID must follow the pixels that were actually
            // painted: assigning a colour to a brush layer should not flood the
            // whole texture.
            LayerKind::NormalPaint { normal } => painted_normal_coverage(normal),
            LayerKind::HeightPaint { height, .. } => painted_height_coverage(height),
            _ => Grid::new(self.size.x, self.size.y, 1.0f32),
        }
    }

    fn id_mask_weight(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Grid<f32> {
        let mut weight = match static_mask(
            layer.mask.as_ref(),
            &layer.mask_settings,
            self.size,
            self.wrap,
        ) {
            Some(mut mask) => {
                for value in &mut mask.data {
                    *value = value.clamp(0.0, 1.0);
                }
                mask
            }
            None => Grid::new(self.size.x, self.size.y, 1.0f32),
        };
        if let Some(mask_id) = layer.mask_layer {
            if let Some(mask_layer) = index.get(&mask_id) {
                let (_, mask_coverage) = self.coverage_of(mask_layer, index, stack, tile);
                apply_clip(&mut weight, &mask_coverage, &layer.mask_settings, self.wrap);
            }
        }
        weight
    }

    /// True when at least one layer carries an ID colour.
    pub fn has_id_colors(&self) -> bool {
        fn visit(layers: &[Layer], tile: u32) -> bool {
            layers.iter().any(|layer| {
                layer.applies_to_tile(tile)
                    && (layer.id_color.is_some()
                        || layer
                            .children()
                            .is_some_and(|children| visit(children, tile)))
            })
        }
        visit(&self.layers, self.active_tile)
    }

    fn composite_height(
        &self,
        acc: Grid<f32>,
        layer: &Layer,
        mask: Option<&Grid<f32>>,
        layer_h: &Grid<f32>,
    ) -> Grid<f32> {
        let w = self.size.x;
        let h = self.size.y;
        let normalize = if matches!(&layer.kind, LayerKind::Shape(_)) {
            1.0 / self.height_scale_px.max(1e-4)
        } else {
            1.0
        };
        Grid::from_fn_par(w, h, |x, y| {
            let m = mask.map(|m| m.get(x, y).clamp(0.0, 1.0)).unwrap_or(1.0);
            let v = *layer_h.get(x, y) * normalize * (layer.opacity * m).clamp(0.0, 1.0);
            acc.get(x, y).max(v)
        })
    }

    fn composite_height_region(
        &self,
        acc: &mut Grid<f32>,
        layer: &Layer,
        mask: Option<&Grid<f32>>,
        region: &Grid<f32>,
        origin: UVec2,
    ) {
        let w = self.size.x as usize;
        let rw = region.w as usize;
        let x0 = origin.x as usize;
        let y0 = origin.y as usize;
        let normalize = if matches!(&layer.kind, LayerKind::Shape(_)) {
            1.0 / self.height_scale_px.max(1e-4)
        } else {
            1.0
        };
        let opacity = layer.opacity;
        acc.data
            .par_chunks_mut(w)
            .enumerate()
            .skip(y0)
            .take(region.h as usize)
            .for_each(|(y, row)| {
                let src_y = y - y0;
                let src = &region.data[src_y * rw..src_y * rw + rw];
                for (x, dst) in row[x0..x0 + rw].iter_mut().enumerate() {
                    let m = mask
                        .map(|m| m.get((x0 + x) as u32, y as u32).clamp(0.0, 1.0))
                        .unwrap_or(1.0);
                    let v = src[x] * normalize * (opacity * m).clamp(0.0, 1.0);
                    *dst = dst.max(v);
                }
            });
    }
}

/// Recursive compositing accumulator used when groups, clipping masks or
/// boolean coverage are present.
struct TreeAccum {
    normal: Option<Grid<Vec3>>,
    height: Option<Grid<f32>>,
    coverage: Grid<f32>,
}

impl TreeAccum {
    fn new(size: UVec2, want_normal: bool, want_height: bool) -> Self {
        Self {
            normal: want_normal.then(|| Grid::new(size.x, size.y, Vec3::Z)),
            height: want_height.then(|| Grid::new(size.x, size.y, 0.0f32)),
            coverage: Grid::new(size.x, size.y, 0.0f32),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn composite(
        &mut self,
        layer: &Layer,
        layer_normal: Option<&Grid<Vec3>>,
        layer_height: Option<&Grid<f32>>,
        layer_alpha: &Grid<f32>,
        layer_coverage: &Grid<f32>,
        height_scale: f32,
        wrap: Wrap,
    ) {
        let w = self.coverage.w as usize;
        let mode = layer.blend;
        let normalize = height_normalize(layer, height_scale);

        // Smart masks read the surface accumulated so far in this list, so
        // they are applied here rather than cached with the layer's alpha.
        let smart = if layer.mask_settings.needs_base() {
            let base = match (&self.normal, &self.height) {
                (Some(normal), _) => Some(BaseSurface::Normal(normal)),
                (None, Some(height)) => Some(BaseSurface::Height(height, height_scale)),
                (None, None) => None,
            };
            base.and_then(|base| smart_mask(&layer.mask_settings, &base, wrap))
        } else {
            None
        };
        let masked;
        let (layer_alpha, layer_coverage) = match &smart {
            Some(smart) => {
                masked = (
                    multiply_grids(layer_alpha, smart),
                    multiply_grids(layer_coverage, smart),
                );
                (&masked.0, &masked.1)
            }
            None => (layer_alpha, layer_coverage),
        };

        if mode.uses_surface() {
            if mode.is_cut() {
                self.composite_cut(
                    mode,
                    layer,
                    layer_normal,
                    layer_height,
                    layer_coverage,
                    normalize,
                    height_scale,
                );
            } else {
                self.composite_boolean(
                    mode,
                    layer_normal,
                    layer_height,
                    layer_coverage,
                    normalize,
                    height_scale,
                );
            }
            return;
        }

        if let (Some(acc), Some(src)) = (self.normal.as_mut(), layer_normal) {
            acc.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                let alpha_row = &layer_alpha.data[y * w..y * w + w];
                let src_row = &src.data[y * w..y * w + w];
                for x in 0..w {
                    row[x] = blend(row[x], src_row[x], mode, alpha_row[x].clamp(0.0, 1.0));
                }
            });
        }

        if let (Some(acc), Some(src)) = (self.height.as_mut(), layer_height) {
            acc.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                let alpha_row = &layer_alpha.data[y * w..y * w + w];
                let src_row = &src.data[y * w..y * w + w];
                for x in 0..w {
                    let value = src_row[x] * normalize * alpha_row[x].clamp(0.0, 1.0);
                    row[x] = row[x].max(value);
                }
            });
        }

        self.coverage
            .data
            .par_chunks_mut(w)
            .enumerate()
            .for_each(|(y, row)| {
                let layer_row = &layer_coverage.data[y * w..y * w + w];
                for (x, value) in row.iter_mut().enumerate() {
                    *value = value.clamp(0.0, 1.0).max(layer_row[x].clamp(0.0, 1.0));
                }
            });
    }

    /// Cut composition for the partial cut, partial slash and clip divide
    /// combine modes.
    ///
    /// The layer is an ordinary shape layer and its own bevel is the cutter.
    /// A partial cut subtracts that height profile from the surface below and
    /// never drops under the base; a slash fades the cut along the layer's
    /// long axis; a clip divide scales the base to zero across the layer's
    /// band so the band's bevel becomes the two seam walls.
    #[allow(clippy::too_many_arguments)]
    fn composite_cut(
        &mut self,
        mode: BlendMode,
        layer: &Layer,
        layer_normal: Option<&Grid<Vec3>>,
        layer_height: Option<&Grid<f32>>,
        layer_coverage: &Grid<f32>,
        normalize: f32,
        height_scale: f32,
    ) {
        let Some(layer_height) = layer_height else {
            return;
        };
        let shape = match &layer.kind {
            LayerKind::Shape(shape) => Some(shape),
            _ => None,
        };
        let (taper_in, taper_out, step_px) = shape
            .map(|shape| (shape.cut_taper_in, shape.cut_taper_out, shape.cut_step_px))
            .unwrap_or((0.0, 0.0, 0.0));
        // The principal axes are only needed when a slash tapers or a divide
        // steps; both are optional, so the common cut skips the moment pass.
        let axis = if (mode == BlendMode::PartialSlash && (taper_in > 0.001 || taper_out > 0.001))
            || (mode == BlendMode::ClipDivide && step_px.abs() > 0.001)
        {
            CutAxis::from_coverage(layer_coverage)
        } else {
            None
        };
        // A divide cuts through regardless of the layer's authored depth: the
        // layer's own profile is normalised to 0..1 across the band.
        let plateau = if mode == BlendMode::ClipDivide {
            layer_height
                .data
                .iter()
                .copied()
                .fold(0.0f32, f32::max)
                .max(1e-4)
        } else {
            1.0
        };
        let step = step_px / height_scale.max(1e-4);
        let w = self.coverage.w as usize;
        let Self {
            normal,
            height,
            coverage,
        } = self;
        let Some(height) = height.as_mut() else {
            return;
        };

        if let (Some(normal), Some(layer_normal)) = (normal.as_mut(), layer_normal) {
            height
                .data
                .par_chunks_mut(w)
                .zip(normal.data.par_chunks_mut(w))
                .enumerate()
                .for_each(|(y, (height_row, normal_row))| {
                    let base_cov_row = &coverage.data[y * w..y * w + w];
                    let layer_cov_row = &layer_coverage.data[y * w..y * w + w];
                    let layer_h_row = &layer_height.data[y * w..y * w + w];
                    let layer_n_row = &layer_normal.data[y * w..y * w + w];
                    for x in 0..w {
                        let layer_cov = layer_cov_row[x].clamp(0.0, 1.0);
                        let base_cov = base_cov_row[x].clamp(0.0, 1.0);
                        let base = height_row[x].max(0.0);
                        if base_cov <= 0.0 {
                            continue;
                        }
                        let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                        let (taper, side) = match axis {
                            Some(axis) => (axis.taper(p, taper_in, taper_out), axis.side(p)),
                            None => (1.0, 0.0),
                        };
                        match mode {
                            BlendMode::PartialCut | BlendMode::PartialSlash => {
                                if layer_cov <= 0.0 {
                                    continue;
                                }
                                if base > 0.0 {
                                    normal_row[x] = subtract_normals(
                                        normal_row[x],
                                        layer_n_row[x],
                                        taper * layer_cov * base_cov,
                                    );
                                }
                                height_row[x] = (base
                                    - layer_h_row[x] * normalize * layer_cov * taper)
                                    .max(0.0);
                            }
                            BlendMode::ClipDivide => {
                                let inside = (layer_h_row[x] / plateau * layer_cov).clamp(0.0, 1.0);
                                if inside > 0.0 {
                                    let level = base + step * side;
                                    // new = (1 - inside) * level, so its slope
                                    // is (1 - inside) * slope(base) minus the
                                    // scaled gradient of the band profile. The
                                    // step is part of the level, which gives
                                    // the two gap edges slightly different
                                    // slopes: the visual step between plates.
                                    let s = slope(normal_row[x]) * (1.0 - inside)
                                        - slope(layer_n_row[x])
                                            * (level * height_scale / plateau)
                                            * layer_cov;
                                    normal_row[x] = Vec3::new(s.x, s.y, 1.0).normalize();
                                }
                                height_row[x] = ((base + step * side) * (1.0 - inside)).max(0.0);
                            }
                            _ => {}
                        }
                    }
                });
            return;
        }

        height
            .data
            .par_chunks_mut(w)
            .enumerate()
            .for_each(|(y, height_row)| {
                let base_cov_row = &coverage.data[y * w..y * w + w];
                let layer_cov_row = &layer_coverage.data[y * w..y * w + w];
                let layer_h_row = &layer_height.data[y * w..y * w + w];
                for x in 0..w {
                    let layer_cov = layer_cov_row[x].clamp(0.0, 1.0);
                    let base_cov = base_cov_row[x].clamp(0.0, 1.0);
                    let base = height_row[x].max(0.0);
                    if base_cov <= 0.0 {
                        continue;
                    }
                    let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let (taper, side) = match axis {
                        Some(axis) => (axis.taper(p, taper_in, taper_out), axis.side(p)),
                        None => (1.0, 0.0),
                    };
                    match mode {
                        BlendMode::PartialCut | BlendMode::PartialSlash => {
                            if layer_cov <= 0.0 {
                                continue;
                            }
                            height_row[x] =
                                (base - layer_h_row[x] * normalize * layer_cov * taper).max(0.0);
                        }
                        BlendMode::ClipDivide => {
                            let inside = (layer_h_row[x] / plateau * layer_cov).clamp(0.0, 1.0);
                            height_row[x] = ((base + step * side) * (1.0 - inside)).max(0.0);
                        }
                        _ => {}
                    }
                }
            });
    }

    /// Height-aware boolean composition.
    ///
    /// A union keeps the taller surface where the shapes overlap, an intersect
    /// keeps the shorter one where both cover, and a difference draws the
    /// layer's surface over the base so its full contour cuts into the lower
    /// shape. Union ties prefer the base surface, so a layer bevel that exactly
    /// reaches the base plateau does not leave a ghost outline inside the
    /// merged silhouette.
    #[allow(clippy::too_many_arguments)]
    fn composite_boolean(
        &mut self,
        mode: BlendMode,
        layer_normal: Option<&Grid<Vec3>>,
        layer_height: Option<&Grid<f32>>,
        layer_coverage: &Grid<f32>,
        normalize: f32,
        height_scale: f32,
    ) {
        let Self {
            normal,
            height,
            coverage,
        } = self;
        let w = coverage.w as usize;
        let eps = (1.0 / height_scale.max(1.0)).clamp(1.0e-6, 0.25);
        // Union must be close to a hard max(). A wide blend lets the lower
        // surface's still-tilted bevel top bleed through the taller plateau and
        // leaves a ghost outline inside the merged silhouette.
        let union_eps = (eps * UNION_EPS_SCALE).max(1.0e-7);

        if let (Some(acc), Some(src)) = (normal.as_mut(), layer_normal) {
            let base_height = height.as_ref();
            let base_coverage = &*coverage;
            acc.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                let base_cov_row = &base_coverage.data[y * w..y * w + w];
                let layer_cov_row = &layer_coverage.data[y * w..y * w + w];
                let src_row = &src.data[y * w..y * w + w];
                let base_h_row = base_height.map(|grid| &grid.data[y * w..y * w + w]);
                for x in 0..w {
                    let base_cov = base_cov_row[x].clamp(0.0, 1.0);
                    let layer_cov = layer_cov_row[x].clamp(0.0, 1.0);
                    let base_h = base_h_row.map(|row| row[x].max(0.0)).unwrap_or(0.0);
                    let layer_h = layer_height
                        .map(|grid| {
                            grid.get(x as u32, y as u32)
                                * normalize
                                * boolean_alpha(mode, base_cov, layer_cov)
                        })
                        .unwrap_or(0.0)
                        .max(0.0);
                    row[x] = match mode {
                        BlendMode::BooleanUnion => {
                            if layer_cov <= 0.0 {
                                row[x]
                            } else if base_cov <= 0.0 {
                                src_row[x]
                            } else {
                                mix_by_height_up(row[x], src_row[x], layer_h - base_h, union_eps)
                            }
                        }
                        BlendMode::BooleanIntersect => {
                            if base_cov <= 0.0 || layer_cov <= 0.0 {
                                Vec3::Z
                            } else {
                                mix_by_height(row[x], src_row[x], base_h - layer_h, eps)
                            }
                        }
                        BlendMode::BooleanDifference => {
                            if layer_cov <= 0.0 {
                                row[x]
                            } else if base_cov <= 0.0 {
                                src_row[x]
                            } else {
                                // The layer's surface is drawn over the base, so
                                // its full bevel cuts into the lower shape while
                                // the layer itself stays visible (the reference
                                // Difference result). Coverage is the union.
                                row[x].lerp(src_row[x], layer_cov).normalize_or_zero()
                            }
                        }
                        _ => unreachable!("non-boolean mode in composite_boolean"),
                    };
                }
            });
        }

        if let Some(acc) = height.as_mut() {
            acc.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                let base_cov_row = &coverage.data[y * w..y * w + w];
                let layer_cov_row = &layer_coverage.data[y * w..y * w + w];
                for x in 0..w {
                    let base_cov = base_cov_row[x].clamp(0.0, 1.0);
                    let layer_cov = layer_cov_row[x].clamp(0.0, 1.0);
                    let base_h = row[x].max(0.0);
                    let layer_h = layer_height
                        .map(|grid| {
                            grid.get(x as u32, y as u32)
                                * normalize
                                * boolean_alpha(mode, base_cov, layer_cov)
                        })
                        .unwrap_or(0.0)
                        .max(0.0);
                    row[x] = match mode {
                        BlendMode::BooleanUnion => {
                            if base_cov <= 0.0 && layer_cov <= 0.0 {
                                0.0
                            } else {
                                base_h.max(layer_h)
                            }
                        }
                        BlendMode::BooleanIntersect => {
                            if base_cov <= 0.0 || layer_cov <= 0.0 {
                                0.0
                            } else {
                                base_h.min(layer_h)
                            }
                        }
                        BlendMode::BooleanDifference => {
                            if layer_cov <= 0.0 {
                                base_h
                            } else {
                                base_h.max(layer_h)
                            }
                        }
                        _ => unreachable!("non-boolean mode in composite_boolean"),
                    };
                }
            });
        }

        coverage
            .data
            .par_chunks_mut(w)
            .enumerate()
            .for_each(|(y, row)| {
                let layer_cov_row = &layer_coverage.data[y * w..y * w + w];
                for x in 0..w {
                    let base_cov = row[x].clamp(0.0, 1.0);
                    let layer_cov = layer_cov_row[x].clamp(0.0, 1.0);
                    row[x] = match mode {
                        BlendMode::BooleanUnion => base_cov.max(layer_cov),
                        BlendMode::BooleanIntersect => base_cov * layer_cov,
                        BlendMode::BooleanDifference => base_cov.max(layer_cov),
                        _ => unreachable!("non-boolean mode in composite_boolean"),
                    };
                }
            });
    }
}

/// Principal axes of a cut layer's coverage.
///
/// The long axis drives a slash taper and the short axis picks the two sides
/// of a clip divide. Deriving them from the coverage instead of the shape type
/// keeps rotated paths and stroked lines working without special cases.
#[derive(Clone, Copy)]
struct CutAxis {
    centroid: Vec2,
    major: Vec2,
    minor: Vec2,
    half_length: f32,
}

impl CutAxis {
    fn from_coverage(coverage: &Grid<f32>) -> Option<Self> {
        let w = coverage.w as usize;
        let (mut weight, mut sx, mut sy, mut sxx, mut sxy, mut syy) =
            (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for (index, value) in coverage.data.iter().enumerate() {
            let value = value.clamp(0.0, 1.0);
            if value <= 1e-4 {
                continue;
            }
            let x = (index % w) as f32 + 0.5;
            let y = (index / w) as f32 + 0.5;
            weight += value;
            sx += value * x;
            sy += value * y;
            sxx += value * x * x;
            sxy += value * x * y;
            syy += value * y * y;
        }
        if weight <= 1e-3 {
            return None;
        }
        let centroid = Vec2::new(sx / weight, sy / weight);
        let cxx = (sxx / weight - centroid.x * centroid.x).max(0.0);
        let cyy = (syy / weight - centroid.y * centroid.y).max(0.0);
        let cxy = sxy / weight - centroid.x * centroid.y;
        let angle = 0.5 * (2.0 * cxy).atan2(cxx - cyy);
        let major = Vec2::new(angle.cos(), angle.sin());
        let minor = Vec2::new(-major.y, major.x);
        let mut half_length = 0.0f32;
        for (index, value) in coverage.data.iter().enumerate() {
            if value.clamp(0.0, 1.0) <= 1e-4 {
                continue;
            }
            let point = Vec2::new((index % w) as f32 + 0.5, (index / w) as f32 + 0.5) - centroid;
            half_length = half_length.max(point.dot(major).abs());
        }
        (half_length > 1e-3).then_some(Self {
            centroid,
            major,
            minor,
            half_length,
        })
    }

    /// Taper multiplier along the long axis. `u` is 0 at one end and 1 at the
    /// other; both fractions are clamped to half the length.
    fn taper(&self, point: Vec2, taper_in: f32, taper_out: f32) -> f32 {
        let projection = (point - self.centroid).dot(self.major) / self.half_length;
        let u = (0.5 + 0.5 * projection.clamp(-1.0, 1.0)).clamp(0.0, 1.0);
        cut_smoothstep(0.0, taper_in.min(0.5), u) * cut_smoothstep(0.0, taper_out.min(0.5), 1.0 - u)
    }

    /// 0 on one side of the seam and 1 on the other, with a two-pixel ramp so
    /// the step does not alias inside the gap.
    fn side(&self, point: Vec2) -> f32 {
        cut_smoothstep(-1.0, 1.0, (point - self.centroid).dot(self.minor))
    }
}

fn cut_smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 {
        return if value >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Fraction of the generic boolean height blend used for union surface
/// selection (about 0.05 px of height).
const UNION_EPS_SCALE: f32 = 0.05;

/// Multiply a layer's alpha by its clipping source's coverage, transformed by
/// the mask settings (invert, grow/shrink, feather).
fn apply_clip(alpha: &mut Grid<f32>, clip: &Grid<f32>, settings: &MaskSettings, wrap: Wrap) {
    let clip = clip_coverage(
        clip,
        settings.invert_clip,
        settings.clip_offset_px,
        settings.clip_feather_px,
        wrap,
    );
    for (dst, src) in alpha.data.iter_mut().zip(clip.data.iter()) {
        *dst *= src.clamp(0.0, 1.0);
    }
}

fn height_normalize(layer: &Layer, height_scale: f32) -> f32 {
    if matches!(layer.kind, LayerKind::Shape(_)) || layer.is_vector_compound() {
        1.0 / height_scale.max(1e-4)
    } else {
        1.0
    }
}

fn mix_by_height(a: Vec3, b: Vec3, difference: f32, eps: f32) -> Vec3 {
    let t = (difference / eps.max(1.0e-6) * 0.5 + 0.5).clamp(0.0, 1.0);
    let n = a.lerp(b, t);
    if n.length_squared() > 1.0e-12 {
        n.normalize()
    } else {
        a
    }
}

/// Union surface selection. Unlike the symmetric [`mix_by_height`], ties keep
/// the base: a layer bevel that exactly reaches the base plateau must not show
/// as a ghost outline inside the merged silhouette.
fn mix_by_height_up(a: Vec3, b: Vec3, difference: f32, eps: f32) -> Vec3 {
    let t = (difference / eps.max(1.0e-6)).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    let n = a.lerp(b, t);
    if n.length_squared() > 1.0e-12 {
        n.normalize()
    } else {
        a
    }
}

fn boolean_alpha(mode: BlendMode, base_coverage: f32, layer_coverage: f32) -> f32 {
    match mode {
        BlendMode::BooleanIntersect => (base_coverage * layer_coverage).clamp(0.0, 1.0),
        // Union and Difference both keep the layer's height wherever it covers;
        // Difference only draws that surface over the base instead of merging
        // silhouettes.
        _ => layer_coverage.clamp(0.0, 1.0),
    }
}

#[derive(Clone, Default)]
struct ArcLayerEval {
    normal: Option<Arc<Grid<Vec3>>>,
    height: Option<Arc<Grid<f32>>>,
    alpha: Option<Arc<Grid<f32>>>,
    coverage: Option<Arc<Grid<f32>>>,
}

type CoveragePair = (Arc<Grid<f32>>, Arc<Grid<f32>>);
type OperandSurface = (Option<Grid<Vec3>>, Option<Grid<f32>>, Grid<f32>);
type CachedOperandSurface = (
    Option<Arc<Grid<Vec3>>>,
    Option<Arc<Grid<f32>>>,
    Arc<Grid<f32>>,
);

impl Document {
    #[allow(clippy::too_many_arguments)]
    fn eval_tree_list(
        &self,
        layers: &[Layer],
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        want_normal: bool,
        want_height: bool,
        tile: u32,
        acc: &mut TreeAccum,
    ) {
        for layer in layers {
            if !layer.paints() || !layer.applies_to_tile(tile) {
                continue;
            }
            let (layer_alpha, layer_coverage) = self.coverage_of(layer, index, stack, tile);
            let (normal, height, coverage) = match &layer.kind {
                LayerKind::Group { children } => {
                    if let Some(op) = layer.vector_op {
                        if op.is_cut() {
                            // Cut groups work on the base's height surface, not
                            // on a combined outline.
                            let geometric = (!layer.boolean_operands.is_empty())
                                .then(|| self.compound_cut_coverage(children, index, stack, tile));
                            let (normal, height) = self.compound_cut_surface_for(
                                children,
                                op,
                                index,
                                stack,
                                tile,
                                want_normal,
                                want_height,
                            );
                            if layer.boolean_operands.is_empty() {
                                (normal, height, layer_coverage)
                            } else {
                                let (normal, height, geometric) = self.apply_boolean_operands(
                                    layer,
                                    normal,
                                    height,
                                    geometric.unwrap_or_else(|| {
                                        Grid::new(self.size.x, self.size.y, 0.0f32)
                                    }),
                                    index,
                                    stack,
                                    want_normal,
                                    want_height,
                                    tile,
                                );
                                (normal, height, multiply_grids(&geometric, &layer_alpha))
                            }
                        } else {
                            let regions = self.compound_regions(children, op, index, stack, tile);
                            // Only needed for extra boolean operands; the surface
                            // rasterizes the single outline itself otherwise.
                            let geometric = (!layer.boolean_operands.is_empty())
                                .then(|| regions_coverage(self.size, &regions));
                            let (normal, height) = self.compound_surface_for(
                                children,
                                &regions,
                                geometric.as_ref(),
                                want_normal,
                                want_height,
                            );
                            if layer.boolean_operands.is_empty() {
                                (normal, height, layer_coverage)
                            } else {
                                let (normal, height, geometric) = self.apply_boolean_operands(
                                    layer,
                                    normal,
                                    height,
                                    geometric.unwrap_or_else(|| {
                                        Grid::new(self.size.x, self.size.y, 0.0f32)
                                    }),
                                    index,
                                    stack,
                                    want_normal,
                                    want_height,
                                    tile,
                                );
                                (normal, height, multiply_grids(&geometric, &layer_alpha))
                            }
                        }
                    } else {
                        let mut child = TreeAccum::new(self.size, want_normal, want_height);
                        self.eval_tree_list(
                            children,
                            index,
                            stack,
                            want_normal,
                            want_height,
                            tile,
                            &mut child,
                        );
                        if layer.boolean_operands.is_empty() {
                            (child.normal, child.height, layer_coverage)
                        } else {
                            let geometric = self.geometric_coverage(layer, index, stack, tile);
                            let (normal, height, geometric) = self.apply_boolean_operands(
                                layer,
                                child.normal,
                                child.height,
                                geometric,
                                index,
                                stack,
                                want_normal,
                                want_height,
                                tile,
                            );
                            (normal, height, multiply_grids(&geometric, &layer_alpha))
                        }
                    }
                }
                _ => {
                    let normal = want_normal.then(|| layer.eval(self.size, self.wrap));
                    let height = if want_height {
                        layer.eval_height(self.size, self.wrap)
                    } else {
                        None
                    };
                    if layer.boolean_operands.is_empty() {
                        (normal, height, layer_coverage)
                    } else {
                        let geometric = self.geometric_coverage(layer, index, stack, tile);
                        let (normal, height, geometric) = self.apply_boolean_operands(
                            layer,
                            normal,
                            height,
                            geometric,
                            index,
                            stack,
                            want_normal,
                            want_height,
                            tile,
                        );
                        (normal, height, multiply_grids(&geometric, &layer_alpha))
                    }
                }
            };
            acc.composite(
                layer,
                normal.as_ref(),
                height.as_ref(),
                &layer_alpha,
                &coverage,
                self.height_scale_px,
                self.wrap,
            );
        }
    }

    /// Returns `(alpha, coverage)` for a layer or group. `alpha` is the
    /// opacity/mask/clipping factor used by ordinary blends; `coverage` also
    /// includes the geometric shape coverage used by boolean operations and
    /// clipping-mask lookups. Visibility is deliberately ignored here: a
    /// hidden shape can still act as a mask without rendering.
    fn coverage_of(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> (Grid<f32>, Grid<f32>) {
        if !stack.insert(layer.id) {
            let full = Grid::new(self.size.x, self.size.y, 1.0f32);
            return (full.clone(), full);
        }

        let geometric = self.geometric_coverage_unchecked(layer, index, stack, tile);
        let alpha = self.layer_alpha(layer, index, stack, tile);
        let coverage = multiply_grids(&geometric, &alpha);
        stack.remove(&layer.id);
        (alpha, coverage)
    }

    /// Geometry-only coverage, before opacity, painted masks and clipping.
    fn geometric_coverage(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Grid<f32> {
        if !stack.insert(layer.id) {
            return Grid::new(self.size.x, self.size.y, 1.0f32);
        }
        let coverage = self.geometric_coverage_unchecked(layer, index, stack, tile);
        stack.remove(&layer.id);
        coverage
    }

    /// Assumes the caller already inserted `layer.id` into the recursion guard.
    fn geometric_coverage_unchecked(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Grid<f32> {
        match &layer.kind {
            LayerKind::Group { children } => {
                if let Some(op) = layer.vector_op {
                    return self.compound_coverage(children, op, index, stack, tile);
                }
                let mut coverage = Grid::new(self.size.x, self.size.y, 0.0f32);
                for child in children {
                    if !child.paints() || !child.applies_to_tile(tile) {
                        continue;
                    }
                    let (_, child_coverage) = self.coverage_of(child, index, stack, tile);
                    for (dst, src) in coverage.data.iter_mut().zip(child_coverage.data.iter()) {
                        *dst = dst.max(*src);
                    }
                }
                coverage
            }
            LayerKind::Shape(shape) => shape.mask_coverage(self.size, self.wrap),
            _ => Grid::new(self.size.x, self.size.y, 1.0f32),
        }
    }

    /// Vector outline of one boolean operand. Layer opacity, blend mode,
    /// painted masks and clipping are deliberately ignored: the boolean
    /// combines the actual shape geometry. The group's own opacity and mask
    /// are applied to the finished result.
    fn operand_loops(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
    ) -> Option<VectorOperand> {
        if !stack.insert(layer.id) {
            return None;
        }
        let operand = match &layer.kind {
            LayerKind::Shape(shape) => shape.boolean_operand(self.size),
            LayerKind::Group { children } => self.group_operand(layer, children, index, stack),
            _ => None,
        };
        stack.remove(&layer.id);
        operand
    }

    /// A group used as one operand: its own boolean result when it is a
    /// boolean group, otherwise the union of its visible children.
    fn group_operand(
        &self,
        layer: &Layer,
        children: &[Layer],
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
    ) -> Option<VectorOperand> {
        let operands: Vec<VectorOperand> = children
            .iter()
            .filter(|child| child.visible)
            .filter_map(|child| self.operand_loops(child, index, stack))
            .collect();
        let loops = match layer.vector_op {
            Some(op) => self.compound_loops(op, &operands),
            None => union_loops(&operands),
        };
        Some(VectorOperand::new(loops, FillRule::EvenOdd))
    }

    /// Boolean result of a set of operands, as one loop set.
    fn compound_loops(&self, op: VectorOp, operands: &[VectorOperand]) -> Vec<Vec<Vec2>> {
        let Some(first) = operands.first() else {
            return Vec::new();
        };
        if operands.len() == 1 {
            return first.loops.clone();
        }
        // Divide's coverage is the union; the normal pass splits the regions.
        let ops = match op.path_boolean() {
            Some(op) => vec![op; operands.len() - 1],
            None => vec![PathBoolean::Add; operands.len() - 1],
        };
        boolean_loops(operands, &ops)
    }

    /// Visible operands of a boolean group, bottom of the stack first.
    fn compound_operands(
        &self,
        children: &[Layer],
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Vec<VectorOperand> {
        children
            .iter()
            .filter(|child| child.paints() && child.applies_to_tile(tile))
            .filter_map(|child| self.operand_loops(child, index, stack))
            .filter(|operand| !operand.is_empty())
            .collect()
    }

    /// Vector result of a boolean group: one filled path per output region.
    ///
    /// Add, Subtract, Intersect and XOR produce a single combined outline.
    /// Divide also produces a single outline here because its region split is
    /// only needed by the bevel pass; use [`Self::compound_regions`] for that.
    fn compound_regions(
        &self,
        children: &[Layer],
        op: VectorOp,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Vec<Path> {
        if op.is_cut() {
            // Cut groups work on height fields; there is no combined outline.
            return Vec::new();
        }
        let operands = self.compound_operands(children, index, stack, tile);
        let Some(first) = operands.first() else {
            return Vec::new();
        };
        if operands.len() == 1 {
            return vec![Path::from_loops(first.loops.clone(), FillRule::EvenOdd)];
        }
        if op.is_divide() && operands.len() > MAX_DIVIDE_OPERANDS {
            // The full arrangement grows with 2^operands. Loaded projects that
            // exceed the limit fall back to a merged outline so evaluation
            // stays bounded.
            return vec![Path::from_loops(union_loops(&operands), FillRule::EvenOdd)];
        }
        if op.is_divide() {
            return divide_loops(&operands)
                .into_iter()
                .map(|loops| Path::from_loops(loops, FillRule::EvenOdd))
                .collect();
        }
        vec![Path::from_loops(
            self.compound_loops(op, &operands),
            FillRule::EvenOdd,
        )]
    }

    /// Combined coverage of a boolean group: the union of its regions.
    fn compound_coverage(
        &self,
        children: &[Layer],
        op: VectorOp,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Grid<f32> {
        if op.is_cut() {
            return self.compound_cut_coverage(children, index, stack, tile);
        }
        regions_coverage(
            self.size,
            &self.compound_regions(children, op, index, stack, tile),
        )
    }

    /// Bevel a boolean group's regions with the base shape's style.
    ///
    /// The result is built from the combined vector outline, so internal
    /// overlaps leave no seam and every cut becomes a real bevel edge. Divide
    /// bevels each region on its own and merges the slopes.
    fn compound_surface(
        &self,
        base: &ShapeLayer,
        regions: &[Path],
        coverage: Option<&Grid<f32>>,
        want_normal: bool,
        want_height: bool,
    ) -> (Option<Grid<Vec3>>, Option<Grid<f32>>) {
        if !want_normal && !want_height {
            return (None, None);
        }
        if regions.is_empty() {
            return (
                want_normal.then(|| Grid::new(self.size.x, self.size.y, Vec3::Z)),
                want_height.then(|| Grid::new(self.size.x, self.size.y, 0.0f32)),
            );
        }
        if regions.len() == 1 {
            // The caller already rasterizes the combined outline when it also
            // feeds extra boolean operands; reuse it instead of rasterizing
            // the same path twice.
            let single;
            let coverage = match coverage {
                Some(grid) => grid,
                None => {
                    single =
                        ShapeSource::Path(regions[0].clone()).rasterize(self.size.x, self.size.y);
                    &single
                }
            };
            return (
                want_normal.then(|| base.eval_coverage(coverage, self.wrap)),
                want_height.then(|| base.height_coverage(coverage, self.wrap)),
            );
        }

        let mut normal_acc = want_normal.then(|| Grid::new(self.size.x, self.size.y, Vec3::ZERO));
        let mut height_acc = want_height.then(|| Grid::new(self.size.x, self.size.y, 0.0f32));
        let w = self.size.x as usize;
        for region in regions {
            let coverage = ShapeSource::Path(region.clone()).rasterize(self.size.x, self.size.y);
            if let Some(acc) = normal_acc.as_mut() {
                let normal = base.eval_coverage(&coverage, self.wrap);
                acc.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                    let normal_row = &normal.data[y * w..y * w + w];
                    let piece_row = &coverage.data[y * w..y * w + w];
                    for x in 0..w {
                        let cov = piece_row[x].clamp(0.0, 1.0);
                        if cov <= 0.0 {
                            continue;
                        }
                        let normal = normal_row[x];
                        if normal.z > 1.0e-4 {
                            row[x].x += normal.x / normal.z * cov;
                            row[x].y += normal.y / normal.z * cov;
                        }
                        row[x].z += cov;
                    }
                });
            }
            if let Some(acc) = height_acc.as_mut() {
                let height = base.height_coverage(&coverage, self.wrap);
                acc.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                    let height_row = &height.data[y * w..y * w + w];
                    for x in 0..w {
                        row[x] = row[x].max(height_row[x]);
                    }
                });
            }
        }
        if let Some(acc) = normal_acc.as_mut() {
            for value in &mut acc.data {
                *value = if value.z > 1.0e-5 {
                    Vec3::new(value.x / value.z, value.y / value.z, 1.0).normalize()
                } else {
                    Vec3::Z
                };
            }
        }
        (normal_acc, height_acc)
    }

    /// Bevel a boolean group's regions with the bottom-most operand's style.
    fn compound_surface_for(
        &self,
        children: &[Layer],
        regions: &[Path],
        coverage: Option<&Grid<f32>>,
        want_normal: bool,
        want_height: bool,
    ) -> (Option<Grid<Vec3>>, Option<Grid<f32>>) {
        let Some(base) = compound_base_shape(children) else {
            return (
                want_normal.then(|| Grid::new(self.size.x, self.size.y, Vec3::Z)),
                want_height.then(|| Grid::new(self.size.x, self.size.y, 0.0f32)),
            );
        };
        self.compound_surface(base, regions, coverage, want_normal, want_height)
    }

    /// Evaluate one operand as cut input. Heights are normalized like the tree
    /// accumulator, and coverage is geometric so child opacity and masks do
    /// not change the boolean footprint (matching the outline operations).
    fn cut_surface(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
        want_normal: bool,
        want_height: bool,
    ) -> (Option<Grid<Vec3>>, Option<Grid<f32>>, Grid<f32>) {
        let coverage = self.geometric_coverage(layer, index, stack, tile);
        if let LayerKind::Group { children } = &layer.kind {
            if let Some(op) = layer.vector_op {
                // Vector compounds return raw height units; cut math works in
                // normalized units, so scale the result once here.
                let (normal, height) = if op.is_cut() {
                    self.compound_cut_surface_for(
                        children,
                        op,
                        index,
                        stack,
                        tile,
                        want_normal,
                        want_height,
                    )
                } else {
                    let regions = self.compound_regions(children, op, index, stack, tile);
                    let region_coverage = regions_coverage(self.size, &regions);
                    self.compound_surface_for(
                        children,
                        &regions,
                        Some(&region_coverage),
                        want_normal,
                        want_height,
                    )
                };
                let normalize = height_normalize(layer, self.height_scale_px);
                let height = height.map(|mut grid| {
                    for value in &mut grid.data {
                        *value = (*value * normalize).max(0.0);
                    }
                    grid
                });
                (normal, height, coverage)
            } else {
                // A plain group's own tree pass already normalizes its height,
                // so it cannot go through `eval_height` (which is `None` for
                // groups).
                let (normal, height, _) =
                    self.operand_surface(layer, index, stack, want_normal, want_height, tile);
                (normal, height, coverage)
            }
        } else {
            let normal = want_normal.then(|| layer.eval(self.size, self.wrap));
            let mut height = if want_height {
                layer.eval_height(self.size, self.wrap)
            } else {
                None
            };
            if let Some(grid) = height.as_mut() {
                let normalize = height_normalize(layer, self.height_scale_px);
                for value in &mut grid.data {
                    *value = (*value * normalize).max(0.0);
                }
            }
            (normal, height, coverage)
        }
    }

    /// Surface of a cut boolean group: the bottom-most visible shape is the
    /// base and every shape above it cuts into that base in stack order.
    #[allow(clippy::too_many_arguments)]
    fn compound_cut_surface_for(
        &self,
        children: &[Layer],
        op: VectorOp,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
        want_normal: bool,
        want_height: bool,
    ) -> (Option<Grid<Vec3>>, Option<Grid<f32>>) {
        let Some(mode) = op.cut_blend_mode() else {
            return (None, None);
        };
        // Cuts compare against the base height even when only normals are
        // wanted, so the height pass is mandatory.
        let want_height = want_height || want_normal;
        let mut operands = children.iter().filter(|child| {
            child.paints() && child.applies_to_tile(tile) && child.has_shape_geometry()
        });
        let Some(base) = operands.next() else {
            return (
                want_normal.then(|| Grid::new(self.size.x, self.size.y, Vec3::Z)),
                want_height.then(|| Grid::new(self.size.x, self.size.y, 0.0f32)),
            );
        };
        let (base_normal, base_height, base_coverage) =
            self.cut_surface(base, index, stack, tile, want_normal, want_height);
        let mut acc = TreeAccum {
            normal: base_normal,
            height: base_height,
            coverage: base_coverage,
        };
        for cutter in operands {
            let (cutter_normal, cutter_height, cutter_coverage) =
                self.cut_surface(cutter, index, stack, tile, want_normal, want_height);
            // `cut_surface` returns normalized heights for every operand, so
            // `composite_cut` must not scale them again.
            acc.composite_cut(
                mode,
                cutter,
                cutter_normal.as_ref(),
                cutter_height.as_ref(),
                &cutter_coverage,
                1.0,
                self.height_scale_px,
            );
        }
        // The tree normalizes a vector compound by 1 / height_scale, so return
        // the same raw units as the outline compound path.
        if let Some(grid) = acc.height.as_mut() {
            for value in &mut grid.data {
                *value *= self.height_scale_px;
            }
        }
        (acc.normal, acc.height)
    }

    /// Footprint of a cut group: the base shape's coverage. Cuts remove height
    /// but do not add area outside the base.
    fn compound_cut_coverage(
        &self,
        children: &[Layer],
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Grid<f32> {
        children
            .iter()
            .find(|child| {
                child.paints() && child.applies_to_tile(tile) && child.has_shape_geometry()
            })
            .map(|base| self.geometric_coverage(base, index, stack, tile))
            .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, 0.0f32))
    }

    /// Vector geometry of a boolean group, cached by the group's revision.
    fn compound_geometry(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> CompoundGeometry {
        let LayerKind::Group { children } = &layer.kind else {
            return CompoundGeometry::empty(self.size);
        };
        let Some(op) = layer.vector_op else {
            return CompoundGeometry::empty(self.size);
        };
        if op.is_cut() {
            return CompoundGeometry {
                regions: Vec::new(),
                coverage: self.compound_cut_coverage(children, index, stack, tile),
            };
        }
        let regions = self.compound_regions(children, op, index, stack, tile);
        let coverage = regions_coverage(self.size, &regions);
        CompoundGeometry { regions, coverage }
    }

    fn compound_geometry_cached(
        &self,
        layer: &Layer,
        cache: &mut EvalCache,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
    ) -> Arc<CompoundGeometry> {
        let tile = self.eval_tile(cache);
        // The operands are tile-filtered, so the cached geometry belongs to one
        // tile and must not be reused when the canvas changes tiles.
        let revision = self.tile_revision(layer, index, tile);
        if let Some(geometry) = cache.compound_get(layer.id, revision) {
            return geometry;
        }
        let geometry = Arc::new(self.compound_geometry(layer, index, stack, tile));
        cache.compound_put(layer.id, revision, geometry.clone());
        geometry
    }

    fn layer_alpha(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        tile: u32,
    ) -> Grid<f32> {
        let mut alpha = self.static_alpha(layer);
        if let Some(mask_id) = layer.mask_layer {
            if let Some(mask_layer) = index.get(&mask_id) {
                let (_, mask_coverage) = self.coverage_of(mask_layer, index, stack, tile);
                apply_clip(&mut alpha, &mask_coverage, &layer.mask_settings, self.wrap);
            }
        }
        alpha
    }

    /// Opacity times the static (painted + noise) mask. Smart masks depend on
    /// the stack below and are applied by [`TreeAccum::composite`].
    fn static_alpha(&self, layer: &Layer) -> Grid<f32> {
        let opacity = layer.opacity.clamp(0.0, 1.0);
        match static_mask(
            layer.mask.as_ref(),
            &layer.mask_settings,
            self.size,
            self.wrap,
        ) {
            Some(mut mask) => {
                for value in &mut mask.data {
                    *value = opacity * value.clamp(0.0, 1.0);
                }
                mask
            }
            None => Grid::new(self.size.x, self.size.y, opacity),
        }
    }

    /// Evaluate a referenced layer as an operand surface. Its own extra
    /// operands are not re-applied here; references are one level deep. The
    /// returned coverage includes the operand's opacity and masks, but
    /// visibility does not matter (a hidden source can still feed a boolean).
    fn operand_surface(
        &self,
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        want_normal: bool,
        want_height: bool,
        tile: u32,
    ) -> OperandSurface {
        let (normal, height) = match &layer.kind {
            LayerKind::Group { children } => {
                let mut child = TreeAccum::new(self.size, want_normal, want_height);
                self.eval_tree_list(
                    children,
                    index,
                    stack,
                    want_normal,
                    want_height,
                    tile,
                    &mut child,
                );
                (child.normal, child.height)
            }
            _ => (
                want_normal.then(|| layer.eval(self.size, self.wrap)),
                if want_height {
                    layer.eval_height(self.size, self.wrap)
                } else {
                    None
                },
            ),
        };
        let (_, coverage) = self.coverage_of(layer, index, stack, tile);
        (normal, height, coverage)
    }

    /// Combine the owner's own surface with its referenced operands, in order.
    #[allow(clippy::too_many_arguments)]
    fn apply_boolean_operands(
        &self,
        layer: &Layer,
        normal: Option<Grid<Vec3>>,
        height: Option<Grid<f32>>,
        geometric: Grid<f32>,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        want_normal: bool,
        want_height: bool,
        tile: u32,
    ) -> (Option<Grid<Vec3>>, Option<Grid<f32>>, Grid<f32>) {
        let target_scale = height_normalize(layer, self.height_scale_px);
        let mut height = height;
        if let Some(grid) = height.as_mut() {
            for value in &mut grid.data {
                *value *= target_scale;
            }
        }
        let mut expression = TreeAccum {
            normal,
            height,
            coverage: geometric,
        };
        for operand in &layer.boolean_operands {
            let Some(operand_layer) = index.get(&operand.layer) else {
                continue;
            };
            if operand.layer == layer.id || self.is_ancestor(operand.layer, layer.id) {
                continue;
            }
            let (operand_normal, operand_height, operand_coverage) =
                self.operand_surface(operand_layer, index, stack, want_normal, want_height, tile);
            let normalize = height_normalize(operand_layer, self.height_scale_px);
            expression.composite_boolean(
                operand.op.blend_mode(),
                operand_normal.as_ref(),
                operand_height.as_ref(),
                &operand_coverage,
                normalize,
                self.height_scale_px,
            );
        }
        let mut height = expression.height;
        if let Some(grid) = height.as_mut() {
            for value in &mut grid.data {
                *value /= target_scale.max(1e-6);
            }
        }
        (expression.normal, height, expression.coverage)
    }

    /// Revision key for cached whole-subtree results. Group evaluation drops
    /// children tagged for another tile, so the key must include the tile or
    /// switching tiles would reuse the previous tile's composite.
    fn tile_revision(&self, layer: &Layer, index: &HashMap<LayerId, &Layer>, tile: u32) -> u64 {
        Self::effective_revision(layer, index, &mut HashSet::new()) ^ (u64::from(tile) << 48)
    }

    fn effective_revision(
        layer: &Layer,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
    ) -> u64 {
        fn mix(hash: u64, value: u64) -> u64 {
            (hash ^ value).wrapping_mul(0x1000_0000_01b3)
        }
        if !stack.insert(layer.id) {
            return 0;
        }
        let id = layer.id.as_u128();
        let mut hash = 0xcbf2_9ce4_8422_2325;
        hash = mix(hash, layer.revision);
        hash = mix(hash, layer.mask_revision);
        hash = mix(hash, id as u64);
        hash = mix(hash, (id >> 64) as u64);
        hash = mix(hash, u64::from(layer.visible));
        if let Some(op) = layer.vector_op {
            hash = mix(hash, op as u64 + 1);
        }
        if let LayerKind::Group { children } = &layer.kind {
            hash = mix(hash, children.len() as u64);
            for child in children {
                hash = mix(hash, Self::effective_revision(child, index, stack));
            }
        }
        if let Some(mask_id) = layer.mask_layer {
            let mask = mask_id.as_u128();
            hash = mix(hash, mask as u64);
            hash = mix(hash, (mask >> 64) as u64);
            if let Some(mask_layer) = index.get(&mask_id) {
                hash = mix(hash, Self::effective_revision(mask_layer, index, stack));
            }
        }
        for operand in &layer.boolean_operands {
            let operand_id = operand.layer.as_u128();
            hash = mix(hash, operand_id as u64);
            hash = mix(hash, (operand_id >> 64) as u64);
            hash = mix(hash, u64::from(operand.op as u8));
            if let Some(operand_layer) = index.get(&operand.layer) {
                hash = mix(hash, Self::effective_revision(operand_layer, index, stack));
            }
        }
        stack.remove(&layer.id);
        hash
    }

    fn evaluate_tree(
        &self,
        want_normal: bool,
        want_height: bool,
        tile: u32,
    ) -> (Option<Grid<Vec3>>, Option<Grid<f32>>) {
        // Boolean operations compare surface heights, so normals cannot be
        // composed correctly without the height fields.
        let want_height = want_height || (want_normal && self.has_boolean_ops());
        let index = self.layer_index();
        let mut stack = HashSet::new();
        let mut acc = TreeAccum::new(self.size, want_normal, want_height);
        self.eval_tree_list(
            &self.layers,
            &index,
            &mut stack,
            want_normal,
            want_height,
            tile,
            &mut acc,
        );
        (acc.normal, acc.height)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_tree_list_cached(
        &self,
        layers: &[Layer],
        cache: &mut EvalCache,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        want_normal: bool,
        want_height: bool,
        acc: &mut TreeAccum,
    ) {
        let tile = self.eval_tile(cache);
        for layer in layers {
            if !layer.paints() || !layer.applies_to_tile(tile) {
                continue;
            }
            let ArcLayerEval {
                normal,
                height,
                alpha,
                coverage,
            } = self.eval_layer_cached(layer, cache, index, stack, want_normal, want_height);
            let (Some(alpha), Some(coverage)) = (alpha, coverage) else {
                continue;
            };
            if layer.boolean_operands.is_empty() {
                acc.composite(
                    layer,
                    normal.as_deref(),
                    height.as_deref(),
                    &alpha,
                    &coverage,
                    self.height_scale_px,
                    self.wrap,
                );
            } else {
                let geometric = self.geometric_coverage_cached(layer, cache, index, stack);
                let (normal, height, geometric) = self.apply_boolean_operands_cached(
                    layer,
                    normal.as_ref().map(|grid| (**grid).clone()),
                    height.as_ref().map(|grid| (**grid).clone()),
                    geometric,
                    cache,
                    index,
                    stack,
                    want_normal,
                    want_height,
                );
                let coverage = Arc::new(multiply_grids(&geometric, &alpha));
                acc.composite(
                    layer,
                    normal.as_ref(),
                    height.as_ref(),
                    &alpha,
                    &coverage,
                    self.height_scale_px,
                    self.wrap,
                );
            }
        }
    }

    fn eval_layer_cached(
        &self,
        layer: &Layer,
        cache: &mut EvalCache,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        want_normal: bool,
        want_height: bool,
    ) -> ArcLayerEval {
        let revision = self.tile_revision(layer, index, self.eval_tile(cache));
        let mut eval = cache.tree_snapshot(layer.id, revision);

        if eval.alpha.is_none() || eval.coverage.is_none() {
            let (alpha, coverage) = self.coverage_cached(layer, cache, index, stack);
            eval.alpha = Some(alpha);
            eval.coverage = Some(coverage);
        }

        if want_normal && eval.normal.is_none() {
            let normal = match &layer.kind {
                LayerKind::Group { children } => {
                    if let Some(op) = layer.vector_op {
                        if op.is_cut() {
                            let (normal, height) = self.compound_cut_surface_for(
                                children,
                                op,
                                index,
                                stack,
                                self.eval_tile(cache),
                                true,
                                want_height && eval.height.is_none(),
                            );
                            if eval.height.is_none() {
                                eval.height = height.map(Arc::new);
                            }
                            normal.unwrap_or_else(|| Grid::new(self.size.x, self.size.y, Vec3::Z))
                        } else {
                            let geometry =
                                self.compound_geometry_cached(layer, cache, index, stack);
                            let (normal, height) = self.compound_surface_for(
                                children,
                                &geometry.regions,
                                Some(&geometry.coverage),
                                true,
                                want_height && eval.height.is_none(),
                            );
                            if eval.height.is_none() {
                                eval.height = height.map(Arc::new);
                            }
                            normal.unwrap_or_else(|| Grid::new(self.size.x, self.size.y, Vec3::Z))
                        }
                    } else {
                        let mut child = TreeAccum::new(self.size, true, want_height);
                        self.eval_tree_list_cached(
                            children,
                            cache,
                            index,
                            stack,
                            true,
                            want_height,
                            &mut child,
                        );
                        child
                            .normal
                            .unwrap_or_else(|| Grid::new(self.size.x, self.size.y, Vec3::Z))
                    }
                }
                _ => layer.eval(self.size, self.wrap),
            };
            eval.normal = Some(Arc::new(normal));
        }

        if want_height && eval.height.is_none() {
            let height = match &layer.kind {
                LayerKind::Group { children } => {
                    if let Some(op) = layer.vector_op {
                        if op.is_cut() {
                            let (_, height) = self.compound_cut_surface_for(
                                children,
                                op,
                                index,
                                stack,
                                self.eval_tile(cache),
                                want_normal,
                                true,
                            );
                            height
                        } else {
                            let geometry =
                                self.compound_geometry_cached(layer, cache, index, stack);
                            let (_, height) = self.compound_surface_for(
                                children,
                                &geometry.regions,
                                Some(&geometry.coverage),
                                false,
                                true,
                            );
                            height
                        }
                    } else {
                        let mut child = TreeAccum::new(self.size, want_normal, true);
                        self.eval_tree_list_cached(
                            children,
                            cache,
                            index,
                            stack,
                            want_normal,
                            true,
                            &mut child,
                        );
                        child.height
                    }
                }
                _ => layer.eval_height(self.size, self.wrap),
            };
            eval.height = height.map(Arc::new);
        }

        cache.tree_store(layer.id, revision, &eval);
        eval
    }

    fn coverage_cached(
        &self,
        layer: &Layer,
        cache: &mut EvalCache,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
    ) -> CoveragePair {
        let revision = self.tile_revision(layer, index, self.eval_tile(cache));
        if let Some(pair) = cache.tree_pair(layer.id, revision) {
            return pair;
        }
        if !stack.insert(layer.id) {
            let full = Arc::new(Grid::new(self.size.x, self.size.y, 1.0f32));
            return (full.clone(), full);
        }

        let geometric = self.geometric_coverage_cached_unchecked(layer, cache, index, stack);

        let mut alpha = self.static_alpha(layer);
        if let Some(mask_id) = layer.mask_layer {
            if let Some(mask_layer) = index.get(&mask_id) {
                let (_, mask_coverage) = self.coverage_cached(mask_layer, cache, index, stack);
                apply_clip(&mut alpha, &mask_coverage, &layer.mask_settings, self.wrap);
            }
        }

        let coverage = multiply_grids(&geometric, &alpha);
        stack.remove(&layer.id);
        let alpha = Arc::new(alpha);
        let coverage = Arc::new(coverage);
        cache.tree_store_pair(layer.id, revision, alpha.clone(), coverage.clone());
        (alpha, coverage)
    }

    fn geometric_coverage_cached(
        &self,
        layer: &Layer,
        cache: &mut EvalCache,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
    ) -> Grid<f32> {
        if !stack.insert(layer.id) {
            return Grid::new(self.size.x, self.size.y, 1.0f32);
        }
        let coverage = self.geometric_coverage_cached_unchecked(layer, cache, index, stack);
        stack.remove(&layer.id);
        coverage
    }

    /// Assumes the caller already inserted `layer.id` into the recursion guard.
    fn geometric_coverage_cached_unchecked(
        &self,
        layer: &Layer,
        cache: &mut EvalCache,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
    ) -> Grid<f32> {
        match &layer.kind {
            LayerKind::Group { children } => {
                if layer.vector_op.is_some() {
                    return self
                        .compound_geometry_cached(layer, cache, index, stack)
                        .coverage
                        .clone();
                }
                let mut coverage = Grid::new(self.size.x, self.size.y, 0.0f32);
                for child in children {
                    if !child.paints() {
                        continue;
                    }
                    let (_, child_coverage) = self.coverage_cached(child, cache, index, stack);
                    for (dst, src) in coverage.data.iter_mut().zip(child_coverage.data.iter()) {
                        *dst = dst.max(*src);
                    }
                }
                coverage
            }
            LayerKind::Shape(shape) => shape.mask_coverage(self.size, self.wrap),
            _ => Grid::new(self.size.x, self.size.y, 1.0f32),
        }
    }

    fn operand_surface_cached(
        &self,
        layer: &Layer,
        cache: &mut EvalCache,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        want_normal: bool,
        want_height: bool,
    ) -> CachedOperandSurface {
        let eval = self.eval_layer_cached(layer, cache, index, stack, want_normal, want_height);
        let (_, coverage) = self.coverage_cached(layer, cache, index, stack);
        (eval.normal, eval.height, coverage)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_boolean_operands_cached(
        &self,
        layer: &Layer,
        normal: Option<Grid<Vec3>>,
        height: Option<Grid<f32>>,
        geometric: Grid<f32>,
        cache: &mut EvalCache,
        index: &HashMap<LayerId, &Layer>,
        stack: &mut HashSet<LayerId>,
        want_normal: bool,
        want_height: bool,
    ) -> (Option<Grid<Vec3>>, Option<Grid<f32>>, Grid<f32>) {
        let target_scale = height_normalize(layer, self.height_scale_px);
        let mut height = height;
        if let Some(grid) = height.as_mut() {
            for value in &mut grid.data {
                *value *= target_scale;
            }
        }
        let mut expression = TreeAccum {
            normal,
            height,
            coverage: geometric,
        };
        for operand in &layer.boolean_operands {
            let Some(operand_layer) = index.get(&operand.layer) else {
                continue;
            };
            if operand.layer == layer.id || self.is_ancestor(operand.layer, layer.id) {
                continue;
            }
            let (operand_normal, operand_height, operand_coverage) = self.operand_surface_cached(
                operand_layer,
                cache,
                index,
                stack,
                want_normal,
                want_height,
            );
            let normalize = height_normalize(operand_layer, self.height_scale_px);
            expression.composite_boolean(
                operand.op.blend_mode(),
                operand_normal.as_deref(),
                operand_height.as_deref(),
                operand_coverage.as_ref(),
                normalize,
                self.height_scale_px,
            );
        }
        let mut height = expression.height;
        if let Some(grid) = height.as_mut() {
            for value in &mut grid.data {
                *value /= target_scale.max(1e-6);
            }
        }
        (expression.normal, height, expression.coverage)
    }

    fn evaluate_tree_cached(
        &self,
        cache: &mut EvalCache,
        want_normal: bool,
        want_height: bool,
    ) -> (Option<Grid<Vec3>>, Option<Grid<f32>>) {
        let want_height = want_height || (want_normal && self.has_boolean_ops());
        let index = self.layer_index();
        let mut stack = HashSet::new();
        let mut acc = TreeAccum::new(self.size, want_normal, want_height);
        self.eval_tree_list_cached(
            &self.layers,
            cache,
            &index,
            &mut stack,
            want_normal,
            want_height,
            &mut acc,
        );
        (acc.normal, acc.height)
    }
}

/// Divide bevels one island per distinct overlap region, so its cost grows
/// with the number of distinct operand combinations. The UI caps the operand
/// count; loaded projects above the limit merge into one outline.
const MAX_DIVIDE_OPERANDS: usize = 8;

/// Vector geometry of a boolean group.
///
/// `regions` holds one filled path per output region (a single combined
/// outline for Add/Subtract/Intersect/XOR, one path per island for Divide).
/// `coverage` is the union of those regions, used by clipping masks and by
/// boolean operands that reference the group.
struct CompoundGeometry {
    regions: Vec<Path>,
    coverage: Grid<f32>,
}

impl CompoundGeometry {
    fn empty(size: UVec2) -> Self {
        Self {
            regions: Vec::new(),
            coverage: Grid::new(size.x, size.y, 0.0f32),
        }
    }
}

/// Rasterize boolean regions into their combined coverage.
///
/// The regions are disjoint, so their loops are rasterized in one even-odd
/// pass: adjacent regions share their boundary exactly, which keeps the seam
/// fully covered instead of half-transparent.
fn regions_coverage(size: UVec2, regions: &[Path]) -> Grid<f32> {
    if regions.is_empty() {
        return Grid::new(size.x, size.y, 0.0f32);
    }
    if regions.len() == 1 {
        return ShapeSource::Path(regions[0].clone()).rasterize(size.x, size.y);
    }
    let merged = Path {
        subpaths: regions
            .iter()
            .flat_map(|region| region.subpaths.iter().cloned())
            .collect(),
        fill_rule: FillRule::EvenOdd,
    };
    ShapeSource::Path(merged).rasterize(size.x, size.y)
}

/// Union of every operand, used when a group takes part as one operand.
fn union_loops(operands: &[VectorOperand]) -> Vec<Vec<Vec2>> {
    let Some(first) = operands.first() else {
        return Vec::new();
    };
    if operands.len() == 1 {
        return first.loops.clone();
    }
    let ops = vec![PathBoolean::Add; operands.len() - 1];
    boolean_loops(operands, &ops)
}

/// Boolean operand built from a generated outline such as a traced raster or
/// an expanded stroke band.
fn traced_operand(path: &Path, fill_rule: FillRule) -> Option<VectorOperand> {
    let operand = VectorOperand::new(
        flatten_path_tolerance(path, VECTOR_BOOLEAN_TOLERANCE),
        fill_rule,
    );
    (!operand.is_empty()).then_some(operand)
}

/// Result of [`Document::cut_layers_below`].
pub struct CutOutcome {
    /// Paths of the pieces that were created, in stack order.
    pub paths: Vec<Vec<usize>>,
    /// How many target layers were replaced by pieces.
    pub targets: usize,
}

/// Bevel style a cut piece inherits: the target shape itself, or a group's
/// bottom-most shape.
fn cut_style(layer: &Layer) -> Option<ShapeLayer> {
    match &layer.kind {
        LayerKind::Shape(shape) => Some(shape.clone()),
        LayerKind::Group { children } => compound_base_shape(children).cloned(),
        _ => None,
    }
}

/// One cut piece: the target's compositing settings with a new outline.
///
/// `excluded` holds layers that are about to disappear (the cutter and the
/// other trimmed targets), so a clipping reference never dangles.
fn cut_piece_layer(
    doc: &Document,
    target: &Layer,
    style: &ShapeLayer,
    loops: Vec<Vec<Vec2>>,
    name: String,
    excluded: &HashSet<LayerId>,
) -> Layer {
    let mut shape = style.clone();
    // A trimmed stroke keeps the surviving band as filled geometry: keeping the
    // Stroke style would band the new outline again and hollow the piece.
    shape.style = ShapeStyle::Fill;
    shape.source = ShapeSource::Path(piece_path(loops));
    let mut piece = Layer::new(name, LayerKind::Shape(shape));
    piece.visible = target.visible;
    piece.opacity = target.opacity;
    piece.blend = target.blend;
    piece.mask = target.mask.clone();
    piece.mask_layer = target.mask_layer.filter(|id| {
        !excluded.contains(id) && doc.find_layer(*id).is_some() && !doc.is_ancestor(target.id, *id)
    });
    piece.mask_settings = target.mask_settings.clone();
    piece.hidden_as_mask = target.hidden_as_mask;
    piece.id_color = target.id_color;
    piece.touch();
    piece
}

/// Outline for one trimmed piece.
///
/// A single island without holes stays a `NonZero` polygon so the GPU can pack
/// it; islands with holes (or several loops) need `EvenOdd` and fall back to
/// the CPU evaluator through the normal fail-closed gate.
fn piece_path(loops: Vec<Vec<Vec2>>) -> Path {
    let fill_rule = if loops.len() == 1 {
        FillRule::NonZero
    } else {
        FillRule::EvenOdd
    };
    Path::from_loops(loops, fill_rule)
}

fn bounds_overlap(a: Bounds, b: Bounds) -> bool {
    a.min.x <= b.max.x && b.min.x <= a.max.x && a.min.y <= b.max.y && b.min.y <= a.max.y
}

/// Total absolute area of the loops.
///
/// A degenerate lasso (two points, or collinear points) must never be able to
/// subtract a whole shape, so trims reject loops without area up front.
fn loops_area(loops: &[Vec<Vec2>]) -> f32 {
    let mut area = 0.0;
    for points in loops {
        let mut sum = 0.0;
        for index in 0..points.len() {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            sum += a.x * b.y - b.x * a.y;
        }
        area += (sum * 0.5).abs();
    }
    area
}

/// Build the committed layer for a boolean group: the base shape's bevel
/// style, the combined vector result, and the group's compositing settings.
/// References that pointed into the group are dropped because those layers
/// disappear.
fn compound_result_layer(
    doc: &Document,
    group: &Layer,
    base: &ShapeLayer,
    source: ShapeSource,
    name: String,
) -> Layer {
    let mut shape = base.clone();
    shape.source = source;
    let mut layer = Layer::new(name, LayerKind::Shape(shape));
    layer.visible = group.visible;
    layer.opacity = group.opacity;
    layer.blend = group.blend;
    layer.mask = group.mask.clone();
    layer.mask_layer = group
        .mask_layer
        .filter(|id| doc.find_layer(*id).is_some() && !doc.is_ancestor(group.id, *id));
    layer.mask_settings = group.mask_settings.clone();
    layer.boolean_operands = group.boolean_operands.clone();
    layer.hidden_as_mask = group.hidden_as_mask;
    layer.id_color = group.id_color;
    layer.touch();
    layer
}

/// Bottom-most visible shape in a compound subtree supplies the bevel style.
fn compound_base_shape(children: &[Layer]) -> Option<&ShapeLayer> {
    for child in children {
        if !child.paints() {
            continue;
        }
        match &child.kind {
            LayerKind::Shape(shape) => return Some(shape),
            LayerKind::Group { children } => {
                if let Some(shape) = compound_base_shape(children) {
                    return Some(shape);
                }
            }
            _ => {}
        }
    }
    None
}

fn multiply_grids(a: &Grid<f32>, b: &Grid<f32>) -> Grid<f32> {
    Grid::from_fn_par(a.w, a.h, |x, y| {
        (*a.get(x, y) * *b.get(x, y)).clamp(0.0, 1.0)
    })
}

/// Per-layer cache for interactive evaluation.
///
/// Cache entries are keyed by layer id and validated against the layer's
/// `revision`. Mutating a layer directly does not bump the revision; call
/// [`Layer::touch`] (the UI does this after every edit).
#[derive(Default)]
pub struct EvalCache {
    size: UVec2,
    wrap: Wrap,
    /// Tile override for the next evaluation. Export and any other caller that
    /// needs one document per UDIM tile sets this instead of mutating
    /// `Document::active_tile`; mutating the live document would force a
    /// copy-on-write clone of every layer grid. `None` uses the document's
    /// active tile.
    pub tile: Option<u32>,
    entries: HashMap<LayerId, CachedLayer>,
    tree: HashMap<LayerId, TreeCacheEntry>,
    compound: HashMap<LayerId, (u64, Arc<CompoundGeometry>)>,
}

struct CachedLayer {
    revision: u64,
    normal: Option<CachedNormal>,
    height: Option<Option<CachedHeight>>,
}

/// A cached layer normal: either the whole canvas or only the shape's
/// padded bounding box.
///
/// Region caches are what keep large documents interactive: a small shape in
/// a 4K document only stores and regenerates its own footprint, and layers
/// whose content revision did not change are never regenerated.
enum CachedNormal {
    Full(Grid<Vec3>),
    Region { origin: UVec2, grid: Grid<Vec3> },
}

/// A cached layer height with the same full/region split as
/// [`CachedNormal`].
enum CachedHeight {
    Full(Grid<f32>),
    Region { origin: UVec2, grid: Grid<f32> },
}

struct TreeCacheEntry {
    revision: u64,
    normal: Option<Arc<Grid<Vec3>>>,
    height: Option<Arc<Grid<f32>>>,
    alpha: Option<Arc<Grid<f32>>>,
    coverage: Option<Arc<Grid<f32>>>,
}

impl TreeCacheEntry {
    fn new(revision: u64) -> Self {
        Self {
            revision,
            normal: None,
            height: None,
            alpha: None,
            coverage: None,
        }
    }
}

impl CachedLayer {
    fn new(revision: u64) -> Self {
        Self {
            revision,
            normal: None,
            height: None,
        }
    }
}

impl EvalCache {
    pub fn new() -> Self {
        Self {
            size: UVec2::ZERO,
            wrap: Wrap::Clamp,
            tile: None,
            entries: HashMap::new(),
            tree: HashMap::new(),
            compound: HashMap::new(),
        }
    }

    /// Drop one layer's cached fields (call after editing it).
    pub fn invalidate(&mut self, id: LayerId) {
        self.entries.remove(&id);
        self.tree.remove(&id);
        self.compound.remove(&id);
    }

    pub fn invalidate_all(&mut self) {
        self.entries.clear();
        self.tree.clear();
        self.compound.clear();
    }

    pub fn cached_layers(&self) -> usize {
        self.entries.len() + self.tree.len() + self.compound.len()
    }

    fn prepare(&mut self, size: UVec2, wrap: Wrap, doc: &Document) {
        if self.size != size || self.wrap != wrap {
            self.size = size;
            self.wrap = wrap;
            self.entries.clear();
            self.tree.clear();
            self.compound.clear();
        }
        let live = doc.all_layer_ids();
        self.entries.retain(|id, _| live.contains(id));
        self.tree.retain(|id, _| live.contains(id));
        self.compound.retain(|id, _| live.contains(id));
    }

    fn compound_get(&mut self, id: LayerId, revision: u64) -> Option<Arc<CompoundGeometry>> {
        match self.compound.get(&id) {
            Some((cached, geometry)) if *cached == revision => Some(geometry.clone()),
            _ => None,
        }
    }

    fn compound_put(&mut self, id: LayerId, revision: u64, geometry: Arc<CompoundGeometry>) {
        self.compound.insert(id, (revision, geometry));
    }

    fn tree_entry(&mut self, id: LayerId, revision: u64) -> &mut TreeCacheEntry {
        let entry = self
            .tree
            .entry(id)
            .or_insert_with(|| TreeCacheEntry::new(revision));
        if entry.revision != revision {
            *entry = TreeCacheEntry::new(revision);
        }
        entry
    }

    fn tree_snapshot(&mut self, id: LayerId, revision: u64) -> ArcLayerEval {
        let entry = self.tree_entry(id, revision);
        ArcLayerEval {
            normal: entry.normal.clone(),
            height: entry.height.clone(),
            alpha: entry.alpha.clone(),
            coverage: entry.coverage.clone(),
        }
    }

    fn tree_pair(&mut self, id: LayerId, revision: u64) -> Option<CoveragePair> {
        let entry = self.tree_entry(id, revision);
        Some((entry.alpha.clone()?, entry.coverage.clone()?))
    }

    fn tree_store(&mut self, id: LayerId, revision: u64, eval: &ArcLayerEval) {
        let entry = self.tree_entry(id, revision);
        entry.normal = eval.normal.clone();
        entry.height = eval.height.clone();
        entry.alpha = eval.alpha.clone();
        entry.coverage = eval.coverage.clone();
    }

    fn tree_store_pair(
        &mut self,
        id: LayerId,
        revision: u64,
        alpha: Arc<Grid<f32>>,
        coverage: Arc<Grid<f32>>,
    ) {
        let entry = self.tree_entry(id, revision);
        entry.alpha = Some(alpha);
        entry.coverage = Some(coverage);
    }
}

/// A layer referenced as an extra boolean operand.
///
/// The referenced layer keeps its position in the layer stack; this is only a
/// surface input for the owner's boolean expression.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BooleanOperand {
    pub layer: LayerId,
    #[serde(default)]
    pub op: BooleanOp,
}

/// Live vector boolean operation for a group of shapes. The operands' outlines
/// are combined before anything is rasterized, so the bevel and the normal map
/// are built from the boolean result instead of the individual shapes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VectorOp {
    /// Union: merge the outlines into one silhouette.
    #[default]
    Add,
    /// Subtract the shapes above the bottom one from it.
    Subtract,
    /// Keep only the overlap.
    Intersect,
    /// Keep the non-overlapping parts (symmetric difference).
    Xor,
    /// Partition the union into separately beveled regions.
    Divide,
    /// Partial cut: the shapes above the bottom one cut their height profile
    /// into the bottom shape and stop at its base.
    PartialCut,
    /// Partial slash: a partial cut tapered along each cutter's long axis.
    PartialSlash,
    /// Clip divide: the cutters split the bottom shape along their bands, with
    /// each band's bevel becoming the two seam walls.
    ClipDivide,
}

impl VectorOp {
    pub const ALL: [Self; 8] = [
        Self::Add,
        Self::Subtract,
        Self::Intersect,
        Self::Xor,
        Self::Divide,
        Self::PartialCut,
        Self::PartialSlash,
        Self::ClipDivide,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Intersect => "Intersect",
            Self::Xor => "XOR",
            Self::Divide => "Divide",
            Self::PartialCut => "Partial cut (pocket)",
            Self::PartialSlash => "Partial slash",
            Self::ClipDivide => "Clip divide",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Intersect => "Intersect",
            Self::Xor => "XOR",
            Self::Divide => "Divide",
            Self::PartialCut => "Pocket",
            Self::PartialSlash => "Slash",
            Self::ClipDivide => "Clip",
        }
    }

    /// Compact layer-row badge.
    pub fn badge_label(self) -> &'static str {
        match self {
            Self::Add => "ADD",
            Self::Subtract => "SUB",
            Self::Intersect => "INT",
            Self::Xor => "XOR",
            Self::Divide => "DIV",
            Self::PartialCut => "PCKT",
            Self::PartialSlash => "SLSH",
            Self::ClipDivide => "CLIP",
        }
    }

    /// Binary operation applied to the operand outlines. [`Self::Divide`] has
    /// no binary form: its regions are derived from the full arrangement. The
    /// cut operations work on height fields, not outlines.
    pub fn path_boolean(self) -> Option<PathBoolean> {
        match self {
            Self::Add => Some(PathBoolean::Add),
            Self::Subtract => Some(PathBoolean::Subtract),
            Self::Intersect => Some(PathBoolean::Intersect),
            Self::Xor => Some(PathBoolean::Xor),
            Self::Divide | Self::PartialCut | Self::PartialSlash | Self::ClipDivide => None,
        }
    }

    pub fn is_divide(self) -> bool {
        self == Self::Divide
    }

    /// Cut operations modify the bottom shape's height surface instead of its
    /// outline. They run after the base is beveled, so the wall profile is the
    /// cutter's own bevel.
    pub fn is_cut(self) -> bool {
        matches!(
            self,
            Self::PartialCut | Self::PartialSlash | Self::ClipDivide
        )
    }

    pub fn cut_blend_mode(self) -> Option<BlendMode> {
        match self {
            Self::PartialCut => Some(BlendMode::PartialCut),
            Self::PartialSlash => Some(BlendMode::PartialSlash),
            Self::ClipDivide => Some(BlendMode::ClipDivide),
            _ => None,
        }
    }
}

/// What a vector layer is for.
///
/// A normal vector is a surface: its outline, bevel and height make pixels. A
/// path guide is authoring geometry instead. It steers operations such as a
/// sweep, and it never paints, so a guide never changes the exported maps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VectorRole {
    /// Render the vector as a normal shape (the behaviour of every old file).
    #[default]
    Surface,
    /// Use the vector's spine as a guide and keep it out of the maps.
    Path,
}

impl VectorRole {
    pub const ALL: [Self; 2] = [Self::Surface, Self::Path];

    pub fn label(self) -> &'static str {
        match self {
            Self::Surface => "Normal shape",
            Self::Path => "Path guide",
        }
    }

    /// Keep the default out of saved files, as the rest of the shape does.
    pub fn is_surface(&self) -> bool {
        *self == Self::Surface
    }
}

/// Name for a duplicated layer: `Name copy`, matching what duplicate tools
/// usually show in the stack.
fn duplicated_name(name: &str) -> String {
    if name.is_empty() {
        "Layer copy".to_string()
    } else {
        format!("{name} copy")
    }
}

/// Keep optional booleans out of saved projects unless they are set.
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    /// UDIM tile this layer belongs to. `None` (the default and every layer in
    /// old projects) is shared by all tiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile: Option<u32>,
    /// Freezes the layer's content: canvas moves/resizes/rotations, nudges,
    /// lasso cleanup, painting, mask edits and deletion skip it. Naming,
    /// visibility, blend/opacity, reordering and selecting still work, and the
    /// lock itself can always be toggled.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    /// Scales slope for normal blending.
    pub opacity: f32,
    pub blend: BlendMode,
    pub mask: Option<Grid<f32>>,
    /// Use another layer or group's coverage as a clipping mask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_layer: Option<LayerId>,
    /// Extra boolean operands combined with this layer's own surface before it
    /// is blended into the stack. Referenced layers are not reparented.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boolean_operands: Vec<BooleanOperand>,
    /// Live vector compound operation when this layer is a group. Operands are
    /// the group's visible shape children: their outlines are combined before
    /// anything is rasterized, so the normal is generated from the boolean
    /// result instead of the individual shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_op: Option<VectorOp>,
    pub kind: LayerKind,
    /// Bumped whenever editable parameters change; see [`Layer::touch`].
    #[serde(default)]
    pub revision: u64,
    /// Bumped when only the painted mask changes. Layer normal/height caches
    /// stay valid across mask edits; tree alpha/coverage uses this revision.
    #[serde(default)]
    pub mask_revision: u64,
    /// True when this layer was hidden by binding it as another layer's mask,
    /// so unbinding can reveal it again without touching user visibility.
    #[serde(default)]
    pub hidden_as_mask: bool,
    /// Non-destructive mask adjustments and procedural generators. Call
    /// [`Layer::touch_mask`] after editing.
    #[serde(default, skip_serializing_if = "MaskSettings::is_default")]
    pub mask_settings: MaskSettings,
    /// Optional material ID colour, sRGB 0–1, used by the exported ID map.
    /// Layers without a colour keep the background or the colour below them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_color: Option<[f32; 3]>,
    /// Authoring data for a cable layer. The layer itself is one baked outline;
    /// these values let Properties rebuild it after creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cable: Option<CableMeta>,
}

impl Layer {
    pub fn new(name: impl Into<String>, kind: LayerKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            visible: true,
            tile: None,
            locked: false,
            opacity: 1.0,
            blend: BlendMode::Derivative,
            mask: None,
            mask_layer: None,
            boolean_operands: Vec::new(),
            vector_op: None,
            kind,
            revision: 0,
            mask_revision: 0,
            hidden_as_mask: false,
            mask_settings: MaskSettings::default(),
            id_color: None,
            cable: None,
        }
    }

    /// Mark the layer as edited so [`EvalCache`] recomputes it.
    pub fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Mark only the painted mask as edited. Normal and height grids do not
    /// depend on the mask, so their caches stay warm; the mask preview and the
    /// tree evaluator use [`Layer::mask_revision`].
    pub fn touch_mask(&mut self) {
        self.mask_revision = self.mask_revision.wrapping_add(1);
    }

    pub fn flat(name: impl Into<String>) -> Self {
        Self::new(name, LayerKind::Flat)
    }

    pub fn fill(name: impl Into<String>, height: f32) -> Self {
        Self::new(name, LayerKind::Fill { height })
    }

    pub fn shape(name: impl Into<String>, source: ShapeSource) -> Self {
        Self::new(name, LayerKind::Shape(ShapeLayer::new(source)))
    }

    pub fn group(name: impl Into<String>, children: Vec<Layer>) -> Self {
        Self::new(name, LayerKind::Group { children })
    }

    pub fn is_group(&self) -> bool {
        matches!(self.kind, LayerKind::Group { .. })
    }

    /// True when the layer contributes to `tile`. Shared layers (no tag) apply
    /// to every tile.
    pub fn applies_to_tile(&self, tile: u32) -> bool {
        self.tile.is_none_or(|own| own == tile)
    }

    /// A group whose children are combined as a live vector boolean before
    /// beveling. The children stay in the stack and remain editable.
    pub fn is_vector_compound(&self) -> bool {
        self.vector_op.is_some() && self.is_group()
    }

    /// True when this is a vector path marked as a guide for operations.
    ///
    /// Only editable paths qualify: a guide is read as a spine, and a primitive
    /// or text layer has no spine to follow. Anything else keeps painting, so a
    /// role left over on a converted layer can never hide content.
    pub fn is_path_guide(&self) -> bool {
        matches!(
            &self.kind,
            LayerKind::Shape(shape)
                if shape.vector_role == VectorRole::Path
                    && matches!(shape.source, ShapeSource::Path(_))
        )
    }

    /// Whether this layer contributes to any rendered map.
    ///
    /// This is [`Self::visible`] plus one exception: a path guide is authoring
    /// geometry and is never composited, so every evaluator asks this instead of
    /// reading the visibility flag directly.
    pub fn paints(&self) -> bool {
        self.visible && !self.is_path_guide()
    }

    pub fn children(&self) -> Option<&[Layer]> {
        match &self.kind {
            LayerKind::Group { children } => Some(children),
            _ => None,
        }
    }

    pub fn children_mut(&mut self) -> Option<&mut Vec<Layer>> {
        match &mut self.kind {
            LayerKind::Group { children } => Some(children),
            _ => None,
        }
    }

    /// Deep copy with fresh ids for this layer and every descendant, so the
    /// copy can live next to its original in the same document.
    ///
    /// The painted mask, mask settings and clipping binding are carried over
    /// unchanged. References that point inside a duplicated set are remapped
    /// afterwards with [`Layer::remap_layer_references`].
    pub fn duplicate(&self) -> Self {
        let mut copy = self.clone();
        copy.refresh_ids();
        copy.name = duplicated_name(&self.name);
        copy
    }

    /// Give this layer and every descendant a new id.
    fn refresh_ids(&mut self) {
        self.id = Uuid::new_v4();
        if let LayerKind::Group { children } = &mut self.kind {
            for child in children {
                child.refresh_ids();
            }
        }
    }

    /// Rewrite mask and boolean references that point at layers in `map`.
    ///
    /// After duplicating several layers together, a copy whose clipping source
    /// or boolean operand was part of the same set points at the copied source
    /// instead of the original, so the duplicated group keeps working as a
    /// unit. References to layers outside the set are left alone.
    pub fn remap_layer_references(&mut self, map: &HashMap<LayerId, LayerId>) {
        if let Some(source) = self.mask_layer.as_mut() {
            if let Some(copied) = map.get(source) {
                *source = *copied;
            }
        }
        for operand in &mut self.boolean_operands {
            if let Some(copied) = map.get(&operand.layer) {
                operand.layer = *copied;
            }
        }
        if let LayerKind::Group { children } = &mut self.kind {
            for child in children {
                child.remap_layer_references(map);
            }
        }
    }

    /// Rough heap footprint of this layer and its descendants; see
    /// [`Document::estimated_bytes`].
    pub fn estimated_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>() + self.name.capacity();
        if let Some(mask) = &self.mask {
            bytes += mask.data.len() * std::mem::size_of::<f32>();
        }
        bytes += match &self.kind {
            LayerKind::Flat | LayerKind::Fill { .. } => 0,
            LayerKind::ImportedNormal { normal } => normal.data.len() * std::mem::size_of::<Vec3>(),
            LayerKind::NormalPaint { normal } => normal.data.len() * std::mem::size_of::<Vec3>(),
            LayerKind::PhotoHeight(photo) => photo.height.data.len() * std::mem::size_of::<f32>(),
            LayerKind::HeightPaint { height, .. } => height.data.len() * std::mem::size_of::<f32>(),
            LayerKind::Shape(shape) => shape.estimated_bytes(),
            LayerKind::Group { children } => children.iter().map(Layer::estimated_bytes).sum(),
        };
        bytes
    }

    /// True when this layer contributes vector coverage: a shape or a group
    /// containing at least one visible shape descendant.
    pub fn has_shape_geometry(&self) -> bool {
        match &self.kind {
            LayerKind::Shape(_) => true,
            LayerKind::Group { children } => children
                .iter()
                .any(|child| child.paints() && child.has_shape_geometry()),
            _ => false,
        }
    }

    /// Number of visible children that contribute shapes to a boolean.
    pub fn visible_vector_operand_count(&self) -> usize {
        self.children()
            .map(|children| {
                children
                    .iter()
                    .filter(|child| child.paints() && child.has_shape_geometry())
                    .count()
            })
            .unwrap_or(0)
    }

    /// Stable hash of this layer and every descendant. Cache validation uses
    /// it instead of the leaf-only `revision` so group edits invalidate the
    /// whole subtree automatically.
    ///
    /// `mask_revision` is deliberately excluded: layer normal and height grids
    /// do not depend on the painted mask, so mask edits keep those caches warm.
    /// The tree evaluator mixes it in separately for alpha/coverage.
    pub fn content_revision(&self) -> u64 {
        fn mix(hash: u64, value: u64) -> u64 {
            (hash ^ value).wrapping_mul(0x1000_0000_01b3)
        }
        let id = self.id.as_u128();
        let mut hash = 0xcbf2_9ce4_8422_2325;
        hash = mix(hash, self.revision);
        hash = mix(hash, id as u64);
        hash = mix(hash, (id >> 64) as u64);
        hash = mix(hash, u64::from(self.visible));
        if let Some(op) = self.vector_op {
            hash = mix(hash, op as u64 + 1);
        }
        if let Some(mask_id) = self.mask_layer {
            let mask_id = mask_id.as_u128();
            hash = mix(hash, mask_id as u64);
            hash = mix(hash, (mask_id >> 64) as u64);
        }
        for operand in &self.boolean_operands {
            let operand_id = operand.layer.as_u128();
            hash = mix(hash, operand_id as u64);
            hash = mix(hash, (operand_id >> 64) as u64);
            hash = mix(hash, u64::from(operand.op as u8));
        }
        if let LayerKind::Group { children } = &self.kind {
            hash = mix(hash, children.len() as u64);
            for child in children {
                hash = mix(hash, child.content_revision());
            }
        }
        hash
    }

    /// Groups, clipping masks and boolean coverage need the tree evaluator.
    pub fn requires_tree(&self) -> bool {
        self.mask_layer.is_some()
            || self.blend.uses_surface()
            || !self.boolean_operands.is_empty()
            || self.vector_op.is_some()
            || match &self.kind {
                LayerKind::Group { children } => children.iter().any(|child| child.requires_tree()),
                _ => false,
            }
            || self.is_group()
    }

    pub fn height_paint(name: impl Into<String>, height: Grid<f32>, strength: f32) -> Self {
        Self::new(name, LayerKind::HeightPaint { height, strength })
    }

    pub fn imported_normal(name: impl Into<String>, normal: Grid<Vec3>) -> Self {
        Self::new(name, LayerKind::ImportedNormal { normal })
    }

    /// A paintable tangent-space normal layer, used by normal-map brushes.
    pub fn normal_paint(name: impl Into<String>, normal: Grid<Vec3>) -> Self {
        Self::new(name, LayerKind::NormalPaint { normal })
    }

    pub fn photo_height(
        name: impl Into<String>,
        height: Grid<f32>,
        strength: f32,
        blur_radii: [f32; 3],
        weights: [f32; 3],
    ) -> Self {
        Self::new(
            name,
            LayerKind::PhotoHeight(PhotoHeightLayer {
                height,
                strength,
                blur_radii,
                weights,
                alpha: false,
            }),
        )
    }

    /// Imported grayscale/alpha mask converted directly into a height layer.
    /// No blur is applied by default so hard alpha edges survive the bake.
    pub fn alpha_height(name: impl Into<String>, height: Grid<f32>, strength: f32) -> Self {
        Self::new(
            name,
            LayerKind::PhotoHeight(PhotoHeightLayer {
                height,
                strength,
                blur_radii: [0.0; 3],
                weights: [1.0, 0.0, 0.0],
                alpha: true,
            }),
        )
    }

    fn eval(&self, size: UVec2, wrap: Wrap) -> Grid<Vec3> {
        match &self.kind {
            LayerKind::Flat => Grid::new(size.x, size.y, Vec3::Z),
            LayerKind::ImportedNormal { normal } => normal.resample_nearest(size.x, size.y),
            LayerKind::NormalPaint { normal } => normal.resample_nearest(size.x, size.y),
            LayerKind::PhotoHeight(photo) => {
                let h = photo.blurred(wrap);
                let normal = height_to_normal(&h, photo.strength, wrap, false);
                normal.resized_normalized(size.x, size.y)
            }
            LayerKind::Shape(shape) => shape.eval(size, wrap),
            LayerKind::HeightPaint { height, strength } => {
                let h = height.resample_nearest(size.x, size.y);
                height_to_normal(&h, *strength, wrap, false)
            }
            LayerKind::Fill { .. } | LayerKind::Group { .. } => Grid::new(size.x, size.y, Vec3::Z),
        }
    }

    fn eval_height(&self, size: UVec2, wrap: Wrap) -> Option<Grid<f32>> {
        match &self.kind {
            LayerKind::Flat
            | LayerKind::ImportedNormal { .. }
            | LayerKind::NormalPaint { .. }
            | LayerKind::Group { .. } => None,
            LayerKind::PhotoHeight(photo) => Some(photo.blurred(wrap).resized(size.x, size.y)),
            LayerKind::Shape(shape) => Some(shape.height(size, wrap)),
            LayerKind::HeightPaint { height, .. } => Some(height.resample_nearest(size.x, size.y)),
            LayerKind::Fill { height } => Some(Grid::new(size.x, size.y, *height)),
        }
    }

    /// Bounding-box-limited normal evaluation for shapes. Replace blending
    /// needs the flat layer normal outside the box, so it falls back to full.
    fn normal_region(&self, size: UVec2, wrap: Wrap) -> Option<(UVec2, Grid<Vec3>)> {
        if self.blend == BlendMode::Replace {
            return None;
        }
        match &self.kind {
            LayerKind::Shape(shape) => shape.normal_region(size, wrap),
            _ => None,
        }
    }

    fn height_region(&self, size: UVec2, wrap: Wrap) -> Option<(UVec2, Grid<f32>)> {
        match &self.kind {
            LayerKind::Shape(shape) => shape.height_region(size, wrap),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LayerKind {
    Flat,
    ImportedNormal {
        normal: Grid<Vec3>,
    },
    /// Painted tangent-space normals, stored directly instead of derived from
    /// a height field. Normal-map brush strokes write here.
    NormalPaint {
        normal: Grid<Vec3>,
    },
    PhotoHeight(PhotoHeightLayer),
    Shape(ShapeLayer),
    HeightPaint {
        height: Grid<f32>,
        strength: f32,
    },
    Fill {
        height: f32,
    },
    Group {
        children: Vec<Layer>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhotoHeightLayer {
    pub height: Grid<f32>,
    pub strength: f32,
    pub blur_radii: [f32; 3],
    pub weights: [f32; 3],
    /// True when the layer was imported as an alpha mask rather than a photo.
    #[serde(default)]
    pub alpha: bool,
}

impl PhotoHeightLayer {
    fn blurred(&self, wrap: Wrap) -> Grid<f32> {
        let weight_sum: f32 = self.weights.iter().sum();
        if weight_sum.abs() < 1e-6 {
            return self.height.clone();
        }

        let mut acc = Grid::new(self.height.w, self.height.h, 0.0f32);
        for i in 0..3 {
            if self.weights[i].abs() < 1e-6 {
                continue;
            }
            let blurred = gaussian_blur(&self.height, self.blur_radii[i].max(0.0), wrap);
            for (dst, src) in acc.data.iter_mut().zip(blurred.data.iter()) {
                *dst += *src * self.weights[i];
            }
        }
        for v in &mut acc.data {
            *v /= weight_sum;
        }
        acc
    }
}

/// How a shape layer fills its outline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShapeStyle {
    /// Fill the whole shape interior.
    #[default]
    Fill,
    /// Generate a band centred on the shape outline.
    Stroke,
}

impl ShapeStyle {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fill => "Fill",
            Self::Stroke => "Outline",
        }
    }

    pub fn is_stroke(self) -> bool {
        matches!(self, Self::Stroke)
    }
}

fn default_stroke_width() -> f32 {
    8.0
}

/// Normal map captured by a group bake, kept beside the coverage region.
///
/// The group's composited normals cannot be reconstructed from a bevel profile,
/// so the bake stores them directly. `origin` is the coverage origin at bake
/// time; moving the RasterRegion shifts the sampled normals by the same amount,
/// which keeps a baked group movable after its children are gone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BakedNormal {
    pub normal: Grid<Vec3>,
    pub origin: IVec2,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShapeLayer {
    pub source: ShapeSource,
    pub radius_px: f32,
    pub height_px: f32,
    pub profile: Profile,
    pub invert: bool,
    pub softness_px: f32,
    /// Blur applied to the outline before beveling: evens out jagged edges,
    /// noisy mask borders and faceted radii without softening the profile.
    #[serde(default)]
    pub smooth_px: f32,
    /// Fades the bevel normal to flat over this many pixels at the silhouette.
    /// Keeps small shapes and one-pixel radii from stair-stepping without
    /// touching the interior slope.
    #[serde(default)]
    pub edge_aa_px: f32,
    /// Fill the interior or outline the boundary.
    #[serde(default)]
    pub style: ShapeStyle,
    /// Total width of the outline band when [`Self::style`] is
    /// [`ShapeStyle::Stroke`]. The band is centred on the shape boundary.
    #[serde(default = "default_stroke_width")]
    pub stroke_width_px: f32,
    /// Partial Slash only: taper fraction (0–0.5) at the start of the layer's
    /// long axis, so the groove fades instead of ending square.
    #[serde(default)]
    pub cut_taper_in: f32,
    /// Partial Slash only: taper fraction at the end of the long axis.
    #[serde(default)]
    pub cut_taper_out: f32,
    /// Clip Divide only: height offset applied to the far side of the seam, in
    /// document pixels. Zero leaves both sides level.
    #[serde(default)]
    pub cut_step_px: f32,
    /// Surface or path guide. Old projects load as [`VectorRole::Surface`].
    #[serde(default, skip_serializing_if = "VectorRole::is_surface")]
    pub vector_role: VectorRole,
    /// Present on a layer produced by baking a group to pixels. The coverage
    /// source supplies the movable extent; this supplies the exact normals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baked_normal: Option<BakedNormal>,
}

impl ShapeLayer {
    pub fn new(source: ShapeSource) -> Self {
        Self {
            source,
            radius_px: 16.0,
            height_px: 8.0,
            profile: Profile::Round,
            invert: false,
            softness_px: 0.0,
            smooth_px: 0.0,
            edge_aa_px: 0.0,
            style: ShapeStyle::Fill,
            stroke_width_px: default_stroke_width(),
            cut_taper_in: 0.0,
            cut_taper_out: 0.0,
            cut_step_px: 0.0,
            vector_role: VectorRole::Surface,
            baked_normal: None,
        }
    }

    /// Total stroke width, clamped to a usable range.
    pub fn stroke_width(&self) -> f32 {
        self.stroke_width_px.clamp(0.5, 4096.0)
    }

    /// Stroke band of an open line as filled geometry.
    ///
    /// An open line has no interior, so banding its fill would trace the
    /// implicit closing edge instead of the stroke. Expanding the outline
    /// gives the band the pen actually draws, which is also what the knife
    /// cuts with.
    ///
    /// Public so the GPU packer can describe an open stroked line exactly the
    /// way the CPU coverage path does, instead of falling back for the whole
    /// stack.
    pub fn open_band(&self) -> Option<ShapeSource> {
        if !self.style.is_stroke() {
            return None;
        }
        let path = self.source.to_path()?;
        if path.subpaths.is_empty() || path.subpaths.iter().all(|subpath| subpath.closed) {
            return None;
        }
        let band = path.stroke_outline(self.stroke_width())?;
        Some(ShapeSource::Path(band))
    }

    /// Rasterized coverage of the shape, and whether it already is a band.
    fn draw_coverage(&self, size: UVec2) -> (Grid<f32>, bool) {
        self.draw_coverage_region(size, UVec2::ZERO, size.x, size.y)
    }

    fn draw_coverage_region(
        &self,
        size: UVec2,
        origin: UVec2,
        w: u32,
        h: u32,
    ) -> (Grid<f32>, bool) {
        if let Some(band) = self.open_band() {
            return (band.rasterize_region(size, origin, w, h), true);
        }
        (self.source.rasterize_region(size, origin, w, h), false)
    }

    /// Replace the editable vector source with its rasterized draw coverage.
    ///
    /// The bevel parameters (radius, height, profile, invert, style) are kept,
    /// and an open stroked line is baked as its stroke band and switched to a
    /// fill. Evaluating the result therefore produces the same normal map,
    /// while a very complex compound path no longer has to be flattened and
    /// distance-tested on every evaluation.
    ///
    /// The pixels are cropped to the shape's coverage, so the project grows by
    /// roughly `width * height * 4` bytes of the shape's own bounding box, not
    /// the whole document.
    pub fn bake_to_raster(&mut self, size: UVec2) -> std::result::Result<(), &'static str> {
        if matches!(
            self.source,
            ShapeSource::Raster(_) | ShapeSource::RasterRegion(_)
        ) {
            return Err("already a raster layer");
        }
        if let ShapeSource::Path(path) = &self.source {
            if path.has_edge_profiles() {
                return Err("per-edge bevel profiles cannot be baked");
            }
        }
        let (coverage, banded) = self.draw_coverage(size);
        self.source = ShapeSource::RasterRegion(RasterRegion::from_document(&coverage));
        if banded {
            // The band is already the drawn stroke geometry; stroking it again
            // would widen it a second time.
            self.style = ShapeStyle::Fill;
            self.stroke_width_px = 0.0;
        }
        Ok(())
    }

    /// Coverage used when this shape acts as a clipping mask.
    fn mask_coverage(&self, size: UVec2, wrap: Wrap) -> Grid<f32> {
        let (coverage, banded) = self.draw_coverage(size);
        if self.style.is_stroke() && !banded {
            stroke_coverage_aa(&coverage, self.stroke_width(), wrap)
        } else {
            coverage
        }
    }

    /// Outline this shape contributes when it acts as a knife.
    ///
    /// A filled shape cuts with its own outline, a stroked line with its
    /// band: the stroke is expanded once into a closed outline with round
    /// joins and caps, the same geometry the shape uses when it takes part in
    /// a boolean.
    fn cut_operand(&self, size: UVec2) -> Option<VectorOperand> {
        self.boolean_operand(size)
    }

    /// Vector outline this shape contributes to a boolean operation.
    ///
    /// Fill shapes hand over their own outline; stroke shapes hand over the
    /// expanded band. Raster sources have no path topology, so their coverage
    /// is traced once into loops before the boolean runs on it.
    fn boolean_operand(&self, size: UVec2) -> Option<VectorOperand> {
        if matches!(
            self.source,
            ShapeSource::Raster(_) | ShapeSource::RasterRegion(_)
        ) {
            let coverage = self.mask_coverage(size, Wrap::Clamp);
            let traced = coverage_to_path(&coverage, 1.0, Vec2::ZERO, FillRule::EvenOdd)?;
            return traced_operand(&traced, FillRule::EvenOdd);
        }
        if self.style.is_stroke() {
            let path = self.source.to_path()?;
            let width = self.stroke_width();
            let subpaths = flatten_path_tolerance_loops(&path, VECTOR_BOOLEAN_TOLERANCE);
            if !subpaths.is_empty() && subpaths.iter().all(|(_, closed)| !*closed) {
                // Open lines are offset edge by edge: a generic stroke
                // expander lets the inside of a sharp corner cross itself,
                // and that crossing is too thin to classify.
                let loops: Vec<Vec<Vec2>> = subpaths
                    .iter()
                    .filter_map(|(points, _)| stroke_polyline_outline(points, width))
                    .collect();
                let band = VectorOperand::new(loops, FillRule::NonZero);
                return (!band.is_empty()).then_some(band);
            }
            // Closed stroke outlines keep a hole and are meant to be filled
            // with the non-zero rule.
            let band = path.stroke_outline(width)?;
            return traced_operand(&band, FillRule::NonZero);
        }
        let path = self.source.to_path()?;
        let operand = VectorOperand::new(
            flatten_path_tolerance(&path, VECTOR_BOOLEAN_TOLERANCE),
            path.fill_rule,
        );
        (!operand.is_empty()).then_some(operand)
    }

    pub fn params(&self) -> BevelParams {
        BevelParams {
            radius_px: self.radius_px,
            height_px: self.height_px,
            profile: self.profile.clone(),
            invert: self.invert,
            softness_px: self.softness_px,
            smooth_px: self.smooth_px,
            edge_aa_px: self.edge_aa_px,
        }
    }

    /// Rough heap footprint of the shape's source geometry.
    fn estimated_bytes(&self) -> usize {
        let mut bytes = match &self.source {
            ShapeSource::Raster(coverage) => coverage.data.len() * std::mem::size_of::<f32>(),
            ShapeSource::RasterRegion(region) => {
                region.coverage.data.len() * std::mem::size_of::<f32>()
            }
            ShapeSource::Path(path) => path
                .subpaths
                .iter()
                .map(|subpath| subpath.nodes.capacity() * std::mem::size_of::<crate::path::Node>())
                .sum(),
            ShapeSource::Primitive(_) => 0,
            ShapeSource::Text(text) => text.content.len(),
        };
        if let Some(baked) = &self.baked_normal {
            bytes += baked.normal.data.len() * std::mem::size_of::<Vec3>();
        }
        bytes
    }

    /// Bevel an already-combined coverage mask. Operand stroke bands are part
    /// of the coverage, so a boolean result is always filled and beveled from
    /// its outer boundaries (including holes).
    fn eval_coverage(&self, coverage: &Grid<f32>, wrap: Wrap) -> Grid<Vec3> {
        bevel_mask_aa(coverage, &self.params(), wrap)
    }

    /// Nearest-edge field for per-edge profiles. Only a path that actually
    /// carries overrides builds one, and only then does the bevel leave the
    /// uniform fast path.
    fn edge_field(&self, origin: UVec2, w: u32, h: u32) -> Option<EdgeProfileField> {
        let ShapeSource::Path(path) = &self.source else {
            return None;
        };
        if !path.has_edge_profiles() {
            return None;
        }
        let (segments, profiles) = path.flatten_edges();
        if segments.is_empty()
            || profiles
                .iter()
                .flatten()
                .all(|profile| *profile == self.profile)
        {
            return None;
        }
        let radius = self.radius_px.max(1e-4);
        let stroke_half = if self.style.is_stroke() {
            self.stroke_width() * 0.5
        } else {
            0.0
        };
        let reach = radius + stroke_half + 2.0;
        let blend_width = (radius * 0.35).clamp(1.0, 4.0);
        let total = (w as usize) * (h as usize);
        let mut ids = Grid::new(w, h, u16::MAX);
        let mut second = Grid::new(w, h, u16::MAX);
        let mut blend = Grid::new(w, h, 0.0f32);
        let mut first_distance = vec![f32::INFINITY; total];
        let mut second_distance = vec![f32::INFINITY; total];
        let ox = origin.x as f32;
        let oy = origin.y as f32;
        let max_x = w.saturating_sub(1) as f32;
        let max_y = h.saturating_sub(1) as f32;

        // Tag pixels near each flattened piece with the closest source edge.
        // Walking each piece's expanded box keeps this proportional to the
        // bevel band rather than pixels × edges.
        for segment in &segments {
            let lo = segment.a.min(segment.b) - Vec2::splat(reach);
            let hi = segment.a.max(segment.b) + Vec2::splat(reach);
            let x0 = (lo.x - ox).floor();
            let y0 = (lo.y - oy).floor();
            let x1 = (hi.x - ox).ceil();
            let y1 = (hi.y - oy).ceil();
            if x1 < 0.0 || y1 < 0.0 || x0 > max_x || y0 > max_y {
                continue;
            }
            let x0 = x0.max(0.0) as u32;
            let y0 = y0.max(0.0) as u32;
            let x1 = x1.min(max_x) as u32;
            let y1 = y1.min(max_y) as u32;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let point = Vec2::new(ox + x as f32 + 0.5, oy + y as f32 + 0.5);
                    let distance = point_segment_distance(point, segment.a, segment.b);
                    if distance > reach {
                        continue;
                    }
                    let index = (y as usize) * (w as usize) + x as usize;
                    let best = *ids.get(x, y);
                    if distance < first_distance[index] {
                        if best != segment.edge {
                            second_distance[index] = first_distance[index];
                            second.set(x, y, best);
                        }
                        first_distance[index] = distance;
                        ids.set(x, y, segment.edge);
                    } else if segment.edge != best && distance < second_distance[index] {
                        second_distance[index] = distance;
                        second.set(x, y, segment.edge);
                    }
                }
            }
        }

        // Fade between the two nearest edges near a corner. Away from the
        // bisector one edge owns the pixel, so the transition is smooth.
        for y in 0..h {
            for x in 0..w {
                let index = (y as usize) * (w as usize) + x as usize;
                let first = *ids.get(x, y);
                let other = *second.get(x, y);
                if other == u16::MAX || other == first || !second_distance[index].is_finite() {
                    continue;
                }
                let delta = (second_distance[index] - first_distance[index]).max(0.0);
                let t = (delta / blend_width).clamp(0.0, 1.0);
                let smooth = t * t * (3.0 - 2.0 * t);
                blend.set(x, y, 0.5 * (1.0 - smooth));
            }
        }

        Some(EdgeProfileField {
            ids,
            second,
            blend,
            profiles,
            default: self.profile.clone(),
        })
    }

    fn height_coverage(&self, coverage: &Grid<f32>, wrap: Wrap) -> Grid<f32> {
        height_mask_aa(coverage, &self.params(), wrap)
    }

    /// True when the coverage is a fill that still needs banding.
    fn needs_band(&self, banded: bool) -> bool {
        self.style.is_stroke() && !banded
    }

    fn eval(&self, size: UVec2, wrap: Wrap) -> Grid<Vec3> {
        if let Some(baked) = &self.baked_normal {
            // The group bake already composited the exact normals; only the
            // movable coverage origin can differ from the bake position.
            let origin = match &self.source {
                ShapeSource::RasterRegion(region) => region.origin,
                _ => baked.origin,
            };
            let shift = origin - baked.origin;
            let (w, h) = (baked.normal.w, baked.normal.h);
            return Grid::from_fn(size.x, size.y, |x, y| {
                let sx = x as i32 - shift.x;
                let sy = y as i32 - shift.y;
                if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                    Vec3::Z
                } else {
                    *baked.normal.get(sx as u32, sy as u32)
                }
            });
        }
        let (coverage, banded) = self.draw_coverage(size);
        let edges = self.edge_field(UVec2::ZERO, size.x, size.y);
        if self.needs_band(banded) {
            bevel_mask_stroke_aa_with(
                &coverage,
                &self.params(),
                self.stroke_width(),
                edges.as_ref(),
                wrap,
            )
        } else {
            bevel_mask_aa_with(&coverage, &self.params(), edges.as_ref(), wrap)
        }
    }

    fn height(&self, size: UVec2, wrap: Wrap) -> Grid<f32> {
        if self.baked_normal.is_some() {
            // A group bake carries normals, not a height field (like the
            // ImportedNormal layer it replaces).
            return Grid::new(size.x, size.y, 0.0);
        }
        let (coverage, banded) = self.draw_coverage(size);
        let edges = self.edge_field(UVec2::ZERO, size.x, size.y);
        if self.needs_band(banded) {
            height_mask_stroke_aa_with(
                &coverage,
                &self.params(),
                self.stroke_width(),
                edges.as_ref(),
                wrap,
            )
        } else {
            height_mask_aa_with(&coverage, &self.params(), edges.as_ref(), wrap)
        }
    }

    fn region_rect(&self, size: UVec2) -> Option<(UVec2, u32, u32)> {
        let bounds = self.source.bounds()?;
        let stroke_pad = if self.style.is_stroke() {
            self.stroke_width() * 0.5
        } else {
            0.0
        };
        let pad = self.radius_px.max(0.0)
            + self.softness_px * 3.0
            + self.smooth_px * 3.0
            + stroke_pad
            + 2.0;
        let min = bounds.min - Vec2::splat(pad);
        let max = bounds.max + Vec2::splat(pad);
        let x0 = min.x.floor().clamp(0.0, size.x as f32) as u32;
        let y0 = min.y.floor().clamp(0.0, size.y as f32) as u32;
        let x1 = (max.x.ceil() + 1.0).clamp(0.0, size.x as f32) as u32;
        let y1 = (max.y.ceil() + 1.0).clamp(0.0, size.y as f32) as u32;
        (x1 > x0 && y1 > y0).then_some((UVec2::new(x0, y0), x1 - x0, y1 - y0))
    }

    fn normal_region(&self, size: UVec2, wrap: Wrap) -> Option<(UVec2, Grid<Vec3>)> {
        if wrap == Wrap::Repeat || self.baked_normal.is_some() {
            return None;
        }
        let (origin, w, h) = self.region_rect(size)?;
        let (coverage, banded) = self.draw_coverage_region(size, origin, w, h);
        let edges = self.edge_field(origin, w, h);
        let normals = if self.needs_band(banded) {
            bevel_mask_stroke_aa_with(
                &coverage,
                &self.params(),
                self.stroke_width(),
                edges.as_ref(),
                Wrap::Clamp,
            )
        } else {
            bevel_mask_aa_with(&coverage, &self.params(), edges.as_ref(), Wrap::Clamp)
        };
        Some((origin, normals))
    }

    fn height_region(&self, size: UVec2, wrap: Wrap) -> Option<(UVec2, Grid<f32>)> {
        if wrap == Wrap::Repeat || self.baked_normal.is_some() {
            return None;
        }
        let (origin, w, h) = self.region_rect(size)?;
        let (coverage, banded) = self.draw_coverage_region(size, origin, w, h);
        let edges = self.edge_field(origin, w, h);
        let heights = if self.needs_band(banded) {
            height_mask_stroke_aa_with(
                &coverage,
                &self.params(),
                self.stroke_width(),
                edges.as_ref(),
                Wrap::Clamp,
            )
        } else {
            height_mask_aa_with(&coverage, &self.params(), edges.as_ref(), Wrap::Clamp)
        };
        Some((origin, heights))
    }
}

/// Distance from `point` to the line piece `a..b`.
fn point_segment_distance(point: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let length_squared = ab.length_squared();
    if length_squared <= 1e-8 {
        return point.distance(a);
    }
    let t = ((point - a).dot(ab) / length_squared).clamp(0.0, 1.0);
    point.distance(a + ab * t)
}

/// Hard coverage of a painted normal layer: a texel belongs to the layer when
/// its normal is no longer the untouched flat (0, 0, 1). The threshold is tiny
/// so a faint stroke still exports an ID while a fresh layer exports nothing.
fn painted_normal_coverage(normal: &Grid<Vec3>) -> Grid<f32> {
    Grid::from_fn(normal.w, normal.h, |x, y| {
        let n = normal.get(x, y);
        if (n.z - 1.0).abs() <= 1e-4 && n.x.abs() <= 1e-4 && n.y.abs() <= 1e-4 {
            0.0
        } else {
            1.0
        }
    })
}

/// Hard coverage of a height-paint layer: any non-zero height counts as paint.
fn painted_height_coverage(height: &Grid<f32>) -> Grid<f32> {
    Grid::from_fn(height.w, height.h, |x, y| {
        if height.get(x, y).abs() <= 1e-6 {
            0.0
        } else {
            1.0
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::{Node, Primitive, SubPath};
    use glam::Vec2;

    #[test]
    fn a_new_base_document_starts_with_one_flat_layer() {
        let doc = Document::new_with_base(UVec2::new(32, 32));
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "base");
        assert!(matches!(doc.layers[0].kind, LayerKind::Flat));
    }

    #[test]
    fn empty_document_is_flat() {
        let doc = Document::new(UVec2::new(32, 32));
        let n = doc.evaluate();
        for v in n.iter() {
            assert!((*v - Vec3::Z).length() < 1e-5);
        }
    }

    #[test]
    fn baking_a_shape_to_raster_preserves_its_normals() {
        let size = UVec2::splat(64);
        let mut doc = Document::new(size);
        doc.layers.push(Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(32.0),
                size: Vec2::splat(40.0),
                corner_radius: 6.0,
            }),
        ));
        let LayerKind::Shape(shape) = &mut doc.layers[0].kind else {
            panic!("expected a shape")
        };
        shape.radius_px = 8.0;
        shape.height_px = 6.0;
        shape.profile = Profile::Smooth;
        shape.invert = true;

        let before = doc.evaluate();
        let LayerKind::Shape(shape) = &mut doc.layers[0].kind else {
            panic!("expected a shape")
        };
        shape.bake_to_raster(size).expect("shape should bake");
        assert!(matches!(shape.source, ShapeSource::RasterRegion(_)));

        let after = doc.evaluate();
        let mut worst = 0.0f32;
        for (a, b) in before.data.iter().zip(after.data.iter()) {
            worst = worst.max((*a - *b).length());
        }
        assert!(worst < 1e-5, "baked normals changed by {worst}");
    }

    fn baked_rect(size: UVec2) -> (Document, Bounds) {
        let mut doc = Document::new(size);
        doc.layers.push(Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(size.x as f32 * 0.5),
                size: Vec2::splat(size.x as f32 * 0.6),
                corner_radius: 6.0,
            }),
        ));
        let LayerKind::Shape(shape) = &mut doc.layers[0].kind else {
            panic!("expected a shape")
        };
        shape.radius_px = 4.0;
        shape.bake_to_raster(size).expect("shape should bake");
        let bounds = shape.source.bounds().expect("baked layer has bounds");
        (doc, bounds)
    }

    #[test]
    fn baking_stores_only_the_shape_bounds() {
        let (doc, bounds) = baked_rect(UVec2::splat(256));
        let LayerKind::Shape(shape) = &doc.layers[0].kind else {
            panic!("expected a shape")
        };
        let ShapeSource::RasterRegion(region) = &shape.source else {
            panic!("expected a cropped raster")
        };
        // A 60% rect in the middle of a 256 px document: cropping must keep
        // the stored grid far smaller than the document.
        assert!(region.coverage.w < 200, "raster is not cropped");
        assert!(region.coverage.h < 200, "raster is not cropped");
        assert!((bounds.min.x - 51.0).abs() < 2.0, "bounds {bounds:?}");
        assert!((bounds.max.x - 205.0).abs() < 2.0, "bounds {bounds:?}");
    }

    #[test]
    fn a_baked_layer_moves_with_whole_pixel_steps() {
        let (mut doc, bounds) = baked_rect(UVec2::splat(128));
        let LayerKind::Shape(shape) = &mut doc.layers[0].kind else {
            panic!("expected a shape")
        };
        shape.source = shape
            .source
            .transformed(Vec2::ONE, Vec2::new(10.0, -4.0))
            .expect("baked pixels should translate");
        let moved = shape.source.bounds().expect("moved layer has bounds");
        assert!((moved.min.x - (bounds.min.x + 10.0)).abs() < 1e-4);
        assert!((moved.min.y - (bounds.min.y - 4.0)).abs() < 1e-4);
        assert!(shape.source.contains(moved.center()));
        doc.layers[0].touch();

        let after = doc.evaluate();
        // The bevel moved with the pixels instead of being left behind.
        let edge = *after.get(moved.min.x as u32 + 2, moved.center().y as u32);
        assert!(
            edge.x.abs() > 1e-3 || edge.y.abs() > 1e-3,
            "translated bevel went flat"
        );
    }

    #[test]
    fn a_baked_layer_scales_with_the_resize_handles() {
        let (mut doc, bounds) = baked_rect(UVec2::splat(128));
        let LayerKind::Shape(shape) = &mut doc.layers[0].kind else {
            panic!("expected a shape")
        };
        let scaled = shape
            .source
            .transformed(Vec2::splat(2.0), Vec2::ZERO)
            .expect("baked pixels should scale");
        let after = scaled.bounds().expect("scaled layer has bounds");
        assert!(after.size().x > bounds.size().x * 1.9);
        assert!(matches!(scaled, ShapeSource::RasterRegion(_)));
        shape.source = scaled;
        doc.layers[0].touch();
        // The enlarged layer still evaluates to a valid normal map.
        let normals = doc.evaluate();
        assert!(normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-3));
    }

    #[test]
    fn layers_only_contribute_to_their_udim_tile() {
        let tile_a = Vec3::new(0.6, 0.0, 0.8).normalize();
        let tile_b = Vec3::new(0.0, 0.6, 0.8).normalize();
        let mut doc = Document::new(UVec2::splat(8));
        let mut a = Layer::imported_normal("a", Grid::new(8, 8, tile_a));
        a.tile = Some(1001);
        let mut b = Layer::imported_normal("b", Grid::new(8, 8, tile_b));
        b.tile = Some(1002);
        doc.layers = vec![a, b];

        doc.active_tile = 1001;
        let first = doc.evaluate();
        assert!((*first.get(4, 4) - tile_a).length() < 1e-4);
        doc.active_tile = 1002;
        let second = doc.evaluate();
        assert!((*second.get(4, 4) - tile_b).length() < 1e-4);
        assert_eq!(doc.layer_tiles(), vec![1001, 1002]);
    }

    #[test]
    fn untagged_layers_contribute_to_every_udim_tile() {
        let shared = Vec3::new(0.5, 0.0, 0.866_025_4).normalize();
        let mut doc = Document::new(UVec2::splat(8));
        doc.layers
            .push(Layer::imported_normal("shared", Grid::new(8, 8, shared)));
        for tile in [1001, 1002, 1011] {
            doc.active_tile = tile;
            let normal = doc.evaluate();
            assert!(
                (*normal.get(4, 4) - shared).length() < 1e-4,
                "shared layer missing on tile {tile}"
            );
        }
    }

    #[test]
    fn a_global_group_only_evaluates_children_of_the_active_tile() {
        let tile_normal = Vec3::new(0.0, 0.6, 0.8).normalize();
        let mut child = Layer::imported_normal("child", Grid::new(8, 8, tile_normal));
        child.tile = Some(1002);
        let mut doc = Document::new(UVec2::splat(8));
        doc.layers = vec![Layer::group("group", vec![child])];

        doc.active_tile = 1001;
        let other = doc.evaluate();
        assert!((*other.get(4, 4) - Vec3::Z).length() < 1e-4);
        doc.active_tile = 1002;
        let own = doc.evaluate();
        assert!((*own.get(4, 4) - tile_normal).length() < 1e-4);
    }

    #[test]
    fn per_tile_evaluation_reuses_one_cache() {
        let tile_a = Vec3::new(0.6, 0.0, 0.8).normalize();
        let tile_b = Vec3::new(0.0, 0.6, 0.8).normalize();
        let mut doc = Document::new(UVec2::splat(8));
        let mut a = Layer::imported_normal("a", Grid::new(8, 8, tile_a));
        a.tile = Some(1001);
        let mut b = Layer::imported_normal("b", Grid::new(8, 8, tile_b));
        b.tile = Some(1002);
        doc.layers = vec![a, b];
        let mut cache = EvalCache::default();

        doc.active_tile = 1001;
        assert!((*doc.evaluate_with(&mut cache).get(4, 4) - tile_a).length() < 1e-4);
        doc.active_tile = 1002;
        assert!((*doc.evaluate_with(&mut cache).get(4, 4) - tile_b).length() < 1e-4);

        // The cache override evaluates another tile without changing the
        // document, which is what per-tile export uses.
        doc.active_tile = 1001;
        cache.tile = Some(1002);
        assert!((*doc.evaluate_with(&mut cache).get(4, 4) - tile_b).length() < 1e-4);
        assert_eq!(doc.active_tile, 1001);
        cache.tile = None;
    }

    #[test]
    fn id_map_only_paints_layers_on_the_active_tile() {
        let mut doc = Document::new(UVec2::splat(8));
        let mut a = Layer::fill("a", 0.0);
        a.id_color = Some([1.0, 0.0, 0.0]);
        a.tile = Some(1001);
        let mut b = Layer::fill("b", 0.0);
        b.id_color = Some([0.0, 1.0, 0.0]);
        b.tile = Some(1002);
        doc.layers = vec![a, b];

        doc.active_tile = 1001;
        assert_eq!(
            doc.evaluate_id_map([0.0, 0.0, 0.0]).get(4, 4),
            &[1.0, 0.0, 0.0]
        );
        doc.active_tile = 1002;
        assert_eq!(
            doc.evaluate_id_map([0.0, 0.0, 0.0]).get(4, 4),
            &[0.0, 1.0, 0.0]
        );
    }

    #[test]
    fn imported_normal_base_blends_with_detail_above() {
        let size = UVec2::new(16, 16);
        let baked = Vec3::new(0.0, 0.4, 0.916_515_1).normalize();
        let detail = Vec3::new(0.3, 0.0, 0.953_939_2).normalize();
        let mut doc = Document::new(size);
        doc.layers
            .push(Layer::imported_normal("baked", Grid::new(16, 16, baked)));
        let base_only = doc.evaluate();
        assert!((*base_only.get(8, 8) - baked).length() < 1e-4);

        doc.layers
            .push(Layer::imported_normal("detail", Grid::new(16, 16, detail)));
        let mixed = doc.evaluate();
        let expected = crate::blend::derivative_blend(baked, detail);
        assert!((*mixed.get(8, 8) - expected).length() < 1e-4);
        assert!((*mixed.get(8, 8) - baked).length() > 0.1);
    }

    #[test]
    fn alpha_height_layer_scales_to_document_size() {
        let mut doc = Document::new(UVec2::new(32, 32));
        let height = Grid::from_fn(8, 8, |x, _| x as f32 / 7.0);
        doc.layers.push(Layer::alpha_height("alpha", height, 1.0));
        let normal = doc.evaluate();
        assert_eq!((normal.w, normal.h), (32, 32));
        assert!(normal.iter().any(|value| value.x.abs() > 1e-4));
    }

    #[test]
    fn fill_layer_is_flat() {
        let mut doc = Document::new(UVec2::new(32, 32));
        doc.layers.push(Layer::fill("fill", 0.5));
        let n = doc.evaluate();
        for v in n.iter() {
            assert!((*v - Vec3::Z).length() < 1e-5);
        }
    }

    #[test]
    fn shape_layer_has_edge_normals() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut layer = Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(32.0, 32.0),
                size: Vec2::new(32.0, 32.0),
                corner_radius: 4.0,
            }),
        );
        // The default radius (16 px) would make the bevel bands meet in the
        // middle of a 32 px-wide rect, leaving no flat center.
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 8.0;
        }
        doc.layers.push(layer);
        let n = doc.evaluate();
        assert!((*n.get(32, 32) - Vec3::Z).length() < 1e-4);
        let edge = *n.get(16, 32);
        assert!(edge.x.abs() > 0.05);
        assert!((edge.length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn id_map_edges_stay_hard_at_the_bevel_silhouette() {
        let mut doc = Document::new(UVec2::splat(64));
        let mut layer = rect_layer("panel", Vec2::splat(32.0), Vec2::splat(32.0));
        layer.id_color = Some([1.0, 0.0, 0.0]);
        doc.layers.push(layer);

        let ids = doc.evaluate_id_map([0.0, 0.0, 0.0]);
        let red = [1.0f32, 0.0, 0.0];
        let black = [0.0f32, 0.0, 0.0];
        // Hard edge: only the assigned colours exist, never a blend of them.
        for color in ids.iter() {
            assert!(*color == red || *color == black, "ID map blended {color:?}");
        }

        // The ID boundary is the bevel silhouette: the outermost covered
        // pixel is the outermost pixel whose normal leaves the flat plane.
        let normal = doc.evaluate();
        assert_eq!(ids.get(32, 32), &red);
        assert_eq!(ids.get(16, 32), &red);
        assert_eq!(ids.get(15, 32), &black);
        assert_eq!(ids.get(47, 32), &red);
        assert_eq!(ids.get(48, 32), &black);
        assert!((*normal.get(15, 32) - Vec3::Z).length() < 0.01);
        assert!((*normal.get(16, 32) - Vec3::Z).length() > 0.01);
        assert!((*normal.get(47, 32) - Vec3::Z).length() > 0.01);
        assert!((*normal.get(48, 32) - Vec3::Z).length() < 0.01);
    }

    #[test]
    fn id_map_follows_painted_masks_and_clipping() {
        let mut doc = Document::new(UVec2::splat(64));
        let mut base = rect_layer("base", Vec2::splat(32.0), Vec2::splat(64.0));
        base.id_color = Some([0.0, 0.0, 1.0]);
        doc.layers.push(base);

        let mut top = rect_layer("top", Vec2::splat(32.0), Vec2::splat(64.0));
        top.id_color = Some([1.0, 0.0, 0.0]);
        // The painted mask crosses 50 % at x = 22 and x = 42, so the ID edge
        // lands on the mask's own half-coverage line.
        top.mask = Some(Grid::from_fn(64, 64, |x, _| match x {
            10..=21 => 0.6,
            22..=31 => 0.4,
            32..=41 => 0.9,
            42..=53 => 0.1,
            _ => 0.0,
        }));
        doc.layers.push(top);

        let clip_source = rect_layer("clip", Vec2::new(32.0, 48.0), Vec2::new(64.0, 32.0));
        let clip_id = clip_source.id;
        doc.layers.push(clip_source);
        let mut clipped = rect_layer("clipped", Vec2::splat(32.0), Vec2::splat(64.0));
        clipped.id_color = Some([0.0, 1.0, 0.0]);
        clipped.mask_layer = Some(clip_id);
        doc.layers.push(clipped);

        let ids = doc.evaluate_id_map([0.0, 0.0, 0.0]);
        let red = [1.0f32, 0.0, 0.0];
        let green = [0.0f32, 1.0, 0.0];
        let blue = [0.0f32, 0.0, 1.0];
        assert_eq!(ids.get(16, 16), &red);
        assert_eq!(ids.get(26, 16), &blue);
        assert_eq!(ids.get(36, 16), &red);
        assert_eq!(ids.get(48, 16), &blue);
        // The clipped layer only owns the half its clip mask covers.
        assert_eq!(ids.get(16, 48), &green);
        assert_eq!(ids.get(48, 48), &green);
    }

    #[test]
    fn id_map_shows_group_colours_and_child_overrides() {
        let large = rect_layer("large", Vec2::splat(32.0), Vec2::splat(32.0));
        let mut small = rect_layer("small", Vec2::splat(32.0), Vec2::splat(16.0));
        small.id_color = Some([1.0, 0.0, 0.0]);
        let mut group = Layer::group("group", vec![large, small]);
        group.id_color = Some([0.0, 0.0, 1.0]);

        let mut doc = Document::new(UVec2::splat(64));
        doc.layers.push(group);
        let ids = doc.evaluate_id_map([0.0, 0.0, 0.0]);
        // Outside the child the group colour remains; inside, the child wins.
        assert_eq!(ids.get(20, 20), &[0.0, 0.0, 1.0]);
        assert_eq!(ids.get(32, 32), &[1.0, 0.0, 0.0]);
        assert!(doc.has_id_colors());
    }

    #[test]
    fn id_map_treats_a_vector_compound_as_one_surface() {
        let left = rect_layer("left", Vec2::new(24.0, 32.0), Vec2::new(24.0, 24.0));
        let right = rect_layer("right", Vec2::new(40.0, 32.0), Vec2::new(24.0, 24.0));
        let mut group = Layer::group("compound", vec![left, right]);
        group.vector_op = Some(VectorOp::Add);
        group.id_color = Some([0.0, 0.8, 0.0]);

        let mut doc = Document::new(UVec2::splat(64));
        doc.layers.push(group);
        doc.layers[0].children_mut().unwrap()[0].id_color = Some([1.0, 0.0, 0.0]);
        doc.layers[0].children_mut().unwrap()[1].id_color = Some([0.0, 0.0, 1.0]);

        let ids = doc.evaluate_id_map([0.0, 0.0, 0.0]);
        let green = [0.0f32, 0.8, 0.0];
        assert_eq!(ids.get(24, 32), &green);
        assert_eq!(ids.get(40, 32), &green);
        assert_eq!(ids.get(2, 2), &[0.0, 0.0, 0.0]);
    }

    #[test]
    fn id_map_ignores_opacity_but_skips_hidden_layers() {
        let mut doc = Document::new(UVec2::splat(32));
        let mut base = rect_layer("base", Vec2::splat(16.0), Vec2::splat(32.0));
        base.id_color = Some([0.0, 0.0, 1.0]);
        doc.layers.push(base);
        let mut top = rect_layer("top", Vec2::splat(16.0), Vec2::splat(16.0));
        top.id_color = Some([1.0, 0.0, 0.0]);
        top.opacity = 0.2;
        doc.layers.push(top);

        let ids = doc.evaluate_id_map([0.0, 0.0, 0.0]);
        assert_eq!(ids.get(16, 16), &[1.0, 0.0, 0.0]);

        doc.layers[1].visible = false;
        let ids = doc.evaluate_id_map([0.0, 0.0, 0.0]);
        assert_eq!(ids.get(16, 16), &[0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_path_guide_does_not_paint_but_stays_visible() {
        let mut doc = Document::new(UVec2::splat(32));
        doc.layers
            .push(rect_layer("base", Vec2::splat(16.0), Vec2::new(12.0, 12.0)));

        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        subpath.nodes.push(Node::corner(Vec2::new(4.0, 24.0)));
        subpath.nodes.push(Node::corner(Vec2::new(28.0, 24.0)));
        path.subpaths.push(subpath);
        let mut guide = Layer::shape("Spine", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut guide.kind {
            shape.vector_role = VectorRole::Path;
            // Whatever the bevel would be, a guide must not reach the maps.
            shape.radius_px = 6.0;
            shape.height_px = 40.0;
        }
        doc.layers.push(guide);

        assert!(doc.layers[1].visible, "a guide is not hidden by the role");
        assert!(doc.layers[1].is_path_guide());
        assert!(!doc.layers[1].paints());
        let normal = doc.evaluate();
        let height = doc.evaluate_height();
        assert!(
            height.data.iter().any(|value| *value > 0.01),
            "the base shape still contributes"
        );

        doc.layers.pop();
        let without_normal = doc.evaluate();
        let without_height = doc.evaluate_height();
        assert!(normal
            .data
            .iter()
            .zip(without_normal.data.iter())
            .all(|(a, b)| (*a - *b).length() < 1e-6));
        assert!(height
            .data
            .iter()
            .zip(without_height.data.iter())
            .all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn a_path_guide_is_not_a_boolean_operand() {
        let mut base = rect_layer("base", Vec2::splat(16.0), Vec2::splat(20.0));
        if let LayerKind::Shape(shape) = &mut base.kind {
            shape.radius_px = 2.0;
            shape.height_px = 10.0;
        }
        let mut guide = rect_layer("guide", Vec2::splat(16.0), Vec2::splat(4.0));
        if let LayerKind::Shape(shape) = &mut guide.kind {
            shape.source = ShapeSource::Path(Path::from_polygon(&[
                Vec2::new(2.0, 14.0),
                Vec2::new(30.0, 14.0),
                Vec2::new(30.0, 18.0),
                Vec2::new(2.0, 18.0),
            ]));
            shape.vector_role = VectorRole::Path;
            shape.radius_px = 20.0;
            shape.height_px = 40.0;
        }
        let mut with_guide = Document::new(UVec2::splat(32));
        let mut group = Layer::group("union", vec![base.clone(), guide]);
        group.vector_op = Some(VectorOp::Add);
        with_guide.layers.push(group);

        let mut plain = Document::new(UVec2::splat(32));
        let mut group = Layer::group("union", vec![base]);
        group.vector_op = Some(VectorOp::Add);
        plain.layers.push(group);

        let with_guide = with_guide.evaluate_height();
        let plain = plain.evaluate_height();
        assert!(with_guide
            .data
            .iter()
            .zip(plain.data.iter())
            .all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn an_id_on_a_paint_layer_follows_the_painted_pixels() {
        let mut normal = Grid::new(32, 32, Vec3::Z);
        normal.set(10, 10, Vec3::new(0.4, 0.0, 0.916));
        let mut paint = Layer::normal_paint("paint", normal);
        paint.id_color = Some([1.0, 0.0, 0.0]);

        let mut doc = Document::new(UVec2::splat(32));
        doc.layers.push(paint);
        let ids = doc.evaluate_id_map([0.0, 0.0, 0.0]);
        assert_eq!(ids.get(10, 10), &[1.0, 0.0, 0.0]);
        assert_eq!(
            ids.get(0, 0),
            &[0.0, 0.0, 0.0],
            "unpainted texels keep the background"
        );
    }

    #[test]
    fn taller_shapes_shade_deeper() {
        let build = |height: f32| {
            let mut layer = rect_layer("panel", Vec2::splat(32.0), Vec2::splat(32.0));
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = 10.0;
                shape.height_px = height;
            }
            let mut doc = Document::new(UVec2::splat(64));
            doc.layers.push(layer);
            doc.evaluate()
        };
        let shallow = build(4.0);
        let deep = build(16.0);
        let shallow_slope = {
            let n = *shallow.get(17, 32);
            (n.x / n.z).abs()
        };
        let deep_slope = {
            let n = *deep.get(17, 32);
            (n.x / n.z).abs()
        };
        assert!(
            deep_slope > shallow_slope * 3.0,
            "4x Depth must read much deeper: {shallow_slope} -> {deep_slope}"
        );
    }

    #[test]
    fn a_per_edge_profile_changes_only_that_edge() {
        let mut path = Path::from_polygon(&[
            Vec2::new(32.0, 32.0),
            Vec2::new(96.0, 32.0),
            Vec2::new(96.0, 96.0),
            Vec2::new(32.0, 96.0),
        ]);
        // The top edge becomes a linear ramp; every other edge keeps the
        // shape's Round profile.
        path.set_edge_profile(0, 0, Some(Profile::Linear));
        let mut layer = Layer::shape("panel", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 12.0;
            shape.height_px = 12.0;
        }
        let mut doc = Document::new(UVec2::splat(128));
        doc.layers.push(layer);
        let normals = doc.evaluate();
        // Same distance from the top and bottom silhouettes (t ~= 0.5).
        let top = *normals.get(64, 38);
        let bottom = *normals.get(64, 90);
        let top_slope = (top.y / top.z).abs();
        let bottom_slope = (bottom.y / bottom.z).abs();
        assert!(
            top_slope > bottom_slope * 1.4,
            "the Linear edge must be steeper than the Round edges: top {top_slope}, bottom {bottom_slope}"
        );
    }

    #[test]
    fn mask_edit_reuses_normal_cache_and_updates_composite() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut layer = Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(32.0, 32.0),
                size: Vec2::new(32.0, 32.0),
                corner_radius: 4.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 8.0;
        }
        layer.mask = Some(Grid::new(64, 64, 1.0));
        doc.layers.push(layer);

        let mut cache = EvalCache::new();
        let before = doc.evaluate_auto(&mut cache, DEFAULT_EVAL_CACHE_BUDGET);
        assert!(before.get(20, 32).x.abs() > 0.01);

        let content_before = doc.layers[0].content_revision();
        doc.layers[0].mask.as_mut().unwrap().data.fill(0.0);
        doc.layers[0].touch_mask();
        assert_eq!(doc.layers[0].content_revision(), content_before);
        assert_eq!(doc.layers[0].mask_revision, 1);

        let after = doc.evaluate_auto(&mut cache, DEFAULT_EVAL_CACHE_BUDGET);
        assert!(after.iter().all(|n| (*n - Vec3::Z).length() < 1e-4));
    }

    #[test]
    fn mask_edits_do_not_invalidate_layer_content() {
        let mut layer = Layer::flat("base");
        layer.mask = Some(Grid::new(8, 8, 1.0));
        let before = layer.content_revision();
        layer.touch_mask();
        assert_eq!(layer.content_revision(), before);
        assert_eq!(layer.mask_revision, 1);
    }

    #[test]
    fn mask_scales_slope() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut layer = Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(32.0, 32.0),
                size: Vec2::new(32.0, 32.0),
                corner_radius: 4.0,
            }),
        );
        let mut mask = Grid::new(64, 64, 1.0);
        for y in 0..64 {
            for x in 0..64 {
                mask.set(x, y, if x < 32 { 1.0 } else { 0.0 });
            }
        }
        layer.mask = Some(mask);
        doc.layers.push(layer);
        let n = doc.evaluate();
        assert!(n.get(20, 32).x.abs() > 0.01);
        assert!((*n.get(44, 32) - Vec3::Z).length() < 1e-4);
    }

    #[test]
    fn group_painted_mask_clips_its_children() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let child = Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(32.0, 32.0),
                size: Vec2::new(32.0, 32.0),
                corner_radius: 4.0,
            }),
        );
        let mut group = Layer::group("group", vec![child]);
        let mut mask = Grid::new(64, 64, 0.0);
        for y in 0..64 {
            for x in 0..32 {
                mask.set(x, y, 1.0);
            }
        }
        group.mask = Some(mask);
        doc.layers.push(group);
        let n = doc.evaluate();
        assert!(n.get(20, 32).x.abs() > 0.01);
        assert!((*n.get(44, 32) - Vec3::Z).length() < 1e-4);
    }

    #[test]
    fn cache_matches_uncached_and_invalidates() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut layer = Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(32.0, 32.0),
                size: Vec2::new(40.0, 40.0),
                corner_radius: 4.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 8.0;
        }
        doc.layers.push(layer);

        let mut cache = EvalCache::new();
        let first = doc.evaluate_with(&mut cache);
        assert_eq!(cache.cached_layers(), 1);

        let cached = doc.evaluate_with(&mut cache);
        assert_eq!(first.data, cached.data);

        // Direct mutation without `touch` reuses the cache.
        if let LayerKind::Shape(shape) = &mut doc.layers[0].kind {
            shape.radius_px = 2.0;
        }
        let stale = doc.evaluate_with(&mut cache);
        assert_eq!(first.data, stale.data);

        doc.layers[0].touch();
        let fresh = doc.evaluate_with(&mut cache);
        let uncached = doc.evaluate();
        assert!((*fresh.get(12, 32) - *stale.get(12, 32)).length() > 1e-3);
        for (a, b) in fresh.data.iter().zip(uncached.data.iter()) {
            assert!((*a - *b).length() < 1e-6);
        }

        let height = doc.evaluate_height_with(&mut cache);
        assert_eq!(height.w, 64);
    }

    #[test]
    fn streaming_and_cached_paths_match() {
        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(32.0),
                size: Vec2::new(42.0, 30.0),
                corner_radius: 5.0,
            }),
        ));
        let mut mask = Grid::new(64, 64, 1.0);
        for y in 0..64 {
            for x in 0..64 {
                mask.set(x, y, if x < 32 { 1.0 } else { 0.0 });
            }
        }
        doc.layers[0].mask = Some(mask);

        let streamed = doc.evaluate();
        let cached = doc.evaluate_with(&mut EvalCache::new());
        for (a, b) in streamed.data.iter().zip(cached.data.iter()) {
            assert!((*a - *b).length() < 1e-6);
        }

        let streamed_h = doc.evaluate_height();
        let cached_h = doc.evaluate_height_with(&mut EvalCache::new());
        for (a, b) in streamed_h.data.iter().zip(cached_h.data.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn zero_budget_forces_streaming_and_stays_correct() {
        let doc = crate::demo::demo_document(128);
        assert!(doc.estimated_cache_bytes() > 0);
        let mut cache = EvalCache::new();
        let auto = doc.evaluate_auto(&mut cache, 0);
        let reference = doc.evaluate();
        for (a, b) in auto.data.iter().zip(reference.data.iter()) {
            assert!((*a - *b).length() < 1e-6);
        }
        let auto_h = doc.evaluate_height_auto(&mut cache, 0);
        let reference_h = doc.evaluate_height();
        for (a, b) in auto_h.data.iter().zip(reference_h.data.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn cancellable_evaluation_matches_the_uncancelled_result() {
        let doc = crate::demo::demo_document(64);
        let token = crate::CancelToken::new();
        let mut cache = EvalCache::new();
        let normals = doc
            .evaluate_auto_cancellable(&mut cache, DEFAULT_EVAL_CACHE_BUDGET, &token)
            .expect("live token");
        let height = doc
            .evaluate_height_auto_cancellable(&mut cache, DEFAULT_EVAL_CACHE_BUDGET, &token)
            .expect("live token");
        for (a, b) in normals.data.iter().zip(doc.evaluate().data.iter()) {
            assert!((*a - *b).length() < 1e-6);
        }
        for (a, b) in height.data.iter().zip(doc.evaluate_height().data.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn cancelled_evaluation_returns_none() {
        let doc = crate::demo::demo_document(64);
        let token = crate::CancelToken::new();
        token.cancel();
        let mut cache = EvalCache::new();
        assert!(doc
            .evaluate_auto_cancellable(&mut cache, DEFAULT_EVAL_CACHE_BUDGET, &token)
            .is_none());
        assert!(doc
            .evaluate_height_auto_cancellable(&mut cache, DEFAULT_EVAL_CACHE_BUDGET, &token)
            .is_none());
    }

    #[test]
    fn imported_normal_layers_are_detected_only_when_visible() {
        let mut doc = Document::new(UVec2::new(16, 16));
        assert!(!doc.has_imported_normals());

        let mut hidden = Layer::imported_normal("hidden normal", Grid::new(16, 16, Vec3::Z));
        hidden.visible = false;
        doc.layers.push(hidden);
        assert!(
            !doc.has_imported_normals(),
            "hidden layers are not evaluated"
        );

        let imported = Layer::imported_normal("normal", Grid::new(16, 16, Vec3::Z));
        doc.layers.push(Layer::group("group", vec![imported]));
        assert!(doc.has_imported_normals(), "grouped imported normals count");
    }

    #[test]
    fn group_derivative_matches_flat_stack() {
        let shape = rect_layer("panel", Vec2::new(32.0, 32.0), Vec2::new(36.0, 28.0));

        let mut flat = Document::new(UVec2::new(64, 64));
        flat.layers.push(Layer::fill("base", 0.0));
        flat.layers.push(shape.clone());

        let mut grouped = Document::new(UVec2::new(64, 64));
        grouped.layers.push(Layer::fill("base", 0.0));
        grouped.layers.push(Layer::group("group", vec![shape]));

        let flat_n = flat.evaluate();
        let group_n = grouped.evaluate();
        for (a, b) in flat_n.data.iter().zip(group_n.data.iter()) {
            assert!((*a - *b).length() < 1e-5);
        }
        let flat_h = flat.evaluate_height();
        let group_h = grouped.evaluate_height();
        for (a, b) in flat_h.data.iter().zip(group_h.data.iter()) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn group_opacity_scales_children() {
        let shape = rect_layer("panel", Vec2::new(32.0, 32.0), Vec2::new(36.0, 28.0));
        let mut group = Layer::group("group", vec![shape.clone()]);
        group.opacity = 0.5;

        let mut half_group = Document::new(UVec2::new(64, 64));
        half_group.layers.push(group);

        let mut half_shape = Document::new(UVec2::new(64, 64));
        let mut faded = shape;
        faded.opacity = 0.5;
        half_shape.layers.push(faded);

        let a = half_group.evaluate();
        let b = half_shape.evaluate();
        for (x, y) in a.data.iter().zip(b.data.iter()) {
            assert!((*x - *y).length() < 1e-5);
        }
    }

    #[test]
    fn clipping_mask_limits_coverage() {
        let mut mask_shape = rect_layer("mask", Vec2::new(16.0, 32.0), Vec2::new(30.0, 60.0));
        mask_shape.visible = false;
        let mut detail = rect_layer("detail", Vec2::new(32.0, 32.0), Vec2::new(60.0, 60.0));
        detail.mask_layer = Some(mask_shape.id);

        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(Layer::fill("base", 0.0));
        doc.layers.push(mask_shape);
        doc.layers.push(detail);

        let n = doc.evaluate();
        assert!(n.get(8, 32).x.abs() > 0.005);
        assert!((*n.get(56, 32) - Vec3::Z).length() < 1e-5);
    }

    #[test]
    fn boolean_intersect_only_when_both_cover() {
        let left = rect_layer("left", Vec2::new(24.0, 32.0), Vec2::new(40.0, 56.0));
        let right = rect_layer("right", Vec2::new(48.0, 32.0), Vec2::new(24.0, 56.0));

        let mut doc = Document::new(UVec2::new(64, 64));
        let mut right_intersect = right.clone();
        right_intersect.blend = BlendMode::BooleanIntersect;
        doc.layers
            .push(Layer::group("group", vec![left.clone(), right_intersect]));
        let intersect = doc.evaluate();
        // Right-only region contributes nothing because the left layer does not
        // cover it.
        assert!((*intersect.get(58, 32) - Vec3::Z).length() < 1e-5);

        let mut right_union = right;
        right_union.blend = BlendMode::BooleanUnion;
        doc.layers[0] = Layer::group("group", vec![left, right_union]);
        let union = doc.evaluate();
        assert!(union.get(58, 32).x.abs() > 0.01);
    }

    fn eval_single(layer: &Layer) -> (Grid<Vec3>, Grid<f32>) {
        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(layer.clone());
        (doc.evaluate(), doc.evaluate_height())
    }

    /// `1` when the pixel is well inside the shape, `-1` when well outside and
    /// `0` on the anti-aliased rim. Boolean tests use this to avoid comparing
    /// partially covered pixels where the effective height is coverage-scaled.
    fn coverage_class(layer: &Layer, x: u32, y: u32) -> i8 {
        let LayerKind::Shape(shape) = &layer.kind else {
            return 0;
        };
        let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
        let offsets = [
            Vec2::ZERO,
            Vec2::new(1.5, 0.0),
            Vec2::new(-1.5, 0.0),
            Vec2::new(0.0, 1.5),
            Vec2::new(0.0, -1.5),
        ];
        let inside = offsets
            .iter()
            .filter(|offset| shape.source.contains(p + **offset))
            .count();
        if inside == offsets.len() {
            1
        } else if inside == 0 {
            -1
        } else {
            0
        }
    }

    #[test]
    fn boolean_union_keeps_the_taller_surface() {
        let mut a = rect_layer("a", Vec2::new(30.0, 32.0), Vec2::new(40.0, 40.0));
        let mut b = rect_layer("b", Vec2::new(34.0, 32.0), Vec2::new(40.0, 40.0));
        if let LayerKind::Shape(shape) = &mut a.kind {
            shape.height_px = 12.0;
        }
        if let LayerKind::Shape(shape) = &mut b.kind {
            shape.height_px = 6.0;
        }
        let (normal_a, height_a) = eval_single(&a);
        let (normal_b, height_b) = eval_single(&b);
        let a_ref = a.clone();
        let b_ref = b.clone();

        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(a);
        let mut union = b;
        union.blend = BlendMode::BooleanUnion;
        doc.layers.push(union);
        let normals = doc.evaluate();
        let heights = doc.evaluate_height();

        let rim = 2.0 / 64.0;
        let mut compared = 0;
        for y in 0..64 {
            for x in 0..64 {
                let ca = coverage_class(&a_ref, x, y);
                let cb = coverage_class(&b_ref, x, y);
                if ca == 0 || cb == 0 || (ca < 0 && cb < 0) {
                    continue;
                }
                let ha = *height_a.get(x, y);
                let hb = *height_b.get(x, y);
                let expected_h = if ca > 0 && cb > 0 {
                    ha.max(hb)
                } else if ca > 0 {
                    ha
                } else {
                    hb
                };
                assert!(
                    (*heights.get(x, y) - expected_h).abs() < 1e-4,
                    "union height at ({x},{y}): {} vs {expected_h} ({ha}, {hb})",
                    heights.get(x, y)
                );
                if ca > 0 && cb > 0 && (ha - hb).abs() < rim {
                    continue;
                }
                let expected_n = if ca > 0 && cb > 0 {
                    if ha > hb {
                        *normal_a.get(x, y)
                    } else {
                        *normal_b.get(x, y)
                    }
                } else if ca > 0 {
                    *normal_a.get(x, y)
                } else {
                    *normal_b.get(x, y)
                };
                assert!(
                    (*normals.get(x, y) - expected_n).length() < 2e-3,
                    "union normal at ({x},{y}): {:?} vs {:?} ({ha}, {hb})",
                    normals.get(x, y),
                    expected_n
                );
                compared += 1;
            }
        }
        assert!(compared > 100, "only compared {compared} texels");
    }

    #[test]
    fn boolean_intersect_keeps_the_shorter_surface() {
        let mut a = rect_layer("a", Vec2::new(30.0, 32.0), Vec2::new(40.0, 40.0));
        let mut b = rect_layer("b", Vec2::new(34.0, 32.0), Vec2::new(40.0, 40.0));
        if let LayerKind::Shape(shape) = &mut a.kind {
            shape.height_px = 12.0;
        }
        if let LayerKind::Shape(shape) = &mut b.kind {
            shape.height_px = 6.0;
        }
        let (normal_a, height_a) = eval_single(&a);
        let (normal_b, height_b) = eval_single(&b);
        let a_ref = a.clone();
        let b_ref = b.clone();

        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(a);
        let mut intersect = b;
        intersect.blend = BlendMode::BooleanIntersect;
        doc.layers.push(intersect);
        let normals = doc.evaluate();
        let heights = doc.evaluate_height();

        let rim = 2.0 / 64.0;
        let mut compared = 0;
        for y in 0..64 {
            for x in 0..64 {
                let ca = coverage_class(&a_ref, x, y);
                let cb = coverage_class(&b_ref, x, y);
                if ca == 0 || cb == 0 || (ca < 0 && cb < 0) {
                    continue;
                }
                if ca < 0 || cb < 0 {
                    assert!(
                        *heights.get(x, y) < 1e-5,
                        "intersection should be empty at ({x},{y}): {}",
                        heights.get(x, y)
                    );
                    assert!(
                        (*normals.get(x, y) - Vec3::Z).length() < 1e-5,
                        "empty intersection should be flat at ({x},{y}): {:?}",
                        normals.get(x, y)
                    );
                    continue;
                }
                let ha = *height_a.get(x, y);
                let hb = *height_b.get(x, y);
                assert!(
                    (*heights.get(x, y) - ha.min(hb)).abs() < 1e-4,
                    "intersection height at ({x},{y}): {} vs {} ({ha}, {hb})",
                    heights.get(x, y),
                    ha.min(hb)
                );
                if (ha - hb).abs() < rim {
                    continue;
                }
                let expected_n = if ha < hb {
                    *normal_a.get(x, y)
                } else {
                    *normal_b.get(x, y)
                };
                assert!(
                    (*normals.get(x, y) - expected_n).length() < 2e-3,
                    "intersection normal at ({x},{y}): {:?} vs {:?} ({ha}, {hb})",
                    normals.get(x, y),
                    expected_n
                );
                compared += 1;
            }
        }
        assert!(compared > 100, "only compared {compared} texels");
    }

    /// A cut boolean group with the base first and cutters above it.
    fn cut_group(op: VectorOp, base: Layer, cutters: Vec<Layer>) -> Layer {
        let children = std::iter::once(base).chain(cutters).collect();
        let mut group = Layer::group("cut", children);
        group.vector_op = Some(op);
        group
    }

    #[test]
    fn a_partial_cut_boolean_leaves_a_floor() {
        let mut plate = rect_layer("plate", Vec2::splat(32.0), Vec2::splat(56.0));
        if let LayerKind::Shape(shape) = &mut plate.kind {
            shape.height_px = 24.0;
            shape.radius_px = 4.0;
        }
        let mut pocket = rect_layer("pocket", Vec2::splat(32.0), Vec2::splat(24.0));
        if let LayerKind::Shape(shape) = &mut pocket.kind {
            shape.height_px = 10.0;
            shape.radius_px = 2.0;
            shape.profile = Profile::Linear;
        }

        let mut doc = Document::new(UVec2::splat(64));
        doc.layers
            .push(cut_group(VectorOp::PartialCut, plate.clone(), vec![pocket]));
        assert!(doc.layers[0].requires_tree(), "cuts need the surface below");
        let heights = doc.evaluate_height();
        let floor = (24.0 - 10.0) / doc.height_scale_px;
        assert!(
            (heights.get(32, 32) - floor).abs() < 0.02,
            "pocket floor {}",
            heights.get(32, 32)
        );
        assert!((heights.get(10, 32) - 24.0 / doc.height_scale_px).abs() < 0.02);

        let normals = doc.evaluate();
        assert!(normals.iter().all(|normal| normal.is_finite()));
        // The pocket wall is sloped; the untouched plate stays flat.
        assert!((normals.get(21, 32) - Vec3::Z).length() > 0.02);

        // A cut deeper than the plate stops at the base, never below it.
        let mut deep = rect_layer("deep", Vec2::splat(32.0), Vec2::splat(24.0));
        if let LayerKind::Shape(shape) = &mut deep.kind {
            shape.height_px = 40.0;
            shape.radius_px = 2.0;
        }
        let mut deep_doc = Document::new(UVec2::splat(64));
        deep_doc
            .layers
            .push(cut_group(VectorOp::PartialCut, plate, vec![deep]));
        let deep_heights = deep_doc.evaluate_height();
        assert!(*deep_heights.get(32, 32) < 1e-4);
    }

    #[test]
    fn a_partial_slash_boolean_tapers_its_ends() {
        let mut plate = rect_layer("plate", Vec2::splat(32.0), Vec2::splat(56.0));
        if let LayerKind::Shape(shape) = &mut plate.kind {
            shape.height_px = 24.0;
            shape.radius_px = 2.0;
        }
        let mut slash = rect_layer("slash", Vec2::splat(32.0), Vec2::new(48.0, 10.0));
        if let LayerKind::Shape(shape) = &mut slash.kind {
            shape.height_px = 12.0;
            shape.radius_px = 2.0;
            shape.profile = Profile::Linear;
            shape.cut_taper_in = 0.3;
            shape.cut_taper_out = 0.3;
        }
        let mut tapered_doc = Document::new(UVec2::splat(64));
        tapered_doc.layers.push(cut_group(
            VectorOp::PartialSlash,
            plate.clone(),
            vec![slash.clone()],
        ));
        let tapered = tapered_doc.evaluate_height();

        if let LayerKind::Shape(shape) = &mut slash.kind {
            shape.cut_taper_in = 0.0;
            shape.cut_taper_out = 0.0;
        }
        let mut plain_doc = Document::new(UVec2::splat(64));
        plain_doc
            .layers
            .push(cut_group(VectorOp::PartialSlash, plate, vec![slash]));
        let plain = plain_doc.evaluate_height();

        // Near an end the taper removes less material; the centre is unchanged.
        assert!(
            *tapered.get(14, 32) > *plain.get(14, 32) + 0.01,
            "tapered {} vs plain {}",
            tapered.get(14, 32),
            plain.get(14, 32)
        );
        assert!((tapered.get(32, 32) - plain.get(32, 32)).abs() < 0.01);
    }

    #[test]
    fn a_clip_divide_boolean_splits_the_plate_with_a_step() {
        let mut plate = rect_layer("plate", Vec2::splat(32.0), Vec2::splat(56.0));
        if let LayerKind::Shape(shape) = &mut plate.kind {
            shape.height_px = 24.0;
            shape.radius_px = 2.0;
        }
        let mut divide = rect_layer("divide", Vec2::splat(32.0), Vec2::new(10.0, 56.0));
        if let LayerKind::Shape(shape) = &mut divide.kind {
            shape.height_px = 8.0;
            shape.radius_px = 5.0;
            shape.profile = Profile::Round;
            shape.cut_step_px = 6.0;
        }

        let mut doc = Document::new(UVec2::splat(64));
        doc.layers
            .push(cut_group(VectorOp::ClipDivide, plate, vec![divide]));
        let heights = doc.evaluate_height();
        let level = 24.0 / doc.height_scale_px;
        let step = 6.0 / doc.height_scale_px;
        assert!(
            *heights.get(32, 32) < 0.01,
            "seam height {}",
            heights.get(32, 32)
        );
        let left = *heights.get(20, 32);
        let right = *heights.get(44, 32);
        assert!(left > 0.0 && right > 0.0, "left {left} right {right}");
        assert!(
            ((left - right).abs() - step).abs() < 0.03,
            "left {left} right {right} step {step}"
        );
        assert!((left.max(right) - (level + step)).abs() < 0.03);

        let normals = doc.evaluate();
        assert!(normals.iter().all(|normal| normal.is_finite()));
    }

    #[test]
    fn cut_booleans_keep_the_bottom_shape_as_the_base() {
        let mut plate = rect_layer("plate", Vec2::splat(32.0), Vec2::splat(56.0));
        if let LayerKind::Shape(shape) = &mut plate.kind {
            shape.height_px = 24.0;
        }
        let mut pocket = rect_layer("pocket", Vec2::splat(32.0), Vec2::splat(20.0));
        if let LayerKind::Shape(shape) = &mut pocket.kind {
            shape.height_px = 8.0;
        }
        let group = cut_group(VectorOp::PartialCut, plate, vec![pocket]);
        let mut doc = Document::new(UVec2::splat(64));
        doc.layers.push(group);
        // Only the base exists outside the cutter, so the cut region has a
        // floor while the rim keeps the plate height.
        let heights = doc.evaluate_height();
        let floor = *heights.get(32, 32);
        let rim = *heights.get(10, 32);
        assert!(floor > 0.0, "pocket cut through: {floor}");
        assert!(floor < rim, "floor {floor} should sit below rim {rim}");
        assert!(*heights.get(2, 32) < 1e-4);
    }

    #[test]
    fn boolean_difference_shows_the_layer_surface_over_the_base() {
        let mut shape = rect_layer("cut", Vec2::new(32.0, 32.0), Vec2::new(28.0, 28.0));
        if let LayerKind::Shape(shape) = &mut shape.kind {
            shape.height_px = 6.0;
        }
        let (normal, shape_heights) = eval_single(&shape);
        let shape_ref = shape.clone();

        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(Layer::fill("base", 0.5));
        let mut difference = shape.clone();
        difference.blend = BlendMode::BooleanDifference;
        doc.layers.push(difference);
        let normals = doc.evaluate();
        let heights = doc.evaluate_height();

        for y in 0..64 {
            for x in 0..64 {
                let class = coverage_class(&shape_ref, x, y);
                if class == 0 {
                    continue;
                }
                if class < 0 {
                    // Outside the cut the base surface is untouched.
                    assert!((*heights.get(x, y) - 0.5).abs() < 1e-5);
                    assert!((*normals.get(x, y) - Vec3::Z).length() < 1e-5);
                    continue;
                }
                // The layer's own surface is drawn over the base: its full
                // bevel cuts into the lower shape while the layer stays visible.
                assert!(
                    (*normals.get(x, y) - *normal.get(x, y)).length() < 2e-3,
                    "difference normal at ({x},{y}): {:?} vs {:?}",
                    normals.get(x, y),
                    normal.get(x, y)
                );
                assert!(
                    (*heights.get(x, y) - 0.5).abs() < 1e-4,
                    "difference height at ({x},{y}): {}",
                    heights.get(x, y)
                );
            }
        }

        // Difference coverage is the union, so a difference layer over an empty
        // base still keeps its own surface and height.
        let mut solo_doc = Document::new(UVec2::new(64, 64));
        let mut only_difference = shape.clone();
        only_difference.blend = BlendMode::BooleanDifference;
        solo_doc.layers.push(only_difference);
        let solo_heights = solo_doc.evaluate_height();
        let mut compared = 0;
        for y in 0..64 {
            for x in 0..64 {
                if coverage_class(&shape_ref, x, y) <= 0 {
                    continue;
                }
                assert!(
                    (*solo_heights.get(x, y) - *shape_heights.get(x, y)).abs() < 1e-4,
                    "difference height over an empty base at ({x},{y}): {} vs {}",
                    solo_heights.get(x, y),
                    shape_heights.get(x, y)
                );
                compared += 1;
            }
        }
        assert!(compared > 100, "only compared {compared} texels");
    }

    #[test]
    fn boolean_operands_match_stacked_boolean_layers() {
        let mut a = rect_layer("a", Vec2::new(30.0, 32.0), Vec2::new(40.0, 40.0));
        let mut b = rect_layer("b", Vec2::new(34.0, 32.0), Vec2::new(40.0, 40.0));
        if let LayerKind::Shape(shape) = &mut a.kind {
            shape.height_px = 12.0;
        }
        if let LayerKind::Shape(shape) = &mut b.kind {
            shape.height_px = 6.0;
        }

        for op in BooleanOp::ALL {
            let mut reference = Document::new(UVec2::new(64, 64));
            reference.layers.push(a.clone());
            let mut stacked = b.clone();
            stacked.blend = op.blend_mode();
            reference.layers.push(stacked);
            let expected_normals = reference.evaluate();
            let expected_heights = reference.evaluate_height();

            let mut operand_doc = Document::new(UVec2::new(64, 64));
            let mut target = a.clone();
            target
                .boolean_operands
                .push(BooleanOperand { layer: b.id, op });
            operand_doc.layers.push(target);
            let mut source = b.clone();
            source.visible = false;
            operand_doc.layers.push(source);

            let normals = operand_doc.evaluate();
            let heights = operand_doc.evaluate_height();
            for (actual, expected) in normals.data.iter().zip(expected_normals.data.iter()) {
                assert!(
                    (*actual - *expected).length() < 1e-6,
                    "{op:?} normal mismatch: {actual:?} vs {expected:?}"
                );
            }
            for (actual, expected) in heights.data.iter().zip(expected_heights.data.iter()) {
                assert!(
                    (actual - expected).abs() < 1e-6,
                    "{op:?} height mismatch: {actual} vs {expected}"
                );
            }

            let mut cache = EvalCache::new();
            let cached_normals = operand_doc.evaluate_with(&mut cache);
            let cached_heights = operand_doc.evaluate_height_with(&mut cache);
            for (actual, expected) in cached_normals.data.iter().zip(normals.data.iter()) {
                assert!(
                    (*actual - *expected).length() < 1e-6,
                    "{op:?} cached normal mismatch"
                );
            }
            for (actual, expected) in cached_heights.data.iter().zip(heights.data.iter()) {
                assert!(
                    (actual - expected).abs() < 1e-6,
                    "{op:?} cached height mismatch"
                );
            }
        }
    }

    #[test]
    fn cannot_add_self_ancestor_or_duplicate_boolean_operands() {
        let mut a = rect_layer("a", Vec2::splat(32.0), Vec2::splat(20.0));
        let b = rect_layer("b", Vec2::splat(32.0), Vec2::splat(20.0));
        let a_id = a.id;
        let b_id = b.id;
        let mut group = Layer::group("group", vec![]);
        let group_id = group.id;
        if let LayerKind::Group { children } = &mut group.kind {
            children.push(a.clone());
        }
        let mut doc = Document::new(UVec2::splat(64));
        doc.layers.push(group);
        doc.layers.push(b);

        assert!(!doc.can_add_boolean_operand(a_id, a_id));
        assert!(!doc.can_add_boolean_operand(a_id, group_id));
        assert!(doc.can_add_boolean_operand(a_id, b_id));
        assert!(doc.can_add_boolean_operand(group_id, a_id));

        a.boolean_operands.push(BooleanOperand {
            layer: b_id,
            op: BooleanOp::Union,
        });
        if let LayerKind::Group { children } = &mut doc.layers[0].kind {
            children[0] = a;
        }
        assert!(!doc.can_add_boolean_operand(a_id, b_id));
    }

    #[test]
    fn boolean_cached_evaluation_matches_uncached() {
        let mut a = rect_layer("a", Vec2::new(30.0, 32.0), Vec2::new(40.0, 40.0));
        let mut b = rect_layer("b", Vec2::new(34.0, 32.0), Vec2::new(40.0, 40.0));
        if let LayerKind::Shape(shape) = &mut a.kind {
            shape.height_px = 12.0;
        }
        if let LayerKind::Shape(shape) = &mut b.kind {
            shape.height_px = 6.0;
            shape.invert = true;
        }
        b.blend = BlendMode::BooleanUnion;
        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(a);
        doc.layers.push(b);

        let normals = doc.evaluate();
        let heights = doc.evaluate_height();
        let mut cache = EvalCache::new();
        let cached_normals = doc.evaluate_with(&mut cache);
        let cached_heights = doc.evaluate_height_with(&mut cache);
        for (a, b) in normals.data.iter().zip(cached_normals.data.iter()) {
            assert!((*a - *b).length() < 1e-6);
        }
        for (a, b) in heights.data.iter().zip(cached_heights.data.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn group_cache_tracks_child_edits() {
        let shape = rect_layer("panel", Vec2::new(32.0, 32.0), Vec2::new(32.0, 32.0));
        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(Layer::group("group", vec![shape]));

        let mut cache = EvalCache::new();
        let first = doc.evaluate_with(&mut cache);
        if let LayerKind::Group { children } = &mut doc.layers[0].kind {
            if let LayerKind::Shape(shape) = &mut children[0].kind {
                shape.radius_px = 2.0;
            }
            children[0].touch();
        }
        let cached = doc.evaluate_with(&mut cache);
        let uncached = doc.evaluate();
        assert!((*cached.get(16, 32) - *first.get(16, 32)).length() > 1e-3);
        for (a, b) in cached.data.iter().zip(uncached.data.iter()) {
            assert!((*a - *b).length() < 1e-6);
        }
    }

    #[test]
    fn mask_source_edits_invalidate_masked_layer() {
        let mut mask = rect_layer("mask", Vec2::new(16.0, 32.0), Vec2::new(30.0, 60.0));
        mask.visible = false;
        let mut detail = rect_layer("detail", Vec2::new(32.0, 32.0), Vec2::new(60.0, 60.0));
        detail.mask_layer = Some(mask.id);
        let mask_id = mask.id;

        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(mask);
        doc.layers.push(detail);

        let mut cache = EvalCache::new();
        let first = doc.evaluate_with(&mut cache);
        if let Some(mask) = doc.find_layer_mut(mask_id) {
            if let LayerKind::Shape(shape) = &mut mask.kind {
                shape.source = ShapeSource::Primitive(Primitive::Rect {
                    center: Vec2::new(48.0, 32.0),
                    size: Vec2::new(30.0, 60.0),
                    corner_radius: 4.0,
                });
            }
            mask.touch();
        }
        let second = doc.evaluate_with(&mut cache);
        let uncached = doc.evaluate();
        assert!((*second.get(8, 32) - *first.get(8, 32)).length() > 1e-3);
        for (a, b) in second.data.iter().zip(uncached.data.iter()) {
            assert!((*a - *b).length() < 1e-6);
        }
    }

    #[test]
    fn group_and_ungroup_preserve_children() {
        let mut doc = Document::new(UVec2::new(32, 32));
        doc.layers
            .push(rect_layer("a", Vec2::splat(16.0), Vec2::splat(8.0)));
        doc.layers
            .push(rect_layer("b", Vec2::splat(16.0), Vec2::splat(8.0)));
        let group_id = doc.group_at(&[1]).expect("group succeeds");
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.layer_count(), 3);
        assert!(doc.find_layer(group_id).is_some_and(Layer::is_group));
        assert_eq!(doc.find_path(group_id), Some(vec![1]));

        let first_child = doc.ungroup_at(&[1]).expect("children restored");
        assert_eq!(first_child, vec![1]);
        assert_eq!(doc.layers.len(), 2);
        assert!(doc.layers.iter().all(|layer| !layer.is_group()));
    }

    #[test]
    fn group_many_preserves_sibling_order() {
        let mut doc = Document::new(UVec2::new(32, 32));
        for name in ["a", "b", "c", "d", "e"] {
            doc.layers
                .push(rect_layer(name, Vec2::splat(16.0), Vec2::splat(8.0)));
        }
        let group_path = doc
            .group_many_at(&[vec![3], vec![1]])
            .expect("siblings grouped");
        assert_eq!(group_path, vec![1]);
        assert_eq!(doc.layers.len(), 4);
        let group = &doc.layers[1];
        let children = group.children().expect("group created");
        assert_eq!(
            children
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "d"]
        );
        assert!(doc.layers[2].name == "c");
    }

    #[test]
    fn duplicating_a_group_refreshes_every_id() {
        let child = rect_layer("child", Vec2::splat(16.0), Vec2::splat(8.0));
        let child_id = child.id;
        let group = Layer::group("group", vec![child]);
        let group_id = group.id;

        let copy = group.duplicate();
        assert_ne!(copy.id, group_id);
        assert_eq!(copy.name, "group copy");
        let copied_child = &copy.children().expect("still a group")[0];
        assert_ne!(copied_child.id, child_id);
        assert_eq!(copied_child.name, "child", "only the top layer is renamed");
        // The original keeps its identity.
        assert_eq!(group.id, group_id);
        assert_eq!(group.children().unwrap()[0].id, child_id);
    }

    #[test]
    fn copied_references_follow_the_duplicated_sources() {
        let source = rect_layer("source", Vec2::splat(16.0), Vec2::splat(8.0));
        let mut masked = rect_layer("masked", Vec2::splat(20.0), Vec2::splat(8.0));
        masked.mask_layer = Some(source.id);
        masked.boolean_operands.push(BooleanOperand {
            layer: source.id,
            op: BooleanOp::Intersect,
        });

        let source_copy = source.duplicate();
        let mut masked_copy = masked.duplicate();
        let map = HashMap::from([(source.id, source_copy.id)]);
        masked_copy.remap_layer_references(&map);

        assert_eq!(masked_copy.mask_layer, Some(source_copy.id));
        assert_eq!(masked_copy.boolean_operands[0].layer, source_copy.id);
        assert_eq!(
            masked.mask_layer,
            Some(source.id),
            "the original is untouched"
        );
    }

    #[test]
    fn mask_candidates_exclude_related_layers() {
        let a = rect_layer("a", Vec2::splat(16.0), Vec2::splat(8.0));
        let b = rect_layer("b", Vec2::splat(16.0), Vec2::splat(8.0));
        let group = Layer::group("group", vec![b]);
        let group_id = group.id;
        let b_id = group.children().unwrap()[0].id;
        let a_id = a.id;
        let mut doc = Document::new(UVec2::new(32, 32));
        doc.layers.push(a);
        doc.layers.push(group);

        let candidates = doc.mask_candidates(group_id);
        assert!(candidates.iter().any(|(id, _, _)| *id == a_id));
        assert!(!candidates.iter().any(|(id, _, _)| *id == b_id));
        assert!(!candidates.iter().any(|(id, _, _)| *id == group_id));
    }

    #[test]
    fn shape_height_rises_inside() {
        let mut doc = Document::new(UVec2::new(64, 64));
        doc.layers.push(Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(32.0, 32.0),
                size: Vec2::new(32.0, 32.0),
                corner_radius: 4.0,
            }),
        ));
        let h = doc.evaluate_height();
        assert!(*h.get(32, 32) > 0.1);
        assert!(*h.get(32, 32) <= 1.0);
        assert!(*h.get(4, 4) < 0.01);
    }

    #[test]
    fn boolean_operands_round_trip_through_ron() {
        let mut a = rect_layer("a", Vec2::splat(32.0), Vec2::splat(20.0));
        let b = rect_layer("b", Vec2::splat(32.0), Vec2::splat(20.0));
        a.boolean_operands.push(BooleanOperand {
            layer: b.id,
            op: BooleanOp::Difference,
        });
        let text = ron::to_string(&a).expect("serialize layer with operands");
        let loaded: Layer = ron::from_str(&text).expect("deserialize layer with operands");
        assert_eq!(loaded.boolean_operands.len(), 1);
        assert_eq!(loaded.boolean_operands[0].layer, b.id);
        assert_eq!(loaded.boolean_operands[0].op, BooleanOp::Difference);
    }

    #[test]
    fn the_lock_is_optional_in_saved_projects() {
        // Old projects have no `locked` field; they must load unlocked.
        let unlocked = Layer::flat("free");
        let text = ron::to_string(&unlocked).expect("serialize unlocked layer");
        assert!(!text.contains("locked"), "default layers stay lean: {text}");
        let loaded: Layer = ron::from_str(&text).expect("deserialize unlocked layer");
        assert!(!loaded.locked);

        let mut locked = Layer::flat("pinned");
        locked.locked = true;
        let text = ron::to_string(&locked).expect("serialize locked layer");
        assert!(text.contains("locked:true"), "{text}");
        let loaded: Layer = ron::from_str(&text).expect("deserialize locked layer");
        assert!(loaded.locked);
    }

    #[test]
    fn live_compound_round_trips_through_ron() {
        let group = compound(
            VectorOp::Divide,
            vec![rect_layer("a", Vec2::splat(32.0), Vec2::splat(20.0))],
        );
        let text = ron::to_string(&group).expect("serialize compound");
        let loaded: Layer = ron::from_str(&text).expect("deserialize compound");
        assert_eq!(loaded.vector_op, Some(VectorOp::Divide));
        assert_eq!(loaded.children().expect("group children").len(), 1);
    }

    #[test]
    fn normal_paint_layers_round_trip_through_ron() {
        let layer = Layer::normal_paint(
            "brush",
            Grid::from_fn(4, 4, |_, _| Vec3::new(0.25, -0.5, 0.83)),
        );
        let text = ron::to_string(&layer).expect("serialize normal paint");
        let loaded: Layer = ron::from_str(&text).expect("deserialize normal paint");
        let LayerKind::NormalPaint { normal } = loaded.kind else {
            panic!("expected a normal paint layer");
        };
        assert_eq!((normal.w, normal.h), (4, 4));
        assert!(normal.get(0, 0).x > 0.2);
    }

    #[test]
    fn old_shape_layers_default_to_fill() {
        let shape: ShapeLayer = ron::from_str(
            "(source: Primitive(Rect(center: (0.0, 0.0), size: (10.0, 10.0), corner_radius: 2.0)), radius_px: 16.0, height_px: 8.0, profile: Round, invert: false, softness_px: 0.0)",
        )
        .expect("shape without style fields should still load");
        assert_eq!(shape.style, ShapeStyle::Fill);
        assert_eq!(shape.stroke_width_px, 8.0);
        assert_eq!(shape.smooth_px, 0.0);
        assert_eq!(shape.cut_taper_in, 0.0);
        assert_eq!(shape.cut_taper_out, 0.0);
        assert_eq!(shape.cut_step_px, 0.0);
    }

    #[test]
    fn cut_booleans_round_trip_through_ron() {
        let base = rect_layer("plate", Vec2::splat(32.0), Vec2::splat(40.0));
        let mut cutter = rect_layer("cutter", Vec2::splat(32.0), Vec2::splat(20.0));
        if let LayerKind::Shape(shape) = &mut cutter.kind {
            shape.cut_taper_in = 0.2;
            shape.cut_taper_out = 0.3;
            shape.cut_step_px = 4.0;
        }
        let group = cut_group(VectorOp::PartialSlash, base, vec![cutter]);
        let text = ron::to_string(&group).expect("serialize cut boolean");
        let loaded: Layer = ron::from_str(&text).expect("deserialize cut boolean");
        assert_eq!(loaded.vector_op, Some(VectorOp::PartialSlash));
        let child = &loaded.children().expect("boolean children")[1];
        let LayerKind::Shape(shape) = &child.kind else {
            panic!("expected a shape cutter");
        };
        assert_eq!(shape.cut_taper_in, 0.2);
        assert_eq!(shape.cut_taper_out, 0.3);
        assert_eq!(shape.cut_step_px, 4.0);
    }

    #[test]
    fn stroke_shape_leaves_the_center_hollow() {
        let mut layer = rect_layer("ring", Vec2::splat(32.0), Vec2::splat(32.0));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 10.0;
            shape.radius_px = 3.0;
            shape.height_px = 6.0;
        }
        let mut doc = Document::new(UVec2::splat(64));
        doc.layers.push(layer);

        let height = doc.evaluate_height();
        assert!(
            *height.get(16, 32) > 0.02,
            "the outline band should rise: {}",
            height.get(16, 32)
        );
        assert!(
            *height.get(32, 32) < 0.01,
            "the center should remain hollow: {}",
            height.get(32, 32)
        );

        let normals = doc.evaluate();
        assert!((*normals.get(32, 32) - Vec3::Z).length() < 1e-4);
        assert!(
            normals.get(20, 32).truncate().length() > 0.01,
            "the band bevel should tilt: {:?}",
            normals.get(20, 32)
        );
    }

    /// A raised panel with a ripple detail layered over it, masked to the
    /// panel's flat top by a slope smart mask.
    fn slope_masked_detail_doc() -> Document {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut panel = rect_layer("panel", Vec2::new(32.0, 32.0), Vec2::new(44.0, 44.0));
        if let LayerKind::Shape(shape) = &mut panel.kind {
            shape.radius_px = 8.0;
            shape.height_px = 8.0;
        }
        let ripple = Grid::from_fn(64, 64, |x, _| (x as f32 * 0.9).sin() * 0.5 + 0.5);
        let mut detail = Layer::height_paint("ripple", ripple, 2.0);
        detail.mask_settings.generators = vec![crate::mask::MaskGenerator::new(
            crate::mask::GeneratorKind::slope(),
        )];
        doc.layers.push(panel);
        doc.layers.push(detail);
        doc
    }

    #[test]
    fn slope_smart_mask_keeps_detail_off_bevels() {
        let doc = slope_masked_detail_doc();
        let masked = doc.evaluate();
        let mut unmasked_doc = doc.clone();
        unmasked_doc.layers[1].mask_settings = MaskSettings::default();
        let unmasked = unmasked_doc.evaluate();
        let mut base_doc = doc.clone();
        base_doc.layers.truncate(1);
        let base = base_doc.evaluate();

        // Plateau centre: the detail is present.
        assert!((*masked.get(32, 32) - *unmasked.get(32, 32)).length() < 1e-4);
        assert!(masked.get(32, 32).x.abs() > 0.05);
        // On the steep bevel the detail is masked out, leaving the panel.
        let bevel = (12, 32);
        assert!(
            base.get(bevel.0, bevel.1).x.abs() > 0.2,
            "probe should sit on the bevel"
        );
        assert!(
            (*masked.get(bevel.0, bevel.1) - *base.get(bevel.0, bevel.1)).length() < 1e-3,
            "bevel should stay clean: {:?} vs {:?}",
            masked.get(bevel.0, bevel.1),
            base.get(bevel.0, bevel.1)
        );
    }

    #[test]
    fn smart_masks_match_between_flat_and_group_paths() {
        let doc = slope_masked_detail_doc();
        let flat = doc.evaluate();
        let mut grouped = doc.clone();
        let children = std::mem::take(&mut grouped.layers);
        grouped.layers.push(Layer::group("group", children));
        assert!(grouped.requires_tree());
        let tree = grouped.evaluate();
        let mut cache = EvalCache::new();
        let cached = grouped.evaluate_with(&mut cache);
        for y in 0..64 {
            for x in 0..64 {
                assert!(
                    (*flat.get(x, y) - *tree.get(x, y)).length() < 1e-4,
                    "({x},{y})"
                );
                assert!(
                    (*tree.get(x, y) - *cached.get(x, y)).length() < 1e-5,
                    "({x},{y})"
                );
            }
        }
    }

    #[test]
    fn mask_preview_reports_the_effective_mask() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut layer = rect_layer("masked", Vec2::new(32.0, 32.0), Vec2::new(40.0, 40.0));
        layer.mask = Some(Grid::new(64, 64, 0.5));
        doc.layers.push(layer);
        let id = doc.layers[0].id;
        let preview = doc.mask_preview(id).expect("painted mask yields a preview");
        assert!((*preview.get(10, 10) - 0.5).abs() < 1e-5);

        // Invert and density are part of what the canvas overlay shows.
        doc.layers[0].mask_settings.invert = true;
        doc.layers[0].mask_settings.density = 0.5;
        let preview = doc.mask_preview(id).unwrap();
        assert!((*preview.get(10, 10) - 0.75).abs() < 1e-5);

        // A layer with no mask at all has nothing to draw.
        let plain = Layer::flat("plain");
        let plain_id = plain.id;
        doc.layers.push(plain);
        assert!(doc.mask_preview(plain_id).is_none());
    }

    #[test]
    fn mask_preview_includes_the_clipping_source() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut source = rect_layer("source", Vec2::new(32.0, 32.0), Vec2::new(24.0, 24.0));
        source.visible = false;
        let source_id = source.id;
        let mut detail = rect_layer("detail", Vec2::new(32.0, 32.0), Vec2::new(40.0, 24.0));
        detail.mask_layer = Some(source_id);
        let detail_id = detail.id;
        doc.layers.push(source);
        doc.layers.push(detail);

        let preview = doc.mask_preview(detail_id).expect("clip yields a preview");
        assert!(*preview.get(32, 32) > 0.99);
        assert!(*preview.get(10, 32) < 0.01);

        // The clip offset widens the previewed mask like it widens the clip.
        doc.layers[1].mask_settings.clip_offset_px = 6.0;
        let preview = doc.mask_preview(detail_id).unwrap();
        assert!(*preview.get(17, 32) > 0.9, "offset should grow the preview");
    }

    #[test]
    fn clip_offset_extends_the_clipped_layer_past_its_source() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut source = rect_layer("source", Vec2::new(32.0, 32.0), Vec2::new(24.0, 24.0));
        source.visible = false;
        let source_id = source.id;
        // The detail spans x = 16..48, so its left bevel (16..24) starts
        // outside the source (20..44).
        let mut detail = rect_layer("detail", Vec2::new(32.0, 32.0), Vec2::new(32.0, 24.0));
        detail.mask_layer = Some(source_id);
        doc.layers.push(source);
        doc.layers.push(detail);

        let clipped = doc.evaluate();
        assert!(
            clipped.get(18, 32).x.abs() < 1e-4,
            "bevel outside the source is cut without an offset"
        );
        doc.layers[1].mask_settings.clip_offset_px = 6.0;
        let grown = doc.evaluate();
        assert!(
            grown.get(18, 32).x.abs() > 1e-3,
            "offset should expose the bevel just past the source"
        );
        assert!(grown.get(10, 32).x.abs() < 1e-4, "further out stays cut");
    }

    #[test]
    fn inverted_clip_uses_the_outside_of_the_source() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut source = rect_layer("source", Vec2::new(16.0, 32.0), Vec2::new(28.0, 60.0));
        source.visible = false;
        let source_id = source.id;
        let mut detail = rect_layer("detail", Vec2::new(32.0, 32.0), Vec2::new(60.0, 40.0));
        detail.mask_layer = Some(source_id);
        doc.layers.push(source);
        doc.layers.push(detail);

        let clipped = doc.evaluate();
        doc.layers[1].mask_settings.invert_clip = true;
        let outside = doc.evaluate();
        // The detail's left bevel sits inside the source, its right bevel outside.
        assert!(clipped.get(5, 32).x.abs() > 0.05);
        assert!((*clipped.get(58, 32) - Vec3::Z).length() < 1e-5);
        assert!((*outside.get(5, 32) - Vec3::Z).length() < 1e-5);
        assert!(outside.get(58, 32).x.abs() > 0.05);
    }

    #[test]
    fn disabling_the_mask_reveals_the_whole_layer() {
        let mut doc = Document::new(UVec2::new(64, 64));
        let mut layer = rect_layer("panel", Vec2::new(32.0, 32.0), Vec2::new(40.0, 40.0));
        layer.mask = Some(Grid::new(64, 64, 0.0));
        doc.layers.push(layer);
        let hidden = doc.evaluate();
        assert!(hidden.iter().all(|n| (*n - Vec3::Z).length() < 1e-6));
        doc.layers[0].mask_settings.enabled = false;
        doc.layers[0].touch_mask();
        let shown = doc.evaluate();
        assert!(shown.get(14, 32).x.abs() > 0.05);
    }

    fn rect_layer(name: &str, center: Vec2, size: Vec2) -> Layer {
        let mut layer = Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size,
                corner_radius: 4.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 8.0;
        }
        layer
    }

    fn compound(op: VectorOp, children: Vec<Layer>) -> Layer {
        let mut group = Layer::group("compound", children);
        group.vector_op = Some(op);
        group
    }

    fn overlapping_pair() -> (Layer, Layer) {
        (
            rect_layer("left", Vec2::new(32.0, 48.0), Vec2::splat(64.0)),
            rect_layer("right", Vec2::new(64.0, 48.0), Vec2::splat(64.0)),
        )
    }

    /// A horizontal stroked line across the middle of a 96 x 96 document.
    fn cut_line(width: f32) -> Layer {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = crate::path::SubPath::new(false);
        subpath
            .nodes
            .push(crate::path::Node::corner(Vec2::new(-4.0, 48.0)));
        subpath
            .nodes
            .push(crate::path::Node::corner(Vec2::new(100.0, 48.0)));
        path.subpaths.push(subpath);
        let mut cutter = Layer::shape("cut line", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut cutter.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = width;
            shape.radius_px = 3.0;
            shape.height_px = 8.0;
        }
        cutter
    }

    #[test]
    fn cutting_with_a_stroked_line_splits_the_shape_into_layers() {
        let mut doc = Document::new(UVec2::splat(96));
        let mut panel = rect_layer("panel", Vec2::splat(48.0), Vec2::splat(80.0));
        if let LayerKind::Shape(shape) = &mut panel.kind {
            shape.radius_px = 6.0;
            shape.height_px = 12.0;
        }
        doc.layers.push(panel);
        let cutter = cut_line(10.0);
        let cutter_id = cutter.id;
        doc.layers.push(cutter);

        let outcome = doc.cut_layers_below(&[1]).expect("cut");
        assert_eq!(outcome.targets, 1);
        assert_eq!(outcome.paths.len(), 2, "one layer per half");
        assert!(
            doc.find_layer(cutter_id).is_none(),
            "the cutter is consumed"
        );
        assert_eq!(doc.layers.len(), 2);
        for path in &outcome.paths {
            let piece = doc.layer_by_path(path).expect("piece exists");
            assert!(piece.name.starts_with("panel"));
            assert!(
                matches!(&piece.kind, LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Path(_))),
                "a piece is an editable path"
            );
        }

        let normal = doc.evaluate();
        let gap = *normal.get(48, 48);
        assert!((gap - Vec3::Z).length() < 0.01, "the stroke gap stays flat");
        // The cut edges are beveled on both halves.
        assert!(normal.get(10, 40).length_squared() > 0.9);
        assert!(normal.get(10, 56).length_squared() > 0.9);
        // Away from every edge the surface stays flat.
        assert!((*normal.get(48, 24) - Vec3::Z).length() < 0.05);
    }

    #[test]
    fn cutting_with_a_sharp_corner_splits_the_shape_in_two() {
        let mut doc = Document::new(UVec2::splat(256));
        let mut panel = rect_layer("panel", Vec2::splat(128.0), Vec2::new(200.0, 160.0));
        if let LayerKind::Shape(shape) = &mut panel.kind {
            shape.radius_px = 20.0;
            shape.height_px = 30.0;
        }
        doc.layers.push(panel);
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = crate::path::SubPath::new(false);
        for point in [
            Vec2::new(10.0, 200.0),
            Vec2::new(90.0, 120.0),
            Vec2::new(160.0, 170.0),
            Vec2::new(250.0, 90.0),
        ] {
            subpath.nodes.push(crate::path::Node::corner(point));
        }
        path.subpaths.push(subpath);
        let mut cutter = Layer::shape("cut line", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut cutter.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 12.0;
            shape.radius_px = 4.0;
            shape.height_px = 10.0;
        }
        doc.layers.push(cutter);

        // An open line has no interior: the stroke rasterizes its expanded
        // band, so the bend is a raised ridge before the cut runs.
        let ridge = (125, 145);
        assert!(
            *doc.evaluate_height().get(ridge.0, ridge.1) > 0.2,
            "the knife line should be drawn where it was drawn: {}",
            doc.evaluate_height().get(ridge.0, ridge.1)
        );

        let outcome = doc.cut_layers_below(&[1]).expect("cut");
        assert_eq!(outcome.paths.len(), 2, "the line crosses the panel twice");
        let height = doc.evaluate_height();
        assert!(
            *height.get(ridge.0, ridge.1) < 0.01,
            "the cut leaves a gap where the line ran"
        );

        let normal = doc.evaluate();
        // Both pieces bevel their own cut edge.
        let along = (Vec2::new(160.0, 170.0) - Vec2::new(90.0, 120.0)).normalize();
        let normal_dir = Vec2::new(-along.y, along.x);
        let edge = Vec2::new(125.0, 145.0);
        for side in [-1.0, 1.0] {
            let probe = edge + normal_dir * side * 7.0;
            let probe = (probe.x as u32, probe.y as u32);
            assert!(
                normal.get(probe.0, probe.1).length_squared() > 0.9,
                "the cut edge tilts on both sides"
            );
        }
        // Away from the cut the pieces stay flat.
        assert!((*normal.get(60, 70) - Vec3::Z).length() < 0.05);
        assert!((*normal.get(190, 180) - Vec3::Z).length() < 0.05);
    }

    /// A diamond with 45 degree edges, the worst case for bevel shading: an
    /// exact distance transform kinks along the diagonal and shows as ribs.
    fn diamond_doc(smooth_px: f32) -> Document {
        let mut doc = Document::new(UVec2::splat(512));
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = crate::path::SubPath::new(true);
        for point in [
            Vec2::new(256.0, 60.0),
            Vec2::new(452.0, 256.0),
            Vec2::new(256.0, 452.0),
            Vec2::new(60.0, 256.0),
        ] {
            subpath.nodes.push(crate::path::Node::corner(point));
        }
        path.subpaths.push(subpath);
        let mut layer = Layer::shape("diamond", ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 40.0;
            shape.height_px = 60.0;
            shape.smooth_px = smooth_px;
        }
        doc.layers.push(layer);
        doc
    }

    /// Sum of the normal changes along the top-left edge, 20 px inside the
    /// silhouette: ribbing shows up here as a large total variation.
    fn bevel_wobble(normal: &Grid<Vec3>) -> f32 {
        let inside = Vec2::new(1.0, 1.0).normalize() * 20.0;
        let mut sum = 0.0;
        let mut prev: Option<Vec3> = None;
        for step in 0..400 {
            let t = step as f32 / 400.0;
            let base = Vec2::new(256.0, 60.0).lerp(Vec2::new(60.0, 256.0), t);
            let p = base + inside;
            let n = *normal.get(p.x as u32, p.y as u32);
            if let Some(prev) = prev {
                sum += (n - prev).length();
            }
            prev = Some(n);
        }
        sum
    }

    #[test]
    fn smooth_evens_out_diagonal_bevel_ribbing() {
        let raw = bevel_wobble(&diamond_doc(0.0).evaluate());
        let smoothed = bevel_wobble(&diamond_doc(4.0).evaluate());
        assert!(
            smoothed < raw * 0.9,
            "smoothing should even out the ribs: {raw} -> {smoothed}"
        );
    }

    #[test]
    fn smooth_keeps_the_bevel_profile_and_silhouette() {
        let raw_doc = diamond_doc(0.0);
        let smooth_doc = diamond_doc(4.0);
        let raw = raw_doc.evaluate();
        let smooth = smooth_doc.evaluate();
        // Mid-band tilt is unchanged: this is shading smoothing, not softness.
        let probe = Vec2::new(256.0, 60.0).lerp(Vec2::new(60.0, 256.0), 0.5)
            + Vec2::new(1.0, 1.0).normalize() * 20.0;
        let cell = (probe.x as u32, probe.y as u32);
        assert!(raw.get(cell.0, cell.1).truncate().length() > 0.2);
        assert!(
            (raw.get(cell.0, cell.1).truncate().length()
                - smooth.get(cell.0, cell.1).truncate().length())
            .abs()
                < 0.1,
            "the bevel keeps its strength"
        );
        // The centre stays at full height and the flat top stays flat.
        let raw_h = raw_doc.evaluate_height();
        let smooth_h = smooth_doc.evaluate_height();
        let (raw_centre, smooth_centre) = (*raw_h.get(256, 256), *smooth_h.get(256, 256));
        assert!(
            raw_centre.abs() > 0.05 && (raw_centre - smooth_centre).abs() < raw_centre.abs() * 0.05,
            "the centre keeps its full height: {raw_centre} vs {smooth_centre}"
        );
        assert!((*smooth.get(256, 256) - Vec3::Z).length() < 0.01);
    }

    #[test]
    fn cutting_ignores_layers_above_and_cutters_without_overlap() {
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers
            .push(rect_layer("panel", Vec2::splat(48.0), Vec2::splat(80.0)));
        let mut cutter = cut_line(4.0);
        if let LayerKind::Shape(shape) = &mut cutter.kind {
            shape.source = ShapeSource::Path({
                let mut path = Path::new(FillRule::NonZero);
                let mut subpath = crate::path::SubPath::new(false);
                subpath
                    .nodes
                    .push(crate::path::Node::corner(Vec2::new(-40.0, 4.0)));
                subpath
                    .nodes
                    .push(crate::path::Node::corner(Vec2::new(-10.0, 4.0)));
                path.subpaths.push(subpath);
                path
            });
        }
        doc.layers.push(cutter);
        assert!(doc.cut_layers_below(&[1]).is_none());
        assert_eq!(doc.layers.len(), 2, "nothing was cut or consumed");
    }

    #[test]
    fn compound_add_bevels_the_merged_outline_not_internal_edges() {
        let (left, right) = overlapping_pair();
        let mut plain_doc = Document::new(UVec2::splat(96));
        plain_doc
            .layers
            .push(Layer::group("plain", vec![left.clone(), right.clone()]));
        let plain = plain_doc.evaluate();
        assert!(plain.get(58, 48).x.abs() > 0.01);

        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(compound(VectorOp::Add, vec![left, right]));
        let merged = doc.evaluate();
        assert!((*merged.get(58, 48) - Vec3::Z).length() < 0.01);
    }

    #[test]
    fn compound_subtract_cuts_a_hole_before_beveling() {
        let outer = rect_layer("outer", Vec2::splat(48.0), Vec2::splat(80.0));
        let hole = rect_layer("hole", Vec2::splat(48.0), Vec2::splat(30.0));
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers
            .push(compound(VectorOp::Subtract, vec![outer, hole]));
        let normal = doc.evaluate();
        let height = doc.evaluate_height();
        assert!((*normal.get(48, 48) - Vec3::Z).length() < 0.01);
        assert!(*height.get(48, 48) < 0.01);
        assert!(*height.get(70, 48) > 0.05);
        assert!(normal.get(66, 48).length_squared() > 0.9);
        assert!(normal.get(66, 48).x.abs() > 0.01);
    }

    #[test]
    fn compound_xor_removes_the_overlap() {
        let (left, right) = overlapping_pair();
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(compound(VectorOp::Xor, vec![left, right]));
        let normal = doc.evaluate();
        let height = doc.evaluate_height();
        assert!(*height.get(48, 48) < 0.01);
        assert!((*normal.get(48, 48) - Vec3::Z).length() < 0.01);
        assert!(*height.get(20, 48) > 0.05);
    }

    #[test]
    fn compound_divide_bevels_internal_cuts() {
        let (left, right) = overlapping_pair();
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers
            .push(compound(VectorOp::Divide, vec![left, right]));
        let normal = doc.evaluate();
        // x=58 is inside the A∩B island, six pixels from its right edge.
        assert!(normal.get(58, 48).length_squared() > 0.9);
        assert!(normal.get(58, 48).x.abs() > 0.01);
    }

    #[test]
    fn divide_regions_partition_the_union() {
        let size = UVec2::splat(96);
        let operands: Vec<VectorOperand> = [
            rect_layer("left", Vec2::new(32.0, 48.0), Vec2::splat(64.0)),
            rect_layer("right", Vec2::new(64.0, 48.0), Vec2::splat(64.0)),
        ]
        .iter()
        .filter_map(|layer| match &layer.kind {
            LayerKind::Shape(shape) => shape.boolean_operand(size),
            _ => None,
        })
        .collect();
        let regions = divide_loops(&operands);
        assert_eq!(regions.len(), 3, "A-only, overlap and B-only");
        let union = ShapeSource::Path(Path::from_loops(
            boolean_loops(&operands, &[PathBoolean::Add]),
            FillRule::EvenOdd,
        ))
        .rasterize(96, 96);
        let grids: Vec<Grid<f32>> = regions
            .iter()
            .map(|loops| {
                ShapeSource::Path(Path::from_loops(loops.clone(), FillRule::EvenOdd))
                    .rasterize(96, 96)
            })
            .collect();
        for y in 0..96u32 {
            for x in 0..96u32 {
                let sum: f32 = grids.iter().map(|grid| *grid.get(x, y)).sum();
                let union = *union.get(x, y);
                assert!(
                    (sum - union).abs() < 1.0e-2,
                    "regions must tile the union exactly (x={x} y={y} sum={sum} union={union})"
                );
            }
        }

        // The merged coverage keeps the internal seams fully covered, so a
        // Divide group still clips and blends like one solid shape.
        let paths: Vec<Path> = regions
            .into_iter()
            .map(|loops| Path::from_loops(loops, FillRule::EvenOdd))
            .collect();
        let merged = regions_coverage(size, &paths);
        for x in [31u32, 32, 33, 63, 64, 65] {
            assert!(*merged.get(x, 48) > 0.99, "seam at x={x}");
        }
    }

    #[test]
    fn compound_cached_evaluation_matches_uncached() {
        let (left, right) = overlapping_pair();
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers
            .push(compound(VectorOp::Divide, vec![left, right]));
        let expected_normal = doc.evaluate();
        let expected_height = doc.evaluate_height();

        let mut cache = EvalCache::new();
        let mut cached_normal = doc.evaluate_with(&mut cache);
        let cached_height = doc.evaluate_height_with(&mut cache);
        for (a, b) in cached_normal.data.iter().zip(expected_normal.data.iter()) {
            assert!((*a - *b).length() < 1.0e-6);
        }
        for (a, b) in cached_height.data.iter().zip(expected_height.data.iter()) {
            assert!((*a - *b).abs() < 1.0e-6);
        }

        // Changing the live operation invalidates the cached compound.
        doc.layers[0].vector_op = Some(VectorOp::Add);
        doc.layers[0].touch();
        cached_normal = doc.evaluate_with(&mut cache);
        assert!((*cached_normal.get(58, 48) - Vec3::Z).length() < 0.01);
    }

    #[test]
    fn flatten_vector_compound_keeps_the_result_and_compositing() {
        let outer = rect_layer("outer", Vec2::splat(48.0), Vec2::splat(80.0));
        let hole = rect_layer("hole", Vec2::splat(48.0), Vec2::splat(30.0));
        let mut group = compound(VectorOp::Subtract, vec![outer, hole]);
        group.name = "cut ring".into();
        group.opacity = 0.55;
        group.blend = BlendMode::Replace;
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(group);

        let flattened = doc.flatten_vector_compound(&[0]).expect("compound flatten");
        assert_eq!(flattened, vec![0]);
        assert_eq!(doc.layers.len(), 1);
        let layer = &doc.layers[0];
        assert_eq!(layer.name, "cut ring");
        assert!((layer.opacity - 0.55).abs() < 1.0e-6);
        assert_eq!(layer.blend, BlendMode::Replace);
        assert!(layer.vector_op.is_none());
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("flattened compound should be a shape layer");
        };
        assert!(matches!(shape.source, ShapeSource::Path(_)));

        let height = doc.evaluate_height();
        assert!(
            *height.get(48, 48) < 0.01,
            "the traced hole should stay open"
        );
        assert!(*height.get(70, 48) > 0.05, "the ring should keep its bevel");
    }

    #[test]
    fn flatten_cut_compound_preserves_the_cut_surface() {
        let mut plate = rect_layer("plate", Vec2::splat(48.0), Vec2::splat(80.0));
        if let LayerKind::Shape(shape) = &mut plate.kind {
            shape.height_px = 24.0;
            shape.radius_px = 4.0;
        }
        let mut pocket = rect_layer("pocket", Vec2::splat(48.0), Vec2::splat(32.0));
        if let LayerKind::Shape(shape) = &mut pocket.kind {
            shape.height_px = 10.0;
            shape.radius_px = 2.0;
            shape.profile = Profile::Linear;
        }
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers
            .push(cut_group(VectorOp::PartialCut, plate, vec![pocket]));

        let before_height = doc.evaluate_height();
        let before_normal = doc.evaluate();
        let flattened = doc.flatten_cut_compound(&[0]).expect("cut flatten");
        assert_eq!(flattened, vec![0]);
        assert!(doc.layers[0].vector_op.is_none());
        let LayerKind::Group { children } = &doc.layers[0].kind else {
            panic!("baked cut should stay a group");
        };
        assert_eq!(children[0].blend, BlendMode::Derivative);
        assert_eq!(children[1].blend, BlendMode::PartialCut);

        let after_height = doc.evaluate_height();
        let after_normal = doc.evaluate();
        for (x, y) in [(20, 48), (36, 48), (48, 48), (60, 48), (80, 48)] {
            let before = *before_height.get(x, y);
            let after = *after_height.get(x, y);
            assert!(
                (after - before).abs() < 1.0e-4,
                "height at {x},{y}: {before} -> {after}"
            );
            let normal_delta = (*before_normal.get(x, y) - *after_normal.get(x, y)).length();
            assert!(
                normal_delta < 1.0e-4,
                "normal at {x},{y} moved by {normal_delta}"
            );
        }
    }

    #[test]
    fn baking_a_group_to_pixels_merges_it_into_one_layer() {
        let (left, right) = overlapping_pair();
        let mut group = Layer::group("stack", vec![left, right]);
        group.opacity = 0.6;
        let id = group.id;
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(group);
        let before = doc.evaluate();

        let merged = doc.bake_group_to_pixels(&[0]).expect("group bake");
        assert_eq!(merged, vec![0]);
        assert_eq!(doc.layers.len(), 1);
        let layer = &doc.layers[0];
        assert_eq!(layer.id, id, "references to the group must keep working");
        assert_eq!(layer.name, "stack");
        assert!((layer.opacity - 0.6).abs() < 1.0e-6);
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("a group bake must stay a movable shape layer");
        };
        assert!(matches!(shape.source, ShapeSource::RasterRegion(_)));
        assert!(
            shape.baked_normal.is_some(),
            "the exact group normals must survive the bake"
        );

        let after = doc.evaluate();
        let mut worst = 0.0f32;
        for (a, b) in before.data.iter().zip(after.data.iter()) {
            worst = worst.max((*a - *b).length());
        }
        assert!(worst < 1.0e-5, "merged group moved normals by {worst}");
    }

    #[test]
    fn a_baked_group_moves_and_carries_its_normals() {
        let (left, right) = overlapping_pair();
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(Layer::group("stack", vec![left, right]));
        doc.bake_group_to_pixels(&[0]).expect("group bake");

        let before_normals = doc.evaluate();
        let LayerKind::Shape(shape) = &mut doc.layers[0].kind else {
            panic!("a group bake must stay a shape layer");
        };
        let before = shape.source.bounds().expect("baked bounds");
        let delta = Vec2::new(9.0, 5.0);
        shape.source = shape
            .source
            .transformed(Vec2::ONE, delta)
            .expect("baked coverage moves");
        doc.layers[0].touch();

        let after_normals = doc.evaluate();
        let LayerKind::Shape(shape) = &doc.layers[0].kind else {
            panic!("a group bake must stay a shape layer");
        };
        let after = shape.source.bounds().expect("moved bounds");
        assert!(
            (after.center() - before.center() - delta).length() < 1.0e-4,
            "bounds did not move by {delta:?}"
        );
        // The baked normal surface must travel with the coverage, not stay at
        // the old document position.
        let center = before.center();
        let moved_center = center + delta;
        let old_normal = *before_normals.get(center.x as u32, center.y as u32);
        let new_normal = *after_normals.get(moved_center.x as u32, moved_center.y as u32);
        assert!(
            (old_normal - new_normal).length() < 1.0e-4,
            "the moved group lost its surface: {old_normal:?} vs {new_normal:?}"
        );
    }

    #[test]
    fn a_group_bake_ignores_hidden_mask_sources_when_sizing_the_result() {
        let mut mask = Layer::flat("mask source");
        mask.visible = false;
        mask.hidden_as_mask = true;
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(Layer::group(
            "masked",
            vec![
                rect_layer("panel", Vec2::splat(48.0), Vec2::splat(20.0)),
                mask,
            ],
        ));

        doc.bake_group_to_pixels(&[0]).expect("group bake");
        let LayerKind::Shape(shape) = &doc.layers[0].kind else {
            panic!("a group bake must stay a shape layer");
        };
        let bounds = shape.source.bounds().expect("baked bounds");
        assert!(
            bounds.size().x < 40.0,
            "a hidden full-canvas mask must not set the baked size: {bounds:?}"
        );
    }

    #[test]
    fn baking_a_cut_group_to_pixels_keeps_its_surface_in_one_layer() {
        let mut plate = rect_layer("plate", Vec2::splat(48.0), Vec2::splat(80.0));
        if let LayerKind::Shape(shape) = &mut plate.kind {
            shape.height_px = 24.0;
            shape.radius_px = 4.0;
        }
        let mut pocket = rect_layer("pocket", Vec2::splat(48.0), Vec2::splat(32.0));
        if let LayerKind::Shape(shape) = &mut pocket.kind {
            shape.height_px = 10.0;
            shape.radius_px = 2.0;
            shape.profile = Profile::Linear;
        }
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers
            .push(cut_group(VectorOp::PartialCut, plate, vec![pocket]));
        let before = doc.evaluate();

        doc.bake_group_to_pixels(&[0]).expect("cut group bake");

        assert_eq!(doc.layers.len(), 1);
        let LayerKind::Shape(shape) = &doc.layers[0].kind else {
            panic!("a cut group bake must stay a movable shape layer");
        };
        assert!(matches!(shape.source, ShapeSource::RasterRegion(_)));
        assert!(shape.baked_normal.is_some());
        let after = doc.evaluate();
        for (x, y) in [(20, 48), (36, 48), (48, 48), (60, 48), (80, 48)] {
            let delta = (*before.get(x, y) - *after.get(x, y)).length();
            assert!(
                delta < 1.0e-4,
                "baked cut normal at {x},{y} moved by {delta}"
            );
        }
    }

    #[test]
    fn release_divide_compound_creates_editable_islands() {
        let (left, right) = overlapping_pair();
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers
            .push(compound(VectorOp::Divide, vec![left, right]));

        let islands = doc.release_divide_compound(&[0]).expect("divide release");
        assert_eq!(islands.len(), 3, "two operands produce three regions");
        assert_eq!(doc.layers.len(), 3);
        for (index, path) in islands.iter().enumerate() {
            assert_eq!(path, &vec![index]);
            let layer = &doc.layers[index];
            assert!(layer.name.contains("island"), "island name: {}", layer.name);
            assert!(
                matches!(&layer.kind, LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Path(_))),
                "islands must be editable paths"
            );
        }
        let height = doc.evaluate_height();
        assert!(*height.get(58, 48) > 0.05, "overlap island keeps its bevel");
        assert!(*height.get(20, 48) > 0.05, "left island keeps its bevel");
        assert!(*height.get(80, 48) > 0.05, "right island keeps its bevel");
    }

    #[test]
    fn evaluate_layer_normal_isolates_a_group_from_the_rest_of_the_stack() {
        let (left, right) = overlapping_pair();
        let group = Layer::group("group", vec![left, right]);
        let group_id = group.id;
        let mut doc = Document::new(UVec2::splat(96));
        // A flat layer below and an unrelated shape above should not leak into
        // the isolated result.
        doc.layers.push(Layer::fill("base", 12.0));
        doc.layers.push(group);
        doc.layers.push(rect_layer(
            "unrelated",
            Vec2::new(80.0, 12.0),
            Vec2::splat(20.0),
        ));

        let isolated = doc
            .evaluate_layer_normal(group_id)
            .expect("group should evaluate");
        let mut expected_doc = Document::new(UVec2::splat(96));
        expected_doc.layers.push(Layer::group(
            "group",
            vec![
                rect_layer("left", Vec2::new(32.0, 48.0), Vec2::splat(64.0)),
                rect_layer("right", Vec2::new(64.0, 48.0), Vec2::splat(64.0)),
            ],
        ));
        let expected = expected_doc.evaluate();
        for (a, b) in isolated.data.iter().zip(expected.data.iter()) {
            assert!((*a - *b).length() < 1.0e-6);
        }
    }

    #[test]
    fn editing_a_compound_operand_updates_the_result() {
        let (left, right) = overlapping_pair();
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(compound(VectorOp::Add, vec![left, right]));
        let before = doc.evaluate_height();
        assert!(*before.get(70, 40) > 0.05);

        if let LayerKind::Group { children } = &mut doc.layers[0].kind {
            if let LayerKind::Shape(shape) = &mut children[1].kind {
                shape.source = ShapeSource::Primitive(Primitive::Rect {
                    center: Vec2::new(64.0, 90.0),
                    size: Vec2::splat(64.0),
                    corner_radius: 4.0,
                });
            }
            children[1].touch();
        }
        doc.layers[0].touch();
        let after = doc.evaluate_height();
        assert!(
            *after.get(70, 40) < 0.01,
            "operand edit should update the compound"
        );
        assert!(*after.get(70, 90) > 0.05);
    }

    #[test]
    fn estimated_bytes_count_masks_and_pixel_grids() {
        let mut doc = Document::new(UVec2::new(8, 8));
        let empty = doc.estimated_bytes();

        let mut masked = rect_layer("masked", Vec2::splat(4.0), Vec2::splat(4.0));
        masked.mask = Some(Grid::new(8, 8, 1.0));
        doc.layers.push(masked);
        assert!(doc.estimated_bytes() >= empty + 8 * 8 * 4);

        let before = doc.estimated_bytes();
        doc.layers
            .push(Layer::imported_normal("baked", Grid::new(8, 8, Vec3::Z)));
        assert!(
            doc.estimated_bytes() >= before + 8 * 8 * std::mem::size_of::<Vec3>(),
            "imported normals are counted at their real size"
        );

        let group = Layer::group(
            "group",
            vec![rect_layer("child", Vec2::splat(4.0), Vec2::splat(4.0))],
        );
        assert!(group.estimated_bytes() > std::mem::size_of::<Layer>());
    }

    #[test]
    fn cached_region_normals_match_the_uncached_oracle() {
        let mut doc = Document::new(UVec2::new(192, 192));
        doc.layers
            .push(rect_layer("big", Vec2::splat(96.0), Vec2::splat(160.0)));
        doc.layers.push(rect_layer(
            "small",
            Vec2::new(30.0, 30.0),
            Vec2::splat(24.0),
        ));
        let mut cache = EvalCache::new();
        let expected = doc.evaluate();
        let cached = doc.evaluate_with(&mut cache);
        compare_grids(&expected, &cached);

        // Moving one shape must still leave the cached path identical to the
        // uncached oracle, which is what makes region caches safe.
        if let LayerKind::Shape(shape) = &mut doc.layers[1].kind {
            shape.source = ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(150.0, 150.0),
                size: Vec2::splat(24.0),
                corner_radius: 4.0,
            });
        }
        doc.layers[1].touch();
        let expected = doc.evaluate();
        let cached = doc.evaluate_with(&mut cache);
        compare_grids(&expected, &cached);
    }

    #[test]
    fn cached_normals_cover_only_the_shape_bounds() {
        let mut doc = Document::new(UVec2::splat(256));
        doc.layers.push(rect_layer(
            "small",
            Vec2::new(40.0, 40.0),
            Vec2::splat(20.0),
        ));
        let mut cache = EvalCache::new();
        doc.evaluate_with(&mut cache);
        doc.evaluate_height_with(&mut cache);
        let entry = &cache.entries[&doc.layers[0].id];
        match entry.normal.as_ref().expect("normal cached") {
            CachedNormal::Region { origin, grid } => {
                assert!(origin.x > 0 && origin.y > 0, "padding stays off-canvas");
                assert!(
                    grid.w < 256 && grid.h < 256,
                    "a small shape keeps a small grid"
                );
            }
            CachedNormal::Full(_) => panic!("a small shape must use a region cache"),
        }
        match entry
            .height
            .as_ref()
            .and_then(|height| height.as_ref())
            .expect("height cached")
        {
            CachedHeight::Region { grid, .. } => assert!(grid.w < 256 && grid.h < 256),
            CachedHeight::Full(_) => panic!("a small shape must use a region height cache"),
        }
    }

    #[test]
    fn repeat_documents_keep_full_layer_caches() {
        let mut doc = Document::new(UVec2::splat(64));
        doc.wrap = Wrap::Repeat;
        doc.layers
            .push(rect_layer("bar", Vec2::splat(32.0), Vec2::splat(20.0)));
        let mut cache = EvalCache::new();
        doc.evaluate_with(&mut cache);
        doc.evaluate_height_with(&mut cache);
        let entry = &cache.entries[&doc.layers[0].id];
        assert!(
            matches!(entry.normal.as_ref(), Some(CachedNormal::Full(_))),
            "wrapped SDFs need the neighbouring tiles, so they stay full"
        );
        assert!(matches!(
            entry.height.as_ref().and_then(|height| height.as_ref()),
            Some(CachedHeight::Full(_))
        ));
    }

    #[test]
    fn replace_blend_normals_are_not_regioned() {
        let mut layer = rect_layer("stamp", Vec2::new(32.0, 32.0), Vec2::splat(20.0));
        layer.blend = BlendMode::Replace;
        let mut doc = Document::new(UVec2::splat(64));
        doc.layers.push(layer);
        let mut cache = EvalCache::new();
        doc.evaluate_with(&mut cache);
        let entry = &cache.entries[&doc.layers[0].id];
        assert!(
            matches!(entry.normal.as_ref(), Some(CachedNormal::Full(_))),
            "Replace needs the untouched surface outside the shape"
        );
    }

    #[test]
    fn repeat_wrap_changes_the_bevel_only_at_the_seam() {
        let make = |wrap: Wrap| {
            let mut doc = Document::new(UVec2::new(64, 64));
            doc.wrap = wrap;
            let mut layer = rect_layer("bar", Vec2::new(60.0, 32.0), Vec2::new(16.0, 40.0));
            if let LayerKind::Shape(shape) = &mut layer.kind {
                shape.radius_px = 8.0;
                shape.height_px = 10.0;
            }
            doc.layers.push(layer);
            doc.evaluate_height()
        };
        let repeat = make(Wrap::Repeat);
        let clamp = make(Wrap::Clamp);
        // Away from the seam the two wrappings agree. The bevel reaches 8 px,
        // so pixels closer than that to x = 64 are deliberately excluded.
        for y in 24..40 {
            for x in 52..56 {
                assert!(
                    (*repeat.get(x, y) - *clamp.get(x, y)).abs() < 1e-4,
                    "interior pixel ({x},{y}) changed with the wrap mode"
                );
            }
        }
        // At the right edge Repeat measures to the next tile's background and
        // cuts the bevel, while Clamp lets the surface run off the canvas.
        let seam_repeat = *repeat.get(63, 32);
        let seam_clamp = *clamp.get(63, 32);
        assert!(
            seam_repeat < seam_clamp,
            "repeat {seam_repeat} should be cut below clamp {seam_clamp}"
        );
        assert!(
            *repeat.get(0, 32) < 1e-4,
            "the clipped bar must not teleport to the opposite edge"
        );
    }

    /// Pixel-exact comparison used by the cache-vs-oracle tests.
    fn compare_grids(a: &Grid<Vec3>, b: &Grid<Vec3>) {
        assert_eq!((a.w, a.h), (b.w, b.h));
        for (index, (left, right)) in a.data.iter().zip(b.data.iter()).enumerate() {
            assert!(
                (*left - *right).length() < 1e-5,
                "pixel {index} differs: {left:?} vs {right:?}"
            );
        }
    }

    /// Square-cornered panel with the standard test bevel.
    fn panel_layer(name: &str, center: Vec2, size: Vec2) -> Layer {
        let mut layer = Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size,
                corner_radius: 0.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 12.0;
            shape.height_px = 10.0;
        }
        layer
    }

    fn trim_panel_doc() -> Document {
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers
            .push(panel_layer("Panel", Vec2::splat(48.0), Vec2::splat(80.0)));
        doc
    }

    /// Closed vertical strip used by the trim tests.
    fn strip_loops(x0: f32, x1: f32, height: f32) -> Vec<Vec<Vec2>> {
        vec![vec![
            Vec2::new(x0, 0.0),
            Vec2::new(x1, 0.0),
            Vec2::new(x1, height),
            Vec2::new(x0, height),
        ]]
    }

    #[test]
    fn trimming_a_shape_rebuilds_the_bevel() {
        let mut doc = trim_panel_doc();
        let outcome = doc
            .trim_layers_with_loops(&[vec![0]], &strip_loops(44.0, 52.0, 96.0))
            .expect("panel trimmed");
        assert_eq!(outcome.targets, 1);
        assert_eq!(outcome.paths.len(), 2, "the strip splits the panel");
        let names: Vec<&str> = doc.layers.iter().map(|layer| layer.name.as_str()).collect();
        assert_eq!(names, ["Panel 1/2", "Panel 2/2"]);

        // The left piece is the panel cut at x = 44. Its normal map must match
        // a freshly drawn shape with that exact outline: the bevel is rebuilt
        // from geometry, not masked away.
        let mut oracle = Document::new(UVec2::splat(96));
        oracle.layers.push(panel_layer(
            "Oracle",
            Vec2::new(26.0, 48.0),
            Vec2::new(36.0, 80.0),
        ));
        let piece = doc
            .evaluate_layer_normal(doc.layers[0].id)
            .expect("piece normal");
        let expected = oracle.evaluate();
        let mut worst = 0.0f32;
        for (left, right) in piece.data.iter().zip(expected.data.iter()) {
            worst = worst.max((*left - *right).length());
        }
        assert!(
            worst < 0.02,
            "piece normal differs from the expected outline: {worst}"
        );
    }

    #[test]
    fn the_trimmed_edge_gets_the_same_bevel() {
        let mut doc = trim_panel_doc();
        doc.trim_layers_with_loops(&[vec![0]], &strip_loops(44.0, 52.0, 96.0))
            .expect("panel trimmed");
        let normals = doc.evaluate();
        // Right edge of the left piece: the surface tilts into the new edge
        // instead of jumping straight to flat.
        let edge = *normals.get(43, 48);
        assert!(edge.x > 0.1, "cut edge should face +X, got {edge:?}");
        assert!(edge.z < 0.999, "cut edge should slope, got {edge:?}");
        // Deep inside the piece the surface stays flat.
        let flat = *normals.get(26, 48);
        assert!(flat.x.abs() < 0.02, "interior should be flat, got {flat:?}");
        // Nothing remains inside the trimmed strip.
        assert!((*normals.get(48, 48) - Vec3::Z).length() < 1e-3);
    }

    #[test]
    fn trimmed_pieces_inherit_every_style_parameter() {
        let mut layer = panel_layer("Panel", Vec2::splat(48.0), Vec2::splat(80.0));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 13.0;
            shape.height_px = 11.0;
            shape.profile = Profile::Chamfer { degrees: 30.0 };
            shape.invert = true;
            shape.softness_px = 2.0;
            shape.smooth_px = 1.0;
            shape.stroke_width_px = 9.0;
        }
        layer.opacity = 0.6;
        layer.blend = BlendMode::Rnm;
        layer.mask = Some(Grid::new(96, 96, 0.5));
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(layer);

        doc.trim_layers_with_loops(&[vec![0]], &strip_loops(44.0, 52.0, 96.0))
            .expect("panel trimmed");
        for piece in &doc.layers {
            let LayerKind::Shape(shape) = &piece.kind else {
                panic!("pieces are shapes")
            };
            assert_eq!(shape.radius_px, 13.0);
            assert_eq!(shape.height_px, 11.0);
            assert!(matches!(
                shape.profile,
                Profile::Chamfer { degrees } if (degrees - 30.0).abs() < 1e-4
            ));
            assert!(shape.invert);
            assert_eq!(shape.softness_px, 2.0);
            assert_eq!(shape.smooth_px, 1.0);
            assert_eq!(piece.opacity, 0.6);
            assert!(matches!(piece.blend, BlendMode::Rnm));
            assert!(piece.mask.is_some(), "the painted mask is inherited");
            assert_eq!(piece.mask.as_ref().unwrap().w, 96);
        }
    }

    #[test]
    fn a_loop_covering_the_whole_shape_removes_it() {
        let mut doc = trim_panel_doc();
        let outcome = doc
            .trim_layers_with_loops(&[vec![0]], &strip_loops(0.0, 96.0, 96.0))
            .expect("panel trimmed");
        assert_eq!(outcome.targets, 1);
        assert!(outcome.paths.is_empty());
        assert!(doc.layers.is_empty(), "no empty layer is left behind");
    }

    #[test]
    fn a_cutter_covering_the_target_removes_both() {
        let mut doc = trim_panel_doc();
        doc.layers
            .push(panel_layer("Cutter", Vec2::splat(48.0), Vec2::splat(120.0)));
        let outcome = doc.cut_layers_below(&[1]).expect("full cut");
        assert_eq!(outcome.targets, 1);
        assert!(outcome.paths.is_empty());
        assert!(doc.layers.is_empty(), "target and cutter are both consumed");
    }

    #[test]
    fn trim_ignores_targets_outside_the_loops() {
        let mut doc = trim_panel_doc();
        let before = doc.layers[0].content_revision();
        assert!(doc
            .trim_layers_with_loops(&[vec![0]], &strip_loops(90.0, 96.0, 96.0))
            .is_none());
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].content_revision(), before);
    }

    #[test]
    fn degenerate_loops_change_nothing() {
        let mut doc = trim_panel_doc();
        assert!(doc
            .trim_layers_with_loops(
                &[vec![0]],
                &[vec![Vec2::new(10.0, 10.0), Vec2::new(60.0, 10.0)]]
            )
            .is_none());
        assert!(doc
            .trim_layers_with_loops(
                &[vec![0]],
                &[vec![
                    Vec2::new(10.0, 10.0),
                    Vec2::new(30.0, 10.0),
                    Vec2::new(50.0, 10.0)
                ]]
            )
            .is_none());
        assert_eq!(doc.layers.len(), 1);
    }

    #[test]
    fn hidden_targets_are_not_trimmed() {
        let mut doc = trim_panel_doc();
        doc.layers[0].visible = false;
        assert!(doc
            .trim_layers_with_loops(&[vec![0]], &strip_loops(44.0, 52.0, 96.0))
            .is_none());
        assert_eq!(doc.layers.len(), 1);
    }

    #[test]
    fn dividing_a_shape_keeps_both_sides() {
        let mut doc = trim_panel_doc();
        // A lasso over the right half of the 80 px panel (8..88).
        let outcome = doc
            .divide_layers_with_loops(&[vec![0]], &strip_loops(48.0, 96.0, 96.0))
            .expect("panel divided");
        assert_eq!(outcome.targets, 1);
        assert_eq!(outcome.paths.len(), 2, "one piece per side");
        assert_eq!(doc.layers.len(), 2);

        let bounds: Vec<Bounds> = doc
            .layers
            .iter()
            .map(|layer| match &layer.kind {
                LayerKind::Shape(shape) => shape.source.bounds().expect("piece bounds"),
                _ => panic!("pieces are shapes"),
            })
            .collect();
        // The remainder under the region's piece: left half, then right half.
        assert!((bounds[0].max.x - 48.0).abs() < 0.5, "{:?}", bounds[0]);
        assert!((bounds[1].min.x - 48.0).abs() < 0.5, "{:?}", bounds[1]);
        assert!((bounds[0].min.x - 8.0).abs() < 0.5, "{:?}", bounds[0]);
        assert!((bounds[1].max.x - 88.0).abs() < 0.5, "{:?}", bounds[1]);
        // The bevel survives the split.
        for layer in &doc.layers {
            let LayerKind::Shape(shape) = &layer.kind else {
                panic!("pieces are shapes")
            };
            assert!((shape.radius_px - 12.0).abs() < 1e-3);
            assert!((shape.height_px - 10.0).abs() < 1e-3);
        }
    }

    #[test]
    fn a_region_that_covers_the_shape_does_not_divide_it() {
        let mut doc = trim_panel_doc();
        assert!(doc
            .divide_layers_with_loops(&[vec![0]], &strip_loops(0.0, 96.0, 96.0))
            .is_none());
        assert_eq!(doc.layers.len(), 1, "nothing to split off");
    }

    #[test]
    fn a_region_that_misses_the_shape_does_not_divide_it() {
        let mut doc = trim_panel_doc();
        assert!(doc
            .divide_layers_with_loops(&[vec![0]], &strip_loops(90.0, 96.0, 96.0))
            .is_none());
        assert_eq!(doc.layers.len(), 1);
    }

    #[test]
    fn hidden_targets_are_not_divided() {
        let mut doc = trim_panel_doc();
        doc.layers[0].visible = false;
        assert!(doc
            .divide_layers_with_loops(&[vec![0]], &strip_loops(48.0, 96.0, 96.0))
            .is_none());
        assert_eq!(doc.layers.len(), 1);
    }

    #[test]
    fn trimming_a_stroke_shape_makes_filled_pieces() {
        let mut layer = panel_layer("Ring", Vec2::splat(48.0), Vec2::splat(80.0));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.style = ShapeStyle::Stroke;
            shape.stroke_width_px = 10.0;
            shape.radius_px = 4.0;
        }
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(layer);
        let outcome = doc
            .trim_layers_with_loops(&[vec![0]], &strip_loops(44.0, 52.0, 96.0))
            .expect("band trimmed");
        assert!(!outcome.paths.is_empty());
        for piece in &doc.layers {
            let LayerKind::Shape(shape) = &piece.kind else {
                panic!("pieces are shapes")
            };
            assert!(
                !shape.style.is_stroke(),
                "a trimmed band must stay filled, not be banded again"
            );
        }
    }

    #[test]
    fn trimming_a_hole_piece_keeps_a_bevel_around_it() {
        let mut doc = trim_panel_doc();
        let hole = vec![vec![
            Vec2::new(32.0, 32.0),
            Vec2::new(64.0, 32.0),
            Vec2::new(64.0, 64.0),
            Vec2::new(32.0, 64.0),
        ]];
        let outcome = doc
            .trim_layers_with_loops(&[vec![0]], &hole)
            .expect("hole cut");
        assert_eq!(outcome.paths.len(), 1);
        let LayerKind::Shape(shape) = &doc.layers[0].kind else {
            panic!("piece is a shape")
        };
        let ShapeSource::Path(path) = &shape.source else {
            panic!("piece is a path")
        };
        assert_eq!(path.fill_rule, FillRule::EvenOdd);
        assert!(path.subpaths.len() >= 2, "outer loop plus the hole");

        let normals = doc.evaluate();
        let hole_edge = *normals.get(31, 48);
        assert!(
            hole_edge.x.abs() > 0.05,
            "the hole edge should slope, got {hole_edge:?}"
        );
        assert!((*normals.get(48, 48) - Vec3::Z).length() < 1e-3);
    }

    #[test]
    fn trimming_a_group_flattens_it_to_pieces() {
        let group = Layer::group(
            "Pair",
            vec![
                panel_layer("left", Vec2::new(30.0, 48.0), Vec2::new(40.0, 60.0)),
                panel_layer("right", Vec2::new(66.0, 48.0), Vec2::new(40.0, 60.0)),
            ],
        );
        let mut doc = Document::new(UVec2::splat(96));
        doc.layers.push(group);
        let outcome = doc
            .trim_layers_with_loops(&[vec![0]], &strip_loops(44.0, 52.0, 96.0))
            .expect("group trimmed");
        assert!(outcome.paths.len() >= 2);
        assert!(
            doc.layers
                .iter()
                .all(|layer| matches!(layer.kind, LayerKind::Shape(_))),
            "a trimmed group becomes flat pieces"
        );
        assert!(doc
            .layers
            .iter()
            .all(|layer| layer.name.starts_with("Pair")));
    }

    #[test]
    fn simple_pieces_stay_gpu_packable_and_holes_fall_back() {
        let square = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(0.0, 10.0),
        ];
        let solid = piece_path(vec![square.clone()]);
        assert_eq!(solid.fill_rule, FillRule::NonZero);
        assert!(
            ShapeSource::Path(solid).as_polygon().is_some(),
            "single islands stay packable by the GPU"
        );

        let hole = vec![
            Vec2::new(3.0, 3.0),
            Vec2::new(7.0, 3.0),
            Vec2::new(7.0, 7.0),
            Vec2::new(3.0, 7.0),
        ];
        let ring = piece_path(vec![square, hole]);
        assert_eq!(ring.fill_rule, FillRule::EvenOdd);
        assert!(
            ShapeSource::Path(ring).as_polygon().is_none(),
            "holes must fall back to the CPU evaluator"
        );
    }

    #[test]
    fn validate_rejects_bad_document_size() {
        for size in [
            UVec2::new(0, 8),
            UVec2::new(8, 0),
            UVec2::new(MAX_DOC_SIDE + 1, 8),
            UVec2::new(8, MAX_DOC_SIDE + 1),
        ] {
            let mut doc = Document::new(UVec2::new(8, 8));
            doc.size = size;
            assert!(doc.validate().is_err(), "{size:?} should be rejected");
        }
        assert!(Document::new(UVec2::splat(8)).validate().is_ok());
    }

    #[test]
    fn validate_rejects_a_mask_that_does_not_match_the_document() {
        let mut doc = Document::new(UVec2::splat(8));
        let mut layer = Layer::fill("masked", 0.5);
        layer.mask = Some(Grid::new(4, 4, 1.0));
        doc.layers.push(layer);
        let error = doc.validate().unwrap_err().to_string();
        assert!(error.contains("mask"), "{error}");
    }

    #[test]
    fn validate_rejects_excessive_nesting_and_layer_counts() {
        let mut doc = Document::new(UVec2::splat(8));
        let mut nested = Layer::flat("leaf");
        for depth in 0..=MAX_LAYER_DEPTH {
            nested = Layer::group(format!("group {depth}"), vec![nested]);
        }
        doc.layers.push(nested);
        assert!(doc.validate().is_err(), "deep nesting must be rejected");

        let mut doc = Document::new(UVec2::splat(8));
        doc.layers = (0..=MAX_LAYER_COUNT)
            .map(|index| Layer::flat(format!("layer {index}")))
            .collect();
        assert!(doc.validate().is_err(), "the layer cap must be enforced");
    }
}
