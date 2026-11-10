//! Vector operations exposed in the Properties dock.
//!
//! This is the first increment of the Operations system: metadata plus a pure,
//! deterministic placement evaluation. Applying a plan duplicates the selection
//! and transforms the copies as one undo step (see `ui/operations.rs`).
//!
//! Keep operation ids stable after release: saved settings may reference them.

use glam::Vec2;
use ns_core::{Bounds, LayerId};

/// Stable operation identifier.
pub type OperationId = &'static str;

/// Operations shipped so far, in the order they appear in the dock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OperationKind {
    #[default]
    Array,
    Mirror,
    Pyramid,
    Radial,
    Sweep,
}

impl OperationKind {
    pub const ALL: [Self; 5] = [
        Self::Array,
        Self::Mirror,
        Self::Pyramid,
        Self::Radial,
        Self::Sweep,
    ];

    pub fn id(self) -> OperationId {
        match self {
            Self::Array => "array",
            Self::Mirror => "mirror",
            Self::Pyramid => "pyramid",
            Self::Radial => "radial",
            Self::Sweep => "sweep",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Array => "Array",
            Self::Mirror => "Mirror",
            Self::Pyramid => "Pyramid",
            Self::Radial => "Radial Array",
            Self::Sweep => "Sweep along path",
        }
    }

    /// Compact form for the equal-width add buttons in the dock.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Array => "Array",
            Self::Mirror => "Mirror",
            Self::Pyramid => "Pyramid",
            Self::Radial => "Radial",
            Self::Sweep => "Sweep",
        }
    }

    /// One line under the operation picker. Says what the copies become.
    pub fn description(self) -> &'static str {
        match self {
            Self::Array => {
                "Duplicate the selected shapes along X and Y. Copies stay the same size."
            }
            Self::Mirror => "Reflect the selected shapes across an axis as new layers.",
            Self::Pyramid => "Stack copies that scale and step each time, forming a pyramid.",
            Self::Radial => {
                "Arrange copies on a ring around the selection, with optional random scale, \
                 distance and shape variation."
            }
            Self::Sweep => {
                "Place copies along a path guide, turning each one to follow the path's tangent."
            }
        }
    }

    pub fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Array => &["repeat", "grid", "pattern", "duplicate", "rows", "columns"],
            Self::Mirror => &["reflect", "symmetry", "flip", "axis", "left", "right"],
            Self::Pyramid => &["pyramid", "stack", "scale", "shrink", "nested", "tower"],
            Self::Radial => &[
                "radial", "ring", "circle", "polar", "spiral", "around", "random",
            ],
            Self::Sweep => &[
                "sweep", "path", "along", "curve", "tangent", "spine", "guide", "flow",
            ],
        }
    }
}

/// One operation in a layer's stack.
///
/// Entries are evaluated in order against the same selection. A disabled entry
/// keeps its settings but contributes no preview and is skipped by Apply, so
/// operations are never active unless the user turns them on.
#[derive(Clone, Debug, PartialEq)]
pub struct OperationEntry {
    pub enabled: bool,
    pub kind: OperationKind,
    /// Hide the source layers while the entry is active. Copies still use the
    /// source's geometry; only the original is removed from the rendered stack.
    pub hide_original: bool,
    pub array: ArrayParams,
    pub mirror: MirrorParams,
    pub pyramid: PyramidParams,
    pub radial: RadialParams,
    pub sweep: SweepParams,
}

impl OperationEntry {
    pub fn new(kind: OperationKind) -> Self {
        Self {
            enabled: true,
            kind,
            hide_original: false,
            array: ArrayParams::default(),
            mirror: MirrorParams::default(),
            pyramid: PyramidParams::default(),
            radial: RadialParams::default(),
            sweep: SweepParams::default(),
        }
    }

    /// Absolute placements of this entry against the selection bounds.
    ///
    /// `source_count` is the number of selected top-level layers or groups; the
    /// radial operation uses it to choose a shape per copy in random mode.
    /// `sweep_path` is the flattened spine of the entry's path guide, which only
    /// the sweep operation reads.
    pub fn placements(
        &self,
        bounds: Bounds,
        document_size: Vec2,
        source_count: usize,
        sweep_path: Option<&SweepSpine>,
    ) -> Vec<Placement> {
        match self.kind {
            OperationKind::Array => self.array.placements(),
            OperationKind::Mirror => self.mirror.placements(bounds, document_size),
            OperationKind::Pyramid => self.pyramid.placements(bounds.center()),
            OperationKind::Radial => self.radial.placements(bounds, document_size, source_count),
            OperationKind::Sweep => self.sweep.placements(
                bounds,
                sweep_path.unwrap_or(&SweepSpine::default()),
                source_count,
            ),
        }
    }
}

/// One layer's contribution to the aggregate operation preview.
#[derive(Clone, Debug, PartialEq)]
pub struct OperationPreviewSource {
    pub layer: LayerId,
    pub stack: Vec<OperationEntry>,
    pub paths: Vec<Vec<usize>>,
}

/// Key that decides when the live preview document must be rebuilt.
///
/// It contains only cheap, comparable state: every layer's enabled operation
/// stack, the selection paths each stack is planned against, and enough
/// document counters to notice edits, undo and redo without hashing every
/// layer. Keeping all stacks here is what lets a layer's preview survive when
/// the artist selects a different layer.
#[derive(Clone, Debug, PartialEq)]
pub struct OperationPreviewKey {
    pub sources: Vec<OperationPreviewSource>,
    pub edit_serial: u64,
    pub undo_len: usize,
    pub redo_len: usize,
    pub layer_count: usize,
}

/// One affine copy: `p' = offset + R(rotation) * (p * scale)`.
///
/// The app's shape transform supports scale and offset; radial copies also need
/// rotation, so it is stored here and applied by `ShapeSource`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub scale: Vec2,
    pub offset: Vec2,
    /// Radians, positive counter-clockwise on screen (Y points down, so the
    /// visible turn is clockwise for a positive angle).
    pub rotation: f32,
    /// Index into the planned source list for a copy that uses one shape; `None`
    /// copies every selected shape.
    pub source: Option<usize>,
}

impl Placement {
    /// Any mirrored axis. Stars cannot represent this with their radius and
    /// rotation fields alone, so the apply step converts them to a path first.
    pub fn has_negative_scale(self) -> bool {
        self.scale.x < 0.0 || self.scale.y < 0.0
    }

    /// A copy that would sit exactly on the original and keep its orientation.
    pub fn is_identity(self) -> bool {
        (self.scale - Vec2::ONE).length() < 1e-4
            && self.rotation.abs() < 1e-4
            && self.offset.length() < 1e-4
    }
}

// ---------------------------------------------------------------------------
// Safety limits (guide section 12)
// ---------------------------------------------------------------------------

/// Warn above this many copies; the dock stays usable.
pub const SOFT_COPY_LIMIT: usize = 2_000;
/// Refuse above this many copies.
pub const HARD_COPY_LIMIT: usize = 4_096;
/// Warn above this many total shapes.
pub const SOFT_SHAPE_LIMIT: usize = 2_000;
/// Refuse above this many total shapes.
pub const HARD_SHAPE_LIMIT: usize = 20_000;

