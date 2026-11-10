//! Operations card for the Properties dock.
//!
//! The card only plans and renders; the document is edited after the UI closes,
//! so the selection borrow never overlaps the apply step. Applying a plan
//! duplicates the original selection once per placement and transforms the
//! copies, all under one undo checkpoint.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::operations::{
    check_limits, guide_spine, ArrayParams, MirrorAxis, MirrorParams, MirrorPivot, MirrorSource,
    OperationEntry, OperationKind, OperationPreviewKey, OperationPreviewSource, Placement,
    PyramidParams, RadialDistanceMode, RadialParams, RadialPivot, RadialScaleMode, RadialShapeMode,
    SweepParams, SweepShapeMode, SweepSpine,
};
use ns_core::path::{boolean_loops, split_loops, VectorOperand};

/// A validated plan plus the numbers shown above the Apply button.
pub(crate) struct OperationPlan {
    pub placements: Vec<Placement>,
    pub shape_count: usize,
    /// Paths this entry repeats. Normally the selection; a Sweep with filled
    /// source slots uses those layers instead.
    pub source_paths: Vec<Vec<usize>>,
    /// Mirror only: everything the clip/merge post-pass needs.
    pub mirror: Option<MirrorPlan>,
    /// A non-blocking precision hint shown under the entry.
    pub warning: Option<String>,
    /// The entry asked for the source layers to be hidden after the copies are
    /// made, so only the array remains visible.
    pub hide_original: bool,
}

/// The pivot and bounds frozen when a mirror entry was planned. Clip and merge
/// run after every entry has copied the untouched originals, so they must not
/// re-read bounds that an earlier entry may have changed.
#[derive(Clone, Debug)]
pub(crate) struct MirrorPlan {
    pub params: MirrorParams,
    pub bounds: Bounds,
    pub document_size: Vec2,
}

