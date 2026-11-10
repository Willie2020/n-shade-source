//! Layer tree rows, glyphs, and thumbnails.

use super::*;

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub(crate) struct LayerRow {
    pub(crate) path: Vec<usize>,
    pub(crate) depth: usize,
    /// Cached display name and type line. Rebuilt only when the document or
    /// collapsed set changes, so the row loop allocates nothing per frame.
    pub(crate) name: Arc<str>,
    pub(crate) type_label: Arc<str>,
}
pub(crate) fn collect_layer_rows(
    layers: &[Layer],
    collapsed: &HashSet<LayerId>,
    parent: &mut Vec<usize>,
    depth: usize,
    rows: &mut Vec<LayerRow>,
) {
    for (index, layer) in layers.iter().enumerate().rev() {
        parent.push(index);
        let base_label: String = if let LayerKind::Group { children } = &layer.kind {
            // Kept short: the type line shares the row with the mask and blend
            // controls, and the mono face is wider than the name's.
            if let Some(op) = layer.vector_op {
                format!("{} · {}", op.label(), layer.visible_vector_operand_count())
            } else {
                format!("Group · {}", children.len())
            }
        } else if layer.is_path_guide() {
            // A guide is a vector layer whose output is switched off on purpose;
            // the type line is the only place the row can say so, and the dock
            // is too narrow for "Shape · Path".
            "Path".into()
        } else {
            kind_label(&layer.kind).into()
        };
        // The tile suffix is the only place a row says which UDIM tile it
        // belongs to, so keep it on the always-visible type line.
        let type_label: Arc<str> = match layer.tile {
            Some(tile) => format!("{base_label} · {tile}").into(),
            None => base_label.into(),
        };
        rows.push(LayerRow {
            path: parent.clone(),
            depth,
            name: layer.name.as_str().into(),
            type_label,
        });
        if let Some(children) = layer.children() {
            if !collapsed.contains(&layer.id) {
                collect_layer_rows(children, collapsed, parent, depth + 1, rows);
            }
        }
        parent.pop();
    }
}

/// Flattened rows for the Layers panel, cached across frames.
///
/// The key is the document edit serial plus a hash of the collapsed set. Any
/// edit that changes names or structure (`mark_dirty`) invalidates it; merely
/// hovering or selecting rows does not.
pub(crate) fn cached_layer_rows(
    ctx: &egui::Context,
    state: &EditorState,
    doc: &DocumentRes,
) -> Arc<[LayerRow]> {
    let key = (state.edit_serial, collapsed_hash(&state.collapsed));
    let id = egui::Id::new("nshade_layer_rows");
    if let Some((cached_key, rows)) =
        ctx.data(|data| data.get_temp::<((u64, u64), Arc<[LayerRow]>)>(id))
    {
        if cached_key == key {
            return rows;
        }
    }
    let mut rows = Vec::new();
    collect_layer_rows(
        &doc.0.layers,
        &state.collapsed,
        &mut Vec::new(),
        0,
        &mut rows,
    );
    let rows: Arc<[LayerRow]> = rows.into();
    ctx.data_mut(|data| data.insert_temp(id, (key, rows.clone())));
    rows
}

fn collapsed_hash(collapsed: &HashSet<LayerId>) -> u64 {
    // XOR is order-independent, so the set's iteration order cannot invalidate
    // the cache and no intermediate Vec is allocated per frame.
    let mut hash = 0x9E37_79B9_7F4A_7C15_u64 ^ collapsed.len() as u64;
    for id in collapsed {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut hasher);
        hash ^= hasher.finish();
    }
    hash
}
#[derive(Clone, Copy)]
pub(crate) enum LayerGlyph {
    Rect,
    Ellipse,
    Star,
    Text,
    Path,
    /// Baked pixels: a checker of four square pixels instead of an outline.
    Raster,
    Flat,
    Fill,
    Photo,
    Alpha,
    Normal,
    Group,
    Compound,
}

pub(crate) fn layer_glyph(layer: &Layer) -> LayerGlyph {
    if layer.vector_op.is_some() {
        return LayerGlyph::Compound;
    }
    match &layer.kind {
        LayerKind::Shape(shape) => match &shape.source {
            ShapeSource::Primitive(Primitive::Rect { .. }) => LayerGlyph::Rect,
            ShapeSource::Primitive(Primitive::Ellipse { .. }) => LayerGlyph::Ellipse,
            ShapeSource::Primitive(Primitive::Star { .. }) => LayerGlyph::Star,
            ShapeSource::Text(_) => LayerGlyph::Text,
            ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) => LayerGlyph::Raster,
            _ => LayerGlyph::Path,
        },
        LayerKind::Flat => LayerGlyph::Flat,
        LayerKind::Fill { .. } => LayerGlyph::Fill,
        LayerKind::PhotoHeight(photo) if photo.alpha => LayerGlyph::Alpha,
        LayerKind::PhotoHeight(_) | LayerKind::HeightPaint { .. } => LayerGlyph::Photo,
        LayerKind::ImportedNormal { .. } | LayerKind::NormalPaint { .. } => LayerGlyph::Normal,
        LayerKind::Group { .. } => LayerGlyph::Group,
    }
}