/// `Ok(Some(warning))` for a large but allowed plan, `Err` for a refused one.
pub fn check_limits(copies: usize, shapes: usize) -> Result<Option<String>, String> {
    if copies > HARD_COPY_LIMIT {
        return Err(format!(
            "This would create {copies} copies; the limit is {HARD_COPY_LIMIT}."
        ));
    }
    if shapes > HARD_SHAPE_LIMIT {
        return Err(format!(
            "This would create {shapes} shapes; the limit is {HARD_SHAPE_LIMIT}. \
             Reduce the grid or the number of selected shapes."
        ));
    }
    if copies > SOFT_COPY_LIMIT || shapes > SOFT_SHAPE_LIMIT {
        return Ok(Some(format!(
            "Large operation: {copies} copies and {shapes} shapes. This may take a moment."
        )));
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Array
// ---------------------------------------------------------------------------

/// Plain linear / grid repeat. Every copy keeps the original size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrayParams {
    pub columns: u32,
    pub rows: u32,
    /// Step between columns and rows, in document pixels.
    pub spacing: Vec2,
    /// Extra offset added once per row, for staggered patterns.
    pub row_shift: Vec2,
}

impl Default for ArrayParams {
    fn default() -> Self {
        Self {
            columns: 3,
            rows: 1,
            spacing: Vec2::new(48.0, 48.0),
            row_shift: Vec2::ZERO,
        }
    }
}

impl ArrayParams {
    pub const MIN_COUNT: u32 = 1;
    pub const MAX_COUNT: u32 = 64;
    pub const SPACING_LIMIT: f32 = 1_024.0;

    pub fn columns(self) -> u32 {
        self.columns.clamp(Self::MIN_COUNT, Self::MAX_COUNT)
    }

    pub fn rows(self) -> u32 {
        self.rows.clamp(Self::MIN_COUNT, Self::MAX_COUNT)
    }

    /// Grid cells including the original at (0, 0).
    pub fn cells(self) -> usize {
        self.columns() as usize * self.rows() as usize
    }

    pub fn copies(self) -> usize {
        self.cells().saturating_sub(1)
    }

    /// Placements for every cell except (0, 0). Copies are pure translations,
    /// so columns repeat along X and rows repeat along Y.
    pub fn placements(self) -> Vec<Placement> {
        let columns = self.columns();
        let rows = self.rows();
        let spacing = self.spacing.clamp(
            Vec2::splat(-Self::SPACING_LIMIT),
            Vec2::splat(Self::SPACING_LIMIT),
        );
        let row_shift = self.row_shift.clamp(
            Vec2::splat(-Self::SPACING_LIMIT),
            Vec2::splat(Self::SPACING_LIMIT),
        );
        let mut out = Vec::with_capacity(self.copies());
        for row in 0..rows {
            for column in 0..columns {
                if row == 0 && column == 0 {
                    continue;
                }
                let grid = Vec2::new(column as f32 * spacing.x, row as f32 * spacing.y)
                    + row_shift * row as f32;
                out.push(Placement {
                    scale: Vec2::ONE,
                    offset: grid,
                    rotation: 0.0,
                    source: None,
                });
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Pyramid
// ---------------------------------------------------------------------------

/// A stack of copies where every step shrinks (or grows) and moves the copy,
/// producing the classic pyramid / stepped tower silhouette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PyramidParams {
    /// Number of copies stacked on top of the original.
    pub copies: u32,
    /// Each copy is scaled by this factor relative to the previous one.
    pub scale_step: f32,
    /// Offset added once per copy, in document pixels. Negative Y rises.
    pub step: Vec2,
}

impl Default for PyramidParams {
    fn default() -> Self {
        Self {
            copies: 4,
            scale_step: 0.85,
            step: Vec2::new(0.0, -28.0),
        }
    }
}

impl PyramidParams {
    pub const MIN_COPIES: u32 = 1;
    pub const MAX_COPIES: u32 = 64;
    pub const STEP_LIMIT: f32 = 1_024.0;
    pub const SCALE_MIN: f32 = 0.5;
    pub const SCALE_MAX: f32 = 1.5;
    /// Compounded scales are clamped so a long stack cannot overflow.
    const SCALE_LIMIT_MIN: f32 = 0.05;
    const SCALE_LIMIT_MAX: f32 = 16.0;

    pub fn copies(self) -> usize {
        self.copies.clamp(Self::MIN_COPIES, Self::MAX_COPIES) as usize
    }

    /// Copies are scaled about `pivot` (the selection bounds center) and moved
    /// by `step` per level. Step 1 is the smallest copy above the original.
    pub fn placements(self, pivot: Vec2) -> Vec<Placement> {
        let step = self.step.clamp(
            Vec2::splat(-Self::STEP_LIMIT),
            Vec2::splat(Self::STEP_LIMIT),
        );
        let scale_step = self.scale_step.clamp(Self::SCALE_MIN, Self::SCALE_MAX);
        (1..=self.copies())
            .map(|level| {
                let scale = scale_step
                    .powi(level as i32)
                    .clamp(Self::SCALE_LIMIT_MIN, Self::SCALE_LIMIT_MAX);
                let offset = pivot * (Vec2::ONE - Vec2::splat(scale)) + step * level as f32;
                Placement {
                    scale: Vec2::splat(scale),
                    offset,
                    rotation: 0.0,
                    source: None,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Mirror
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MirrorAxis {
    /// Reflect left to right across a vertical line.
    #[default]
    Vertical,
    /// Reflect top to bottom across a horizontal line.
    Horizontal,
    /// Reflect both axes at once.
    Both,
}

impl MirrorAxis {
    pub const ALL: [Self; 3] = [Self::Vertical, Self::Horizontal, Self::Both];

    pub fn label(self) -> &'static str {
        match self {
            Self::Vertical => "Left / right",
            Self::Horizontal => "Top / bottom",
            Self::Both => "Both",
        }
    }
}

/// Which side of the mirror plane is the authoritative source.
///
/// `Both` is the original behaviour: keep the whole shape and add a reflected
/// copy. `Negative`/`Positive` mirror one half onto the other, replacing the
/// destination half — Blender's symmetrize direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MirrorSource {
    /// Keep both sides and add a reflected copy.
    #[default]
    Both,
    /// Keep the negative side (left/top) and mirror it to the positive side.
    Negative,
    /// Keep the positive side (right/bottom) and mirror it to the negative side.
    Positive,
}

impl MirrorSource {
    pub const ALL: [Self; 3] = [Self::Both, Self::Negative, Self::Positive];

    pub fn is_source_side(self) -> bool {
        !matches!(self, Self::Both)
    }

    /// Label that names the direction for the active axis, so the combo never
    /// says "negative" when the user is thinking "left" or "top".
    pub fn label(self, axis: MirrorAxis) -> &'static str {
        match (self, axis) {
            (Self::Both, _) => "Both sides",
            (Self::Negative, MirrorAxis::Vertical) => "Left → right",
            (Self::Positive, MirrorAxis::Vertical) => "Right → left",
            (Self::Negative, MirrorAxis::Horizontal) => "Top → bottom",
            (Self::Positive, MirrorAxis::Horizontal) => "Bottom → top",
            (Self::Negative, MirrorAxis::Both) => "Top-left → bottom-right",
            (Self::Positive, MirrorAxis::Both) => "Bottom-right → top-left",
        }
    }

    pub fn source_name(self, axis: MirrorAxis) -> &'static str {
        match (self, axis) {
            (Self::Both, _) => "both sides",
            (Self::Negative, MirrorAxis::Vertical) => "left side",
            (Self::Positive, MirrorAxis::Vertical) => "right side",
            (Self::Negative, MirrorAxis::Horizontal) => "top side",
            (Self::Positive, MirrorAxis::Horizontal) => "bottom side",
            (Self::Negative, MirrorAxis::Both) => "top-left quadrant",
            (Self::Positive, MirrorAxis::Both) => "bottom-right quadrant",
        }
    }

    pub fn target_name(self, axis: MirrorAxis) -> &'static str {
        match (self, axis) {
            (Self::Both, _) => "both sides",
            (Self::Negative, MirrorAxis::Vertical) => "right side",
            (Self::Positive, MirrorAxis::Vertical) => "left side",
            (Self::Negative, MirrorAxis::Horizontal) => "bottom side",
            (Self::Positive, MirrorAxis::Horizontal) => "top side",
            (Self::Negative, MirrorAxis::Both) => "bottom-right quadrant",
            (Self::Positive, MirrorAxis::Both) => "top-left quadrant",
        }
    }

    pub fn hint(self, axis: MirrorAxis) -> String {
        if self == Self::Both {
            return "Keep the original on both sides of the pivot and add a reflected copy."
                .to_string();
        }
        format!(
            "Keep only the {} and mirror it onto the {}. The {} of the original is replaced.",
            self.source_name(axis),
            self.target_name(axis),
            self.target_name(axis),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MirrorPivot {
    /// The selection's right/bottom edge: the copy sits next to the original.
    #[default]
    SelectionEdge,
    /// The selection center: the copy overlaps the original.
    SelectionCenter,
    /// The middle of the document.
    DocumentCenter,
}

impl MirrorPivot {
    pub const ALL: [Self; 3] = [
        Self::SelectionEdge,
        Self::SelectionCenter,
        Self::DocumentCenter,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::SelectionEdge => "Selection edge",
            Self::SelectionCenter => "Selection center",
            Self::DocumentCenter => "Document center",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MirrorParams {
    pub axis: MirrorAxis,
    pub pivot: MirrorPivot,
    /// Which half is authoritative. `Both` keeps the original and adds a
    /// reflection; a single side replaces the opposite half.
    pub source: MirrorSource,
    /// Extra distance between the original and the reflection, in pixels.
    pub gap: f32,
    /// Bisect the original and the reflection at the pivot plane. Each side keeps
    /// only its own half, so the result cannot overlap itself.
    pub clip: bool,
    /// Weld the two halves into one shape when their seams meet within
    /// [`Self::merge_threshold`], like Blender's mirror Merge.
    pub merge: bool,
    /// Largest seam gap, in pixels, that Merge still welds.
    pub merge_threshold: f32,
}

impl Default for MirrorParams {
    fn default() -> Self {
        Self {
            axis: MirrorAxis::Vertical,
            pivot: MirrorPivot::SelectionEdge,
            source: MirrorSource::Both,
            gap: 0.0,
            // Blender's defaults: clipping and merging keep a mirror precise.
            clip: true,
            merge: true,
            merge_threshold: 1.0,
        }
    }
}

impl MirrorParams {
    pub const GAP_LIMIT: f32 = 512.0;
    pub const MERGE_LIMIT: f32 = 16.0;

    /// The mirror line (`Vertical`/`Horizontal`) or corner (`Both`) in document
    /// space. `document_size` is only used for the document pivot.
    pub fn pivot_point(self, bounds: Bounds, document_size: Vec2) -> Vec2 {
        let center = bounds.center();
        match self.pivot {
            MirrorPivot::SelectionEdge => match self.axis {
                MirrorAxis::Vertical => Vec2::new(bounds.max.x, center.y),
                MirrorAxis::Horizontal => Vec2::new(center.x, bounds.max.y),
                MirrorAxis::Both => bounds.max,
            },
            MirrorPivot::SelectionCenter => center,
            MirrorPivot::DocumentCenter => document_size * 0.5,
        }
    }

    /// One reflected copy. `document_size` is only used for the document pivot.
    pub fn placements(self, bounds: Bounds, document_size: Vec2) -> Vec<Placement> {
        let center = bounds.center();
        let pivot = self.pivot_point(bounds, document_size);
        let gap = self.gap.clamp(0.0, Self::GAP_LIMIT);
        // Push the copy away from the pivot on the side it lands. Using the
        // chosen source side (instead of the shape centre) keeps the gap
        // direction correct when the authoritative half is not the half the
        // centre happens to sit on.
        let side = self.reflected_side(self.source_side(pivot, center));
        let (scale, offset) = match self.axis {
            MirrorAxis::Vertical => (
                Vec2::new(-1.0, 1.0),
                Vec2::new(2.0 * pivot.x + side.x * gap, 0.0),
            ),
            MirrorAxis::Horizontal => (
                Vec2::new(1.0, -1.0),
                Vec2::new(0.0, 2.0 * pivot.y + side.y * gap),
            ),
            MirrorAxis::Both => (
                Vec2::new(-1.0, -1.0),
                Vec2::new(2.0 * pivot.x + side.x * gap, 2.0 * pivot.y + side.y * gap),
            ),
        };
        vec![Placement {
            scale,
            offset,
            rotation: 0.0,
            source: None,
        }]
    }

    /// Per-axis half-space the authoritative source occupies: `+1` keeps the
    /// positive side of the pivot, `-1` the negative side. Unused axes stay
    /// zero. `Both` derives the side from the source centre, preserving the
    /// original mirror behaviour.
    pub fn source_side(self, pivot: Vec2, center: Vec2) -> Vec2 {
        let mut side = Vec2::ZERO;
        if matches!(self.axis, MirrorAxis::Vertical | MirrorAxis::Both) {
            side.x = match self.source {
                MirrorSource::Both => {
                    if center.x >= pivot.x {
                        1.0
                    } else {
                        -1.0
                    }
                }
                MirrorSource::Negative => -1.0,
                MirrorSource::Positive => 1.0,
            };
        }
        if matches!(self.axis, MirrorAxis::Horizontal | MirrorAxis::Both) {
            side.y = match self.source {
                MirrorSource::Both => {
                    if center.y >= pivot.y {
                        1.0
                    } else {
                        -1.0
                    }
                }
                MirrorSource::Negative => -1.0,
                MirrorSource::Positive => 1.0,
            };
        }
        side
    }

    /// Clipping is mandatory when the user picks an authoritative side; the
    /// destination half must be removed before the reflection is laid down.
    pub fn clips_at_plane(self) -> bool {
        self.clip || self.source.is_source_side()
    }

    /// True when `bounds` actually reaches into the chosen source half-space.
    /// Used to reject a source side that would erase the selection.
    pub fn source_side_has_geometry(self, pivot: Vec2, bounds: Bounds) -> bool {
        if !self.source.is_source_side() {
            return true;
        }
        let side = self.source_side(pivot, bounds.center());
        let x_ok = !matches!(self.axis, MirrorAxis::Vertical | MirrorAxis::Both)
            || if side.x >= 0.0 {
                bounds.max.x > pivot.x
            } else {
                bounds.min.x < pivot.x
            };
        let y_ok = !matches!(self.axis, MirrorAxis::Horizontal | MirrorAxis::Both)
            || if side.y >= 0.0 {
                bounds.max.y > pivot.y
            } else {
                bounds.min.y < pivot.y
            };
        x_ok && y_ok
    }

    /// The half-space the reflection occupies: every mirrored axis flips.
    pub fn reflected_side(self, side: Vec2) -> Vec2 {
        match self.axis {
            MirrorAxis::Vertical => Vec2::new(-side.x, side.y),
            MirrorAxis::Horizontal => Vec2::new(side.x, -side.y),
            MirrorAxis::Both => -side,
        }
    }

    /// True when at least one mirror plane cuts through `bounds`.
    pub fn cuts(self, pivot: Vec2, bounds: Bounds) -> bool {
        let cuts_x = matches!(self.axis, MirrorAxis::Vertical | MirrorAxis::Both)
            && bounds.min.x < pivot.x
            && bounds.max.x > pivot.x;
        let cuts_y = matches!(self.axis, MirrorAxis::Horizontal | MirrorAxis::Both)
            && bounds.min.y < pivot.y
            && bounds.max.y > pivot.y;
        cuts_x || cuts_y
    }

    /// Closed loops for the kept half-plane (or quadrant), expanded past
    /// `extent` so the cutter comfortably covers the whole layer.
    pub fn clip_loops(self, pivot: Vec2, side: Vec2, extent: Bounds) -> Vec<Vec<Vec2>> {
        let margin = extent.size().length().max(1.0) * 2.0 + 64.0;
        let mut min = extent.min - Vec2::splat(margin);
        let mut max = extent.max + Vec2::splat(margin);
        if matches!(self.axis, MirrorAxis::Vertical | MirrorAxis::Both) {
            if side.x >= 0.0 {
                min.x = pivot.x;
            } else {
                max.x = pivot.x;
            }
        }
        if matches!(self.axis, MirrorAxis::Horizontal | MirrorAxis::Both) {
            if side.y >= 0.0 {
                min.y = pivot.y;
            } else {
                max.y = pivot.y;
            }
        }
        vec![vec![
            Vec2::new(min.x, min.y),
            Vec2::new(max.x, min.y),
            Vec2::new(max.x, max.y),
            Vec2::new(min.x, max.y),
        ]]
    }

    /// True when the two bounds meet (or overlap) on both axes within
    /// `threshold`. Merge only welds halves that actually touch, so a gapped
    /// or side-by-side reflection stays two independent layers.
    pub fn seams_touch(self, a: Bounds, b: Bounds, threshold: f32) -> bool {
        let threshold = threshold.clamp(0.0, Self::MERGE_LIMIT);
        let gap = |a0: f32, a1: f32, b0: f32, b1: f32| (b0 - a1).max(a0 - b1).max(0.0);
        gap(a.min.x, a.max.x, b.min.x, b.max.x) <= threshold
            && gap(a.min.y, a.max.y, b.min.y, b.max.y) <= threshold
    }
}

// ---------------------------------------------------------------------------
// Radial array
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadialScaleMode {
    /// Every copy keeps the same size.
    #[default]
    Equal,
    /// Each copy compounds the scale step.
    Step,
    /// Each copy picks a scale between the min and max.
    Random,
}

impl RadialScaleMode {
    pub const ALL: [Self; 3] = [Self::Equal, Self::Step, Self::Random];

    pub fn label(self) -> &'static str {
        match self {
            Self::Equal => "Equal",
            Self::Step => "Step",
            Self::Random => "Random",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadialDistanceMode {
    /// Every copy sits on the same radius.
    #[default]
    Equal,
    /// The radius is jittered per copy.
    Random,
}

impl RadialDistanceMode {
    pub const ALL: [Self; 2] = [Self::Equal, Self::Random];

    pub fn label(self) -> &'static str {
        match self {
            Self::Equal => "Equal",
            Self::Random => "Random",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadialShapeMode {
    /// Every selected shape is copied at every step.
    #[default]
    All,
    /// Each step copies one randomly chosen shape from the selection.
    Random,
}

impl RadialShapeMode {
    pub const ALL: [Self; 2] = [Self::All, Self::Random];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All shapes",
            Self::Random => "Random shape",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadialPivot {
    /// Arrange the ring around the selection's centre.
    #[default]
    SelectionCenter,
    /// Arrange the ring around the middle of the document.
    DocumentCenter,
}

impl RadialPivot {
    pub const ALL: [Self; 2] = [Self::SelectionCenter, Self::DocumentCenter];

    pub fn label(self) -> &'static str {
        match self {
            Self::SelectionCenter => "Selection center",
            Self::DocumentCenter => "Document center",
        }
    }
}

/// Copies arranged on a circle (or arc / spiral) around a pivot.
///
/// The original stays where it is; every copy's bounds centre lands on the
/// ring. `rotate` turns each copy to follow the ring, which is what makes a
/// radial array of rectangles read as a fan rather than a stack of uprights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialParams {
    /// Copies on the ring, excluding the original.
    pub copies: u32,
    /// Distance from the pivot to each copy centre, in document pixels.
    pub radius: f32,
    /// Radius added per copy, turning the ring into a spiral.
    pub radius_step: f32,
    /// Angle of the first copy, in degrees.
    pub start_angle: f32,
    /// Arc covered by the copies, in degrees. 360 fills the ring.
    pub sweep: f32,
    /// Turn each copy to follow its angle on the ring.
    pub rotate: bool,
    pub pivot: RadialPivot,
    pub scale_mode: RadialScaleMode,
    /// Size of every copy in equal mode.
    pub scale: f32,
    /// Compound factor per copy in step mode.
    pub scale_step: f32,
    /// Random scale range (inclusive) in random mode.
    pub scale_min: f32,
    pub scale_max: f32,
    pub distance_mode: RadialDistanceMode,
    /// Fraction of the radius the random distance may vary by (0..=1).
    pub distance_jitter: f32,
    pub shape_mode: RadialShapeMode,
    /// Seed for the random modes; the same seed always builds the same ring.
    pub seed: u32,
}

impl Default for RadialParams {
    fn default() -> Self {
        Self {
            copies: 6,
            radius: 120.0,
            radius_step: 0.0,
            start_angle: 0.0,
            sweep: 360.0,
            rotate: true,
            pivot: RadialPivot::SelectionCenter,
            scale_mode: RadialScaleMode::Equal,
            scale: 1.0,
            scale_step: 0.9,
            scale_min: 0.6,
            scale_max: 1.2,
            distance_mode: RadialDistanceMode::Equal,
            distance_jitter: 0.25,
            shape_mode: RadialShapeMode::All,
            seed: 1,
        }
    }
}

impl RadialParams {
    pub const MIN_COPIES: u32 = 1;
    pub const MAX_COPIES: u32 = 64;
    pub const RADIUS_LIMIT: f32 = 2_048.0;
    pub const RADIUS_STEP_LIMIT: f32 = 512.0;
    pub const SCALE_MIN: f32 = 0.05;
    pub const SCALE_MAX: f32 = 4.0;
    /// Compounded scales are clamped so a long ring cannot overflow.
    const SCALE_LIMIT_MIN: f32 = 0.01;
    const SCALE_LIMIT_MAX: f32 = 16.0;

    pub fn copies(self) -> usize {
        self.copies.clamp(Self::MIN_COPIES, Self::MAX_COPIES) as usize
    }

    /// Placements for every copy. `source_count` is the number of selected
    /// top-level layers/groups; random shape mode uses it to choose one per
    /// copy, and every other mode copies the whole selection.
    pub fn placements(
        self,
        bounds: Bounds,
        document_size: Vec2,
        source_count: usize,
    ) -> Vec<Placement> {
        let copies = self.copies();
        let center = bounds.center();
        let pivot = match self.pivot {
            RadialPivot::SelectionCenter => center,
            RadialPivot::DocumentCenter => document_size * 0.5,
        };
        let radius = self.radius.clamp(0.0, Self::RADIUS_LIMIT);
        let radius_step = self
            .radius_step
            .clamp(-Self::RADIUS_STEP_LIMIT, Self::RADIUS_STEP_LIMIT);
        let start = self.start_angle.to_radians();
        let sweep = self.sweep.clamp(0.0, 360.0).to_radians();
        // The original sits at the pivot, so N copies divide the arc into N
        // steps; a full turn therefore has no gap and no duplicate at 360º.
        let angle_step = sweep / copies as f32;
        let jitter = self.distance_jitter.clamp(0.0, 1.0);
        let scale_step = self.scale_step.clamp(Self::SCALE_MIN, Self::SCALE_MAX);
        let (scale_low, scale_high) = if self.scale_min <= self.scale_max {
            (self.scale_min, self.scale_max)
        } else {
            (self.scale_max, self.scale_min)
        };

        (0..copies)
            .map(|index| {
                let order = index as f32 + 1.0;
                let angle = start + angle_step * order;
                let distance = (radius + radius_step * order).max(0.0)
                    * match self.distance_mode {
                        RadialDistanceMode::Equal => 1.0,
                        RadialDistanceMode::Random => {
                            1.0 + jitter * random_signed(self.seed, 0x00D1_5A7C, index as u32)
                        }
                    };
                let target = pivot + Vec2::new(angle.cos(), angle.sin()) * distance;
                let scale = match self.scale_mode {
                    RadialScaleMode::Equal => self.scale,
                    RadialScaleMode::Step => scale_step.powf(order),
                    RadialScaleMode::Random => {
                        scale_low
                            + (scale_high - scale_low)
                                * random_unit(self.seed, 0x5CA1_E000, index as u32)
                    }
                }
                .clamp(Self::SCALE_LIMIT_MIN, Self::SCALE_LIMIT_MAX);
                let rotation = if self.rotate { angle } else { 0.0 };
                // p' = target + R(rotation) * (p - center) * scale, expanded to
                // the placement's p' = offset + R(rotation) * (p * scale).
                let offset = target - rotate_vec(center * scale, rotation);
                let source = match self.shape_mode {
                    RadialShapeMode::All => None,
                    RadialShapeMode::Random if source_count > 1 => Some(
                        (random_unit(self.seed, 0x5417_0000, index as u32) * source_count as f32)
                            as usize
                            % source_count,
                    ),
                    RadialShapeMode::Random => None,
                };
                Placement {
                    scale: Vec2::splat(scale),
                    offset,
                    rotation,
                    source,
                }
            })
            .collect()
    }
}

/// Deterministic 24-bit value in `[0, 1)`; same seed and index always agree, so
/// the live preview and Apply place random copies identically.
fn random_unit(seed: u32, salt: u32, index: u32) -> f32 {
    let mut value =
        (seed as u64) ^ ((salt as u64) << 32) ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    (value >> 40) as f32 / (1u32 << 24) as f32
}

/// Deterministic value in `[-1, 1)`.
fn random_signed(seed: u32, salt: u32, index: u32) -> f32 {
    random_unit(seed, salt, index) * 2.0 - 1.0
}

fn rotate_vec(value: Vec2, angle: f32) -> Vec2 {
    let (sin, cos) = angle.sin_cos();
    Vec2::new(value.x * cos - value.y * sin, value.x * sin + value.y * cos)
}

// ---------------------------------------------------------------------------
// Sweep
// ---------------------------------------------------------------------------

/// How multiple source slots are used along the path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SweepShapeMode {
    /// Every filled slot is copied at every placement.
    #[default]
    Together,
    /// One filled slot is used per placement, cycling through the slots.
    Cycle,
}

impl SweepShapeMode {
    pub const ALL: [Self; 2] = [Self::Together, Self::Cycle];

    pub fn label(self) -> &'static str {
        match self {
            Self::Together => "Together",
            Self::Cycle => "Cycle",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Together => "Every filled slot is copied at each placement.",
            Self::Cycle => "Use one filled slot per placement and cycle through them.",
        }
    }
}

/// Copies marched along a path guide, optionally turned to its tangent.
///
/// The guide is not painted: it is a vector path marked `Path` in its
/// properties. The original selection stays where it is, matching the other
/// operations, while the copies sample the spine from `start` to `end`.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepParams {
    /// Copies placed along the path, excluding the original.
    pub copies: u32,
    /// First sample as a fraction of the path's length (0 is its start).
    pub start: f32,
    /// Last sample as a fraction of the path's length (1 is its end).
    pub end: f32,
    /// Turn each copy so its local +X axis follows the path tangent.
    pub rotate: bool,
    /// Extra rotation in degrees, applied after the tangent alignment.
    pub angle_offset: f32,
    /// Path guide this sweep follows. `None` means no path was chosen yet.
    pub path: Option<LayerId>,
    /// Shapes dragged into the entry's source slots. A filled slot replaces
    /// the selection as the thing that gets swept; empty slots fall back to
    /// the selection so the original workflow still works.
    pub sources: Vec<Option<LayerId>>,
    /// Whether several filled slots are copied together or cycled per copy.
    pub shape_mode: SweepShapeMode,
}

impl Default for SweepParams {
    fn default() -> Self {
        Self {
            copies: 5,
            start: 0.0,
            end: 1.0,
            rotate: true,
            angle_offset: 0.0,
            path: None,
            sources: vec![None],
            shape_mode: SweepShapeMode::default(),
        }
    }
}

impl SweepParams {
    pub const MIN_COPIES: u32 = 1;
    pub const MAX_COPIES: u32 = 64;

    pub fn copies(&self) -> usize {
        self.copies.clamp(Self::MIN_COPIES, Self::MAX_COPIES) as usize
    }

    /// Placements for every copy, evenly spaced by arc length.
    ///
    /// The first copy lands at `start` and the last at `end`; a single copy sits
    /// in the middle of that span so a one-copy sweep still reads as "on the
    /// path". A closed guide that is swept over its whole length excludes the
    /// end sample: the first and last copies would otherwise land on the same
    /// point where the loop meets itself.
    /// Placements for every copy, evenly spaced by arc length.
    ///
    /// `source_count` is the number of filled source slots. In
    /// [`SweepShapeMode::Cycle`] each placement records which slot it uses, so
    /// Apply and the preview copy only that shape.
    pub fn placements(
        &self,
        bounds: Bounds,
        spine: &SweepSpine,
        source_count: usize,
    ) -> Vec<Placement> {
        let samples = sample_polyline(
            &spine.points,
            self.copies(),
            self.start,
            self.end,
            spine.closed,
        );
        let center = bounds.center();
        let offset_angle = self.angle_offset.to_radians();
        samples
            .into_iter()
            .enumerate()
            .map(|(index, (point, tangent))| {
                let rotation = if self.rotate {
                    tangent.y.atan2(tangent.x) + offset_angle
                } else {
                    offset_angle
                };
                let source = match self.shape_mode {
                    SweepShapeMode::Together => None,
                    SweepShapeMode::Cycle if source_count > 0 => Some(index % source_count),
                    SweepShapeMode::Cycle => None,
                };
                // p' = point + R(rotation) * (p - center), expanded to the
                // placement's p' = offset + R(rotation) * p.
                Placement {
                    scale: Vec2::ONE,
                    offset: point - rotate_vec(center, rotation),
                    rotation,
                    source,
                }
            })
            .collect()
    }
}

/// Even arc-length samples along a flattened polyline, with the segment
/// direction at each sample.
///
/// `closed` means the points end where they start (the closing point is already
/// in the list). A full loop then uses a half-open range, while an open path and
/// a partial loop include both ends.
fn sample_polyline(
    path: &[Vec2],
    count: usize,
    start: f32,
    end: f32,
    closed: bool,
) -> Vec<(Vec2, Vec2)> {
    let mut points: Vec<Vec2> = Vec::with_capacity(path.len());
    for point in path {
        if !point.is_finite() {
            continue;
        }
        if points
            .last()
            .is_none_or(|last: &Vec2| last.distance_squared(*point) > 1e-8)
        {
            points.push(*point);
        }
    }
    if points.len() < 2 || count == 0 {
        return Vec::new();
    }
    let mut cumulative = Vec::with_capacity(points.len());
    let mut total = 0.0f32;
    cumulative.push(0.0);
    for pair in points.windows(2) {
        total += pair[0].distance(pair[1]);
        cumulative.push(total);
    }
    if total <= 1e-4 {
        return Vec::new();
    }
    let (low, high) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let low = low.clamp(0.0, 1.0);
    let high = high.clamp(0.0, 1.0);
    let span = high - low;
    let wraps = closed && span >= 1.0 - 1e-4;
    (0..count)
        .map(|index| {
            let t = if count == 1 {
                (low + high) * 0.5
            } else if wraps {
                low + span * index as f32 / count as f32
            } else {
                low + span * index as f32 / (count - 1) as f32
            };
            let distance = t * total;
            // The first cumulative entry past `distance` ends the segment that
            // contains it. Clamping keeps an exact end sample in the last
            // segment instead of running off the array.
            let segment = cumulative
                .partition_point(|value| *value <= distance)
                .clamp(1, points.len() - 1)
                - 1;
            let from = points[segment];
            let to = points[segment + 1];
            let seg = (cumulative[segment + 1] - cumulative[segment]).max(1e-6);
            let local = ((distance - cumulative[segment]) / seg).clamp(0.0, 1.0);
            let direction = (to - from).normalize_or_zero();
            (from.lerp(to, local), direction)
        })
        .collect()
}

/// Spine of a path guide: one continuous flattened polyline.
///
/// `closed` is true for a loop, in which case `points` ends on the first point
/// so the sweep can run around the seam.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SweepSpine {
    pub points: Vec<Vec2>,
    pub closed: bool,
}

impl SweepSpine {
    /// Convenience for tests and callers that already hold a flat polyline.
    #[cfg(test)]
    pub fn open(points: Vec<Vec2>) -> Self {
        Self {
            points,
            closed: false,
        }
    }
}

/// Build the spine from an editable path.
///
/// A guide can hold several subpaths; the sweep follows the longest one so it
/// never jumps across the gap between loops.
pub fn guide_spine(path: &ns_core::Path) -> SweepSpine {
    let mut best = SweepSpine::default();
    let mut best_length = 0.0f32;
    for (mut points, closed) in ns_core::path::flatten_path_subpaths(path) {
        if closed {
            if let (Some(first), Some(last)) = (points.first().copied(), points.last().copied()) {
                if first.distance_squared(last) > 1e-8 {
                    points.push(first);
                }
            }
        }
        let length: f32 = points
            .windows(2)
            .map(|pair| pair[0].distance(pair[1]))
            .sum();
        if length > best_length {
            best_length = length;
            best = SweepSpine { points, closed };
        }
    }
    best
}

/// Points of the guide's spine, for drawing the dashed overlay.
pub fn guide_polyline(path: &ns_core::Path) -> Vec<Vec2> {
    guide_spine(path).points
}

#[cfg(test)]
mod tests {
    use super::*;
    use ns_core::{FillRule, Node, Path, SubPath};

    fn approx(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-4
    }

    fn square() -> Bounds {
        Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(20.0, 20.0),
        }
    }

    fn open_spine(points: &[Vec2]) -> SweepSpine {
        SweepSpine::open(points.to_vec())
    }

    /// Where a placement puts the selection centre.
    fn placed_centre(bounds: Bounds, placement: &Placement) -> Vec2 {
        placement.offset + rotate_vec(bounds.center() * placement.scale, placement.rotation)
    }

    #[test]
    fn array_places_a_row_at_the_requested_spacing() {
        let params = ArrayParams {
            columns: 3,
            rows: 1,
            spacing: Vec2::new(10.0, 0.0),
            row_shift: Vec2::ZERO,
        };
        let placements = params.placements();
        assert_eq!(placements.len(), 2);
        assert!(approx(placements[0].offset, Vec2::new(10.0, 0.0)));
        assert!(approx(placements[1].offset, Vec2::new(20.0, 0.0)));
        assert!(placements.iter().all(|p| p.scale == Vec2::ONE));
    }

    #[test]
    fn array_places_a_column_along_y() {
        let params = ArrayParams {
            columns: 1,
            rows: 3,
            spacing: Vec2::new(0.0, 12.0),
            row_shift: Vec2::ZERO,
        };
        let placements = params.placements();
        assert_eq!(placements.len(), 2);
        assert!(approx(placements[0].offset, Vec2::new(0.0, 12.0)));
        assert!(approx(placements[1].offset, Vec2::new(0.0, 24.0)));
    }

    #[test]
    fn array_grid_uses_rows_and_the_stagger() {
        let params = ArrayParams {
            columns: 2,
            rows: 2,
            spacing: Vec2::new(10.0, 20.0),
            row_shift: Vec2::new(5.0, 0.0),
        };
        let placements = params.placements();
        // Row 0 column 1, row 1 column 0, row 1 column 1.
        assert_eq!(placements.len(), 3);
        assert!(approx(placements[0].offset, Vec2::new(10.0, 0.0)));
        assert!(approx(placements[1].offset, Vec2::new(5.0, 20.0)));
        assert!(approx(placements[2].offset, Vec2::new(15.0, 20.0)));
    }

    #[test]
    fn pyramid_scales_and_steps_each_copy_about_the_pivot() {
        let params = PyramidParams {
            copies: 2,
            scale_step: 0.5,
            step: Vec2::ZERO,
        };
        let placements = params.placements(Vec2::new(100.0, 0.0));
        assert_eq!(placements.len(), 2);
        assert!(approx(placements[0].scale, Vec2::splat(0.5)));
        assert!(approx(placements[0].offset, Vec2::new(50.0, 0.0)));
        assert!(approx(placements[1].scale, Vec2::splat(0.25)));
        assert!(approx(placements[1].offset, Vec2::new(75.0, 0.0)));
    }

    #[test]
    fn pyramid_adds_the_step_once_per_level() {
        let params = PyramidParams {
            copies: 3,
            scale_step: 1.0,
            step: Vec2::new(4.0, -10.0),
        };
        let placements = params.placements(Vec2::ZERO);
        assert_eq!(placements.len(), 3);
        assert!(approx(placements[0].offset, Vec2::new(4.0, -10.0)));
        assert!(approx(placements[1].offset, Vec2::new(8.0, -20.0)));
        assert!(approx(placements[2].offset, Vec2::new(12.0, -30.0)));
        assert!(placements.iter().all(|p| p.scale == Vec2::ONE));
    }

    #[test]
    fn huge_pyramid_scales_stay_finite() {
        let params = PyramidParams {
            copies: 64,
            scale_step: 1.5,
            step: Vec2::ZERO,
        };
        let placements = params.placements(Vec2::ZERO);
        assert_eq!(placements.len(), 64);
        assert!(placements
            .iter()
            .all(|p| p.scale.x.is_finite() && p.scale.x <= 16.0));
    }

    #[test]
    fn one_cell_array_has_no_copies() {
        let params = ArrayParams {
            columns: 1,
            rows: 1,
            ..Default::default()
        };
        assert_eq!(params.copies(), 0);
        assert!(params.placements().is_empty());
    }

    #[test]
    fn mirror_edge_places_the_copy_next_to_the_bounds() {
        let params = MirrorParams {
            axis: MirrorAxis::Vertical,
            pivot: MirrorPivot::SelectionEdge,
            gap: 5.0,
            ..Default::default()
        };
        let bounds = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(10.0, 20.0),
        };
        let placements = params.placements(bounds, Vec2::new(100.0, 100.0));
        assert_eq!(placements.len(), 1);
        assert!(approx(placements[0].scale, Vec2::new(-1.0, 1.0)));
        assert!(approx(placements[0].offset, Vec2::new(25.0, 0.0)));
        assert!(placements[0].has_negative_scale());
    }

    #[test]
    fn mirror_center_overlaps_the_original() {
        let params = MirrorParams {
            axis: MirrorAxis::Vertical,
            pivot: MirrorPivot::SelectionCenter,
            gap: 0.0,
            ..Default::default()
        };
        let bounds = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(10.0, 10.0),
        };
        let placements = params.placements(bounds, Vec2::splat(100.0));
        assert!(approx(placements[0].offset, Vec2::new(10.0, 0.0)));
    }

    #[test]
    fn mirror_both_axes_reflects_corner_to_corner() {
        let params = MirrorParams {
            axis: MirrorAxis::Both,
            pivot: MirrorPivot::SelectionEdge,
            gap: 2.0,
            ..Default::default()
        };
        let bounds = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(10.0, 20.0),
        };
        let placements = params.placements(bounds, Vec2::splat(100.0));
        assert!(approx(placements[0].scale, Vec2::new(-1.0, -1.0)));
        assert!(approx(placements[0].offset, Vec2::new(22.0, 42.0)));
        assert!(placements[0].has_negative_scale());
    }

    #[test]
    fn mirror_gap_moves_the_copy_away_from_the_plane() {
        let params = MirrorParams {
            axis: MirrorAxis::Vertical,
            pivot: MirrorPivot::DocumentCenter,
            gap: 6.0,
            ..Default::default()
        };
        let bounds = Bounds {
            min: Vec2::new(70.0, 0.0),
            max: Vec2::new(90.0, 10.0),
        };
        let placements = params.placements(bounds, Vec2::splat(100.0));
        // Source centre 80 is right of the pivot at 50; the reflection lands at
        // 20 and the gap pushes it 6 px further left (offset 94, centre 14).
        assert!(approx(placements[0].offset, Vec2::new(94.0, 0.0)));
        assert!(approx(
            placed_centre(bounds, &placements[0]),
            Vec2::new(14.0, 5.0)
        ));
    }

    #[test]
    fn mirror_source_side_overrides_the_center_for_gap_direction() {
        let params = MirrorParams {
            axis: MirrorAxis::Vertical,
            pivot: MirrorPivot::DocumentCenter,
            source: MirrorSource::Negative,
            gap: 5.0,
            ..Default::default()
        };
        let bounds = Bounds {
            min: Vec2::new(70.0, 0.0),
            max: Vec2::new(90.0, 10.0),
        };
        let pivot = params.pivot_point(bounds, Vec2::splat(100.0));
        assert!(approx(
            params.source_side(pivot, bounds.center()),
            Vec2::new(-1.0, 0.0)
        ));
        let placements = params.placements(bounds, Vec2::splat(100.0));
        // The copy lands left of the pivot at x=20, then the gap pushes it 5 px
        // further left. The old centre-derived direction would have pushed it
        // right because the shape centre is on the positive side.
        assert!(approx(placements[0].offset, Vec2::new(105.0, 0.0)));
        assert!(approx(
            placed_centre(bounds, &placements[0]),
            Vec2::new(25.0, 5.0)
        ));
    }

    #[test]
    fn mirror_source_side_forces_clipping_and_validates_geometry() {
        let params = MirrorParams {
            axis: MirrorAxis::Vertical,
            pivot: MirrorPivot::DocumentCenter,
            source: MirrorSource::Negative,
            clip: false,
            ..Default::default()
        };
        assert!(params.clips_at_plane());
        let pivot = Vec2::splat(50.0);
        let positive_only = Bounds {
            min: Vec2::new(70.0, 0.0),
            max: Vec2::new(90.0, 10.0),
        };
        assert!(!params.source_side_has_geometry(pivot, positive_only));
        let crossing = Bounds {
            min: Vec2::new(30.0, 0.0),
            max: Vec2::new(90.0, 10.0),
        };
        assert!(params.source_side_has_geometry(pivot, crossing));
    }

    #[test]
    fn mirror_horizontal_source_side_maps_top_and_bottom() {
        let params = MirrorParams {
            axis: MirrorAxis::Horizontal,
            pivot: MirrorPivot::DocumentCenter,
            source: MirrorSource::Negative,
            ..Default::default()
        };
        let pivot = Vec2::splat(50.0);
        let below = Bounds {
            min: Vec2::new(0.0, 70.0),
            max: Vec2::new(10.0, 90.0),
        };
        // Negative Y is the top half in image space.
        assert!(approx(
            params.source_side(pivot, below.center()),
            Vec2::new(0.0, -1.0)
        ));
        assert!(!params.source_side_has_geometry(pivot, below));
        let crossing = Bounds {
            min: Vec2::new(0.0, 30.0),
            max: Vec2::new(10.0, 90.0),
        };
        assert!(params.source_side_has_geometry(pivot, crossing));
    }

    #[test]
    fn mirror_both_axes_negative_source_keeps_the_negative_quadrant() {
        let params = MirrorParams {
            axis: MirrorAxis::Both,
            pivot: MirrorPivot::SelectionCenter,
            source: MirrorSource::Negative,
            ..Default::default()
        };
        let bounds = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(10.0, 20.0),
        };
        let pivot = params.pivot_point(bounds, Vec2::splat(100.0));
        let source = params.source_side(pivot, bounds.center());
        assert!(approx(source, Vec2::new(-1.0, -1.0)));
        assert!(approx(params.reflected_side(source), Vec2::new(1.0, 1.0)));
    }

    #[test]
    fn mirror_clip_loops_keep_the_chosen_half() {
        let params = MirrorParams::default();
        let extent = Bounds {
            min: Vec2::ZERO,
            max: Vec2::splat(100.0),
        };
        let pivot = Vec2::new(40.0, 50.0);
        let left = params.clip_loops(pivot, Vec2::new(-1.0, 0.0), extent);
        let left = Bounds::from_points(left[0].iter().copied()).unwrap();
        assert!(left.max.x <= 40.0 + 1e-4);
        assert!(left.min.x < -100.0, "cutter reaches past the canvas");

        let right = params.clip_loops(pivot, Vec2::new(1.0, 0.0), extent);
        let right = Bounds::from_points(right[0].iter().copied()).unwrap();
        assert!(right.min.x >= 40.0 - 1e-4);
        assert!(right.max.x > 200.0);
    }

    #[test]
    fn mirror_seams_touch_only_within_the_threshold() {
        let params = MirrorParams::default();
        let a = Bounds {
            min: Vec2::ZERO,
            max: Vec2::new(10.0, 10.0),
        };
        let b = Bounds {
            min: Vec2::new(10.5, 0.0),
            max: Vec2::new(20.5, 10.0),
        };
        assert!(params.seams_touch(a, b, 1.0));
        assert!(!params.seams_touch(a, b, 0.25));
        let far = Bounds {
            min: Vec2::new(30.0, 0.0),
            max: Vec2::new(40.0, 10.0),
        };
        assert!(!params.seams_touch(a, far, 1.0));
    }

    #[test]
    fn radial_puts_equal_copies_on_a_ring() {
        let params = RadialParams {
            copies: 4,
            radius: 100.0,
            radius_step: 0.0,
            start_angle: 0.0,
            sweep: 360.0,
            rotate: false,
            pivot: RadialPivot::SelectionCenter,
            scale_mode: RadialScaleMode::Equal,
            scale: 1.0,
            distance_mode: RadialDistanceMode::Equal,
            ..Default::default()
        };
        let bounds = square();
        let placements = params.placements(bounds, Vec2::splat(256.0), 1);
        assert_eq!(placements.len(), 4);
        for (index, placement) in placements.iter().enumerate() {
            let angle = (index as f32 + 1.0) * std::f32::consts::FRAC_PI_2;
            let expected = bounds.center() + Vec2::new(angle.cos(), angle.sin()) * 100.0;
            assert!(
                approx(placed_centre(bounds, placement), expected),
                "copy {index} landed at {:?}",
                placed_centre(bounds, placement)
            );
            assert!(placement.rotation.abs() < 1e-4, "rotation is off");
        }
    }

    #[test]
    fn radial_rotation_follows_the_ring() {
        let params = RadialParams {
            copies: 2,
            radius: 50.0,
            sweep: 180.0,
            rotate: true,
            ..Default::default()
        };
        let placements = params.placements(square(), Vec2::splat(256.0), 1);
        assert!((placements[0].rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
        assert!((placements[1].rotation - std::f32::consts::PI).abs() < 1e-4);
    }

    #[test]
    fn radial_scale_modes_are_equal_step_or_random() {
        let equal = RadialParams {
            copies: 3,
            scale_mode: RadialScaleMode::Equal,
            scale: 0.5,
            ..Default::default()
        }
        .placements(square(), Vec2::splat(256.0), 1);
        assert!(equal.iter().all(|p| approx(p.scale, Vec2::splat(0.5))));

        let step = RadialParams {
            copies: 3,
            scale_mode: RadialScaleMode::Step,
            scale_step: 0.5,
            ..Default::default()
        }
        .placements(square(), Vec2::splat(256.0), 1);
        assert!(approx(step[0].scale, Vec2::splat(0.5)));
        assert!(approx(step[1].scale, Vec2::splat(0.25)));
        assert!(approx(step[2].scale, Vec2::splat(0.125)));

        let random = RadialParams {
            copies: 16,
            scale_mode: RadialScaleMode::Random,
            scale_min: 0.6,
            scale_max: 0.8,
            ..Default::default()
        }
        .placements(square(), Vec2::splat(256.0), 1);
        assert!(random
            .iter()
            .all(|p| p.scale.x >= 0.6 - 1e-4 && p.scale.x <= 0.8 + 1e-4));
        assert!(
            random.windows(2).any(|pair| pair[0].scale != pair[1].scale),
            "random scale varied"
        );
    }

    #[test]
    fn radial_random_modes_are_seeded() {
        let params = RadialParams {
            copies: 12,
            distance_mode: RadialDistanceMode::Random,
            distance_jitter: 0.5,
            scale_mode: RadialScaleMode::Random,
            shape_mode: RadialShapeMode::Random,
            seed: 7,
            ..Default::default()
        };
        let first = params.placements(square(), Vec2::splat(256.0), 3);
        let second = params.placements(square(), Vec2::splat(256.0), 3);
        assert_eq!(first, second, "the same seed reproduces the ring");

        let mut other = params;
        other.seed = 8;
        assert_ne!(
            first,
            other.placements(square(), Vec2::splat(256.0), 3),
            "a new seed changes the ring"
        );
    }

    #[test]
    fn radial_random_distance_and_spiral_control_the_radius() {
        let bounds = square();
        let random = RadialParams {
            copies: 16,
            radius: 100.0,
            distance_mode: RadialDistanceMode::Random,
            distance_jitter: 0.5,
            ..Default::default()
        }
        .placements(bounds, Vec2::splat(256.0), 1);
        let distances: Vec<f32> = random
            .iter()
            .map(|p| (placed_centre(bounds, p) - bounds.center()).length())
            .collect();
        assert!(distances
            .iter()
            .all(|d| *d >= 50.0 - 1e-3 && *d <= 150.0 + 1e-3));
        assert!(
            distances
                .windows(2)
                .any(|pair| (pair[0] - pair[1]).abs() > 1e-3),
            "the jitter varies the distance"
        );

        let spiral = RadialParams {
            copies: 3,
            radius: 100.0,
            radius_step: 10.0,
            distance_mode: RadialDistanceMode::Equal,
            ..Default::default()
        }
        .placements(bounds, Vec2::splat(256.0), 1);
        let distance =
            |index: usize| (placed_centre(bounds, &spiral[index]) - bounds.center()).length();
        assert!((distance(0) - 110.0).abs() < 1e-3);
        assert!((distance(1) - 120.0).abs() < 1e-3);
        assert!((distance(2) - 130.0).abs() < 1e-3);
    }

    #[test]
    fn radial_random_shape_picks_one_source_per_copy() {
        let params = RadialParams {
            copies: 16,
            shape_mode: RadialShapeMode::Random,
            ..Default::default()
        };
        let placements = params.placements(square(), Vec2::splat(256.0), 3);
        assert!(placements
            .iter()
            .all(|p| p.source.is_some_and(|index| index < 3)));
        let mut chosen: Vec<usize> = placements.iter().filter_map(|p| p.source).collect();
        chosen.sort_unstable();
        chosen.dedup();
        assert!(chosen.len() > 1, "random shape mixes the selection");

        let all = RadialParams {
            copies: 4,
            shape_mode: RadialShapeMode::All,
            ..Default::default()
        }
        .placements(square(), Vec2::splat(256.0), 3);
        assert!(all.iter().all(|p| p.source.is_none()));
    }

    #[test]
    fn placement_evaluation_is_deterministic() {
        let params = ArrayParams {
            columns: 4,
            rows: 3,
            spacing: Vec2::new(13.0, 17.0),
            row_shift: Vec2::new(3.0, -2.0),
        };
        let first = params.placements();
        let second = params.placements();
        assert_eq!(first, second);

        let pyramid = PyramidParams {
            copies: 5,
            scale_step: 0.93,
            step: Vec2::new(2.0, -8.0),
        };
        assert_eq!(
            pyramid.placements(Vec2::new(32.0, 48.0)),
            pyramid.placements(Vec2::new(32.0, 48.0))
        );
    }

    #[test]
    fn sweep_samples_both_ends_of_a_straight_path() {
        let params = SweepParams {
            copies: 3,
            ..Default::default()
        };
        let path = [Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)];
        let placements = params.placements(square(), &open_spine(&path), 0);
        assert_eq!(placements.len(), 3);
        let centres: Vec<Vec2> = placements
            .iter()
            .map(|placement| placed_centre(square(), placement))
            .collect();
        assert!(approx(centres[0], Vec2::ZERO));
        assert!(approx(centres[1], Vec2::new(50.0, 0.0)));
        assert!(approx(centres[2], Vec2::new(100.0, 0.0)));
        assert!(placements.iter().all(|p| p.rotation.abs() < 1e-4));
    }

    #[test]
    fn sweep_turns_copies_to_the_path_tangent() {
        let params = SweepParams {
            copies: 2,
            ..Default::default()
        };
        let path = [Vec2::new(0.0, 0.0), Vec2::new(0.0, 100.0)];
        let placements = params.placements(square(), &open_spine(&path), 0);
        assert!(placements
            .iter()
            .all(|p| (p.rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-4));
    }

    #[test]
    fn sweep_rotation_can_be_switched_off() {
        let params = SweepParams {
            copies: 2,
            rotate: false,
            ..Default::default()
        };
        let path = [Vec2::new(0.0, 0.0), Vec2::new(0.0, 100.0)];
        assert!(params
            .placements(square(), &open_spine(&path), 0)
            .iter()
            .all(|p| p.rotation.abs() < 1e-4));
    }

    #[test]
    fn sweep_angle_offset_turns_every_copy() {
        let params = SweepParams {
            copies: 2,
            angle_offset: 90.0,
            ..Default::default()
        };
        let path = [Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)];
        assert!(params
            .placements(square(), &open_spine(&path), 0)
            .iter()
            .all(|p| (p.rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-4));
    }

    #[test]
    fn sweep_start_and_end_sample_a_subrange() {
        let params = SweepParams {
            copies: 3,
            start: 0.0,
            end: 0.5,
            ..Default::default()
        };
        let path = [Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)];
        let centres: Vec<Vec2> = params
            .placements(square(), &open_spine(&path), 0)
            .iter()
            .map(|placement| placed_centre(square(), placement))
            .collect();
        assert!(approx(centres[0], Vec2::ZERO));
        assert!(approx(centres[1], Vec2::new(25.0, 0.0)));
        assert!(approx(centres[2], Vec2::new(50.0, 0.0)));
    }

    #[test]
    fn sweep_one_copy_sits_in_the_middle_of_the_span() {
        let params = SweepParams {
            copies: 1,
            ..Default::default()
        };
        let path = [Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)];
        let placements = params.placements(square(), &open_spine(&path), 0);
        assert_eq!(placements.len(), 1);
        assert!(approx(
            placed_centre(square(), &placements[0]),
            Vec2::new(50.0, 0.0)
        ));
    }

    #[test]
    fn sweep_spaces_copies_by_arc_length() {
        // Two segments of different lengths: the middle sample follows the
        // distance along the path, not the segment index.
        let params = SweepParams {
            copies: 3,
            ..Default::default()
        };
        let path = [
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 10.0),
        ];
        let centres: Vec<Vec2> = params
            .placements(square(), &open_spine(&path), 0)
            .iter()
            .map(|placement| placed_centre(square(), placement))
            .collect();
        assert!(approx(centres[1], Vec2::new(55.0, 0.0)));
    }

    #[test]
    fn sweep_around_a_closed_guide_does_not_double_the_start() {
        let params = SweepParams {
            copies: 4,
            ..Default::default()
        };
        // A 100 x 100 loop, already closed back onto its first point.
        let spine = SweepSpine {
            points: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(0.0, 100.0),
                Vec2::new(100.0, 100.0),
                Vec2::new(100.0, 0.0),
                Vec2::new(0.0, 0.0),
            ],
            closed: true,
        };
        let centres: Vec<Vec2> = params
            .placements(square(), &spine, 0)
            .iter()
            .map(|placement| placed_centre(square(), placement))
            .collect();
        assert!(approx(centres[0], Vec2::ZERO));
        assert!(approx(centres[1], Vec2::new(0.0, 100.0)));
        assert!(approx(centres[2], Vec2::new(100.0, 100.0)));
        assert!(approx(centres[3], Vec2::new(100.0, 0.0)));
    }

    #[test]
    fn sweep_needs_two_distinct_points() {
        let params = SweepParams::default();
        assert!(params
            .placements(square(), &SweepSpine::default(), 1)
            .is_empty());
        assert!(params
            .placements(square(), &open_spine(&[Vec2::ONE, Vec2::ONE]), 1)
            .is_empty());
    }

    #[test]
    fn sweep_cycle_uses_one_slot_per_copy() {
        let params = SweepParams {
            copies: 4,
            shape_mode: SweepShapeMode::Cycle,
            ..Default::default()
        };
        let spine = open_spine(&[Vec2::ZERO, Vec2::new(100.0, 0.0)]);
        let sources: Vec<Option<usize>> = params
            .placements(square(), &spine, 2)
            .into_iter()
            .map(|placement| placement.source)
            .collect();
        assert_eq!(sources, vec![Some(0), Some(1), Some(0), Some(1)]);

        // Together copies every slot at every placement, as it did before.
        let together = SweepParams {
            copies: 2,
            ..Default::default()
        };
        assert!(together
            .placements(square(), &spine, 2)
            .iter()
            .all(|placement| placement.source.is_none()));
    }

    #[test]
    fn guide_polyline_takes_the_longest_subpath_and_closes_loops() {
        let mut path = Path::new(FillRule::NonZero);
        let mut short = SubPath::new(false);
        short.nodes = vec![Node::corner(Vec2::ZERO), Node::corner(Vec2::X)];
        path.subpaths.push(short);
        let mut loop_subpath = SubPath::new(true);
        for point in [
            Vec2::new(0.0, 0.0),
            Vec2::new(0.0, 40.0),
            Vec2::new(40.0, 40.0),
            Vec2::new(40.0, 0.0),
        ] {
            loop_subpath.nodes.push(Node::corner(point));
        }
        path.subpaths.push(loop_subpath);

        let spine = guide_spine(&path);
        assert!(spine.points.len() > 2, "the square wins over the unit line");
        assert!(spine.closed, "the loop stays flagged as closed");
        assert_eq!(
            spine.points.first(),
            spine.points.last(),
            "the loop is closed back onto its start"
        );
    }

    #[test]
    fn limits_warn_then_refuse() {
        assert_eq!(check_limits(4, 4), Ok(None));
        assert!(matches!(check_limits(SOFT_COPY_LIMIT + 1, 10), Ok(Some(_))));
        assert!(check_limits(HARD_COPY_LIMIT + 1, 10).is_err());
        assert!(matches!(check_limits(1, SOFT_SHAPE_LIMIT + 1), Ok(Some(_))));
        assert!(check_limits(1, HARD_SHAPE_LIMIT + 1).is_err());
    }
}