/// Draw the Operations card. Returns `true` when the user asked to apply it;
/// the caller runs [`apply_selected_operation`] after the whole dock is drawn.
pub(crate) fn operations_card(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
) -> bool {
    // Forget stacks whose layer was deleted or undone away.
    state
        .operation_stacks
        .retain(|id, _| doc.0.find_path(*id).is_some());

    let Some(path) = state.selected.clone() else {
        return false;
    };
    let (layer_id, valid) = match doc.0.layer_by_path(&path) {
        Some(layer) => (
            layer.id,
            matches!(layer.kind, LayerKind::Shape(_) | LayerKind::Group { .. })
                && count_shapes(layer) > 0,
        ),
        None => return false,
    };
    if !valid {
        return false;
    }
    // Active stacks on other layers keep contributing to the shared preview.
    let other_active = state
        .operation_stacks
        .iter()
        .filter(|(id, stack)| **id != layer_id && stack.iter().any(|entry| entry.enabled))
        .count();

    let selected = normalized_paths(selected_paths(state));
    let mut stack = state
        .operation_stacks
        .get(&layer_id)
        .cloned()
        .unwrap_or_default();
    let mut apply = false;
    let mut final_placements: Vec<Placement> = Vec::new();
    let mut final_shape_count = 0usize;
    let mut final_enabled = 0usize;
    let mut final_error = false;

    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        // Reference: the section pill reports the count, the button below does
        // the stacking, so the heading does not need a separate "Stack" tag.
        let active_operations = stack.iter().filter(|entry| entry.enabled).count();
        let active_pill = format!("{active_operations} active");
        section_title_with_pill(
            ui,
            Icon::Grid,
            "Operations",
            Some((&active_pill, active_operations > 0)),
        );
        ui.label(
            egui::RichText::new(
                "Operations stay inactive until enabled. Each active entry adds copies of the selection.",
            )
            .size(13.0)
            .color(MUTED),
        );
        ui.add_space(4.0);

        if stack.is_empty() {
            ui.label(
                egui::RichText::new("No operations on this layer.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }

        let mut remove: Option<usize> = None;
        for (index, entry) in stack.iter_mut().enumerate() {
            if index > 0 {
                ui.separator();
            }
            ui.push_id(("operation_entry", index), |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Checkbox::without_text(&mut entry.enabled))
                        .on_hover_text("Enable or disable this operation");
                    ui.label(
                        egui::RichText::new(entry.kind.label())
                            .font(bold(14.0))
                            .color(if entry.enabled { TEXT } else { MUTED }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button_sized(
                            ui,
                            Icon::Trash,
                            false,
                            true,
                            "Remove this operation",
                            egui::vec2(20.0, 20.0),
                        )
                        .clicked()
                        {
                            remove = Some(index);
                        }
                        ui.label(
                            egui::RichText::new(if entry.enabled { "Active" } else { "Off" })
                                .size(TEXT_CAPTION)
                                .color(if entry.enabled { ACCENT_TEXT } else { MUTED }),
                        );
                    });
                });

                if entry.enabled {
                    m3_checkbox(ui, &mut entry.hide_original, "Hide original (copies only)")
                        .on_hover_text(
                            "Hide the source layers while this operation is active. The copies keep \
                             their original size and placement; Apply leaves the source hidden too, \
                             so the original never conflicts with the array.",
                        );
                    match entry.kind {
                        OperationKind::Array => array_controls(ui, &mut entry.array),
                        OperationKind::Mirror => mirror_controls(ui, &mut entry.mirror),
                        OperationKind::Pyramid => pyramid_controls(ui, &mut entry.pyramid),
                        OperationKind::Radial => radial_controls(ui, &mut entry.radial),
                        OperationKind::Sweep => {
                            sweep_controls(ui, &mut entry.sweep, &doc.0, layer_id, state)
                        }
                    }
                    match plan_entry(entry, &doc.0, &selected) {
                        Ok(plan) => {
                            if let Some(warning) = &plan.warning {
                                ui.label(
                                    egui::RichText::new(warning.as_str())
                                        .size(TEXT_CAPTION)
                                        .color(WARNING),
                                );
                            }
                        }
                        Err(reason) => {
                            ui.label(
                                egui::RichText::new(reason)
                                    .size(TEXT_CAPTION)
                                    .color(WARNING),
                            );
                        }
                    }
                } else {
                    ui.label(
                        egui::RichText::new("Inactive — preview and Apply skip this entry.")
                            .size(TEXT_CAPTION)
                            .color(MUTED),
                    );
                }
            });
        }
        if let Some(index) = remove {
            stack.remove(index);
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Add operation")
                .size(TEXT_SMALL)
                .color(INSPECTOR_MUTED),
        );
        // Equal columns, as in the reference: the row reads as one control group,
        // and a kind already in the stack keeps the accent tint. The column floor
        // covers the widest short label ("+ Pyramid") including button padding;
        // a narrower dock falls back to two columns instead of clipping the text.
        let gap = 8.0;
        let available = ui.available_width();
        let preferred = if OperationKind::ALL.len() > 4 { 3 } else { 4 };
        let min_button = 68.0;
        let columns = if available
            >= preferred as f32 * min_button + (preferred as f32 - 1.0) * gap
        {
            preferred
        } else {
            2
        };
        let button_width =
            ((available - gap * (columns as f32 - 1.0)) / columns as f32).max(48.0);
        for row in OperationKind::ALL.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for kind in row {
                    ui.push_id(("add_operation", kind.id()), |ui| {
                        let active = stack.iter().any(|entry| entry.kind == *kind);
                        let label = format!("+ {}", kind.short_label());
                        let response = dashed_chip(ui, &label, active, button_width).on_hover_text(
                            format!(
                                "{}\n{}",
                                kind.description(),
                                kind.keywords().join(" · ")
                            ),
                        );
                        if response.clicked() {
                            let mut entry = OperationEntry::new(*kind);
                            if *kind == OperationKind::Sweep {
                                // A path drawn with the Pen tool starts as a normal
                                // shape. Adding Sweep is the moment it needs a
                                // spine, so convert it in the same undo step and
                                // let the artist drag shapes into the slots.
                                let convert = owner_is_plain_path(&doc.0, layer_id);
                                if convert {
                                    state.checkpoint_document(&doc.0);
                                    make_owner_a_guide(&mut doc.0, layer_id);
                                    state.status =
                                        "Path guide · drag shapes into the Sweep slots".into();
                                    state.mark_dirty();
                                }
                                entry.sweep.path = if convert {
                                    Some(layer_id)
                                } else {
                                    default_sweep_path(&doc.0, layer_id, &selected)
                                };
                                // After creating a sweep the path is the thing
                                // to refine, so open its on-model handles. The
                                // view stays where it is; switching to 3D shows
                                // the handles immediately.
                                if entry.sweep.path.is_some() {
                                    state.sweep_path_edit = true;
                                    state.preview_gizmo = PreviewGizmo::Off;
                                    state.auto_rotate = false;
                                }
                            }
                            stack.push(entry);
                        }
                    });
                }
            });
        }

        // Plan the final stack once so an entry added or removed this frame
        // cannot leave the preview key out of sync with the placements.
        for entry in stack.iter().filter(|entry| entry.enabled) {
            final_enabled += 1;
            match plan_entry(entry, &doc.0, &selected) {
                Ok(plan) => {
                    final_shape_count = plan.shape_count;
                    final_placements.extend(plan.placements);
                }
                Err(_) => final_error = true,
            }
        }

        ui.add_space(2.0);
        if final_enabled == 0 {
            ui.label(
                egui::RichText::new("No active operations. Toggle one on or add another.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        } else {
            let copies = final_placements.len();
            let total_shapes = final_shape_count.saturating_mul(copies);
            let limits = check_limits(copies, total_shapes);
            match &limits {
                Ok(Some(warning)) => {
                    ui.label(
                        egui::RichText::new(warning.as_str())
                            .size(TEXT_CAPTION)
                            .color(WARNING),
                    );
                }
                Err(reason) => {
                    ui.label(
                        egui::RichText::new(reason.as_str())
                            .size(TEXT_CAPTION)
                            .color(WARNING),
                    );
                }
                Ok(None) => {}
            }
            ui.label(
                egui::RichText::new(format!(
                    "{final_enabled} active · {copies} cop{} · {total_shapes} shape{} total",
                    if copies == 1 { "y" } else { "ies" },
                    if total_shapes == 1 { "" } else { "s" },
                ))
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
            let can_apply = !final_error && limits.is_ok() && copies > 0;
            ui.add_enabled_ui(can_apply, |ui| {
                if filled_button(ui, "Apply stack", 28.0).clicked() {
                    apply = true;
                }
            });
            if final_error {
                ui.label(
                    egui::RichText::new("Fix the highlighted operation before applying.")
                        .size(TEXT_CAPTION)
                        .color(WARNING),
                );
            }
        }
        ui.label(
            egui::RichText::new(
                "Live preview when at least one operation is enabled · Apply is one undo step",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        ui.label(
            egui::RichText::new(
                "Shift-drag a slider for fine control · Ctrl/Alt-drag for ultra-fine · double-click a number to type it",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        if other_active > 0 {
            ui.label(
                egui::RichText::new(format!(
                    "Preview also includes {other_active} other active layer{}.",
                    if other_active == 1 { "" } else { "s" }
                ))
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        }
    });

    // Persist the stack for this layer (empty stacks are dropped).
    if stack.is_empty() {
        state.operation_stacks.remove(&layer_id);
    } else {
        state.operation_stacks.insert(layer_id, stack.clone());
    }

    apply
}

/// Retire the transient preview document. The evaluator falls back to the real
/// document on its next tick; no undo entry is created.
pub(crate) fn clear_operation_preview(state: &mut EditorState) {
    let had_preview = state.operation_preview.take().is_some();
    state.operation_preview_key = None;
    if had_preview {
        state.mark_eval_dirty();
    }
}

/// Rebuild the live preview from every layer's enabled operation stack.
///
/// The preview used to belong to the selected layer only, so selecting another
/// layer cleared the copies from both the 2D canvas and the 3D material. Each
/// stack is planned against its own source now: the selected layer keeps the
/// full multi-selection, while every other layer previews itself. The preview
/// therefore survives selection changes and still reflects edits to any stack.
pub(crate) fn sync_operation_preview(state: &mut EditorState, doc: &DocumentRes) {
    state
        .operation_stacks
        .retain(|id, _| doc.0.find_path(*id).is_some());

    let selection = normalized_paths(selected_paths(state));
    let mut stacks: Vec<(LayerId, Vec<OperationEntry>)> = state
        .operation_stacks
        .iter()
        .map(|(id, stack)| (*id, stack.clone()))
        .collect();
    stacks.sort_by_key(|(id, _)| *id);

    let mut sources = Vec::new();
    let mut groups: Vec<PreviewGroup> = Vec::new();
    for (layer_id, stack) in stacks {
        let Some(path) = doc.0.find_path(layer_id) else {
            continue;
        };
        let primary = state.selected.as_deref() == Some(path.as_slice());
        let paths = if primary {
            selection.clone()
        } else {
            vec![path]
        };
        let mut plans = Vec::new();
        for entry in stack.iter().filter(|entry| entry.enabled) {
            if let Ok(plan) = plan_entry(entry, &doc.0, &paths) {
                plans.push(plan);
            }
        }
        if plans.is_empty() {
            continue;
        }
        sources.push(OperationPreviewSource {
            layer: layer_id,
            stack,
            paths: paths.clone(),
        });
        for plan in plans {
            groups.push(PreviewGroup { plan });
        }
    }

    let key = OperationPreviewKey {
        sources,
        edit_serial: state.edit_serial,
        undo_len: state.undo.len(),
        redo_len: state.redo.len(),
        layer_count: doc.0.layer_count(),
    };
    if state.operation_preview_key.as_ref() == Some(&key) {
        return;
    }
    if key.sources.is_empty() {
        clear_operation_preview(state);
        return;
    }
    let preview = build_preview_document(&doc.0, &groups);
    state.operation_preview = Some(preview.into());
    state.operation_preview_key = Some(key);
    state.mark_eval_dirty();
}

/// One planned entry in the preview. Keeping entries apart lets the mirror
/// post-pass know exactly which copies belong to a clip/merge plan.
struct PreviewGroup {
    plan: OperationPlan,
}

/// Clone the document and append every stack's planned copies as ordinary
/// layers with fresh ids. The original document, selection and undo stack are
/// untouched.
fn build_preview_document(doc: &Document, groups: &[PreviewGroup]) -> Document {
    let mut preview = doc.clone();
    let source_ids: Vec<Vec<LayerId>> = groups
        .iter()
        .map(|group| {
            group
                .plan
                .source_paths
                .iter()
                .filter_map(|path| doc.layer_by_path(path))
                .filter(|layer| !layer.is_path_guide())
                .map(|layer| layer.id)
                .collect()
        })
        .collect();
    let mut pairs: Vec<Vec<(LayerId, LayerId)>> = vec![Vec::new(); groups.len()];
    let mut mirror_order: Vec<usize> = Vec::new();
    // Flatten every entry's placements, then insert in reverse. Apply uses the
    // same order, so the preview stack matches the committed layer stack.
    let mut queued: Vec<(usize, Placement)> = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        for placement in &group.plan.placements {
            queued.push((index, *placement));
        }
    }
    for (group_index, placement) in queued.into_iter().rev() {
        if source_ids[group_index].is_empty() {
            continue;
        }
        if groups[group_index].plan.mirror.is_some() && !mirror_order.contains(&group_index) {
            mirror_order.push(group_index);
        }
        // A random-shape copy duplicates one chosen source; every other
        // placement copies the whole selection.
        let sources: Vec<LayerId> = match placement.source {
            Some(index) => vec![source_ids[group_index][index % source_ids[group_index].len()]],
            None => source_ids[group_index].clone(),
        };
        let mut copies: Vec<(Vec<usize>, LayerId, LayerId, Layer)> = Vec::new();
        for id in &sources {
            let Some(path) = preview.find_path(*id) else {
                continue;
            };
            let Some(original) = preview.layer_by_path(&path) else {
                continue;
            };
            let mut copy = original.duplicate();
            transform_layer(&mut copy, placement);
            copies.push((path, original.id, copy.id, copy));
        }
        if copies.is_empty() {
            continue;
        }
        // Copies that reference each other keep pointing inside the copied set.
        let mut id_map = HashMap::new();
        for (path, _, _, copy) in &copies {
            if let Some(original) = preview.layer_by_path(path) {
                collect_id_pairs(original, copy, &mut id_map);
            }
        }
        for (_, _, _, copy) in &mut copies {
            copy.remap_layer_references(&id_map);
        }
        // Insert from the end so earlier sibling indices stay valid.
        copies.sort_by(|a, b| b.0.cmp(&a.0));
        for (path, source_id, copy_id, copy) in copies {
            let parent = path[..path.len() - 1].to_vec();
            let index = path[path.len() - 1];
            preview.insert_at(&parent, index + 1, copy);
            if groups[group_index].plan.mirror.is_some() {
                pairs[group_index].push((source_id, copy_id));
            }
        }
    }
    // Run clip/merge only after every entry has copied the untouched originals,
    // so an earlier merge cannot starve a later entry of its sources. The job
    // order follows Apply's reversed placement order.
    let mut anchor_map: HashMap<LayerId, LayerId> = HashMap::new();
    for index in mirror_order {
        if let Some(mirror) = &groups[index].plan.mirror {
            let report = finish_mirror(&mut preview, &pairs[index], mirror);
            anchor_map.extend(report.anchors);
        }
    }
    // Hide source layers only after every copy was duplicated from a visible
    // original. The preview keeps the rows (hidden) so the layer list and the
    // canvas agree; disabling the entry restores them.
    let mut hidden: Vec<LayerId> = Vec::new();
    for group in groups {
        if !group.plan.hide_original {
            continue;
        }
        for path in &group.plan.source_paths {
            if let Some(layer) = doc.layer_by_path(path) {
                hidden.push(layer.id);
            }
        }
    }
    hidden.sort();
    hidden.dedup();
    for id in hidden {
        let id = anchor_map.get(&id).copied().unwrap_or(id);
        let Some(path) = preview.find_path(id) else {
            continue;
        };
        if let Some(layer) = preview.layer_by_path_mut(&path) {
            layer.visible = false;
            layer.touch();
        }
    }
    preview
}

fn collect_id_pairs(original: &Layer, copy: &Layer, map: &mut HashMap<LayerId, LayerId>) {
    map.insert(original.id, copy.id);
    if let (Some(original), Some(copy)) = (original.children(), copy.children()) {
        for (original, copy) in original.iter().zip(copy) {
            collect_id_pairs(original, copy, map);
        }
    }
}

/// Drop child paths whose parent is already selected, matching the duplicate
/// tool. The preview would otherwise add the child twice.
fn normalized_paths(mut paths: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
    paths.retain(|path| !path.is_empty());
    paths.sort();
    paths.dedup();
    let mut out: Vec<Vec<usize>> = Vec::new();
    for path in paths {
        if out
            .iter()
            .any(|kept| path.len() > kept.len() && path.starts_with(kept))
        {
            continue;
        }
        out.retain(|kept| !(kept.len() > path.len() && kept.starts_with(&path)));
        out.push(path);
    }
    out
}

/// Numbered names for applied copies: `Rect 001`, `Group 002`, ... Existing
/// names are reserved, so repeated operations keep counting up.
struct CopyNamer {
    existing: HashSet<String>,
    next: HashMap<&'static str, u32>,
}

impl CopyNamer {
    fn new(doc: &Document) -> Self {
        fn visit(layers: &[Layer], names: &mut HashSet<String>) {
            for layer in layers {
                names.insert(layer.name.clone());
                if let Some(children) = layer.children() {
                    visit(children, names);
                }
            }
        }
        let mut existing = HashSet::new();
        visit(&doc.layers, &mut existing);
        Self {
            existing,
            next: HashMap::new(),
        }
    }

    fn next_name(&mut self, prefix: &'static str) -> String {
        let next = self.next.entry(prefix).or_insert(1);
        loop {
            let candidate = format!("{prefix} {next:03}");
            *next += 1;
            if self.existing.insert(candidate.clone()) {
                return candidate;
            }
        }
    }
}

/// User-facing copy name stem from the layer's shape type or kind.
fn copy_prefix(layer: &Layer) -> &'static str {
    match &layer.kind {
        LayerKind::Shape(shape) => match &shape.source {
            ShapeSource::Primitive(Primitive::Rect { .. }) => "Rect",
            ShapeSource::Primitive(Primitive::Ellipse { .. }) => "Ellipse",
            ShapeSource::Primitive(Primitive::Polygon { .. }) => "Polygon",
            ShapeSource::Primitive(Primitive::Star { .. }) => "Star",
            ShapeSource::Path(_) => "Path",
            ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) => "Raster",
            ShapeSource::Text(_) => "Text",
        },
        LayerKind::Group { .. } => "Group",
        _ => "Layer",
    }
}

fn array_controls(ui: &mut egui::Ui, params: &mut ArrayParams) {
    // Keep this row self-contained: a long hint here would force the whole
    // resizable dock wider (egui adopts an overflowing row as the panel width).
    let compact = ui.available_width() < 220.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            egui::RichText::new(if compact { "Cols" } else { "Columns" })
                .size(TEXT_SMALL)
                .color(MUTED),
        );
        ui.add(
            egui::DragValue::new(&mut params.columns)
                .range(ArrayParams::MIN_COUNT..=ArrayParams::MAX_COUNT)
                .speed(0.2)
                .fixed_decimals(0),
        );
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Rows").size(TEXT_SMALL).color(MUTED));
        ui.add(
            egui::DragValue::new(&mut params.rows)
                .range(ArrayParams::MIN_COUNT..=ArrayParams::MAX_COUNT)
                .speed(0.2)
                .fixed_decimals(0),
        );
    });
    ui.label(
        egui::RichText::new(
            "The original is the first cell; Hide original removes it and leaves only the copies.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
    let limit = ArrayParams::SPACING_LIMIT;
    slider_row(
        ui,
        "Spacing X",
        &mut params.spacing.x,
        -limit..=limit,
        0.25,
        1,
    );
    slider_row(
        ui,
        "Spacing Y",
        &mut params.spacing.y,
        -limit..=limit,
        0.25,
        1,
    );
    slider_row(
        ui,
        "Row shift",
        &mut params.row_shift.x,
        -limit..=limit,
        0.25,
        1,
    );
    ui.label(
        egui::RichText::new(
            "Columns repeat along X, rows along Y. Row shift staggers each row; copies keep the original size.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
}

fn pyramid_controls(ui: &mut egui::Ui, params: &mut PyramidParams) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new("Copies").size(TEXT_SMALL).color(MUTED));
        ui.add(
            egui::DragValue::new(&mut params.copies)
                .range(PyramidParams::MIN_COPIES..=PyramidParams::MAX_COPIES)
                .speed(0.2)
                .fixed_decimals(0),
        );
    });
    ui.label(
        egui::RichText::new(
            "The original stays at the base; every copy steps and scales from it. Hide original \
             removes the base row.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
    let limit = PyramidParams::STEP_LIMIT;
    slider_row(ui, "Step X", &mut params.step.x, -limit..=limit, 0.25, 1);
    slider_row(ui, "Step Y", &mut params.step.y, -limit..=limit, 0.25, 1);
    slider_row(
        ui,
        "Scale step",
        &mut params.scale_step,
        PyramidParams::SCALE_MIN..=PyramidParams::SCALE_MAX,
        0.01,
        2,
    );
    ui.label(
        egui::RichText::new(
            "Negative Y steps up. Scale step compounds per copy; 0.85 shrinks each one to 85%.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
}

fn radial_controls(ui: &mut egui::Ui, params: &mut RadialParams) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new("Copies").size(TEXT_SMALL).color(MUTED));
        ui.add(
            egui::DragValue::new(&mut params.copies)
                .range(RadialParams::MIN_COPIES..=RadialParams::MAX_COPIES)
                .speed(0.2)
                .fixed_decimals(0),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(egui::RichText::new("Randomize").size(TEXT_SMALL))
                .on_hover_text("New random pattern (the seed below stays editable)")
                .clicked()
            {
                params.seed = params
                    .seed
                    .wrapping_mul(1_664_525)
                    .wrapping_add(1_013_904_223);
            }
        });
    });
    ui.label(
        egui::RichText::new(
            "The original stays at the pivot; every copy's centre lands on the ring. Hide original \
             removes the pivot copy.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
    slider_row(
        ui,
        "Radius (px)",
        &mut params.radius,
        0.0..=RadialParams::RADIUS_LIMIT,
        0.5,
        1,
    );
    slider_row(
        ui,
        "Radius step",
        &mut params.radius_step,
        -RadialParams::RADIUS_STEP_LIMIT..=RadialParams::RADIUS_STEP_LIMIT,
        0.5,
        1,
    );
    slider_row(
        ui,
        "Start angle",
        &mut params.start_angle,
        0.0..=360.0,
        0.5,
        1,
    );
    slider_row(ui, "Sweep", &mut params.sweep, 0.0..=360.0, 0.5, 1);
    labeled_combo(
        ui,
        "Pivot",
        "operation_radial_pivot",
        params.pivot.label(),
        |ui| {
            for pivot in RadialPivot::ALL {
                ui.selectable_value(&mut params.pivot, pivot, pivot.label());
            }
        },
    );
    ui.checkbox(&mut params.rotate, "Rotate copies")
        .on_hover_text("Turn each copy to follow its angle on the ring");

    ui.separator();
    labeled_combo(
        ui,
        "Scale",
        "operation_radial_scale",
        params.scale_mode.label(),
        |ui| {
            for mode in RadialScaleMode::ALL {
                ui.selectable_value(&mut params.scale_mode, mode, mode.label());
            }
        },
    );
    match params.scale_mode {
        RadialScaleMode::Equal => {
            slider_row(
                ui,
                "Size",
                &mut params.scale,
                RadialParams::SCALE_MIN..=RadialParams::SCALE_MAX,
                0.01,
                2,
            );
        }
        RadialScaleMode::Step => {
            slider_row(ui, "Scale step", &mut params.scale_step, 0.1..=2.0, 0.01, 2);
            ui.label(
                egui::RichText::new("Each copy compounds the previous copy's size.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }
        RadialScaleMode::Random => {
            slider_row(
                ui,
                "Min size",
                &mut params.scale_min,
                RadialParams::SCALE_MIN..=RadialParams::SCALE_MAX,
                0.01,
                2,
            );
            slider_row(
                ui,
                "Max size",
                &mut params.scale_max,
                RadialParams::SCALE_MIN..=RadialParams::SCALE_MAX,
                0.01,
                2,
            );
        }
    }

    labeled_combo(
        ui,
        "Distance",
        "operation_radial_distance",
        params.distance_mode.label(),
        |ui| {
            for mode in RadialDistanceMode::ALL {
                ui.selectable_value(&mut params.distance_mode, mode, mode.label());
            }
        },
    );
    if params.distance_mode == RadialDistanceMode::Random {
        slider_row(
            ui,
            "Distance jitter",
            &mut params.distance_jitter,
            0.0..=1.0,
            0.01,
            2,
        );
        ui.label(
            egui::RichText::new(
                "Each copy varies its distance from the pivot by up to this fraction of the radius.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
    }

    labeled_combo(
        ui,
        "Shapes",
        "operation_radial_shapes",
        params.shape_mode.label(),
        |ui| {
            for mode in RadialShapeMode::ALL {
                ui.selectable_value(&mut params.shape_mode, mode, mode.label());
            }
        },
    );
    if params.shape_mode == RadialShapeMode::Random {
        ui.label(
            egui::RichText::new(
                "Each step copies one randomly chosen shape from the selection, so a ring can mix \
                 rectangles, stars and other shapes.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new("Seed").size(TEXT_SMALL).color(MUTED));
        ui.add(
            egui::DragValue::new(&mut params.seed)
                .speed(0.2)
                .fixed_decimals(0),
        );
    });
    ui.label(
        egui::RichText::new("Same seed = same pattern; Preview and Apply always agree.")
            .size(TEXT_CAPTION)
            .color(MUTED),
    );
    ui.label(
        egui::RichText::new(
            "Sweep 360 closes the ring; smaller values make a fan. Radius step spirals out, \
             negative values spiral in.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
}

fn sweep_controls(
    ui: &mut egui::Ui,
    params: &mut SweepParams,
    doc: &Document,
    owner: LayerId,
    state: &mut EditorState,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new("Copies").size(TEXT_SMALL).color(MUTED));
        ui.add(
            egui::DragValue::new(&mut params.copies)
                .range(SweepParams::MIN_COPIES..=SweepParams::MAX_COPIES)
                .speed(0.2)
                .fixed_decimals(0),
        );
    });
    ui.label(
        egui::RichText::new(
            "The original stays where it is; every copy lands on the path guide's spine. Hide \
             original leaves only the swept copies.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );

    let mut guides = Vec::new();
    collect_path_guides(&doc.layers, &mut guides);
    let selected = params
        .path
        .and_then(|id| guides.iter().find(|(guide, _)| *guide == id))
        .map(|(_, name)| name.as_str());
    labeled_combo(
        ui,
        "Path",
        "operation_sweep_path",
        selected.unwrap_or("Choose a guide"),
        |ui| {
            for (id, name) in &guides {
                ui.selectable_value(&mut params.path, Some(*id), name);
            }
        },
    );
    if guides.is_empty() {
        ui.label(
            egui::RichText::new(
                "No path guides yet. Select a vector path, then set Use as to Path guide in its \
                 Properties.",
            )
            .size(TEXT_CAPTION)
            .color(WARNING),
        );
    }
    if params.path.is_some_and(|id| {
        doc.find_layer(id)
            .is_some_and(|layer| layer.is_path_guide())
    }) {
        ui.add_space(2.0);
        let editing = state.sweep_path_edit;
        if ui
            .button(if editing {
                "Editing path on the model"
            } else {
                "Edit path on the model"
            })
            .on_hover_text(
                "Drag the guide's anchors and Bezier handles on the 3D mesh. Every handle snaps \
                 to the surface and writes the same path the 2D canvas shows. The swept shapes \
                 keep their size and only follow the new spine.",
            )
            .clicked()
        {
            state.sweep_path_edit = !editing;
            if state.sweep_path_edit {
                state.preview_gizmo = PreviewGizmo::Off;
                state.auto_rotate = false;
                state.viewport_tab = ViewportTab::ThreeD;
                state.status =
                    "Sweep path · drag anchors/handles on the model · surface snap".into();
            } else {
                state.sweep_path_drag = None;
                state.sweep_path_hover = None;
                state.sweep_path_readout.clear();
            }
        }
        if editing && !state.sweep_path_readout.is_empty() {
            ui.label(
                egui::RichText::new(state.sweep_path_readout.as_str())
                    .font(mono(11.0))
                    .color(ACCENT_TEXT),
            );
        }
    }

    ui.add_space(3.0);
    sweep_source_slots(ui, params, doc, owner);
    labeled_combo(
        ui,
        "Pattern",
        "operation_sweep_pattern",
        params.shape_mode.label(),
        |ui| {
            for mode in SweepShapeMode::ALL {
                ui.selectable_value(&mut params.shape_mode, mode, mode.label())
                    .on_hover_text(mode.hint());
            }
        },
    );

    ui.checkbox(&mut params.rotate, "Rotate to tangent")
        .on_hover_text(
            "Turn each copy so its +X axis follows the path; the original keeps its orientation.",
        );
    slider_row(
        ui,
        "Angle",
        &mut params.angle_offset,
        -180.0..=180.0,
        0.5,
        1,
    );
    slider_row(ui, "Start", &mut params.start, 0.0..=1.0, 0.005, 3);
    slider_row(ui, "End", &mut params.end, 0.0..=1.0, 0.005, 3);
    ui.label(
        egui::RichText::new(
            "Start and End are fractions of the path's length. Rotate follows the path tangent; \
             Angle adds rotation in degrees.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
}

/// Source slots for a sweep: drag layers from the Layers panel into a box and
/// the sweep repeats those layers instead of the current selection. Allocate
/// more slots to sweep several shapes together.
fn sweep_source_slots(ui: &mut egui::Ui, params: &mut SweepParams, doc: &Document, owner: LayerId) {
    if params.sources.is_empty() {
        params.sources.push(None);
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Shapes").size(TEXT_SMALL).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut slots = params.sources.len().clamp(1, 16);
            let changed = ui
                .add(
                    egui::DragValue::new(&mut slots)
                        .range(1..=16)
                        .speed(0.1)
                        .fixed_decimals(0),
                )
                .on_hover_text("How many source shapes this sweep repeats")
                .changed();
            if changed {
                params.sources.resize(slots as usize, None);
            }
            ui.label(egui::RichText::new("Slots").size(TEXT_CAPTION).color(MUTED));
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        let count = params.sources.len();
        for index in 0..count {
            // Drop references to deleted layers or layers that became guides;
            // a stale id would otherwise keep a slot occupied invisibly.
            if params.sources[index].is_some_and(|id| {
                !doc.find_layer(id)
                    .is_some_and(|layer| !layer.is_path_guide())
            }) {
                params.sources[index] = None;
            }
            let filled = params.sources[index]
                .and_then(|id| doc.find_layer(id))
                .map(|layer| (layer.id, layer.name.clone()));
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(58.0, 30.0), egui::Sense::click());
            let base = state_layer(CONTAINER, SURFACE_LOWEST, 0.45);
            let fill = if response.hovered() {
                hover_layer(base)
            } else {
                base
            };
            let stroke = if filled.is_some() { ACCENT } else { CONTAINER };
            ui.painter().rect_filled(rect, 4, fill);
            ui.painter().rect_stroke(
                rect,
                4,
                egui::Stroke::new(1.0_f32, stroke),
                egui::StrokeKind::Inside,
            );
            let (label, color) = match &filled {
                Some((_, name)) => (name.as_str(), TEXT),
                None => ("Drop", MUTED),
            };
            ui.painter().with_clip_rect(rect.shrink(3.0)).text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(10.0),
                color,
            );
            if response.clicked() && filled.is_some() {
                params.sources[index] = None;
            }
            if let Some(payload) = response.dnd_release_payload::<LayerDragPayload>() {
                let valid = payload.id != owner
                    && doc
                        .find_layer(payload.id)
                        .is_some_and(|layer| !layer.is_path_guide())
                    && !params
                        .sources
                        .iter()
                        .enumerate()
                        .any(|(other, slot)| other != index && *slot == Some(payload.id));
                if valid {
                    params.sources[index] = Some(payload.id);
                }
            }
            let hint = match &filled {
                Some(_) => "Click to clear · drag another layer here to replace it",
                None => {
                    "Drag a vector shape or group from the Layers panel here. Empty slots \
                     repeat the selection."
                }
            };
            response.on_hover_text(hint);
        }
    });
    ui.label(
        egui::RichText::new(
            "Filled slots are the sweep source; empty slots fall back to the selection.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
}

/// Every path guide in the document, in layer order. Hidden guides stay in the
/// list: hiding one only stops its overlay, not its use as a sweep spine.
fn collect_path_guides(layers: &[Layer], out: &mut Vec<(LayerId, String)>) {
    for layer in layers {
        if layer.is_path_guide() {
            out.push((layer.id, layer.name.clone()));
        }
        if let Some(children) = layer.children() {
            collect_path_guides(children, out);
        }
    }
}

/// Path a new Sweep entry should follow: the layer it lives on when that is a
/// guide, otherwise the one guide in the selection, otherwise the one guide in
/// the document. With several candidates the artist picks in the entry.
fn default_sweep_path(doc: &Document, owner: LayerId, paths: &[Vec<usize>]) -> Option<LayerId> {
    if doc.find_layer(owner).is_some_and(Layer::is_path_guide) {
        return Some(owner);
    }
    let selected: Vec<LayerId> = paths
        .iter()
        .filter_map(|path| doc.layer_by_path(path))
        .filter(|layer| layer.is_path_guide())
        .map(|layer| layer.id)
        .collect();
    if selected.len() == 1 {
        return Some(selected[0]);
    }
    let mut all = Vec::new();
    collect_path_guides(&doc.layers, &mut all);
    if all.len() == 1 {
        Some(all[0].0)
    } else {
        None
    }
}

/// True when the layer can become a path guide by flipping `Use as`.
fn owner_is_plain_path(doc: &Document, id: LayerId) -> bool {
    doc.find_layer(id).is_some_and(|layer| {
        matches!(&layer.kind, LayerKind::Shape(shape)
            if shape.vector_role == VectorRole::Surface
                && matches!(shape.source, ShapeSource::Path(_)))
    })
}

/// Switch an editable path from painting geometry to a sweep guide.
fn make_owner_a_guide(doc: &mut Document, id: LayerId) -> bool {
    let Some(layer) = doc.find_layer_mut(id) else {
        return false;
    };
    let LayerKind::Shape(shape) = &mut layer.kind else {
        return false;
    };
    if !matches!(shape.source, ShapeSource::Path(_)) || shape.vector_role == VectorRole::Path {
        return false;
    }
    shape.vector_role = VectorRole::Path;
    layer.touch();
    true
}

fn mirror_controls(ui: &mut egui::Ui, params: &mut MirrorParams) {
    labeled_combo(
        ui,
        "Axis",
        "operation_mirror_axis",
        params.axis.label(),
        |ui| {
            for axis in MirrorAxis::ALL {
                ui.selectable_value(&mut params.axis, axis, axis.label());
            }
        },
    );
    labeled_combo(
        ui,
        "Pivot",
        "operation_mirror_pivot",
        params.pivot.label(),
        |ui| {
            for pivot in MirrorPivot::ALL {
                ui.selectable_value(&mut params.pivot, pivot, pivot.label());
            }
        },
    );
    labeled_combo(
        ui,
        "Source",
        "operation_mirror_source",
        params.source.label(params.axis),
        |ui| {
            for source in MirrorSource::ALL {
                ui.selectable_value(&mut params.source, source, source.label(params.axis))
                    .on_hover_text(source.hint(params.axis));
            }
        },
    );
    // A chosen source side replaces the opposite half, so the original must be
    // bisected even when the Clip checkbox is off.
    if params.source.is_source_side() {
        params.clip = true;
    }
    slider_row(
        ui,
        "Gap (px)",
        &mut params.gap,
        0.0..=MirrorParams::GAP_LIMIT,
        0.5,
        0,
    );
    ui.horizontal(|ui| {
        let clip_response = ui.add_enabled(
            !params.source.is_source_side(),
            egui::Checkbox::new(&mut params.clip, "Clip at plane"),
        );
        if params.source.is_source_side() {
            clip_response.on_disabled_hover_text(
                "Always on when a source side is selected: the destination half is removed.",
            );
        } else {
            clip_response.on_hover_text(
                "Bisect the original and the reflection at the pivot, so each side keeps only \
                 its own half and the two cannot overlap.",
            );
        };
        ui.checkbox(&mut params.merge, "Merge seam").on_hover_text(
            "Weld touching halves into one shape instead of leaving a doubled seam.",
        );
    });
    ui.add_enabled_ui(params.merge, |ui| {
        slider_row(
            ui,
            "Merge (px)",
            &mut params.merge_threshold,
            0.0..=MirrorParams::MERGE_LIMIT,
            0.1,
            1,
        );
    });
    ui.label(
        egui::RichText::new(
            "Both sides keeps the original and adds a reflection. A single source side is \
             clipped at the pivot and mirrored over the other side, replacing it; use a pivot \
             that crosses the selection. Gap moves the copy away from the pivot; Merge welds \
             their seam.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
}

/// Validate one stack entry against a selection without touching the document.
fn plan_entry(
    entry: &OperationEntry,
    doc: &Document,
    paths: &[Vec<usize>],
) -> std::result::Result<OperationPlan, String> {
    let source_paths = entry_source_paths(entry, doc, paths);
    if source_paths.is_empty() {
        return Err(match entry.kind {
            OperationKind::Sweep => {
                "Drop a shape into a Sweep slot, or select the shapes to repeat.".into()
            }
            _ => "Select a shape or group first.".into(),
        });
    }

    let mut bounds: Option<Bounds> = None;
    let mut shape_count = 0usize;
    let mut untransformable = false;
    let mut found = false;
    let mut sources = 0usize;
    for path in &source_paths {
        let Some(layer) = doc.layer_by_path(path) else {
            continue;
        };
        // A path guide steers a sweep; it is never one of the shapes that get
        // repeated, so it must not widen the bounds or the source count.
        if layer.is_path_guide() {
            continue;
        }
        found = true;
        sources += 1;
        shape_count += count_shapes(layer);
        untransformable |= has_untransformable(layer);
        if let Some(layer_bounds) = layer_bounds(layer) {
            bounds = Some(bounds.map_or(layer_bounds, |current: Bounds| Bounds {
                min: current.min.min(layer_bounds.min),
                max: current.max.max(layer_bounds.max),
            }));
        }
    }
    if !found {
        return Err(
            "Select the shapes to repeat. Path guides steer operations and are not copied.".into(),
        );
    }
    if untransformable {
        return Err(
            "Raster, photo and painted layers cannot be transformed. Select vector shapes or \
             convert the layer."
                .into(),
        );
    }
    if shape_count == 0 {
        return Err("The selection has no vector shape to repeat.".into());
    }
    let Some(bounds) = bounds else {
        return Err("The selection has no outline to transform.".into());
    };

    let document_size = Vec2::new(doc.size.x as f32, doc.size.y as f32);
    let sweep_path = if entry.kind == OperationKind::Sweep {
        let Some(guide_id) = entry.sweep.path else {
            return Err(
                "Sweep needs a path guide. Set a vector path's Use as to Path guide, then pick \
                 it here."
                    .into(),
            );
        };
        let Some(guide) = doc.find_layer(guide_id) else {
            return Err("This sweep's path guide was deleted. Pick another path.".into());
        };
        let points = match &guide.kind {
            LayerKind::Shape(shape) if guide.is_path_guide() => match &shape.source {
                ShapeSource::Path(path) => guide_spine(path),
                _ => SweepSpine::default(),
            },
            _ => SweepSpine::default(),
        };
        if points.points.len() < 2 {
            return Err(
                "The path guide has no usable spine. Give it two distinct nodes and set it to \
                 Path guide."
                    .into(),
            );
        }
        Some(points)
    } else {
        None
    };
    let placements = entry.placements(bounds, document_size, sources, sweep_path.as_ref());
    if placements.is_empty() {
        return Err(match entry.kind {
            OperationKind::Sweep => {
                "The path guide has no length. Give it two distinct points.".into()
            }
            _ => "This operation produces no copies. Increase its count.".into(),
        });
    }
    if placements.iter().all(|placement| placement.is_identity()) {
        return Err(match entry.kind {
            OperationKind::Array => {
                "Copies would sit on the original. Set spacing or a row shift.".into()
            }
            OperationKind::Pyramid => {
                "Copies would sit on the original. Set a step or a scale step below 1.00.".into()
            }
            OperationKind::Mirror => "The reflection would sit on the original.".into(),
            OperationKind::Radial => {
                "Copies would sit on the original. Set a radius, a scale or rotate the copies."
                    .into()
            }
            OperationKind::Sweep => {
                "Copies would sit on the original. Move the guide or change the sweep range.".into()
            }
        });
    }

    let total_shapes = shape_count.saturating_mul(placements.len());
    check_limits(placements.len(), total_shapes).map(|_| ())?;
    let mirror = (entry.kind == OperationKind::Mirror).then(|| MirrorPlan {
        params: entry.mirror,
        bounds,
        document_size,
    });
    if let Some(plan) = mirror.as_ref() {
        let pivot = plan.params.pivot_point(plan.bounds, plan.document_size);
        if plan.params.source.is_source_side()
            && !plan.params.source_side_has_geometry(pivot, plan.bounds)
        {
            return Err(format!(
                "The {} has no geometry. Choose the other source side or Both sides.",
                plan.params.source.source_name(plan.params.axis)
            ));
        }
    }
    let warning = mirror.as_ref().and_then(|plan| {
        let pivot = plan.params.pivot_point(plan.bounds, plan.document_size);
        if !plan.params.clips_at_plane() && plan.params.cuts(pivot, plan.bounds) {
            Some(
                "Without Clip the reflection crosses the pivot and overlaps the original. \
                 Enable Clip at plane or Merge seam."
                    .to_string(),
            )
        } else if plan.params.clips_at_plane()
            && plan.params.merge
            && plan.params.gap > plan.params.merge_threshold
        {
            Some(format!(
                "Gap {:.1} px is wider than the merge threshold; the halves stay separate.",
                plan.params.gap
            ))
        } else {
            None
        }
    });
    Ok(OperationPlan {
        placements,
        shape_count,
        source_paths,
        mirror,
        warning,
        hide_original: entry.hide_original,
    })
}

/// Paths an entry repeats: a Sweep's filled source slots when they have any
/// layers, otherwise the selection. Guides are always skipped.
fn entry_source_paths(
    entry: &OperationEntry,
    doc: &Document,
    paths: &[Vec<usize>],
) -> Vec<Vec<usize>> {
    if entry.kind == OperationKind::Sweep {
        let slots: Vec<Vec<usize>> = entry
            .sweep
            .sources
            .iter()
            .flatten()
            .filter_map(|id| doc.find_path(*id))
            .filter(|path| {
                doc.layer_by_path(path)
                    .is_some_and(|layer| !layer.is_path_guide())
            })
            .collect();
        if !slots.is_empty() {
            return slots;
        }
    }
    paths
        .iter()
        .filter_map(|path| {
            doc.layer_by_path(path)
                .filter(|layer| !layer.is_path_guide())
                .map(|_| path.clone())
        })
        .collect()
}

/// Apply every enabled operation in the selected layer's stack once. All
/// copies share one undo snapshot; applied entries are switched off afterwards.
pub(crate) fn apply_selected_operation(
    state: &mut EditorState,
    doc: &mut DocumentRes,
) -> std::result::Result<usize, String> {
    let Some(layer_id) = state
        .selected
        .as_ref()
        .and_then(|path| doc.0.layer_by_path(path))
        .map(|layer| layer.id)
    else {
        return Err("Select a layer first.".into());
    };
    let stack = state
        .operation_stacks
        .get(&layer_id)
        .cloned()
        .unwrap_or_default();
    let enabled: Vec<OperationEntry> = stack.into_iter().filter(|entry| entry.enabled).collect();
    if enabled.is_empty() {
        return Err("Enable an operation before applying.".into());
    }

    let selection_paths = normalized_paths(selected_paths(state));
    let mut plans: Vec<(OperationEntry, OperationPlan)> = Vec::new();
    let mut total_placements = 0usize;
    let mut shape_count = 0usize;
    for entry in &enabled {
        let plan = plan_entry(entry, &doc.0, &selection_paths)?;
        shape_count = plan.shape_count;
        total_placements += plan.placements.len();
        plans.push((entry.clone(), plan));
    }
    if total_placements == 0 {
        return Err("The enabled operations produced no copies.".into());
    }
    check_limits(
        total_placements,
        shape_count.saturating_mul(total_placements),
    )
    .map(|_| ())?;

    let source_ids: Vec<Vec<LayerId>> = plans
        .iter()
        .map(|(_, plan)| {
            plan.source_paths
                .iter()
                .filter_map(|path| doc.0.layer_by_path(path))
                .filter(|layer| !layer.is_path_guide())
                .map(|layer| layer.id)
                .collect()
        })
        .collect();
    if source_ids.iter().all(Vec::is_empty) {
        return Err("Select a layer first.".into());
    }

    // The real copies replace the preview in this same frame.
    clear_operation_preview(state);
    state.checkpoint_document(&doc.0);
    let mut created: Vec<Vec<(LayerId, &'static str)>> = vec![Vec::new(); total_placements];
    let mut mirror_jobs: Vec<(usize, Vec<(LayerId, LayerId)>, MirrorPlan)> = Vec::new();
    let mut skipped = 0usize;
    // Flatten the stack into placement order, then insert in reverse so the
    // final layer order still reads top to bottom. Keeping every entry's own
    // mirror plan lets clip/merge run per entry later.
    let mut queued: Vec<(usize, usize, Placement)> = Vec::new();
    let mut base = 0usize;
    for (plan_index, (_, plan)) in plans.iter().enumerate() {
        for (index, placement) in plan.placements.iter().enumerate() {
            queued.push((base + index, plan_index, *placement));
        }
        base += plan.placements.len();
    }
    for (slot, plan_index, placement) in queued.into_iter().rev() {
        let (_, plan) = &plans[plan_index];
        let ids = &source_ids[plan_index];
        let mut sources: Vec<Vec<usize>> = match placement.source {
            Some(source) if !ids.is_empty() => {
                let id = ids[source % ids.len()];
                doc.0
                    .find_path(id)
                    .map(|path| vec![path])
                    .unwrap_or_default()
            }
            _ => ids.iter().filter_map(|id| doc.0.find_path(*id)).collect(),
        };
        sources.retain(|path| doc.0.layer_by_path(path).is_some());
        if sources.is_empty() {
            continue;
        }
        // `duplicate_layers` sorts the paths, so sort here too: the returned
        // copies can then be paired with their sources by position.
        sources.sort();
        let copies = duplicate_layers(state, doc, &sources);
        if copies.is_empty() {
            continue;
        }
        for copy_path in &copies {
            // Capture the name stem before the transform: a mirrored star becomes
            // a path but should still be named `Star 001`.
            let prefix = doc
                .0
                .layer_by_path(copy_path)
                .map(copy_prefix)
                .unwrap_or("Layer");
            if let Some(copy) = doc.0.layer_by_path_mut(copy_path) {
                created[slot].push((copy.id, prefix));
            }
            let (_, failed) = transform_path(doc, copy_path, placement);
            skipped += failed;
        }
        if let Some(mirror) = &plan.mirror {
            let source_order: Vec<LayerId> = sources
                .iter()
                .filter_map(|path| doc.0.layer_by_path(path).map(|layer| layer.id))
                .collect();
            let copy_order: Vec<LayerId> = copies
                .iter()
                .filter_map(|path| doc.0.layer_by_path(path).map(|layer| layer.id))
                .collect();
            if source_order.len() == copy_order.len() {
                mirror_jobs.push((
                    slot,
                    source_order.into_iter().zip(copy_order).collect(),
                    mirror.clone(),
                ));
            }
        }
    }

    // Name the copies in placement order after every insertion, so the numbering
    // still reads top to bottom even though the inserts ran backwards.
    let mut namer = CopyNamer::new(&doc.0);
    for (id, prefix) in created.iter().flatten() {
        let name = namer.next_name(prefix);
        let Some(path) = doc.0.find_path(*id) else {
            continue;
        };
        if let Some(copy) = doc.0.layer_by_path_mut(&path) {
            copy.name = name;
        }
    }

    // Clip and merge only after every entry copied the untouched originals, so
    // an earlier merge cannot starve a later entry of its sources.
    let mut clipped = 0usize;
    let mut merged = 0usize;
    let mut merge_skipped = 0usize;
    let mut stack_owner = layer_id;
    let mut anchor_map: HashMap<LayerId, LayerId> = HashMap::new();
    for (slot, pairs, mirror) in mirror_jobs {
        let report = finish_mirror(&mut doc.0, &pairs, &mirror);
        clipped += report.clipped;
        merged += report.merged;
        merge_skipped += report.skipped_merge;
        if let Some(anchor) = report.anchors.get(&layer_id).copied() {
            stack_owner = anchor;
        }
        anchor_map.extend(report.anchors);
        created[slot] = report
            .result_ids
            .into_iter()
            .map(|id| (id, "Layer"))
            .collect();
    }

    // Hide the originals after every copy exists. Hiding earlier would make the
    // duplicates inherit `visible = false`; hiding now leaves one undo step and
    // only affects the source rows, not the array.
    let mut hidden: Vec<LayerId> = Vec::new();
    for (entry, plan) in &plans {
        if !entry.hide_original {
            continue;
        }
        for path in &plan.source_paths {
            if let Some(layer) = doc.0.layer_by_path(path) {
                hidden.push(layer.id);
            }
        }
    }
    hidden.sort();
    hidden.dedup();
    let mut hidden_count = 0usize;
    for id in hidden {
        let id = anchor_map.get(&id).copied().unwrap_or(id);
        if let Some(layer) = doc.0.find_layer_mut(id) {
            if layer.visible {
                layer.visible = false;
                layer.touch();
                hidden_count += 1;
            }
        }
    }

    let mut created_ids: Vec<LayerId> = created.into_iter().flatten().map(|(id, _)| id).collect();
    let mut seen = HashSet::new();
    created_ids.retain(|id| seen.insert(*id));

    if created_ids.is_empty() {
        return Err("Nothing was copied; the selection may be empty.".into());
    }

    // Turn the applied entries off; their settings stay for a later re-run.
    // A clipped source can change id, so move the stack to the surviving piece.
    if let Some(mut stack) = state.operation_stacks.remove(&layer_id) {
        for entry in stack.iter_mut() {
            entry.enabled = false;
        }
        if doc.0.find_layer(stack_owner).is_some() {
            state.operation_stacks.insert(stack_owner, stack);
        }
    }

    let created_paths: Vec<Vec<usize>> = created_ids
        .iter()
        .filter_map(|id| doc.0.find_path(*id))
        .collect();
    state.selected = created_paths.first().cloned();
    state.multi_selected = created_paths.into_iter().skip(1).collect();
    state.mark_dirty();
    let copies = created_ids.len();
    let operation_count = enabled.len();
    let mut status = format!(
        "{operation_count} operation{} applied · {copies} layer{}",
        if operation_count == 1 { "" } else { "s" },
        if copies == 1 { "" } else { "s" },
    );
    if clipped > 0 {
        status.push_str(&format!(" · {clipped} clipped"));
    }
    if merged > 0 {
        status.push_str(&format!(" · {merged} merged"));
    }
    if merge_skipped > 0 {
        status.push_str(&format!(" · {merge_skipped} seam(s) kept apart"));
    }
    if skipped > 0 {
        status.push_str(&format!(" · {skipped} shape(s) skipped"));
    }
    if hidden_count > 0 {
        status.push_str(&format!(" · {hidden_count} original hidden"));
    }
    state.status = status;
    Ok(copies)
}

fn selected_paths(state: &EditorState) -> Vec<Vec<usize>> {
    let mut paths = Vec::new();
    if let Some(path) = &state.selected {
        paths.push(path.clone());
    }
    paths.extend(state.multi_selected.iter().cloned());
    paths
}

fn count_shapes(layer: &Layer) -> usize {
    match &layer.kind {
        LayerKind::Shape(_) => 1,
        LayerKind::Group { children } => children.iter().map(count_shapes).sum(),
        _ => 0,
    }
}

fn has_untransformable(layer: &Layer) -> bool {
    match &layer.kind {
        // A full-document raster has no origin to transform. Baked regions do.
        LayerKind::Shape(shape) => matches!(shape.source, ShapeSource::Raster(_)),
        // These store document-space grids and have no affine representation.
        // Copying them unchanged would stack a second copy on the original.
        LayerKind::PhotoHeight(_)
        | LayerKind::HeightPaint { .. }
        | LayerKind::NormalPaint { .. }
        | LayerKind::ImportedNormal { .. }
        | LayerKind::Fill { .. } => true,
        LayerKind::Group { children } => children.iter().any(has_untransformable),
        _ => false,
    }
}

/// What the mirror post-pass did to one entry's copies.
#[derive(Default)]
struct MirrorReport {
    /// Final layers created by the reflection, in stack order.
    result_ids: Vec<LayerId>,
    /// Original source id -> the layer that now holds its clipped/merged
    /// geometry. A clipped shape can change id, so Apply moves the operation
    /// stack to the surviving piece instead of silently dropping its settings.
    anchors: HashMap<LayerId, LayerId>,
    /// Pairs that were bisected at the pivot.
    clipped: usize,
    /// Pairs whose halves were welded into one shape.
    merged: usize,
    /// Pairs where Merge was requested but the seams did not meet.
    skipped_merge: usize,
}

/// Clip and merge a mirror entry after its copies exist.
///
/// Clip bisects each shape at the pivot: the source keeps the half its centre
/// occupies and the reflection keeps the opposite half, so no two pieces can
/// overlap across the plane. Merge then unions the two sides when their bounds
/// touch within the threshold, which welds the seam into one continuous outline.
fn finish_mirror(
    doc: &mut Document,
    pairs: &[(LayerId, LayerId)],
    plan: &MirrorPlan,
) -> MirrorReport {
    let mut report = MirrorReport::default();
    // Choosing an authoritative source side always requires the bisect step,
    // even when the Clip checkbox was left off.
    if !plan.params.clips_at_plane() && !plan.params.merge {
        report.result_ids = pairs
            .iter()
            .map(|(_, copy)| *copy)
            .filter(|id| doc.find_layer(*id).is_some())
            .collect();
        return report;
    }

    let pivot = plan.params.pivot_point(plan.bounds, plan.document_size);
    let extent = mirror_extent(doc, pairs);
    for (source_id, copy_id) in pairs {
        let Some(source) = doc.find_layer(*source_id) else {
            continue;
        };
        let Some(bounds) = layer_bounds(source) else {
            continue;
        };
        let source_side = plan.params.source_side(pivot, bounds.center());
        let copy_side = plan.params.reflected_side(source_side);

        let mut source_roots = vec![*source_id];
        let mut copy_roots = vec![*copy_id];
        let mut copy_clipped = false;
        if plan.params.clips_at_plane() && plan.params.cuts(pivot, bounds) {
            let (source_out, source_changed) =
                clip_tree(doc, *source_id, plan.params, pivot, source_side, extent);
            let (copy_out, copy_changed) =
                clip_tree(doc, *copy_id, plan.params, pivot, copy_side, extent);
            source_roots = source_out;
            copy_roots = copy_out;
            copy_clipped = copy_changed;
            if source_changed || copy_changed {
                report.clipped += 1;
            }
        }

        let mut result = if copy_clipped {
            collect_shape_ids(doc, &copy_roots)
        } else {
            copy_roots.clone()
        };

        // Remember where the source side ended up, even when the reflection is
        // kept as a separate layer.
        let source_anchor = if doc.find_layer(*source_id).is_some() {
            Some(*source_id)
        } else {
            source_roots
                .iter()
                .copied()
                .find(|id| doc.find_layer(*id).is_some())
        };
        if let Some(anchor) = source_anchor {
            report.anchors.insert(*source_id, anchor);
        }

        if plan.params.merge {
            let source_shapes = collect_shape_ids(doc, &source_roots);
            let copy_shapes = collect_shape_ids(doc, &copy_roots);
            let source_bounds = bounds_of_ids(doc, &source_shapes);
            let copy_bounds = bounds_of_ids(doc, &copy_shapes);
            let touches = match (source_bounds, copy_bounds) {
                (Some(a), Some(b)) => plan.params.seams_touch(a, b, plan.params.merge_threshold),
                _ => false,
            };
            let merged = if touches && !source_shapes.is_empty() && !copy_shapes.is_empty() {
                merge_shapes(doc, *source_id, &source_roots, *copy_id, &copy_roots)
            } else {
                None
            };
            match merged {
                Some(anchor) => {
                    report.merged += 1;
                    report.anchors.insert(*source_id, anchor);
                    result = vec![anchor];
                }
                None => report.skipped_merge += 1,
            }
        }

        report.result_ids.extend(
            result
                .into_iter()
                .filter(|id| doc.find_layer(*id).is_some()),
        );
    }
    let mut seen = HashSet::new();
    report.result_ids.retain(|id| seen.insert(*id));
    report
}

/// Clip every shape below `root_id` to the kept half-space and return the ids
/// of the surviving pieces. Groups keep their structure; a shape that is cut
/// into islands becomes several shape children, exactly like a lasso trim.
fn clip_tree(
    doc: &mut Document,
    root_id: LayerId,
    params: MirrorParams,
    pivot: Vec2,
    side: Vec2,
    extent: Bounds,
) -> (Vec<LayerId>, bool) {
    let shapes = collect_shape_ids(doc, &[root_id]);
    if shapes.is_empty() {
        return (vec![root_id], false);
    }
    // Trim subtracts the loops, so cut away the half-plane opposite the side
    // this piece keeps.
    let loops = params.clip_loops(pivot, -side, extent);
    let mut out = Vec::new();
    let mut changed = false;
    for id in shapes {
        let Some(path) = doc.find_path(id) else {
            continue;
        };
        match doc.trim_layers_with_loops(&[path], &loops) {
            Some(outcome) => {
                changed = true;
                for piece_path in outcome.paths {
                    if let Some(piece) = doc.layer_by_path(&piece_path) {
                        out.push(piece.id);
                    }
                }
            }
            None => out.push(id),
        }
    }
    (out, changed)
}

/// Union every shape in both halves and replace the source with the result.
/// Returns the surviving layer id, or `None` when a layer has no vector outline
/// (a raster, text or imported-normal child) and merging must be skipped.
fn merge_shapes(
    doc: &mut Document,
    source_root: LayerId,
    source_roots: &[LayerId],
    copy_root: LayerId,
    copy_roots: &[LayerId],
) -> Option<LayerId> {
    let mut shape_ids = collect_shape_ids(doc, source_roots);
    shape_ids.extend(collect_shape_ids(doc, copy_roots));
    shape_ids.sort_unstable();
    shape_ids.dedup();
    if shape_ids.len() < 2 {
        return None;
    }

    let mut operands = Vec::with_capacity(shape_ids.len());
    for id in &shape_ids {
        let layer = doc.find_layer(*id)?;
        let LayerKind::Shape(shape) = &layer.kind else {
            return None;
        };
        let path = shape.source.to_path()?;
        operands.push(VectorOperand::from_path(&path));
    }
    let ops = vec![PathBoolean::Add; operands.len() - 1];
    let loops = boolean_loops(&operands, &ops);
    if loops.is_empty() {
        return None;
    }
    // Only a connected union is a real weld. Two islands that merely sit inside
    // the threshold would silently collapse unrelated layers into one, so keep
    // those as separate layers instead.
    if split_loops(&loops).len() != 1 {
        return None;
    }

    // Prefer the source itself so its id, name, mask and stack position survive.
    let anchor = if doc.find_layer(source_root).is_some() {
        source_root
    } else {
        *collect_shape_ids(doc, source_roots).first()?
    };
    let style = {
        let layer = doc.find_layer(anchor)?;
        first_shape_layer(layer)?.clone()
    };
    let fill_rule = if loops.len() == 1 {
        FillRule::NonZero
    } else {
        FillRule::EvenOdd
    };
    let mut shape = style;
    // The union is filled geometry; keeping a stroke style would band the new
    // outer outline again.
    shape.style = ShapeStyle::Fill;
    shape.source = ShapeSource::Path(Path::from_loops(loops, fill_rule));
    let path = doc.find_path(anchor)?;
    {
        let layer = doc.layer_by_path_mut(&path)?;
        layer.kind = LayerKind::Shape(shape);
        layer.vector_op = None;
        layer.touch();
    }

    let mut remove = vec![copy_root];
    for id in source_roots
        .iter()
        .chain(copy_roots)
        .chain(shape_ids.iter())
    {
        if *id != anchor {
            remove.push(*id);
        }
    }
    remove.sort_unstable();
    remove.dedup();
    for id in remove {
        if let Some(path) = doc.find_path(id) {
            doc.remove_at(&path);
        }
    }
    Some(anchor)
}

fn first_shape_layer(layer: &Layer) -> Option<&ShapeLayer> {
    match &layer.kind {
        LayerKind::Shape(shape) => Some(shape),
        LayerKind::Group { children } => children.iter().find_map(first_shape_layer),
        _ => None,
    }
}

fn collect_shape_ids(doc: &Document, roots: &[LayerId]) -> Vec<LayerId> {
    fn visit(doc: &Document, id: LayerId, out: &mut Vec<LayerId>) {
        let Some(layer) = doc.find_layer(id) else {
            return;
        };
        match &layer.kind {
            LayerKind::Shape(_) => out.push(id),
            LayerKind::Group { children } => {
                for child in children {
                    visit(doc, child.id, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for id in roots {
        visit(doc, *id, &mut out);
    }
    out
}

fn bounds_of_ids(doc: &Document, ids: &[LayerId]) -> Option<Bounds> {
    ids.iter()
        .filter_map(|id| doc.find_layer(*id).and_then(layer_bounds))
        .reduce(|a, b| Bounds {
            min: a.min.min(b.min),
            max: a.max.max(b.max),
        })
}

fn mirror_extent(doc: &Document, pairs: &[(LayerId, LayerId)]) -> Bounds {
    let mut extent = Bounds {
        min: Vec2::ZERO,
        max: Vec2::new(doc.size.x as f32, doc.size.y as f32),
    };
    for (source, copy) in pairs {
        for id in [source, copy] {
            if let Some(bounds) = doc.find_layer(*id).and_then(layer_bounds) {
                extent.min = extent.min.min(bounds.min);
                extent.max = extent.max.max(bounds.max);
            }
        }
    }
    extent
}

/// Transform every shape under `path`, returning (moved, failed).
fn transform_path(doc: &mut DocumentRes, path: &[usize], placement: Placement) -> (usize, usize) {
    let Some(layer) = doc.0.layer_by_path_mut(path) else {
        return (0, 0);
    };
    transform_layer(layer, placement)
}

fn transform_layer(layer: &mut Layer, placement: Placement) -> (usize, usize) {
    match &mut layer.kind {
        LayerKind::Shape(shape) => {
            // A star stores radii + rotation, which cannot represent a mirror;
            // convert just that copy to an editable path instead.
            let source = if placement.has_negative_scale()
                && matches!(shape.source, ShapeSource::Primitive(Primitive::Star { .. }))
            {
                shape
                    .source
                    .to_path()
                    .map(ShapeSource::Path)
                    .unwrap_or_else(|| shape.source.clone())
            } else {
                shape.source.clone()
            };
            match source.transformed_affine(placement.scale, placement.rotation, placement.offset) {
                Some(transformed) => {
                    shape.source = transformed;
                    layer.touch();
                    (1, 0)
                }
                None => (0, 1),
            }
        }
        LayerKind::Group { children } => {
            let mut moved = 0;
            let mut failed = 0;
            for child in children {
                let (child_moved, child_failed) = transform_layer(child, placement);
                moved += child_moved;
                failed += child_failed;
            }
            if moved > 0 {
                layer.touch();
            }
            (moved, failed)
        }
        _ => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;
    use ns_core::{Document, FillRule, Node, Path, Primitive, ShapeSource, SubPath, VectorRole};

    fn rect(name: &str, center: Vec2, size: Vec2) -> Layer {
        Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size,
                corner_radius: 0.0,
            }),
        )
    }

    fn document() -> DocumentRes {
        DocumentRes::new(Document::new(UVec2::splat(256)))
    }

    fn centre(layer: &Layer) -> Vec2 {
        layer_bounds(layer).expect("layer has bounds").center()
    }

    fn array_entry(columns: u32, rows: u32, spacing: Vec2) -> OperationEntry {
        let mut entry = OperationEntry::new(OperationKind::Array);
        entry.array = ArrayParams {
            columns,
            rows,
            spacing,
            row_shift: Vec2::ZERO,
        };
        entry
    }

    fn mirror_entry(axis: MirrorAxis, pivot: MirrorPivot, gap: f32) -> OperationEntry {
        let mut entry = OperationEntry::new(OperationKind::Mirror);
        entry.mirror = MirrorParams {
            axis,
            pivot,
            gap,
            ..Default::default()
        };
        entry
    }

    fn pyramid_entry(copies: u32, scale_step: f32, step: Vec2) -> OperationEntry {
        let mut entry = OperationEntry::new(OperationKind::Pyramid);
        entry.pyramid = PyramidParams {
            copies,
            scale_step,
            step,
        };
        entry
    }

    fn radial_entry(copies: u32, radius: f32) -> OperationEntry {
        let mut entry = OperationEntry::new(OperationKind::Radial);
        entry.radial = RadialParams {
            copies,
            radius,
            sweep: 360.0,
            rotate: false,
            ..Default::default()
        };
        entry
    }

    fn guide(name: &str, points: &[Vec2]) -> Layer {
        let mut path = Path::new(FillRule::NonZero);
        let mut subpath = SubPath::new(false);
        for point in points {
            subpath.nodes.push(Node::corner(*point));
        }
        path.subpaths.push(subpath);
        let mut layer = Layer::shape(name, ShapeSource::Path(path));
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.vector_role = VectorRole::Path;
        }
        layer
    }

    fn sweep_entry(path: LayerId, copies: u32) -> OperationEntry {
        let mut entry = OperationEntry::new(OperationKind::Sweep);
        entry.sweep = SweepParams {
            copies,
            path: Some(path),
            ..Default::default()
        };
        entry
    }

    fn ellipse(name: &str, center: Vec2, radii: Vec2) -> Layer {
        Layer::shape(
            name,
            ShapeSource::Primitive(Primitive::Ellipse { center, radii }),
        )
    }

    fn polygon(name: &str, points: Vec<Vec2>) -> Layer {
        Layer::shape(name, ShapeSource::Primitive(Primitive::Polygon { points }))
    }

    fn state_with(doc: &DocumentRes, entries: Vec<OperationEntry>) -> EditorState {
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        state.operation_stacks.insert(doc.0.layers[0].id, entries);
        state
    }

    #[test]
    fn array_applies_a_row_and_undoes_as_one_step() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        let mut state = state_with(&doc, vec![array_entry(3, 1, Vec2::new(30.0, 0.0))]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(2));
        assert_eq!(doc.0.layers.len(), 3);
        assert_eq!(doc.0.layers[1].name, "Rect 001");
        assert_eq!(doc.0.layers[2].name, "Rect 002");
        assert!((centre(&doc.0.layers[1]) - Vec2::new(70.0, 40.0)).length() < 1e-4);
        assert!((centre(&doc.0.layers[2]) - Vec2::new(100.0, 40.0)).length() < 1e-4);
        assert_eq!(state.undo.len(), 1, "the whole array is one undo step");

        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers.len(), 1);
    }

    #[test]
    fn grid_places_rows_and_shifts_them() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)));
        let mut entry = array_entry(2, 2, Vec2::new(30.0, 50.0));
        entry.array.row_shift.x = 10.0;
        let mut state = state_with(&doc, vec![entry]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(3));
        let centres: Vec<Vec2> = doc.0.layers.iter().map(centre).collect();
        assert!((centres[1] - Vec2::new(70.0, 40.0)).length() < 1e-4);
        assert!((centres[2] - Vec2::new(50.0, 90.0)).length() < 1e-4);
        assert!((centres[3] - Vec2::new(80.0, 90.0)).length() < 1e-4);
    }

    #[test]
    fn mirror_places_a_copy_beside_the_selection() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::new(20.0, 30.0)));
        let mut state = state_with(
            &doc,
            vec![mirror_entry(
                MirrorAxis::Vertical,
                MirrorPivot::SelectionEdge,
                4.0,
            )],
        );

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
        assert_eq!(doc.0.layers.len(), 2);
        let copy = &doc.0.layers[1];
        assert_eq!(copy.name, "Rect 001");
        // Original spans 30..50; the copy starts 4 px past the right edge.
        assert!((centre(copy) - Vec2::new(64.0, 40.0)).length() < 1e-4);
        assert_eq!(state.selected, Some(vec![1]));
    }

    #[test]
    fn mirror_clip_splits_the_halves_at_the_pivot() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::new(20.0, 30.0)));
        let mut entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionCenter, 0.0);
        entry.mirror.merge = false;
        let mut state = state_with(&doc, vec![entry]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
        assert_eq!(doc.0.layers.len(), 2);
        let source = layer_bounds(&doc.0.layers[0]).unwrap();
        let copy = layer_bounds(&doc.0.layers[1]).unwrap();
        assert!(
            (source.min.x - 40.0).abs() < 1e-3,
            "source keeps the right half"
        );
        assert!((copy.max.x - 40.0).abs() < 1e-3, "copy keeps the left half");
        assert!((source.size().x - 10.0).abs() < 1e-3);
        assert!((copy.size().x - 10.0).abs() < 1e-3);
    }

    #[test]
    fn a_clipped_source_keeps_its_operation_stack_on_the_piece() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::new(20.0, 30.0)));
        let original_id = doc.0.layers[0].id;
        let mut entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionCenter, 0.0);
        entry.mirror.merge = false;
        let mut state = state_with(&doc, vec![entry]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
        assert!(
            doc.0.find_layer(original_id).is_none(),
            "the clip replaced the source shape"
        );
        let owner = *state
            .operation_stacks
            .keys()
            .next()
            .expect("the stack follows the surviving piece");
        assert!(doc.0.find_layer(owner).is_some());
        assert_eq!(owner, doc.0.layers[0].id);
        assert!(state.operation_stacks[&owner]
            .iter()
            .all(|entry| !entry.enabled));
    }

    #[test]
    fn mirror_source_side_replaces_the_destination_half() {
        let mut doc = document();
        // Left half is short (y 20..30), right half is tall (y 20..50). The
        // bounds centre is the pivot, so only the source-side choice decides
        // which half survives.
        doc.0.layers.push(polygon(
            "step",
            vec![
                Vec2::new(20.0, 20.0),
                Vec2::new(60.0, 20.0),
                Vec2::new(60.0, 50.0),
                Vec2::new(40.0, 50.0),
                Vec2::new(40.0, 30.0),
                Vec2::new(20.0, 30.0),
            ],
        ));
        let mut entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionCenter, 0.0);
        entry.mirror.source = MirrorSource::Positive;
        // Clip is forced on by the source-side choice even when the box is off.
        entry.mirror.clip = false;
        entry.mirror.merge = false;
        let mut state = state_with(&doc, vec![entry]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
        assert_eq!(doc.0.layers.len(), 2);
        let mut bounds: Vec<Bounds> = doc.0.layers.iter().filter_map(layer_bounds).collect();
        bounds.sort_by(|a, b| a.min.x.partial_cmp(&b.min.x).unwrap());
        assert!((bounds[0].min.x - 20.0).abs() < 1e-3);
        assert!((bounds[0].max.x - 40.0).abs() < 1e-3);
        assert!((bounds[1].min.x - 40.0).abs() < 1e-3);
        assert!((bounds[1].max.x - 60.0).abs() < 1e-3);
        for half in &bounds {
            assert!(
                (half.size().y - 30.0).abs() < 1e-3,
                "the short destination half must be replaced by the tall source half"
            );
        }
    }

    #[test]
    fn mirror_source_side_without_geometry_is_rejected() {
        let mut doc = document();
        doc.0.layers.push(rect(
            "panel",
            Vec2::new(180.0, 128.0),
            Vec2::new(20.0, 20.0),
        ));
        let mut entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::DocumentCenter, 0.0);
        entry.mirror.source = MirrorSource::Negative;
        entry.mirror.merge = false;
        let mut state = state_with(&doc, vec![entry]);

        let error = apply_selected_operation(&mut state, &mut doc).unwrap_err();
        assert!(error.contains("has no geometry"), "{error}");
        assert_eq!(doc.0.layers.len(), 1, "nothing was changed");
    }

    #[test]
    fn mirror_clip_and_merge_weld_the_halves_into_one_shape() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::new(20.0, 30.0)));
        let entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionCenter, 0.0);
        let mut state = state_with(&doc, vec![entry]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
        assert_eq!(doc.0.layers.len(), 1, "the two halves become one layer");
        let bounds = layer_bounds(&doc.0.layers[0]).unwrap();
        assert!((bounds.min.x - 30.0).abs() < 1e-3);
        assert!((bounds.max.x - 50.0).abs() < 1e-3);
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("merged layer is a shape");
        };
        assert!(matches!(shape.source, ShapeSource::Path(_)));
    }

    #[test]
    fn mirror_merge_welds_edge_copies() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::new(20.0, 30.0)));
        let entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionEdge, 0.0);
        let mut state = state_with(&doc, vec![entry]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
        assert_eq!(doc.0.layers.len(), 1);
        let bounds = layer_bounds(&doc.0.layers[0]).unwrap();
        assert!((bounds.min.x - 30.0).abs() < 1e-3);
        assert!((bounds.max.x - 70.0).abs() < 1e-3);
    }

    #[test]
    fn mirror_preview_clips_without_touching_the_document() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::new(20.0, 30.0)));
        let mut entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionCenter, 0.0);
        entry.mirror.merge = false;
        let plan = plan_entry(&entry, &doc.0, &[vec![0]]).expect("mirror plans");
        let preview = build_preview_document(&doc.0, &[PreviewGroup { plan }]);

        assert_eq!(doc.0.layers.len(), 1, "the real document is untouched");
        assert_eq!(preview.layers.len(), 2);
        let source = layer_bounds(&preview.layers[0]).unwrap();
        let copy = layer_bounds(&preview.layers[1]).unwrap();
        assert!((source.min.x - 40.0).abs() < 1e-3);
        assert!((copy.max.x - 40.0).abs() < 1e-3);
    }

    #[test]
    fn pyramid_shrinks_copies_about_the_selection_center() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)));
        let mut state = state_with(&doc, vec![pyramid_entry(2, 0.5, Vec2::ZERO)]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(2));
        let first = layer_bounds(&doc.0.layers[1]).unwrap();
        let second = layer_bounds(&doc.0.layers[2]).unwrap();
        assert!((first.size().x - 10.0).abs() < 1e-4);
        assert!((second.size().x - 5.0).abs() < 1e-4);
        // Centered on the same pivot: the original center.
        assert!((first.center() - Vec2::splat(40.0)).length() < 1e-4);
        assert!((second.center() - Vec2::splat(40.0)).length() < 1e-4);
    }

    #[test]
    fn pyramid_steps_up_and_keeps_unique_names() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)));
        let mut state = state_with(&doc, vec![pyramid_entry(2, 1.0, Vec2::new(0.0, -30.0))]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(2));
        assert_eq!(doc.0.layers[1].name, "Rect 001");
        assert_eq!(doc.0.layers[2].name, "Rect 002");
        assert!((centre(&doc.0.layers[1]) - Vec2::new(40.0, 10.0)).length() < 1e-4);
        assert!((centre(&doc.0.layers[2]) - Vec2::new(40.0, -20.0)).length() < 1e-4);
    }

    #[test]
    fn mirror_clip_is_symmetric_about_the_plane() {
        for axis in [MirrorAxis::Vertical, MirrorAxis::Horizontal] {
            for (source_name, layer) in [
                (
                    "polygon",
                    polygon(
                        "step",
                        vec![
                            Vec2::new(22.0, 18.0),
                            Vec2::new(58.0, 18.0),
                            Vec2::new(58.0, 31.0),
                            Vec2::new(31.0, 31.0),
                            Vec2::new(31.0, 62.0),
                            Vec2::new(22.0, 62.0),
                        ],
                    ),
                ),
                (
                    "ellipse",
                    ellipse("oval", Vec2::new(33.0, 41.0), Vec2::new(21.0, 16.0)),
                ),
            ] {
                let mut doc = document();
                doc.0.layers.push(layer);
                let pivot = layer_bounds(&doc.0.layers[0]).unwrap().center();
                let mut entry = mirror_entry(axis, MirrorPivot::SelectionCenter, 0.0);
                entry.mirror.merge = false;
                let mut state = state_with(&doc, vec![entry]);
                assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
                let covers = |q: Vec2| {
                    doc.0.layers.iter().any(
                        |layer| matches!(&layer.kind, LayerKind::Shape(s) if s.source.contains(q)),
                    )
                };
                let mut negative = 0i64;
                let mut positive = 0i64;
                let mut mismatch = 0i64;
                for y in 0..128 {
                    for x in 0..128 {
                        let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                        let reflected = match axis {
                            MirrorAxis::Vertical => Vec2::new(2.0 * pivot.x - p.x, p.y),
                            MirrorAxis::Horizontal => Vec2::new(p.x, 2.0 * pivot.y - p.y),
                            MirrorAxis::Both => Vec2::new(2.0 * pivot.x - p.x, 2.0 * pivot.y - p.y),
                        };
                        if covers(p) != covers(reflected) {
                            mismatch += 1;
                        }
                        if !covers(p) {
                            continue;
                        }
                        match axis {
                            MirrorAxis::Vertical if p.x < pivot.x => negative += 1,
                            MirrorAxis::Vertical => positive += 1,
                            MirrorAxis::Horizontal if p.y < pivot.y => negative += 1,
                            MirrorAxis::Horizontal => positive += 1,
                            MirrorAxis::Both => {}
                        }
                    }
                }
                assert_eq!(mismatch, 0, "{source_name} is not symmetric on {axis:?}");
                assert_eq!(
                    negative, positive,
                    "{source_name} has unequal halves on {axis:?}"
                );
            }
        }
    }

    #[test]
    fn mirror_merge_matches_the_union_of_the_source_and_its_reflection() {
        for pivot in [MirrorPivot::SelectionCenter, MirrorPivot::SelectionEdge] {
            let rounded = Primitive::Rect {
                center: Vec2::new(40.0, 40.0),
                size: Vec2::new(44.0, 34.0),
                corner_radius: 12.0,
            };
            let source = ShapeSource::Primitive(rounded);
            let bounds = source.bounds().unwrap();
            let pivot_x = match pivot {
                MirrorPivot::SelectionCenter => bounds.center().x,
                MirrorPivot::SelectionEdge => bounds.max.x,
                MirrorPivot::DocumentCenter => 128.0,
            };
            let mut doc = document();
            doc.0.layers.push(Layer::shape("panel", source.clone()));
            let mut entry = mirror_entry(MirrorAxis::Vertical, pivot, 0.0);
            entry.mirror.merge = true;
            let mut state = state_with(&doc, vec![entry]);
            assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
            let LayerKind::Shape(result) = &doc.0.layers[0].kind else {
                panic!("merged result is a shape");
            };
            let mut mismatched = Vec::new();
            for y in 0..128 {
                for x in 0..128 {
                    let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let reflected = Vec2::new(2.0 * pivot_x - p.x, p.y);
                    let expected = source.contains(p) || source.contains(reflected);
                    if result.source.contains(p) != expected {
                        mismatched.push((x, y));
                    }
                }
            }
            assert!(
                mismatched.is_empty(),
                "{pivot:?} merge differs from the exact union at {:?}",
                &mismatched[..mismatched.len().min(8)]
            );
        }
    }

    #[test]
    fn mirror_bakes_text_to_a_mirrored_path() {
        let mut doc = document();
        doc.0.layers.push(Layer::shape(
            "title",
            ShapeSource::Text(ns_core::TextSource {
                content: "N".into(),
                font: ns_core::FontSource::Builtin(ns_core::BuiltinFont::InterSemiBold),
                anchor: Vec2::new(30.0, 30.0),
                size_px: 40.0,
                tracking_px: 0.0,
                line_height: 1.0,
                align: ns_core::TextAlign::Left,
            }),
        ));
        let mut entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionEdge, 0.0);
        entry.mirror.merge = false;
        let mut state = state_with(&doc, vec![entry]);

        assert!(apply_selected_operation(&mut state, &mut doc).is_ok());
        let LayerKind::Shape(copy) = &doc.0.layers[1].kind else {
            panic!("mirrored text is a shape");
        };
        assert!(
            matches!(copy.source, ShapeSource::Path(_)),
            "a mirrored text copy must bake to a path so the glyphs actually flip"
        );
        let LayerKind::Shape(original) = &doc.0.layers[0].kind else {
            panic!("source is a shape");
        };
        let ink = original
            .source
            .to_path()
            .expect("text outlines")
            .bounds()
            .expect("ink bounds");
        let mirrored = layer_bounds(&doc.0.layers[1]).unwrap();
        assert!((mirrored.size().x - ink.size().x).abs() < 1e-3);
        assert!(mirrored.center().x > ink.center().x);
    }

    #[test]
    fn mirror_converts_a_star_to_a_path() {
        let mut doc = document();
        let star = Layer::shape(
            "rivet",
            ShapeSource::Primitive(Primitive::Star {
                center: Vec2::splat(40.0),
                inner_radius: 6.0,
                outer_radius: 12.0,
                points: 5,
                rotation: 0.1,
            }),
        );
        doc.0.layers.push(star);
        let mut entry = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionEdge, 0.0);
        // Keep the copy separate so this test can inspect the converted star.
        entry.mirror.merge = false;
        let mut state = state_with(&doc, vec![entry]);

        assert!(apply_selected_operation(&mut state, &mut doc).is_ok());
        assert_eq!(doc.0.layers[1].name, "Star 001");
        let LayerKind::Shape(copy) = &doc.0.layers[1].kind else {
            panic!("copy is a shape");
        };
        assert!(matches!(copy.source, ShapeSource::Path(_)));
    }

    fn collect_ids(layer: &Layer, out: &mut Vec<LayerId>) {
        out.push(layer.id);
        if let Some(children) = layer.children() {
            for child in children {
                collect_ids(child, out);
            }
        }
    }

    #[test]
    fn applied_group_copies_are_numbered_and_have_unique_ids() {
        let first = rect("first", Vec2::new(30.0, 30.0), Vec2::splat(10.0));
        let second = rect("second", Vec2::new(50.0, 30.0), Vec2::splat(10.0));
        let mut doc = document();
        doc.0
            .layers
            .push(Layer::group("group", vec![first, second]));
        let mut state = state_with(&doc, vec![array_entry(3, 1, Vec2::new(40.0, 0.0))]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(2));
        assert_eq!(doc.0.layers[1].name, "Group 001");
        assert_eq!(doc.0.layers[2].name, "Group 002");

        let mut ids = Vec::new();
        for layer in &doc.0.layers {
            collect_ids(layer, &mut ids);
        }
        let unique: HashSet<LayerId> = ids.iter().copied().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "every copied group and child needs a fresh id"
        );
    }

    #[test]
    fn the_preview_document_adds_copies_without_touching_the_source() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        let entry = array_entry(3, 1, Vec2::new(30.0, 0.0));
        let plan = OperationPlan {
            placements: entry.array.placements(),
            shape_count: 1,
            source_paths: vec![vec![0]],
            mirror: None,
            warning: None,
            hide_original: false,
        };
        let preview = build_preview_document(&doc.0, &[PreviewGroup { plan }]);

        assert_eq!(doc.0.layers.len(), 1, "the source document is untouched");
        assert_eq!(preview.layers.len(), 3);
        assert!((centre(&preview.layers[1]) - Vec2::new(70.0, 40.0)).length() < 1e-4);
        assert!((centre(&preview.layers[2]) - Vec2::new(100.0, 40.0)).length() < 1e-4);

        let mut ids = Vec::new();
        for layer in &preview.layers {
            collect_ids(layer, &mut ids);
        }
        let unique: HashSet<LayerId> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len());
    }

    #[test]
    fn active_stacks_on_other_layers_survive_selection_changes() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0
            .layers
            .push(rect("other", Vec2::new(90.0, 40.0), Vec2::splat(10.0)));
        let mut state = state_with(&doc, vec![array_entry(2, 1, Vec2::new(30.0, 0.0))]);

        sync_operation_preview(&mut state, &doc);
        assert_eq!(
            state
                .operation_preview
                .as_ref()
                .map(|preview| preview.layer_count()),
            Some(3),
            "the selected layer's array preview is built"
        );

        // Selecting a layer without operations must not drop the panel's copy.
        state.selected = Some(vec![1]);
        sync_operation_preview(&mut state, &doc);
        let preview = state
            .operation_preview
            .as_ref()
            .expect("preview survives a selection change");
        assert_eq!(preview.layer_count(), 3);
        assert_eq!(doc.0.layers.len(), 2, "the real document is untouched");

        // Active stacks on both layers contribute to the same preview.
        state.operation_stacks.insert(
            doc.0.layers[1].id,
            vec![array_entry(2, 1, Vec2::new(20.0, 0.0))],
        );
        sync_operation_preview(&mut state, &doc);
        let preview = state.operation_preview.as_ref().unwrap();
        assert_eq!(preview.layer_count(), 4);
    }

    #[test]
    fn a_raster_selection_is_refused_without_touching_the_undo_stack() {
        let mut doc = document();
        doc.0.layers.push(Layer::shape(
            "painted",
            ShapeSource::Raster(Grid::new(8, 8, 1.0)),
        ));
        let mut state = state_with(&doc, vec![array_entry(3, 1, Vec2::new(20.0, 0.0))]);

        assert!(apply_selected_operation(&mut state, &mut doc).is_err());
        assert!(state.undo.is_empty());
        assert_eq!(doc.0.layers.len(), 1);
    }

    #[test]
    fn a_group_with_painted_content_is_refused() {
        let mut doc = document();
        doc.0.layers.push(Layer::group(
            "mixed",
            vec![
                rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)),
                Layer::fill("floor", 0.4),
            ],
        ));
        let mut state = state_with(&doc, vec![array_entry(2, 1, Vec2::new(30.0, 0.0))]);

        let error = apply_selected_operation(&mut state, &mut doc).unwrap_err();
        assert!(error.contains("cannot be transformed"), "{error}");
        assert!(state.undo.is_empty());
        assert_eq!(doc.0.layers.len(), 1);
        assert!(doc.0.layers[0].is_group());
    }

    #[test]
    fn a_one_cell_array_is_refused() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(30.0), Vec2::splat(10.0)));
        let entry = array_entry(1, 1, Vec2::new(20.0, 0.0));
        assert!(plan_entry(&entry, &doc.0, &[vec![0]]).is_err());

        let mut state = state_with(&doc, vec![entry]);
        assert!(apply_selected_operation(&mut state, &mut doc).is_err());
        assert_eq!(doc.0.layers.len(), 1);
    }

    #[test]
    fn overlapping_copies_are_refused() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(30.0), Vec2::splat(10.0)));
        let entry = array_entry(3, 1, Vec2::ZERO);

        assert!(plan_entry(&entry, &doc.0, &[vec![0]]).is_err());
    }

    #[test]
    fn limits_refuse_a_huge_shape_count() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(30.0), Vec2::splat(10.0)));
        let entry = array_entry(64, 64, Vec2::new(1.0, 1.0));

        // 4,095 copies of one shape stays under the shape cap; adding many
        // shapes to the selection pushes it over and must be refused.
        assert!(plan_entry(&entry, &doc.0, &[vec![0]]).is_ok());
        let mut group = Layer::group("many", Vec::new());
        if let LayerKind::Group { children } = &mut group.kind {
            for index in 0..6 {
                children.push(rect(
                    "panel",
                    Vec2::new(20.0 + index as f32 * 8.0, 30.0),
                    Vec2::splat(4.0),
                ));
            }
        }
        doc.0.layers = vec![group];
        assert!(plan_entry(&entry, &doc.0, &[vec![0]]).is_err());
    }

    #[test]
    fn disabled_operations_are_skipped_by_apply() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        let original_id = doc.0.layers[0].id;
        let mut mirror = mirror_entry(MirrorAxis::Vertical, MirrorPivot::SelectionEdge, 0.0);
        mirror.enabled = false;
        let mut state = state_with(&doc, vec![array_entry(3, 1, Vec2::new(30.0, 0.0)), mirror]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(2));
        assert_eq!(doc.0.layers.len(), 3, "the disabled mirror adds nothing");
        let stack = state.operation_stacks.get(&original_id).unwrap();
        assert!(
            stack.iter().all(|entry| !entry.enabled),
            "applied entries switch off for the next run"
        );
    }

    #[test]
    fn a_stack_applies_every_enabled_operation() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)));
        let mut state = state_with(
            &doc,
            vec![
                array_entry(3, 1, Vec2::new(30.0, 0.0)),
                pyramid_entry(2, 0.5, Vec2::ZERO),
            ],
        );

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(4));
        assert_eq!(doc.0.layers.len(), 5);
        assert_eq!(state.undo.len(), 1, "the whole stack is one undo step");
        assert_eq!(doc.0.layers[1].name, "Rect 001");
        assert_eq!(doc.0.layers[2].name, "Rect 002");
        assert_eq!(doc.0.layers[3].name, "Rect 003");
        assert_eq!(doc.0.layers[4].name, "Rect 004");
        assert!((centre(&doc.0.layers[1]) - Vec2::new(70.0, 40.0)).length() < 1e-4);
        assert!((centre(&doc.0.layers[2]) - Vec2::new(100.0, 40.0)).length() < 1e-4);
        assert!((centre(&doc.0.layers[3]) - Vec2::splat(40.0)).length() < 1e-4);
        assert!((centre(&doc.0.layers[4]) - Vec2::splat(40.0)).length() < 1e-4);
        assert!((layer_bounds(&doc.0.layers[3]).unwrap().size().x - 10.0).abs() < 1e-4);
        assert!((layer_bounds(&doc.0.layers[4]).unwrap().size().x - 5.0).abs() < 1e-4);
    }

    #[test]
    fn an_inactive_stack_is_refused() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(30.0), Vec2::splat(10.0)));
        let mut entry = array_entry(3, 1, Vec2::new(20.0, 0.0));
        entry.enabled = false;
        let mut state = state_with(&doc, vec![entry]);

        assert!(apply_selected_operation(&mut state, &mut doc).is_err());
        assert!(state.undo.is_empty());
        assert_eq!(doc.0.layers.len(), 1);
    }

    #[test]
    fn stacks_are_keyed_by_layer() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("first", Vec2::splat(30.0), Vec2::splat(10.0)));
        doc.0
            .layers
            .push(rect("second", Vec2::splat(60.0), Vec2::splat(10.0)));
        let mut state = state_with(&doc, vec![array_entry(3, 1, Vec2::new(20.0, 0.0))]);
        state.selected = Some(vec![1]);

        assert!(apply_selected_operation(&mut state, &mut doc).is_err());
        assert_eq!(doc.0.layers.len(), 2);
    }

    #[test]
    fn radial_applies_a_ring_and_undoes_as_one_step() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)));
        let mut state = state_with(&doc, vec![radial_entry(4, 50.0)]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(4));
        assert_eq!(doc.0.layers.len(), 5);
        assert_eq!(doc.0.layers[1].name, "Rect 001");
        assert_eq!(doc.0.layers[4].name, "Rect 004");
        // 90 degrees apart on a 50 px ring around the original centre (40, 40).
        for (index, expected) in [
            Vec2::new(40.0, 90.0),
            Vec2::new(-10.0, 40.0),
            Vec2::new(40.0, -10.0),
            Vec2::new(90.0, 40.0),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                (centre(&doc.0.layers[index + 1]) - expected).length() < 1e-3,
                "copy {index} at {:?}",
                centre(&doc.0.layers[index + 1])
            );
        }
        assert_eq!(state.undo.len(), 1, "the whole ring is one undo step");

        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers.len(), 1);
    }

    #[test]
    fn radial_rotates_primitive_copies_into_paths() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)));
        let mut entry = radial_entry(1, 50.0);
        entry.radial.rotate = true;
        entry.radial.sweep = 90.0;
        let mut state = state_with(&doc, vec![entry]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(1));
        let LayerKind::Shape(copy) = &doc.0.layers[1].kind else {
            panic!("copy is a shape");
        };
        // A rotated rect has no rotation field, so the copy bakes to a path.
        assert!(matches!(copy.source, ShapeSource::Path(_)));
        assert!((centre(&doc.0.layers[1]) - Vec2::new(40.0, 90.0)).length() < 1e-3);
    }

    #[test]
    fn radial_all_shapes_copies_the_whole_selection() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(30.0, 40.0), Vec2::splat(20.0)));
        doc.0
            .layers
            .push(ellipse("lens", Vec2::new(60.0, 40.0), Vec2::new(8.0, 4.0)));
        let mut state = state_with(&doc, vec![radial_entry(3, 60.0)]);
        state.selected = Some(vec![0]);
        state.multi_selected = vec![vec![1]];

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(6));
        assert_eq!(doc.0.layers.len(), 8, "three copies of each of two shapes");
        let names: Vec<&str> = doc
            .0
            .layers
            .iter()
            .map(|layer| layer.name.as_str())
            .collect();
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("Rect "))
                .count(),
            3
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("Ellipse "))
                .count(),
            3
        );
    }

    #[test]
    fn radial_random_shape_copies_one_source_per_step() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(30.0, 40.0), Vec2::splat(20.0)));
        doc.0
            .layers
            .push(ellipse("lens", Vec2::new(60.0, 40.0), Vec2::new(8.0, 4.0)));
        let mut entry = radial_entry(8, 60.0);
        entry.radial.shape_mode = RadialShapeMode::Random;
        entry.radial.seed = 3;
        let mut state = state_with(&doc, vec![entry]);
        state.selected = Some(vec![0]);
        state.multi_selected = vec![vec![1]];
        let originals = [doc.0.layers[0].id, doc.0.layers[1].id];

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(8));
        assert_eq!(doc.0.layers.len(), 10, "one copy per step, not per shape");
        let originals: HashSet<LayerId> = originals.into_iter().collect();
        let copies: Vec<&Layer> = doc
            .0
            .layers
            .iter()
            .filter(|layer| !originals.contains(&layer.id))
            .collect();
        assert_eq!(copies.len(), 8);
        assert!(copies
            .iter()
            .all(|layer| matches!(layer.kind, LayerKind::Shape(_))));
        let names: Vec<&str> = copies.iter().map(|layer| layer.name.as_str()).collect();
        assert!(
            names
                .iter()
                .all(|name| name.starts_with("Rect ") || name.starts_with("Ellipse ")),
            "copy names: {names:?}"
        );
    }

    #[test]
    fn a_degenerate_radial_ring_is_refused() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(30.0), Vec2::splat(10.0)));
        let mut entry = radial_entry(4, 0.0);
        entry.radial.rotate = false;

        assert!(plan_entry(&entry, &doc.0, &[vec![0]]).is_err());
        assert!(apply_selected_operation(&mut state_with(&doc, vec![entry]), &mut doc).is_err());
        assert_eq!(doc.0.layers.len(), 1);
    }

    #[test]
    fn sweep_arrays_the_selection_along_the_guide() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0.layers.push(guide(
            "Spine",
            &[Vec2::new(0.0, 100.0), Vec2::new(100.0, 100.0)],
        ));
        let guide_id = doc.0.layers[1].id;
        let mut state = state_with(&doc, vec![sweep_entry(guide_id, 2)]);
        // The guide is selected with the shape, as the dock asks the artist to do.
        state.multi_selected = vec![vec![1]];

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(2));
        let rects: Vec<Vec2> = doc
            .0
            .layers
            .iter()
            .filter(|layer| matches!(layer.kind, LayerKind::Shape(_)) && !layer.is_path_guide())
            .map(centre)
            .collect();
        assert_eq!(rects.len(), 3, "the original plus two copies");
        assert!(rects.contains(&Vec2::new(40.0, 40.0)));
        assert!(rects
            .iter()
            .any(|p| (*p - Vec2::new(0.0, 100.0)).length() < 1e-4));
        assert!(rects
            .iter()
            .any(|p| (*p - Vec2::new(100.0, 100.0)).length() < 1e-4));
        assert_eq!(
            doc.0
                .layers
                .iter()
                .filter(|layer| layer.is_path_guide())
                .count(),
            1,
            "the guide itself is never copied"
        );
    }

    #[test]
    fn a_sweep_can_hide_its_original() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0.layers.push(guide(
            "Spine",
            &[Vec2::new(0.0, 100.0), Vec2::new(100.0, 100.0)],
        ));
        let stud_id = doc.0.layers[0].id;
        let guide_id = doc.0.layers[1].id;
        let mut entry = sweep_entry(guide_id, 3);
        entry.hide_original = true;
        let mut state = state_with(&doc, vec![entry]);
        state.multi_selected = vec![vec![1]];

        sync_operation_preview(&mut state, &doc);
        let preview = state.operation_preview.as_ref().expect("preview");
        assert!(
            !preview.find_layer(stud_id).expect("stud").visible,
            "the preview must hide the original"
        );
        let visible_copies = preview
            .layers
            .iter()
            .filter(|layer| !layer.is_path_guide() && layer.id != stud_id && layer.visible)
            .count();
        assert_eq!(visible_copies, 3, "the swept copies stay visible");

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(3));
        assert!(
            !doc.0.find_layer(stud_id).expect("stud").visible,
            "Apply leaves the source hidden"
        );
        assert!(
            doc.0
                .layers
                .iter()
                .filter(|layer| !layer.is_path_guide() && layer.id != stud_id)
                .all(|layer| layer.visible),
            "only the original is hidden"
        );
    }

    #[test]
    fn sweep_plan_ignores_the_guide_as_a_source() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0.layers.push(guide(
            "Spine",
            &[Vec2::new(0.0, 100.0), Vec2::new(100.0, 100.0)],
        ));
        let guide_id = doc.0.layers[1].id;
        let entry = sweep_entry(guide_id, 2);

        let plan = plan_entry(&entry, &doc.0, &[vec![0], vec![1]]).expect("plan");
        assert_eq!(plan.shape_count, 1, "only the shape is repeated");
        assert_eq!(plan.placements.len(), 2);
    }

    #[test]
    fn default_sweep_path_is_none_without_any_guide() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        let id = doc.0.layers[0].id;
        assert_eq!(default_sweep_path(&doc.0, id, &[vec![0]]), None);
    }

    #[test]
    fn a_sweep_slot_uses_the_dropped_shape_not_the_selection() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("stud", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0.layers.push(guide(
            "Spine",
            &[Vec2::new(0.0, 100.0), Vec2::new(100.0, 100.0)],
        ));
        let stud_id = doc.0.layers[0].id;
        let guide_id = doc.0.layers[1].id;
        let mut entry = sweep_entry(guide_id, 3);
        entry.sweep.sources = vec![Some(stud_id)];

        // The selection is only the guide; the slot supplies the shape.
        let plan = plan_entry(&entry, &doc.0, &[vec![1]]).expect("slot plan");
        assert_eq!(plan.source_paths, vec![vec![0]]);
        assert_eq!(plan.shape_count, 1);
        assert_eq!(plan.placements.len(), 3);
    }

    #[test]
    fn applying_a_sweep_with_a_slot_copies_only_the_slotted_shape() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("stud", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0.layers.push(guide(
            "Spine",
            &[Vec2::new(100.0, 100.0), Vec2::new(200.0, 100.0)],
        ));
        let stud_id = doc.0.layers[0].id;
        let guide_id = doc.0.layers[1].id;
        let mut entry = sweep_entry(guide_id, 2);
        entry.sweep.sources = vec![Some(stud_id)];
        let mut state = EditorState {
            selected: Some(vec![1]),
            ..Default::default()
        };
        state.operation_stacks.insert(guide_id, vec![entry]);

        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(2));
        let non_guides = doc
            .0
            .layers
            .iter()
            .filter(|layer| !layer.is_path_guide())
            .count();
        assert_eq!(non_guides, 3, "the original stud plus two copies");
        assert_eq!(
            doc.0
                .layers
                .iter()
                .filter(|layer| layer.is_path_guide())
                .count(),
            1,
            "the guide itself must never be copied"
        );
    }

    #[test]
    fn applying_a_sweep_with_two_slots_copies_both_shapes() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("stud", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0
            .layers
            .push(rect("tab", Vec2::new(40.0, 80.0), Vec2::splat(16.0)));
        doc.0.layers.push(guide(
            "Spine",
            &[Vec2::new(100.0, 100.0), Vec2::new(200.0, 100.0)],
        ));
        let stud_id = doc.0.layers[0].id;
        let tab_id = doc.0.layers[1].id;
        let guide_id = doc.0.layers[2].id;
        let mut entry = sweep_entry(guide_id, 2);
        entry.sweep.sources = vec![Some(stud_id), Some(tab_id)];
        let mut state = EditorState {
            selected: Some(vec![2]),
            ..Default::default()
        };
        state.operation_stacks.insert(guide_id, vec![entry]);

        // Together copies both slots at each of the two placements.
        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(4));
        assert_eq!(doc.0.layers.len(), 7, "two sources + guide + four copies");
    }

    #[test]
    fn applying_a_cycle_sweep_copies_one_slot_per_placement() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("stud", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0
            .layers
            .push(rect("tab", Vec2::new(40.0, 80.0), Vec2::splat(16.0)));
        doc.0.layers.push(guide(
            "Spine",
            &[Vec2::new(100.0, 100.0), Vec2::new(200.0, 100.0)],
        ));
        let stud_id = doc.0.layers[0].id;
        let tab_id = doc.0.layers[1].id;
        let guide_id = doc.0.layers[2].id;
        let mut entry = sweep_entry(guide_id, 2);
        entry.sweep.sources = vec![Some(stud_id), Some(tab_id)];
        entry.sweep.shape_mode = SweepShapeMode::Cycle;
        let mut state = EditorState {
            selected: Some(vec![2]),
            ..Default::default()
        };
        state.operation_stacks.insert(guide_id, vec![entry]);

        // Cycle uses one slot per placement: two copies, not four.
        assert_eq!(apply_selected_operation(&mut state, &mut doc), Ok(2));
        assert_eq!(doc.0.layers.len(), 5, "two sources + guide + two copies");
    }

    #[test]
    fn adding_a_sweep_can_turn_a_plain_path_into_a_guide() {
        let mut doc = document();
        doc.0
            .layers
            .push(guide("Line", &[Vec2::ZERO, Vec2::new(100.0, 0.0)]));
        if let LayerKind::Shape(shape) = &mut doc.0.layers[0].kind {
            shape.vector_role = VectorRole::Surface;
        }
        let id = doc.0.layers[0].id;
        assert!(owner_is_plain_path(&doc.0, id));
        assert!(make_owner_a_guide(&mut doc.0, id));
        assert!(doc.0.layers[0].is_path_guide());
        assert!(!owner_is_plain_path(&doc.0, id));
    }

    #[test]
    fn sweep_without_a_guide_is_refused() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        let mut entry = sweep_entry(doc.0.layers[0].id, 2);
        entry.sweep.path = None;
        let error = plan_entry(&entry, &doc.0, &[vec![0]])
            .err()
            .expect("no guide");
        assert!(error.contains("path guide"));
    }

    #[test]
    fn sweep_refuses_a_path_that_is_not_a_guide() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        let mut surface = guide("Line", &[Vec2::ZERO, Vec2::new(100.0, 0.0)]);
        if let LayerKind::Shape(shape) = &mut surface.kind {
            shape.vector_role = VectorRole::Surface;
        }
        let surface_id = surface.id;
        doc.0.layers.push(surface);

        let entry = sweep_entry(surface_id, 2);
        let error = plan_entry(&entry, &doc.0, &[vec![0]])
            .err()
            .expect("not a guide");
        assert!(error.contains("no usable spine"));
    }

    #[test]
    fn sweep_turns_copies_to_the_guide_tangent() {
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::new(40.0, 40.0), Vec2::splat(20.0)));
        doc.0.layers.push(guide(
            "Spine",
            &[Vec2::new(20.0, 20.0), Vec2::new(20.0, 220.0)],
        ));
        let guide_id = doc.0.layers[1].id;

        let plan = plan_entry(&sweep_entry(guide_id, 2), &doc.0, &[vec![0]]).expect("plan");
        assert!(plan
            .placements
            .iter()
            .all(|placement| (placement.rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-4));
    }

    #[test]
    fn the_add_operation_row_never_widens_the_dock() {
        let ctx = egui::Context::default();
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        state
            .operation_stacks
            .insert(doc.0.layers[0].id, vec![radial_entry(6, 120.0)]);
        let mut available = 0.0;
        let mut used = 0.0;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_width(240.0);
            available = ui.available_width();
            used = ui
                .scope(|ui| {
                    operations_card(ui, &mut state, &mut doc);
                })
                .response
                .rect
                .width();
        });
        output.drop_without_applying_deltas();
        assert!(
            used <= available + 0.01,
            "the operations card is {used} px wide in a {available} px dock"
        );
    }

    #[test]
    fn sweep_still_fits_the_operations_card() {
        let ctx = egui::Context::default();
        let mut doc = document();
        doc.0
            .layers
            .push(rect("panel", Vec2::splat(40.0), Vec2::splat(20.0)));
        doc.0
            .layers
            .push(guide("Spine", &[Vec2::ZERO, Vec2::new(100.0, 60.0)]));
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        state
            .operation_stacks
            .insert(doc.0.layers[0].id, vec![sweep_entry(doc.0.layers[1].id, 4)]);
        let mut available = 0.0;
        let mut used = 0.0;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_width(240.0);
            available = ui.available_width();
            used = ui
                .scope(|ui| {
                    operations_card(ui, &mut state, &mut doc);
                })
                .response
                .rect
                .width();
        });
        output.drop_without_applying_deltas();
        assert!(
            used <= available + 0.01,
            "the sweep entry is {used} px wide in a {available} px dock"
        );
    }
}