pub(crate) const LAYER_DIM: egui::Color32 = TEXT_DISABLED;

/// How a click on a layer row changes the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowSelection {
    /// Plain click: this row becomes the whole selection.
    Replace,
    /// Ctrl/Cmd-click: add this row or drop it from the set.
    Toggle,
    /// Shift-click: select every sibling between the primary row and this one.
    Range,
}

impl RowSelection {
    pub(crate) fn from_modifiers(modifiers: egui::Modifiers) -> Self {
        if modifiers.shift {
            Self::Range
        } else if modifiers.ctrl || modifiers.command {
            Self::Toggle
        } else {
            Self::Replace
        }
    }
}

/// Apply a row click to the selection.
///
/// `selected` is always the primary row (the one Properties edits and the row
/// booleans use as the bottom shape); `multi_selected` holds the rest of the
/// set.
pub(crate) fn select_row(state: &mut EditorState, path: &[usize], mode: RowSelection) {
    // Selecting a row leaves vector-edit state behind.
    state.edge_selection = None;
    match mode {
        RowSelection::Replace => {
            state.selected = Some(path.to_vec());
            state.multi_selected.clear();
        }
        RowSelection::Toggle => toggle_row(state, path),
        RowSelection::Range => {
            if path.is_empty() {
                return;
            }
            let shares_parent = state.selected.as_deref().is_some_and(|primary| {
                primary.len() == path.len()
                    && primary[..primary.len() - 1] == path[..path.len() - 1]
            });
            if !shares_parent {
                // A range needs two rows from the same list; without that a
                // shift-click simply extends the set.
                toggle_row(state, path);
                return;
            }
            let depth = path.len();
            let anchor = state
                .selected
                .as_deref()
                .and_then(|primary| primary.last().copied())
                .unwrap_or(path[depth - 1]);
            let (low, high) = if anchor <= path[depth - 1] {
                (anchor, path[depth - 1])
            } else {
                (path[depth - 1], anchor)
            };
            let parent = path[..depth - 1].to_vec();
            let mut range = Vec::with_capacity(high - low + 1);
            for index in low..=high {
                let mut sibling = parent.clone();
                sibling.push(index);
                if sibling != path {
                    range.push(sibling);
                }
            }
            state.multi_selected = range;
            state.selected = Some(path.to_vec());
        }
    }
}

/// Add or remove one row without disturbing the rest of the set.
fn toggle_row(state: &mut EditorState, path: &[usize]) {
    let is_primary = state.selected.as_deref() == Some(path);
    let in_multi = state.multi_selected.iter().any(|row| row == path);
    if is_primary || in_multi {
        state.multi_selected.retain(|row| row != path);
        if is_primary {
            state.selected = state.multi_selected.pop();
        }
    } else if let Some(previous) = state.selected.replace(path.to_vec()) {
        if !state.multi_selected.contains(&previous) {
            state.multi_selected.push(previous);
        }
    }
}

/// A thin drop strip between rows. Hovering draws an insertion line; releasing
/// moves the dragged layer to that position.
pub(crate) fn layer_drop_strip(
    ui: &mut egui::Ui,
    doc: &DocumentRes,
    parent: Option<LayerId>,
    index: usize,
    depth: usize,
    height: f32,
    drop: &mut Option<LayerDrop>,
) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    // Strict containment, like the rows: `dnd_*` also accepts pointers within
    // egui's interact radius, and a strip 6 px away from a row would otherwise
    // fight the row for the same release.
    let pointer_inside = ui
        .ctx()
        .pointer_hover_pos()
        .is_some_and(|pos| rect.contains(pos));
    let payload = response
        .dnd_hover_payload::<LayerDragPayload>()
        .filter(|_| pointer_inside);
    let valid = payload
        .as_ref()
        .is_some_and(|payload| can_drop_layer(&doc.0, payload.id, parent));
    if valid {
        let painter = ui.painter();
        let tall = height > 24.0;
        let band = if tall {
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 5.0))
        } else {
            rect.shrink2(egui::vec2(0.0, (height - 5.0).max(0.0) * 0.5))
        };
        painter.rect_filled(band, 0.0, ACCENT.gamma_multiply(0.16));
        let y = if tall {
            rect.top() + 2.5
        } else {
            rect.center().y
        };
        draw_drop_line(ui, rect, y, depth);
        if tall {
            painter.text(
                egui::pos2(
                    rect.left() + drop_line_indent(depth) + 8.0,
                    rect.top() + 8.0,
                ),
                egui::Align2::LEFT_TOP,
                "Move to root",
                egui::FontId::proportional(TEXT_SMALL),
                ACCENT,
            );
        }
    }
    if !pointer_inside {
        return;
    }
    if let Some(payload) = response.dnd_release_payload::<LayerDragPayload>() {
        if can_drop_layer(&doc.0, payload.id, parent) {
            *drop = Some(LayerDrop {
                // Holding Alt at drag start or at the drop duplicates.
                payload: LayerDragPayload {
                    copy: payload.copy || ui.input(|input| input.modifiers.alt),
                    ..*payload
                },
                target: LayerDropTarget::Before { parent, index },
            });
        }
    }
}

pub(crate) fn draw_layer_drag_ghost(ui: &egui::Ui, response: &egui::Response, name: &str) {
    if !response.dragged() {
        return;
    }
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("layer_drag_ghost"),
    ));
    let label = if name.is_empty() { "(unnamed)" } else { name };
    // Alt turns the drop into a duplicate; say so on the ghost.
    let label = if ui.input(|input| input.modifiers.alt) {
        format!("{label} · copy")
    } else {
        label.to_string()
    };
    let galley = painter.layout_no_wrap(label, egui::FontId::proportional(TEXT_LABEL), TEXT);
    let size = galley.size() + egui::vec2(20.0, 12.0);
    let rect = egui::Rect::from_min_size(pointer + egui::vec2(14.0, 10.0), size);
    painter.rect_filled(rect, egui::CornerRadius::same(RADIUS_SM), CONTAINER_HIGH);
    // Dragged state layer (16%) per M3.
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(RADIUS_SM),
        state_layer(CONTAINER_HIGH, ON_SURFACE, DRAG_ALPHA),
    );
    painter.galley(rect.min + egui::vec2(10.0, 6.0), galley, ON_SURFACE);
}

/// Make `path` the primary row for a context menu without losing the rest of
/// the selection.
///
/// A right-click on a row that is already part of the multi-selection keeps
/// the whole set, so the **Boolean** submenu can still combine those rows.
pub(crate) fn ensure_row_selected(state: &mut EditorState, path: &[usize]) {
    if state.selected.as_deref() == Some(path) {
        return;
    }
    if let Some(index) = state.multi_selected.iter().position(|row| row == path) {
        let row = state.multi_selected.remove(index);
        if let Some(previous) = state.selected.replace(row) {
            if !state.multi_selected.contains(&previous) {
                state.multi_selected.push(previous);
            }
        }
        return;
    }
    state.selected = Some(path.to_vec());
    state.multi_selected.clear();
}

/// Start a group from the row menu.
///
/// Keep the rest of the selection: `GroupSelected` wraps the primary row plus
/// every row in `multi_selected`, so clearing the set here would silently
/// group only the row that was right-clicked.
fn make_group(state: &mut EditorState, path: &[usize], action: &mut Option<LayerAction>) {
    ensure_row_selected(state, path);
    *action = Some(LayerAction::GroupSelected);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn layer_row_menu(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    action: &mut Option<LayerAction>,
    path: &[usize],
    is_group: bool,
    locked: bool,
    can_up: bool,
    can_down: bool,
    vector_op: Option<VectorOp>,
) {
    if ui.button("Select").clicked() {
        state.selected = Some(path.to_vec());
        state.multi_selected.clear();
        ui.close();
    }
    if ui
        .button(if locked { "Unlock" } else { "Lock" })
        .on_hover_text(if locked {
            "Allow editing this layer again"
        } else {
            "Protect this layer from moving, trimming, painting and deleting"
        })
        .clicked()
    {
        ensure_row_selected(state, path);
        *action = Some(LayerAction::ToggleLock);
        ui.close();
    }
    if ui
        .button("Duplicate")
        .on_hover_text("Copy the selection directly above the originals (or Alt-drag a layer)")
        .clicked()
    {
        ensure_row_selected(state, path);
        *action = Some(LayerAction::Duplicate);
        ui.close();
    }
    if ui
        .button("Duplicate array...")
        .on_hover_text("Repeat the selection at a precise X/Y offset")
        .clicked()
    {
        ensure_row_selected(state, path);
        state.show_duplicate_array = true;
        ui.close();
    }
    if ui
        .button("Make Group")
        .on_hover_text("Wrap the selected layers in a new group")
        .clicked()
    {
        make_group(state, path, action);
        ui.close();
    }
    if is_group
        && ui
            .button(if vector_op.is_some() {
                "Release shapes"
            } else {
                "Ungroup"
            })
            .clicked()
    {
        state.selected = Some(path.to_vec());
        state.multi_selected.clear();
        *action = Some(LayerAction::UngroupSelected);
        ui.close();
    }
    // Boolean operations live on the layer: they combine the selected shapes
    // into one outline that is beveled as a single shape.
    ui.menu_button("Boolean", |ui| {
        for op in VectorOp::ALL {
            let active = vector_op == Some(op);
            let label = egui::RichText::new(op.label()).size(TEXT_SMALL);
            let mut row = egui::Button::new(label);
            if active {
                row = row.fill(SELECTED_BG);
            }
            if ui.add(row).clicked() {
                state.selected = Some(path.to_vec());
                if vector_op.is_some() {
                    state.multi_selected.clear();
                    *action = Some(LayerAction::SetBoolean(op));
                } else {
                    // Keep the rest of the selection: those are the shapes
                    // that get combined with this row.
                    *action = Some(LayerAction::VectorCompound(op));
                }
                ui.close();
            }
        }
        if let Some(op) = vector_op {
            ui.separator();
            // Divide has no single-outline bake; use the island split instead.
            let bake_label = if op.is_cut() {
                "Bake cut to shapes"
            } else {
                "Bake to shape"
            };
            if !op.is_divide() && ui.button(bake_label).clicked() {
                state.selected = Some(path.to_vec());
                state.multi_selected.clear();
                *action = Some(LayerAction::FlattenCompound);
                ui.close();
            }
            if op.is_divide() && ui.button("Split into shapes").clicked() {
                state.selected = Some(path.to_vec());
                state.multi_selected.clear();
                *action = Some(LayerAction::ReleaseDivide);
                ui.close();
            }
        }
    });
    if can_up && ui.button("Move up").clicked() {
        state.selected = Some(path.to_vec());
        state.multi_selected.clear();
        *action = Some(LayerAction::Up);
        ui.close();
    }
    if can_down && ui.button("Move down").clicked() {
        state.selected = Some(path.to_vec());
        state.multi_selected.clear();
        *action = Some(LayerAction::Down);
        ui.close();
    }
    if !is_group && ui.button("Outline stroke").clicked() {
        state.selected = Some(path.to_vec());
        state.multi_selected.clear();
        *action = Some(LayerAction::OutlineStroke);
        ui.close();
    }
    if ui
        .button("Bake to pixels")
        .on_hover_text(
            "Convert the selected shapes to pixels at document resolution; a \
             selected group merges into one baked normal layer with the group's \
             blend, opacity and mask. The bevel and normal result stay the same, \
             but the layers stop being vector-editable. Baked layers are \
             precomputed once and composited by the GPU.",
        )
        .clicked()
    {
        // Keep the rest of the selection: Bake converts every selected shape.
        ensure_row_selected(state, path);
        *action = Some(LayerAction::BakeToRaster);
        ui.close();
    }
    if !is_group
        && ui
            .button("Cut shapes below")
            .on_hover_text(
                "Subtract this line's stroke from the shapes behind it and keep every cut piece as its own layer",
            )
            .clicked()
    {
        state.selected = Some(path.to_vec());
        state.multi_selected.clear();
        *action = Some(LayerAction::CutBelow);
        ui.close();
    }
    ui.separator();
    ui.menu_button("Mask", |ui| {
        if ui.button("Bind layer below as mask").clicked() {
            state.selected = Some(path.to_vec());
            state.multi_selected.clear();
            *action = Some(LayerAction::ToggleMask);
            ui.close();
        }
        if ui.button("Bind this layer to the one above").clicked() {
            state.selected = Some(path.to_vec());
            state.multi_selected.clear();
            *action = Some(LayerAction::BindMaskToAbove);
            ui.close();
        }
        if ui.button("Add mask layer below").clicked() {
            state.selected = Some(path.to_vec());
            state.multi_selected.clear();
            *action = Some(LayerAction::NewMaskLayer);
            ui.close();
        }
        ui.separator();
        if ui.button("Paint this layer's mask...").clicked() {
            state.selected = Some(path.to_vec());
            state.multi_selected.clear();
            *action = Some(LayerAction::EditMask);
            ui.close();
        }
    });
    ui.separator();
    if ui.button("Delete").clicked() {
        state.selected = Some(path.to_vec());
        state.multi_selected.clear();
        *action = Some(LayerAction::Delete);
        ui.close();
    }
}

pub(crate) fn draw_blend_badge(
    painter: &egui::Painter,
    rect: egui::Rect,
    mode: BlendMode,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.3_f32, color);
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.5;
    let faint = color.gamma_multiply(0.45);
    match mode {
        BlendMode::Derivative => {
            for offset in [-0.28, 0.28] {
                let y = c.y + r * offset;
                painter.line_segment(
                    [
                        egui::pos2(c.x - r * 0.75, y + r * 0.16),
                        egui::pos2(c.x + r * 0.75, y - r * 0.16),
                    ],
                    stroke,
                );
            }
        }
        BlendMode::Rnm => {
            let points = arc_points(c, r * 0.62, 0.35, 4.7, 14);
            painter.add(egui::Shape::line(points, stroke));
            arrow_head(
                painter,
                c + egui::vec2(4.7f32.cos(), 4.7f32.sin()) * r * 0.62,
                4.7,
                stroke,
            );
        }
        BlendMode::Replace => {
            let a =
                egui::Rect::from_center_size(c - egui::vec2(r * 0.2, r * 0.2), egui::vec2(r, r));
            let b =
                egui::Rect::from_center_size(c + egui::vec2(r * 0.2, r * 0.2), egui::vec2(r, r));
            painter.rect_stroke(a, 1.0, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(b, 1.0, stroke, egui::StrokeKind::Inside);
        }
        BlendMode::Whiteout => {
            // XY add with a solid core: the detail is summed and Z multiplied.
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.7, c.y),
                    egui::pos2(c.x + r * 0.7, c.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y - r * 0.7),
                    egui::pos2(c.x, c.y + r * 0.7),
                ],
                stroke,
            );
            painter.circle_filled(c, r * 0.18, color);
        }
        BlendMode::Udn => {
            // An up arrow over a base bar: add XY, keep the base Z.
            let tip = c - egui::vec2(0.0, r * 0.75);
            painter.line_segment([c + egui::vec2(0.0, r * 0.2), tip], stroke);
            arrow_head(painter, tip, -std::f32::consts::FRAC_PI_2, stroke);
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.55, c.y + r * 0.7),
                    egui::pos2(c.x + r * 0.55, c.y + r * 0.7),
                ],
                egui::Stroke::new(1.0_f32, faint),
            );
        }
        BlendMode::BooleanUnion => {
            let left = c - egui::vec2(r * 0.28, 0.0);
            let right = c + egui::vec2(r * 0.28, 0.0);
            painter.circle_filled(left, r * 0.42, faint);
            painter.circle_filled(right, r * 0.42, faint);
            painter.circle_stroke(left, r * 0.42, stroke);
            painter.circle_stroke(right, r * 0.42, stroke);
        }
        BlendMode::BooleanIntersect => {
            let left = c - egui::vec2(r * 0.28, 0.0);
            let right = c + egui::vec2(r * 0.28, 0.0);
            painter.circle_stroke(left, r * 0.42, stroke);
            painter.circle_stroke(right, r * 0.42, stroke);
            painter.circle_filled(c, r * 0.18, color);
        }
        BlendMode::BooleanDifference => {
            let left = c - egui::vec2(r * 0.28, 0.0);
            let right = c + egui::vec2(r * 0.28, 0.0);
            painter.circle_filled(left, r * 0.4, faint);
            painter.circle_stroke(left, r * 0.4, stroke);
            painter.circle_stroke(right, r * 0.4, stroke);
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.1, c.y + r * 0.55),
                    egui::pos2(c.x + r * 0.45, c.y + r * 0.55),
                ],
                egui::Stroke::new(1.0_f32, color),
            );
        }
        BlendMode::PartialCut => {
            // A plate with a pocket notch cut into its top edge.
            let left = c.x - r * 0.7;
            let right = c.x + r * 0.7;
            let top = c.y - r * 0.5;
            let bottom = c.y + r * 0.65;
            painter.add(egui::Shape::line(
                vec![
                    egui::pos2(left, top),
                    egui::pos2(left, bottom),
                    egui::pos2(right, bottom),
                    egui::pos2(right, top),
                    egui::pos2(c.x + r * 0.28, top),
                    egui::pos2(c.x + r * 0.28, c.y + r * 0.1),
                    egui::pos2(c.x - r * 0.28, c.y + r * 0.1),
                    egui::pos2(c.x - r * 0.28, top),
                ],
                stroke,
            ));
        }
        BlendMode::PartialSlash => {
            // A diagonal groove: two parallel blade strokes.
            let blade = egui::vec2(r * 0.8, r * 0.45);
            painter.line_segment([c - blade, c + blade], stroke);
            painter.line_segment(
                [
                    c - blade + egui::vec2(0.0, r * 0.3),
                    c + blade + egui::vec2(0.0, r * 0.3),
                ],
                egui::Stroke::new(1.0_f32, faint),
            );
        }
        BlendMode::ClipDivide => {
            // A seam with a gap: the two surfaces after the cut.
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y - r * 0.75),
                    egui::pos2(c.x, c.y - r * 0.12),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, c.y + r * 0.12),
                    egui::pos2(c.x, c.y + r * 0.75),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x + r * 0.35, c.y + r * 0.45),
                    egui::pos2(c.x + r * 0.65, c.y + r * 0.45),
                ],
                egui::Stroke::new(1.0_f32, faint),
            );
        }
    }
}

pub(crate) fn draw_layer_thumbnail(
    painter: &egui::Painter,
    rect: egui::Rect,
    glyph: LayerGlyph,
    visible: bool,
) {
    // Preview tile: a shape layer carries the reference's 145deg violet
    // gradient, a flat layer is the solid violet with an inset white ring.
    let radius = egui::CornerRadius::same(RADIUS_MD);
    if matches!(glyph, LayerGlyph::Flat) {
        painter.rect_filled(rect, radius, egui::Color32::from_rgb(0x7B, 0x7B, 0xF6));
        // inset 0 0 0 1.5px rgba(255,255,255,.3) sits inside the edge.
        painter.rect_stroke(
            rect.shrink(0.75),
            egui::CornerRadius::same(RADIUS_MD - 1),
            egui::Stroke::new(
                1.5_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 77),
            ),
            egui::StrokeKind::Middle,
        );
    } else {
        fill_rounded_gradient(
            painter,
            rect,
            RADIUS_MD as f32,
            egui::Color32::from_rgb(0xA3, 0xA3, 0xFF),
            egui::Color32::from_rgb(0x78, 0x78, 0xF2),
            egui::vec2(0.574, 0.819),
        );
    }
    if !visible {
        painter.rect_filled(rect, radius, egui::Color32::from_black_alpha(120));
    }

    let c = rect.center();
    let tint = if !visible {
        LAYER_DIM
    } else {
        egui::Color32::from_rgb(0xE7, 0xE7, 0xFF)
    };
    let stroke = egui::Stroke::new(1.6_f32, tint);
    match glyph {
        LayerGlyph::Rect => {
            // The reference draws the shape as a filled 22 × 15 r3 tile.
            painter.rect_filled(
                egui::Rect::from_center_size(c, egui::vec2(22.0, 15.0)),
                3.0,
                tint,
            );
        }
        LayerGlyph::Ellipse => {
            painter.circle_filled(c, 7.5, tint);
        }
        LayerGlyph::Star => {
            painter.add(egui::Shape::closed_line(
                star_points(c, 9.0, 4.0, 5),
                stroke,
            ));
        }
        LayerGlyph::Text => {
            // Capital T with a short baseline serif, matching the rail icon.
            painter.line_segment(
                [
                    egui::pos2(c.x - 7.5, c.y - 7.0),
                    egui::pos2(c.x + 7.5, c.y - 7.0),
                ],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x, c.y - 7.0), egui::pos2(c.x, c.y + 7.0)],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x - 3.5, c.y + 7.0),
                    egui::pos2(c.x + 3.5, c.y + 7.0),
                ],
                stroke,
            );
        }
        LayerGlyph::Path => {
            let points = vec![
                egui::pos2(c.x - 8.0, c.y + 5.0),
                egui::pos2(c.x - 4.0, c.y - 5.0),
                egui::pos2(c.x, c.y + 4.0),
                egui::pos2(c.x + 4.0, c.y - 5.0),
                egui::pos2(c.x + 8.0, c.y + 5.0),
            ];
            painter.add(egui::Shape::line(points, stroke));
        }
        LayerGlyph::Raster => {
            // Four chunky pixels: a baked layer has no outline to edit.
            let tile = 7.0;
            let gap = 2.0;
            let origin = c - egui::vec2(tile + gap * 0.5, tile + gap * 0.5);
            for (dx, dy, alpha) in [
                (0.0, 0.0, 255),
                (tile + gap, 0.0, 128),
                (0.0, tile + gap, 128),
                (tile + gap, tile + gap, 255),
            ] {
                painter.rect_filled(
                    egui::Rect::from_min_size(origin + egui::vec2(dx, dy), egui::vec2(tile, tile)),
                    1.0,
                    egui::Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), alpha),
                );
            }
        }
        // Flat layers are a bare field; the kind line already says "Flat".
        LayerGlyph::Flat => {}
        LayerGlyph::Fill => {
            painter.rect_filled(
                egui::Rect::from_center_size(c, egui::vec2(18.0, 12.0)),
                2.0,
                egui::Color32::from_rgb(80, 92, 111),
            );
        }
        LayerGlyph::Photo => {
            let points = (0..=8)
                .map(|index| {
                    let t = index as f32 / 8.0;
                    egui::pos2(
                        c.x - 9.0 + t * 18.0,
                        c.y + (t * std::f32::consts::TAU).sin() * 4.0,
                    )
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
        }
        LayerGlyph::Alpha => {
            // Transparency checkerboard so alpha masks read differently from photos.
            let square = egui::Rect::from_center_size(c, egui::vec2(16.0, 16.0));
            painter.rect_stroke(square, 2.0, stroke, egui::StrokeKind::Inside);
            let half = 8.0;
            let cells = [
                (square.min, true),
                (square.min + egui::vec2(half, 0.0), false),
                (square.min + egui::vec2(0.0, half), false),
                (square.min + egui::vec2(half, half), true),
            ];
            for (origin, filled) in cells {
                if !filled {
                    continue;
                }
                painter.rect_filled(
                    egui::Rect::from_min_size(origin, egui::vec2(half, half)),
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), 96),
                );
            }
        }
        LayerGlyph::Normal => {
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                "N",
                egui::FontId::proportional(14.0),
                tint,
            );
        }
        LayerGlyph::Group => {
            let folder = egui::Rect::from_center_size(c, egui::vec2(18.0, 13.0));
            painter.rect_stroke(folder, 2.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    folder.left_top() + egui::vec2(2.0, -3.0),
                    folder.left_top() + egui::vec2(9.0, -3.0),
                ],
                egui::Stroke::new(1.2_f32, tint),
            );
            painter.line_segment(
                [
                    folder.left_top() + egui::vec2(9.0, -3.0),
                    folder.left_top() + egui::vec2(11.0, 0.0),
                ],
                egui::Stroke::new(1.2_f32, tint),
            );
        }
        LayerGlyph::Compound => {
            // Two overlapping live operands.
            let back =
                egui::Rect::from_center_size(c + egui::vec2(-3.0, -2.0), egui::vec2(13.0, 11.0));
            let front =
                egui::Rect::from_center_size(c + egui::vec2(3.0, 2.0), egui::vec2(13.0, 11.0));
            painter.rect_filled(back, 1.5, tint.gamma_multiply(0.22));
            painter.rect_stroke(back, 1.5, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(front, 1.5, stroke, egui::StrokeKind::Inside);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "perf measurement; run with --ignored --nocapture"]
    fn perf_layer_rows_cache() {
        use std::time::Instant;

        let ctx = egui::Context::default();
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(8)));
        for i in 0..256 {
            doc.0.layers.push(Layer::flat(format!("layer {i}")));
        }
        let state = EditorState::default();
        std::hint::black_box(cached_layer_rows(&ctx, &state, &doc));

        let start = Instant::now();
        for _ in 0..1_000 {
            std::hint::black_box(cached_layer_rows(&ctx, &state, &doc));
        }
        let cached_us = start.elapsed().as_secs_f64() * 1_000_000.0 / 1_000.0;

        let mut rows = Vec::new();
        let start = Instant::now();
        for _ in 0..1_000 {
            rows.clear();
            collect_layer_rows(
                &doc.0.layers,
                &state.collapsed,
                &mut Vec::new(),
                0,
                &mut rows,
            );
            std::hint::black_box(&rows);
        }
        let rebuild_us = start.elapsed().as_secs_f64() * 1_000_000.0 / 1_000.0;
        println!(
            "layer rows ({} rows): rebuild {rebuild_us:.1} us | cached hit {cached_us:.2} us",
            rows.len()
        );
    }

    #[test]
    fn rows_follow_nesting_and_collapse() {
        let group = Layer::group("group", vec![Layer::flat("inner"), Layer::flat("child2")]);
        let group_id = group.id;
        let mut doc = Document::new(glam::UVec2::splat(8));
        doc.layers.push(Layer::flat("top"));
        doc.layers.push(group);

        let mut rows = Vec::new();
        collect_layer_rows(&doc.layers, &HashSet::new(), &mut Vec::new(), 0, &mut rows);
        let paths: Vec<Vec<usize>> = rows.iter().map(|row| row.path.clone()).collect();
        assert_eq!(paths, vec![vec![1], vec![1, 1], vec![1, 0], vec![0]]);
        assert_eq!(rows[0].type_label.as_ref(), "Group · 2");
        assert_eq!(rows[1].name.as_ref(), "child2");

        let collapsed = HashSet::from([group_id]);
        let mut collapsed_rows = Vec::new();
        collect_layer_rows(
            &doc.layers,
            &collapsed,
            &mut Vec::new(),
            0,
            &mut collapsed_rows,
        );
        let paths: Vec<Vec<usize>> = collapsed_rows.iter().map(|row| row.path.clone()).collect();
        assert_eq!(paths, vec![vec![1], vec![0]]);
    }

    #[test]
    fn cached_rows_reuse_until_the_document_or_collapse_changes() {
        let ctx = egui::Context::default();
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(8)));
        doc.0.layers.push(Layer::flat("a"));
        let mut state = EditorState::default();

        let first = cached_layer_rows(&ctx, &state, &doc);
        let second = cached_layer_rows(&ctx, &state, &doc);
        assert!(Arc::ptr_eq(&first, &second));

        // Selection and other non-document state keep the cache.
        state.selected = Some(vec![0]);
        let third = cached_layer_rows(&ctx, &state, &doc);
        assert!(Arc::ptr_eq(&first, &third));

        // A document edit rebuilds the rows.
        state.mark_dirty();
        let fourth = cached_layer_rows(&ctx, &state, &doc);
        assert!(!Arc::ptr_eq(&first, &fourth));
    }

    #[test]
    fn toggle_click_builds_a_sibling_set() {
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };

        select_row(&mut state, &[1], RowSelection::Toggle);
        assert_eq!(state.selected, Some(vec![1]));
        assert_eq!(state.multi_selected, vec![vec![0]]);

        select_row(&mut state, &[2], RowSelection::Toggle);
        assert_eq!(state.selected, Some(vec![2]));
        assert_eq!(state.multi_selected, vec![vec![0], vec![1]]);

        // Toggling a member out keeps the primary row.
        select_row(&mut state, &[0], RowSelection::Toggle);
        assert_eq!(state.multi_selected, vec![vec![1]]);
        assert_eq!(state.selected, Some(vec![2]));

        // Re-clicking the primary row hands the primary to the remaining row.
        select_row(&mut state, &[2], RowSelection::Toggle);
        assert_eq!(state.selected, Some(vec![1]));
        assert!(state.multi_selected.is_empty());
    }

    #[test]
    fn shift_click_selects_a_sibling_range() {
        let mut state = EditorState {
            selected: Some(vec![1]),
            ..Default::default()
        };

        select_row(&mut state, &[4], RowSelection::Range);
        assert_eq!(state.selected, Some(vec![4]));
        assert_eq!(state.multi_selected, vec![vec![1], vec![2], vec![3]]);

        // Nested rows keep their parent and only span their own siblings.
        let mut nested = EditorState {
            selected: Some(vec![0, 2]),
            ..Default::default()
        };
        select_row(&mut nested, &[0, 4], RowSelection::Range);
        assert_eq!(nested.selected, Some(vec![0, 4]));
        assert_eq!(nested.multi_selected, vec![vec![0, 2], vec![0, 3]]);
    }

    #[test]
    fn plain_click_replaces_the_set() {
        let mut state = EditorState {
            selected: Some(vec![2]),
            multi_selected: vec![vec![0], vec![1]],
            ..Default::default()
        };
        select_row(&mut state, &[3], RowSelection::Replace);
        assert_eq!(state.selected, Some(vec![3]));
        assert!(state.multi_selected.is_empty());
    }

    #[test]
    fn right_clicking_a_multi_row_keeps_the_set() {
        let mut state = EditorState {
            selected: Some(vec![2]),
            multi_selected: vec![vec![0], vec![1]],
            ..Default::default()
        };

        ensure_row_selected(&mut state, &[0]);
        assert_eq!(state.selected, Some(vec![0]));
        assert_eq!(state.multi_selected, vec![vec![1], vec![2]]);

        // A row outside the set still takes over the selection alone.
        ensure_row_selected(&mut state, &[5]);
        assert_eq!(state.selected, Some(vec![5]));
        assert!(state.multi_selected.is_empty());
    }

    #[test]
    fn make_group_keeps_every_selected_row() {
        let mut state = EditorState {
            selected: Some(vec![2]),
            multi_selected: vec![vec![0], vec![1]],
            ..Default::default()
        };
        let mut action = None;
        // Right-clicked a secondary row: it becomes primary and the rest stay.
        make_group(&mut state, &[1], &mut action);
        assert!(matches!(action, Some(LayerAction::GroupSelected)));
        assert_eq!(state.selected, Some(vec![1]));
        assert_eq!(state.multi_selected, vec![vec![0], vec![2]]);
    }
}
