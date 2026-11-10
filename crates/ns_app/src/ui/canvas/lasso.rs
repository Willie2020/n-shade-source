//! Lasso tools: trim shape geometry or erase a region.
//!
//! **Trim shape** (default) subtracts the drawn region from every targeted
//! shape outline and splits what remains into editable pieces, so the bevel is
//! rebuilt along the new edge exactly as if the shape had been drawn that way.
//! The drag direction picks the gesture: **left→right** and **top→bottom**
//! trim, while **right→left** and **bottom→top** divide the shape and keep both
//! sides. Cleanup follows the selection by default: the selected layer, the
//! selected group as one unit, or every layer in a multi-selection. With
//! nothing selected — or with Alt held — the whole stack is targeted instead.
//! Non-shape layers (paint, photo, imported normals) have no outline to
//! rebuild, so they keep the original behavior: the region is hidden through
//! their painted mask. **Erase pixels** forces that mask path for every layer.
//! Shift keeps its restore meaning for masks; a geometric trim is reverted with
//! undo.

use super::*;

use ns_core::path::{
    flatten_path_tolerance_loops, simplify_closed_polyline, stroke_polyline_outline,
};

/// Drag distance (document px) below which a box lasso is treated as a click.
const MIN_REGION: f32 = 1.5;
/// Shortest straight cut the Line Lasso accepts.
const MIN_CUT_LENGTH: f32 = 2.0;
/// Bounds of the Line Lasso's cut-width control.
const MIN_CUT_WIDTH: f32 = 1.0;
const MAX_CUT_WIDTH: f32 = 64.0;
/// Freehand sampling step, in screen pixels.
const FREEHAND_STEP: f32 = 2.5;
/// Screen radius for closing a polygon lasso on its first point.
const POLYGON_CLOSE_RADIUS: f32 = 10.0;
/// Freehand loops are simplified to this many document pixels before the
/// boolean engine sees them.
const FREEHAND_SIMPLIFY_PX: f32 = 0.75;
/// Below this travel a stroke has not picked a direction yet.
const MIN_GESTURE_DIRECTION: f32 = 2.0;

/// True when the drag direction asks the lasso to divide instead of trim.
///
/// Forward drags (left→right and top→bottom) cut the region away; backward
/// drags (right→left and bottom→top) keep the inside and the remainder as two
/// pieces. The dominant axis decides, with ties going horizontal.
///
/// Box, ellipse and line lassos read the final corner because the pulled shape
/// shows which way it was drawn. Freehand and polygon strokes usually end near
/// where they started, so they read the first leg — the direction the stroke
/// set off in.
fn lasso_divides(tool: CanvasTool, draft: &LassoDraft) -> bool {
    let Some(start) = draft.points.first().copied() else {
        return false;
    };
    let delta = match tool {
        CanvasTool::LassoFree | CanvasTool::LassoPolygon => first_leg(draft),
        _ => {
            let end = draft.hover.or_else(|| draft.points.last().copied());
            match end {
                Some(end) if end.distance(start) >= MIN_GESTURE_DIRECTION => end - start,
                _ => first_leg(draft),
            }
        }
    };
    if delta.length_squared() < MIN_GESTURE_DIRECTION * MIN_GESTURE_DIRECTION {
        return false;
    }
    if delta.x.abs() >= delta.y.abs() {
        delta.x < 0.0
    } else {
        delta.y < 0.0
    }
}

/// The first leg of a loop stroke: how the gesture set off.
fn first_leg(draft: &LassoDraft) -> Vec2 {
    let Some(start) = draft.points.first().copied() else {
        return Vec2::ZERO;
    };
    for point in draft.points.iter().skip(1) {
        let delta = *point - start;
        if delta.length_squared() >= MIN_GESTURE_DIRECTION * MIN_GESTURE_DIRECTION {
            return delta;
        }
    }
    draft.hover.map_or(Vec2::ZERO, |hover| hover - start)
}

/// Refresh the marquee mode from the draft. Called after every drag sample
/// and polygon corner, so the tint and chip match what release will do.
fn refresh_divide(state: &mut EditorState, tool: CanvasTool, restore: bool) {
    let allowed =
        !restore && state.mask_edit_layer.is_none() && state.lasso_mode == LassoMode::TrimShape;
    state.lasso_divide = allowed
        && state
            .lasso_draft
            .as_ref()
            .is_some_and(|draft| lasso_divides(tool, draft));
}

/// Input for the active lasso tool.
///
/// `move_draft` is Space held over an uncommitted marquee: the region is then
/// translated instead of drawn, like a temporary move mode.
pub(crate) fn lasso_edit(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    move_draft: bool,
) {
    let primary = egui::PointerButton::Primary;
    let secondary = egui::PointerButton::Secondary;
    // Shift reverses the gesture: the region is restored instead of cleaned.
    // Cleanup follows the selected layer, group or multi-selection; Alt targets
    // the whole stack instead.
    let (restore, whole_stack) = ui.input(|input| (input.modifiers.shift, input.modifiers.alt));
    let selected_only = lasso_targets_selection(state, whole_stack);

    // Space drags the uncommitted marquee. The canvas pan is suspended by the
    // caller, so the same gesture moves the region instead of the view.
    if move_draft && state.lasso_draft.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        state.active_guides.clear();
        if response.dragged_by(primary) || response.dragged_by(secondary) {
            let screen_delta = response.drag_delta();
            let delta = Vec2::new(screen_delta.x, screen_delta.y) / view.scale();
            if delta.length_squared() > 0.0 {
                if let Some(draft) = &mut state.lasso_draft {
                    for point in &mut draft.points {
                        *point += delta;
                    }
                    if let Some(hover) = &mut draft.hover {
                        *hover += delta;
                    }
                }
                state.status = "Moving the lasso · release Space to keep drawing".into();
            }
        }
        // A drag tool that ends while its region is being moved still applies
        // at the moved position; the polygon waits for Enter or a closing click.
        if response.drag_stopped()
            && matches!(
                state.canvas_tool,
                CanvasTool::LassoRect
                    | CanvasTool::LassoEllipse
                    | CanvasTool::LassoFree
                    | CanvasTool::LassoLine
            )
        {
            let restore = restore && state.canvas_tool != CanvasTool::LassoLine;
            commit(state, doc, restore, selected_only);
        }
        return;
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }

    if state.canvas_tool == CanvasTool::LassoPolygon {
        if let Some(pointer) = response.hover_pos() {
            let (hover, guides) = polyline_place(state, doc, view, view.screen_to_doc(pointer));
            state
                .lasso_draft
                .get_or_insert_with(LassoDraft::default)
                .hover = Some(hover);
            // Smart guides are drawn from the active list, like the Line Lasso.
            state.active_guides = guides;
        } else {
            if let Some(draft) = &mut state.lasso_draft {
                draft.hover = None;
            }
            state.active_guides.clear();
        }
        if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            commit(state, doc, restore, selected_only);
            return;
        }
        if response.double_clicked() {
            commit(state, doc, restore, selected_only);
            return;
        }
        // A corner is placed as soon as the button goes down, not only on a
        // completed click: egui reclassifies a press as a drag once it outlasts
        // its 0.8 s click window or moves more than 6 px, and `clicked` never
        // fires for those. Aiming a corner and holding the button is normal.
        // Either button places corners; the drag direction picks cut or divide.
        let mut corner: Option<egui::Pos2> = None;
        if response.drag_started_by(primary) || response.drag_started_by(secondary) {
            if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
                corner = Some(origin);
            }
        }
        if corner.is_none() {
            if let Some(pointer) = response.interact_pointer_pos() {
                if response.clicked_by(primary) || response.clicked_by(secondary) {
                    corner = Some(pointer);
                }
            }
        }
        if let Some(pointer) = corner {
            place_polygon_corner(state, doc, view, pointer, restore, selected_only);
            // The first leg decides the loop direction, so refresh after each
            // corner to keep the marquee honest as the polygon grows.
            refresh_divide(state, CanvasTool::LassoPolygon, restore);
        }
        return;
    }

    if state.canvas_tool == CanvasTool::LassoFree {
        let started = response.drag_started_by(primary) || response.drag_started_by(secondary);
        if started {
            if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
                state.lasso_divide = false;
                state.lasso_draft = Some(LassoDraft {
                    points: vec![view.screen_to_doc(origin)],
                    hover: None,
                });
            }
        }
        if response.dragged_by(primary) || response.dragged_by(secondary) {
            if let Some(pointer) = response.interact_pointer_pos() {
                let point = view.screen_to_doc(pointer);
                // Sample by screen distance so the vertex count does not
                // explode when zoomed in.
                let step = view.screen_to_doc(pointer + egui::vec2(1.0, 0.0)).x - point.x;
                let step = (step * FREEHAND_STEP).abs().max(1e-3);
                let draft = state.lasso_draft.get_or_insert_with(LassoDraft::default);
                if draft
                    .points
                    .last()
                    .is_none_or(|last| last.distance(point) > step)
                {
                    draft.points.push(point);
                }
                draft.hover = Some(point);
            }
            refresh_divide(state, CanvasTool::LassoFree, restore);
        }
        if response.drag_stopped_by(primary) || response.drag_stopped_by(secondary) {
            commit(state, doc, restore, selected_only);
        }
        return;
    }

    // A straight cut: the endpoint follows the smart guides, and Shift falls
    // back to 45°/ruler when no guide is near. Shift does not restore masks
    // for this tool; there is no mask region under a line.
    if state.canvas_tool == CanvasTool::LassoLine {
        let started = response.drag_started_by(primary) || response.drag_started_by(secondary);
        if started {
            if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
                let start = view.screen_to_doc(origin);
                state.lasso_divide = false;
                state.lasso_draft = Some(LassoDraft {
                    points: vec![start],
                    hover: Some(start),
                });
            }
        }
        if response.dragged_by(primary) || response.dragged_by(secondary) {
            if let Some(pointer) = response.interact_pointer_pos() {
                let raw = view.screen_to_doc(pointer);
                let start = state
                    .lasso_draft
                    .as_ref()
                    .and_then(|draft| draft.points.first().copied());
                if let Some(start) = start {
                    let shift = ui.input(|input| input.modifiers.shift);
                    let (snapped, guides) = line_place(state, doc, view, start, raw, shift);
                    if let Some(draft) = &mut state.lasso_draft {
                        draft.hover = Some(snapped);
                    }
                    state.active_guides = guides;
                }
            }
            refresh_divide(state, CanvasTool::LassoLine, false);
        }
        if response.drag_stopped_by(primary) || response.drag_stopped_by(secondary) {
            commit(state, doc, false, selected_only);
        }
        return;
    }

    // Rectangular and elliptical lassos drag out a box. The corners snap to
    // the selected object's middle before the regular smart guides, so the box
    // can start or end exactly on the centre of the layer being cleaned.
    let started = response.drag_started_by(primary) || response.drag_started_by(secondary);
    if started {
        if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
            let raw = view.screen_to_doc(origin);
            let (start, guides) = lasso_place(state, doc, view, raw, &[]);
            state.lasso_divide = false;
            state.lasso_draft = Some(LassoDraft {
                points: vec![start],
                hover: Some(start),
            });
            state.active_guides = guides;
        }
    }
    if response.dragged_by(primary) || response.dragged_by(secondary) {
        if let Some(pointer) = response.interact_pointer_pos() {
            let raw = view.screen_to_doc(pointer);
            let anchors = state
                .lasso_draft
                .as_ref()
                .map(|draft| draft.points.clone())
                .unwrap_or_default();
            let (point, guides) = lasso_place(state, doc, view, raw, &anchors);
            if let Some(draft) = &mut state.lasso_draft {
                draft.hover = Some(point);
            }
            state.active_guides = guides;
        }
        let tool = state.canvas_tool;
        refresh_divide(state, tool, restore);
    }
    if response.drag_stopped_by(primary) || response.drag_stopped_by(secondary) {
        commit(state, doc, restore, selected_only);
        state.active_guides.clear();
    }
}

/// True when the gesture should edit the current selection instead of the
/// whole stack. The selection is the default; Alt (or an empty selection)
/// falls back to every layer.
fn lasso_targets_selection(state: &EditorState, whole_stack: bool) -> bool {
    !whole_stack && (state.selected.is_some() || !state.multi_selected.is_empty())
}

/// Every layer path in the document, parents before their children.
fn collect_all_paths(layers: &[Layer], path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
    for (index, layer) in layers.iter().enumerate() {
        path.push(index);
        out.push(path.clone());
        if let Some(children) = layer.children() {
            collect_all_paths(children, path, out);
        }
        path.pop();
    }
}

/// True when a click would close the polygon lasso on its first point.
fn polygon_close_candidate(state: &EditorState, view: &CanvasView, pointer: egui::Pos2) -> bool {
    let Some(draft) = &state.lasso_draft else {
        return false;
    };
    draft.points.len() >= 3
        && draft.points.first().is_some_and(|first| {
            view.doc_to_screen(*first).distance(pointer) <= POLYGON_CLOSE_RADIUS
        })
}

/// Place a corner at `pointer`, or close and commit when it lands on the first
/// corner. Used for both the press and the completed click, so a held button and
/// a quick tap behave the same.
fn place_polygon_corner(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    view: &CanvasView,
    pointer: egui::Pos2,
    restore: bool,
    selected_only: bool,
) {
    if polygon_close_candidate(state, view, pointer) {
        commit(state, doc, restore, selected_only);
        return;
    }
    let (point, _) = polyline_place(state, doc, view, view.screen_to_doc(pointer));
    state
        .lasso_draft
        .get_or_insert_with(LassoDraft::default)
        .points
        .push(point);
}

/// Turn the drafted region into a committed edit.
fn commit(state: &mut EditorState, doc: &mut DocumentRes, restore: bool, selected_only: bool) {
    let Some(draft) = state.lasso_draft.take() else {
        return;
    };
    state.active_guides.clear();
    let tool = state.canvas_tool;
    // The direction picks trim or divide at release, so a LassoPolygon closed
    // with Enter uses the same rule as a dragged box.
    let divide = !restore
        && state.mask_edit_layer.is_none()
        && state.lasso_mode == LassoMode::TrimShape
        && lasso_divides(tool, &draft);
    state.lasso_divide = false;
    apply_lasso_gesture(state, doc, tool, &draft, restore, selected_only, divide);
}

/// Commit a lasso draft with the default (trim/erase) gesture. Tests drive this
/// entry point; the editor goes through [`commit`], which applies the
/// direction rule.
#[cfg(test)]
pub(crate) fn apply_lasso(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    tool: CanvasTool,
    draft: &LassoDraft,
    restore: bool,
    selected_only: bool,
) -> usize {
    apply_lasso_gesture(state, doc, tool, draft, restore, selected_only, false)
}

/// Commit a lasso draft. Returns how many layers the gesture changed.
///
/// One drag is one undo step: geometry trims and mask erasing share a single
/// checkpoint, and a mixed selection is handled in the same pass.
fn apply_lasso_gesture(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    tool: CanvasTool,
    draft: &LassoDraft,
    restore: bool,
    selected_only: bool,
    divide: bool,
) -> usize {
    let Some(source) = (if tool == CanvasTool::LassoLine {
        let Some(source) = line_region_source(draft, state.lasso_line_width) else {
            state.status = "Cut line too short · drag a longer line".into();
            return 0;
        };
        Some(source)
    } else {
        region_source(tool, draft)
    }) else {
        state.status = "Lasso region too small · draw a closed area".into();
        return 0;
    };
    let Some(paths) = resolve_targets(state, doc, selected_only) else {
        return 0;
    };

    // Backward drags (right→left or bottom→top) keep both sides: the region
    // and the remainder become separate editable layers.
    if divide {
        if state.mask_edit_layer.is_some() {
            state.status = "Divide is not available while a mask is open".into();
            return 0;
        }
        let geometry = trim_targets(doc, &paths, selected_only);
        if geometry.is_empty() {
            state.status =
                "Divide needs a shape layer · paint and photo layers have no outline".into();
            return 0;
        }
        let Some(loops) = lasso_loops(tool, draft, &source) else {
            state.status = "Lasso region is empty · draw a closed area".into();
            return 0;
        };
        state.checkpoint_document(&doc.0);
        let Some(outcome) = doc.0.divide_layers_with_loops(&geometry, &loops) else {
            state.status = "Divide: the region has to cross a shape on the layer".into();
            return 0;
        };
        state.selected = outcome.paths.first().cloned();
        state.multi_selected = outcome.paths.iter().skip(1).cloned().collect();
        state.node_selection.clear();
        state.node_hover = None;
        state.mark_dirty();
        let shapes = if outcome.targets == 1 { "" } else { "s" };
        let pieces = if outcome.paths.len() == 1 { "" } else { "s" };
        state.status = format!(
            "Divided {} shape{shapes} into {} piece{pieces} · undo restores the original",
            outcome.targets,
            outcome.paths.len(),
        );
        return outcome.targets;
    }

    // While a mask is open the lasso keeps its old mask-only behavior: the
    // mask canvas owns the gesture and geometry trims would edit layers
    // behind the editor's back.
    let trim =
        !restore && state.mask_edit_layer.is_none() && state.lasso_mode == LassoMode::TrimShape;
    let geometry = if trim {
        trim_targets(doc, &paths, selected_only)
    } else {
        Vec::new()
    };
    if geometry.is_empty() {
        // Nothing has an outline to rebuild: this is the original mask-only
        // gesture (erase mode, Shift restore, or only paint layers selected).
        let cleaned = clean_up_region(state, doc, &source, restore, selected_only);
        if cleaned > 0 && tool == CanvasTool::LassoLine {
            let scope = if selected_only {
                "selection"
            } else {
                "whole stack"
            };
            state.status = if restore {
                format!(
                    "Restored the cut line on the {scope} ({cleaned} layer{})",
                    plural(cleaned)
                )
            } else {
                format!(
                    "Erased the cut line on the {scope} ({cleaned} layer{})",
                    plural(cleaned)
                )
            };
        }
        return cleaned;
    }
    // A trimmed parent disappears with its descendants, so they must not be
    // erased afterwards.
    let masks: Vec<Vec<usize>> = paths
        .into_iter()
        .filter(|path| {
            !geometry
                .iter()
                .any(|target| path.len() >= target.len() && path.starts_with(target))
        })
        .collect();

    let Some(loops) = lasso_loops(tool, draft, &source) else {
        state.status = "Lasso region is empty · draw a closed area".into();
        return 0;
    };
    let coverage = if masks.is_empty() {
        None
    } else {
        let coverage = source.rasterize(doc.0.size.x, doc.0.size.y);
        if !coverage.data.iter().any(|value| *value > 0.001) {
            state.status = "Lasso region is empty · draw a closed area".into();
            return 0;
        }
        Some(coverage)
    };
    // Mask targets are resolved to ids now: trimming shapes changes sibling
    // indices before the masks are written.
    let mask_ids: Vec<LayerId> = masks
        .iter()
        .filter_map(|path| doc.0.layer_by_path(path).map(|layer| layer.id))
        .collect();
    if geometry.is_empty() && mask_ids.is_empty() {
        state.status = "Lasso: nothing to change under the region".into();
        return 0;
    }

    state.checkpoint_document(&doc.0);
    let mut trimmed = 0;
    let mut pieces = 0;
    let mut created = Vec::new();
    if !geometry.is_empty() {
        if let Some(outcome) = doc.0.trim_layers_with_loops(&geometry, &loops) {
            trimmed = outcome.targets;
            pieces = outcome.paths.len();
            created = outcome.paths;
        }
    }
    let cleaned = coverage.as_ref().map_or(0, |coverage| {
        clean_up_layers(doc, coverage, restore, &mask_ids)
    });

    if !created.is_empty() {
        state.selected = created.first().cloned();
        state.multi_selected = created.iter().skip(1).cloned().collect();
    } else {
        drop_stale_selection(state, doc);
    }
    state.node_selection.clear();
    state.node_hover = None;
    if state
        .mask_edit_layer
        .is_some_and(|id| doc.0.find_layer(id).is_none())
    {
        end_mask_edit(state);
    }
    state.mark_dirty();
    state.status = lasso_status(
        restore,
        trimmed,
        pieces,
        cleaned,
        selected_only,
        tool == CanvasTool::LassoLine,
    );
    trimmed + cleaned
}

/// Target paths for the gesture: the selection (Alt) or every layer.
fn resolve_targets(
    state: &mut EditorState,
    doc: &DocumentRes,
    selected_only: bool,
) -> Option<Vec<Vec<usize>>> {
    let mut paths: Vec<Vec<usize>> = Vec::new();
    if selected_only {
        if let Some(path) = state.selected.clone() {
            paths.push(path);
        }
        paths.extend(state.multi_selected.iter().cloned());
        paths.retain(|path| doc.0.layer_by_path(path).is_some());
        // Locked layers (and the contents of a locked group) are frozen.
        if !paths.is_empty() && paths.iter().all(|path| path_locked(&doc.0, path)) {
            state.status = "Layer is locked · unlock it to clean up".into();
            return None;
        }
        paths.retain(|path| !path_locked(&doc.0, path));
        if paths.is_empty() {
            state.status = "Select a layer, group or several layers to clean up".into();
            return None;
        }
    } else {
        collect_all_paths(&doc.0.layers, &mut Vec::new(), &mut paths);
        paths.retain(|path| !path_locked(&doc.0, path));
    }
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// Visible shapes and vector-only groups. With Alt the selection is used as
/// given; across the whole stack only the top-most vector subtree of each
/// branch is trimmed. A mixed group (one holding paint/photo/flat layers) is
/// masked as a unit, because flattening it would silently lose those children.
fn trim_targets(doc: &DocumentRes, paths: &[Vec<usize>], selected_only: bool) -> Vec<Vec<usize>> {
    if !selected_only {
        let mut targets = Vec::new();
        collect_vector_targets(&doc.0.layers, &mut Vec::new(), &mut targets);
        targets.retain(|path| !path_locked(&doc.0, path));
        return targets;
    }

    let mut targets: Vec<Vec<usize>> = Vec::new();
    for path in paths {
        let Some(layer) = doc.0.layer_by_path(path) else {
            continue;
        };
        if !layer.visible {
            continue;
        }
        let trimable = match &layer.kind {
            LayerKind::Shape(_) => true,
            LayerKind::Group { children } => group_is_vector_only(children),
            _ => false,
        };
        if !trimable {
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
    targets
}

fn collect_vector_targets(layers: &[Layer], parent: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
    for (index, layer) in layers.iter().enumerate() {
        if !layer.visible {
            continue;
        }
        parent.push(index);
        match &layer.kind {
            LayerKind::Shape(_) => out.push(parent.clone()),
            LayerKind::Group { children } if group_is_vector_only(children) => {
                out.push(parent.clone());
            }
            _ => {}
        }
        parent.pop();
    }
}

fn group_is_vector_only(children: &[Layer]) -> bool {
    let mut any_visible = false;
    for child in children.iter().filter(|child| child.visible) {
        any_visible = true;
        let vector = match &child.kind {
            LayerKind::Shape(_) => true,
            LayerKind::Group { children } => group_is_vector_only(children),
            _ => false,
        };
        if !vector {
            return false;
        }
    }
    any_visible
}

/// Closed loops for the boolean trim, or `None` when the draft has no area.
fn lasso_loops(
    tool: CanvasTool,
    draft: &LassoDraft,
    source: &ShapeSource,
) -> Option<Vec<Vec<Vec2>>> {
    match tool {
        CanvasTool::LassoRect | CanvasTool::LassoEllipse | CanvasTool::LassoLine => {
            let path = source.to_path()?;
            let loops: Vec<Vec<Vec2>> = flatten_path_tolerance_loops(&path, 0.1)
                .into_iter()
                .filter(|(_, closed)| *closed)
                .map(|(points, _)| points)
                .filter(|points| points.len() >= 3)
                .collect();
            (!loops.is_empty()).then_some(loops)
        }
        _ => {
            let points = if tool == CanvasTool::LassoFree {
                simplify_closed_polyline(&draft.points, FREEHAND_SIMPLIFY_PX)
            } else {
                draft.points.clone()
            };
            (points.len() >= 3).then(|| vec![points])
        }
    }
}

fn drop_stale_selection(state: &mut EditorState, doc: &DocumentRes) {
    state.selected = state
        .selected
        .take()
        .filter(|path| doc.0.layer_by_path(path).is_some());
    state
        .multi_selected
        .retain(|path| doc.0.layer_by_path(path).is_some());
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn lasso_status(
    restore: bool,
    trimmed: usize,
    pieces: usize,
    cleaned: usize,
    selected_only: bool,
    cut: bool,
) -> String {
    let scope = if selected_only {
        "selection"
    } else {
        "whole stack"
    };
    if restore {
        return format!(
            "Restored the lasso area on the {scope} ({cleaned} layer{})",
            plural(cleaned)
        );
    }
    let verb = if cut { "Cut" } else { "Trimmed" };
    match (trimmed, cleaned) {
        (0, 0) => {
            if cut {
                "Cut line: nothing to change under it".into()
            } else {
                "Lasso: nothing to change under the region".into()
            }
        }
        (0, cleaned) => format!(
            "Erased the cut line on the {scope} ({cleaned} layer{})",
            plural(cleaned)
        ),
        (trimmed, 0) => format!(
            "{verb} {trimmed} shape{} into {pieces} piece{} · undo restores the outline",
            plural(trimmed),
            plural(pieces)
        ),
        (trimmed, cleaned) => format!(
            "{verb} {trimmed} shape{} into {pieces} piece{} · erased {cleaned} layer{}",
            plural(trimmed),
            plural(pieces),
            plural(cleaned)
        ),
    }
}

/// Geometry of the drafted region, or `None` when it is degenerate.
///
/// The Line Lasso has no enclosed area, so it builds a thin band around the
/// segment instead; the boolean then subtracts that band exactly like the
/// knife's stroked cutter.
fn line_region_source(draft: &LassoDraft, width: f32) -> Option<ShapeSource> {
    let start = *draft.points.first()?;
    let end = draft.hover.or_else(|| draft.points.last().copied())?;
    if start.distance(end) < MIN_CUT_LENGTH {
        return None;
    }
    let width = width.clamp(MIN_CUT_WIDTH, MAX_CUT_WIDTH);
    let outline = stroke_polyline_outline(&[start, end], width)?;
    Some(ShapeSource::Path(Path::from_polygon(&outline)))
}

fn region_source(tool: CanvasTool, draft: &LassoDraft) -> Option<ShapeSource> {
    match tool {
        CanvasTool::LassoRect | CanvasTool::LassoEllipse => {
            let start = *draft.points.first()?;
            let end = draft.hover.or_else(|| draft.points.last().copied())?;
            let bounds = Bounds {
                min: start.min(end),
                max: start.max(end),
            };
            let size = bounds.size();
            if size.x < MIN_REGION || size.y < MIN_REGION {
                return None;
            }
            let primitive = if tool == CanvasTool::LassoRect {
                Primitive::Rect {
                    center: bounds.center(),
                    size,
                    corner_radius: 0.0,
                }
            } else {
                Primitive::Ellipse {
                    center: bounds.center(),
                    radii: size * 0.5,
                }
            };
            Some(ShapeSource::Primitive(primitive))
        }
        _ => {
            if draft.points.len() < 3 {
                return None;
            }
            Some(ShapeSource::Path(Path::from_polygon(&draft.points)))
        }
    }
}

/// Hide (`restore == false`) or restore the region by compositing it into
/// layer masks.
///
/// This is the **Erase pixels** path and the fallback for layer kinds without
/// an outline. By default **every layer** is masked, so the *composited* normal
/// goes flat in the region; `selected_only` (Alt) limits the edit to the
/// current selection. Returns how many layers were touched.
pub(crate) fn clean_up_region(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    source: &ShapeSource,
    restore: bool,
    selected_only: bool,
) -> usize {
    let Some(paths) = resolve_targets(state, doc, selected_only) else {
        return 0;
    };
    let coverage = source.rasterize(doc.0.size.x, doc.0.size.y);
    if !coverage.data.iter().any(|value| *value > 0.001) {
        state.status = "Lasso region is empty · draw a closed area".into();
        return 0;
    }
    state.checkpoint_document(&doc.0);
    let ids: Vec<LayerId> = paths
        .iter()
        .filter_map(|path| doc.0.layer_by_path(path).map(|layer| layer.id))
        .collect();
    let cleaned = clean_up_layers(doc, &coverage, restore, &ids);
    if cleaned == 0 {
        return 0;
    }
    state.mark_dirty();
    let scope = if selected_only {
        "selection"
    } else {
        "whole stack"
    };
    state.status = if restore {
        format!(
            "Restored the lasso area on the {scope} ({cleaned} layer{})",
            plural(cleaned)
        )
    } else {
        format!(
            "Cleaned up the lasso area on the {scope} ({cleaned} layer{}) · masks stay editable",
            plural(cleaned)
        )
    };
    cleaned
}

/// Composite a rasterized region into the listed layers' painted masks.
fn clean_up_layers(
    doc: &mut DocumentRes,
    coverage: &Grid<f32>,
    restore: bool,
    ids: &[LayerId],
) -> usize {
    let size = doc.0.size;
    let value = if restore { 1.0 } else { 0.0 };
    let mut cleaned = 0;
    for id in ids {
        let Some(layer) = doc.0.find_layer_mut(*id) else {
            continue;
        };
        let mask = layer
            .mask
            .get_or_insert_with(|| Grid::new(size.x, size.y, 1.0));
        if mask.w != size.x || mask.h != size.y {
            *mask = mask.resized(size.x, size.y);
        }
        for (dst, region) in mask.data.iter_mut().zip(coverage.data.iter()) {
            *dst += (value - *dst) * region.clamp(0.0, 1.0);
        }
        layer.touch_mask();
        cleaned += 1;
    }
    cleaned
}

/// Lasso mode chips above the canvas, shown while a lasso tool is active.
pub(crate) fn lasso_options_bar(ui: &mut egui::Ui, state: &mut EditorState) {
    if !state.canvas_tool.is_lasso_tool() || state.mask_edit_layer.is_some() {
        return;
    }
    egui::Frame::new()
        .fill(CONTAINER_HIGH)
        .stroke(egui::Stroke::NONE)
        .corner_radius(egui::CornerRadius::same(RADIUS_SM))
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                let (icon_rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                draw_icon(
                    ui.painter(),
                    icon_rect,
                    lasso_tool_icon(state.canvas_tool),
                    ACCENT,
                );
                ui.label(egui::RichText::new("Lasso").size(TEXT_LABEL).strong());
                ui.separator();
                for mode in LassoMode::ALL {
                    if ui
                        .selectable_label(state.lasso_mode == mode, mode.label())
                        .on_hover_text(mode.hint())
                        .clicked()
                    {
                        state.lasso_mode = mode;
                    }
                }
                ui.separator();
                let line = state.canvas_tool == CanvasTool::LassoLine;
                if line {
                    ui.label(
                        egui::RichText::new("Cut width")
                            .size(TEXT_SMALL)
                            .color(MUTED),
                    );
                    ui.add(
                        egui::DragValue::new(&mut state.lasso_line_width)
                            .range(MIN_CUT_WIDTH..=MAX_CUT_WIDTH)
                            .speed(0.25)
                            .suffix(" px"),
                    );
                    ui.separator();
                }
                let scoped = lasso_targets_selection(state, false);
                let hint = match (line, scoped) {
                    (true, true) => {
                        "Scoped to the selection · Space moves the cut · drag right/down cuts, left/up divides · smart guides snap the ends · Shift 45°/ruler · Alt whole stack"
                    }
                    (true, false) => {
                        "Whole stack · select layers to scope · Space moves the cut · drag right/down cuts, left/up divides · smart guides snap the ends · Shift 45°/ruler"
                    }
                    (false, true) => {
                        "Scoped to the selection · Alt targets the whole stack · Shift restores · Space moves · drag right/down cuts, left/up divides"
                    }
                    (false, false) => {
                        "Whole stack · select a layer, group or several layers to scope · Shift restores · Space moves · drag right/down cuts, left/up divides"
                    }
                };
                ui.label(egui::RichText::new(hint).size(TEXT_CAPTION).color(MUTED));
            });
        });
}

/// Dashed region preview while a lasso is being drawn.
pub(crate) fn draw_lasso(painter: &egui::Painter, view: &CanvasView, state: &EditorState) {
    let Some(draft) = &state.lasso_draft else {
        return;
    };
    let to_screen = |point: Vec2| view.doc_to_screen(point);
    let (outline, vertices) = match state.canvas_tool {
        CanvasTool::LassoRect => {
            let Some(start) = draft.points.first().copied() else {
                return;
            };
            let end = draft.hover.unwrap_or(start);
            let min = start.min(end);
            let max = start.max(end);
            let corners = [
                Vec2::new(min.x, min.y),
                Vec2::new(max.x, min.y),
                Vec2::new(max.x, max.y),
                Vec2::new(min.x, max.y),
            ];
            (
                corners.iter().map(|corner| to_screen(*corner)).collect(),
                Vec::new(),
            )
        }
        CanvasTool::LassoEllipse => {
            let Some(start) = draft.points.first().copied() else {
                return;
            };
            let end = draft.hover.unwrap_or(start);
            let center = (start + end) * 0.5;
            let radii = (end - start).abs() * 0.5;
            let points = (0..48)
                .map(|step| {
                    let angle = std::f32::consts::TAU * step as f32 / 48.0;
                    to_screen(center + Vec2::new(angle.cos() * radii.x, angle.sin() * radii.y))
                })
                .collect();
            (points, Vec::new())
        }
        CanvasTool::LassoLine => {
            let Some(start) = draft.points.first().copied() else {
                return;
            };
            let end = draft.hover.unwrap_or(start);
            let width = state.lasso_line_width.clamp(MIN_CUT_WIDTH, MAX_CUT_WIDTH);
            let Some(outline) = stroke_polyline_outline(&[start, end], width) else {
                return;
            };
            (
                outline.iter().map(|point| to_screen(*point)).collect(),
                vec![to_screen(start), to_screen(end)],
            )
        }
        _ => {
            let mut points: Vec<egui::Pos2> = draft.points.iter().map(|p| to_screen(*p)).collect();
            if let Some(hover) = draft.hover {
                if state.canvas_tool == CanvasTool::LassoPolygon {
                    points.push(to_screen(hover));
                }
            }
            let vertices = draft.points.iter().map(|p| to_screen(*p)).collect();
            (points, vertices)
        }
    };
    if outline.len() < 2 {
        return;
    }
    let mut closed = outline.clone();
    if let Some(first) = closed.first().copied() {
        closed.push(first);
    }
    // Marching-ants look: a dark underlay keeps the dashes readable on any map.
    // Orange means the direction will divide instead of trim.
    let dashes = if state.lasso_divide {
        ACCENT
    } else {
        egui::Color32::WHITE
    };
    painter.add(egui::Shape::line(
        closed.clone(),
        egui::Stroke::new(2.0_f32, egui::Color32::from_black_alpha(140)),
    ));
    painter.add(egui::Shape::dashed_line(
        &closed,
        egui::Stroke::new(1.0_f32, dashes),
        6.0,
        4.0,
    ));
    for vertex in vertices {
        painter.circle_filled(vertex, 2.5, dashes);
        painter.circle_stroke(vertex, 2.5, egui::Stroke::new(1.0_f32, ACCENT));
    }

    // Direction readout: the cut/divide rule is invisible otherwise, so name
    // the gesture next to the pointer while the marquee is live.
    let color = if state.lasso_divide { ACCENT } else { dashes };
    // Shift is the 45°/ruler constraint for the Line Lasso, not a mask restore.
    let shift = painter.ctx().input(|input| input.modifiers.shift)
        && state.canvas_tool != CanvasTool::LassoLine;
    let label = if shift {
        "Restore"
    } else if state.mask_edit_layer.is_some() || state.lasso_mode == LassoMode::ErasePixels {
        "Erase"
    } else if state.lasso_divide {
        "Divide"
    } else {
        "Cut"
    };
    if let Some(anchor) = draft
        .hover
        .or_else(|| draft.points.last().copied())
        .map(to_screen)
    {
        let galley = painter.layout_no_wrap(label.to_owned(), egui::FontId::monospace(11.0), color);
        let pos = egui::pos2(anchor.x + 16.0, anchor.y - galley.size().y - 14.0);
        let chip = egui::Rect::from_min_size(pos, galley.size()).expand2(egui::vec2(5.0, 3.0));
        painter.rect_filled(chip, 3.0, egui::Color32::from_black_alpha(180));
        painter.rect_stroke(
            chip,
            3.0,
            egui::Stroke::new(1.0_f32, color),
            egui::StrokeKind::Inside,
        );
        painter.galley(pos, galley, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape_doc() -> DocumentRes {
        let mut layer = Layer::shape(
            "dial",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: Vec2::splat(48.0),
                radii: Vec2::splat(40.0),
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 10.0;
            shape.height_px = 12.0;
        }
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(96)));
        doc.0.layers.push(layer);
        doc
    }

    #[test]
    fn a_box_lasso_builds_the_region() {
        let draft = LassoDraft {
            points: vec![Vec2::new(20.0, 30.0)],
            hover: Some(Vec2::new(60.0, 70.0)),
        };
        let source = region_source(CanvasTool::LassoRect, &draft).expect("rect region");
        let bounds = source.bounds().expect("bounds");
        assert_eq!(bounds.min, Vec2::new(20.0, 30.0));
        assert_eq!(bounds.max, Vec2::new(60.0, 70.0));

        let source = region_source(CanvasTool::LassoEllipse, &draft).expect("ellipse");
        assert!(matches!(
            source,
            ShapeSource::Primitive(Primitive::Ellipse { .. })
        ));
    }

    #[test]
    fn a_tiny_box_lasso_is_ignored() {
        let draft = LassoDraft {
            points: vec![Vec2::new(20.0, 30.0)],
            hover: Some(Vec2::new(20.5, 30.5)),
        };
        assert!(region_source(CanvasTool::LassoRect, &draft).is_none());
    }

    #[test]
    fn a_polygon_needs_three_points() {
        let two = LassoDraft {
            points: vec![Vec2::new(10.0, 10.0), Vec2::new(40.0, 12.0)],
            hover: None,
        };
        assert!(region_source(CanvasTool::LassoPolygon, &two).is_none());
        let three = LassoDraft {
            points: vec![
                Vec2::new(10.0, 10.0),
                Vec2::new(40.0, 12.0),
                Vec2::new(24.0, 40.0),
            ],
            hover: None,
        };
        assert!(region_source(CanvasTool::LassoPolygon, &three).is_some());
    }

    fn region() -> ShapeSource {
        ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::new(30.0, 30.0),
            size: Vec2::splat(20.0),
            corner_radius: 0.0,
        })
    }

    #[test]
    fn the_default_cleans_the_whole_stack() {
        // Two stacked shapes: the lasso has to remove both, otherwise the
        // normal behind the selected layer keeps showing its edges.
        let mut doc = shape_doc();
        let mut upper = Layer::shape(
            "upper",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(30.0),
                size: Vec2::splat(20.0),
                corner_radius: 2.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut upper.kind {
            shape.radius_px = 4.0;
            shape.height_px = 6.0;
        }
        doc.0.layers.push(upper);
        let mut state = EditorState {
            selected: Some(vec![1]),
            ..Default::default()
        };
        assert_eq!(
            clean_up_region(&mut state, &mut doc, &region(), false, false),
            2
        );
        for layer in &doc.0.layers {
            assert!(
                layer
                    .mask
                    .as_ref()
                    .is_some_and(|mask| *mask.get(30, 30) < 0.01),
                "every layer is cleaned: {}",
                layer.name
            );
            assert!(
                layer
                    .mask
                    .as_ref()
                    .is_some_and(|mask| *mask.get(80, 80) > 0.99),
                "outside the region stays"
            );
        }
        assert!(state.status.contains("whole stack"));
    }

    #[test]
    fn alt_cleans_only_the_selected_layer() {
        let mut doc = shape_doc();
        let mut upper = Layer::shape(
            "upper",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: Vec2::splat(48.0),
                radii: Vec2::splat(20.0),
            }),
        );
        upper.touch();
        doc.0.layers.push(upper);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        assert_eq!(
            clean_up_region(&mut state, &mut doc, &region(), false, true),
            1
        );
        assert!(
            doc.0.layers[0].mask.is_some(),
            "the selected layer is cleaned"
        );
        assert!(doc.0.layers[1].mask.is_none(), "the rest is untouched");
        assert!(state.status.contains("selection"));
    }

    #[test]
    fn cleaning_hides_the_region_in_the_painted_mask() {
        let mut doc = shape_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        assert_eq!(
            clean_up_region(&mut state, &mut doc, &region(), false, true),
            1
        );
        let mask = doc.0.layers[0].mask.clone().expect("mask created");
        assert!(*mask.get(30, 30) < 0.01, "the region is hidden");
        assert!(*mask.get(80, 80) > 0.99, "the rest stays visible");
        // Non destructive: the vector source is untouched and undo restores it.
        assert!(matches!(
            &doc.0.layers[0].kind,
            LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Primitive(_))
        ));
        assert!(state.undo(&mut doc));
        assert!(doc.0.layers[0].mask.is_none(), "undo drops the mask again");
    }

    #[test]
    fn restoring_paints_the_region_back() {
        let mut doc = shape_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        clean_up_region(&mut state, &mut doc, &region(), false, true);
        assert!(doc.0.layers[0]
            .mask
            .as_ref()
            .is_some_and(|mask| *mask.get(30, 30) < 0.01));
        clean_up_region(&mut state, &mut doc, &region(), true, true);
        assert!(doc.0.layers[0]
            .mask
            .as_ref()
            .is_some_and(|mask| *mask.get(30, 30) > 0.99));
    }

    #[test]
    fn an_empty_region_changes_nothing() {
        let mut doc = shape_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        // Collinear points enclose no area, so nothing is covered.
        let region = ShapeSource::Path(Path::from_polygon(&[
            Vec2::new(10.0, 10.0),
            Vec2::new(40.0, 10.0),
            Vec2::new(70.0, 10.0),
        ]));
        assert_eq!(
            clean_up_region(&mut state, &mut doc, &region, false, false),
            0
        );
        assert!(doc.0.layers[0].mask.is_none());
        assert!(state.status.contains("empty"));
    }

    #[test]
    fn selected_only_without_a_selection_reports_it() {
        let mut doc = shape_doc();
        let mut state = EditorState::default();
        assert_eq!(
            clean_up_region(&mut state, &mut doc, &region(), false, true),
            0
        );
        assert!(state.status.contains("Select a layer"));
        assert!(doc.0.layers[0].mask.is_none());
    }

    fn panel_doc() -> DocumentRes {
        let mut layer = Layer::shape(
            "panel",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(48.0),
                size: Vec2::splat(80.0),
                corner_radius: 0.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut layer.kind {
            shape.radius_px = 12.0;
            shape.height_px = 10.0;
        }
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(96)));
        doc.0.layers.push(layer);
        doc
    }

    fn rect_trim(
        state: &mut EditorState,
        doc: &mut DocumentRes,
        x0: f32,
        x1: f32,
        restore: bool,
        selected_only: bool,
    ) -> usize {
        let draft = LassoDraft {
            points: vec![Vec2::new(x0, 0.0)],
            hover: Some(Vec2::new(x1, 96.0)),
        };
        apply_lasso(
            state,
            doc,
            CanvasTool::LassoRect,
            &draft,
            restore,
            selected_only,
        )
    }

    #[test]
    fn trim_mode_cuts_geometry_and_selects_the_pieces() {
        let mut doc = panel_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };
        assert_eq!(rect_trim(&mut state, &mut doc, 44.0, 52.0, false, true), 1);
        assert_eq!(doc.0.layers.len(), 2, "the strip splits the panel");
        assert!(matches!(doc.0.layers[0].kind, LayerKind::Shape(_)));
        assert_eq!(state.selected, Some(vec![0]));
        assert_eq!(state.multi_selected, vec![vec![1]]);
        assert_eq!(state.undo.len(), 1, "one drag, one undo step");

        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers.len(), 1);
        assert!(matches!(
            &doc.0.layers[0].kind,
            LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Primitive(_))
        ));
    }

    #[test]
    fn trim_mode_cuts_shapes_and_erases_other_layers_in_one_step() {
        let mut doc = panel_doc();
        doc.0
            .layers
            .push(Layer::height_paint("paint", Grid::new(96, 96, 0.4), 1.0));
        let mut state = EditorState {
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };
        assert_eq!(rect_trim(&mut state, &mut doc, 44.0, 52.0, false, false), 2);
        assert_eq!(doc.0.layers.len(), 3, "two pieces plus the paint layer");
        let paint = doc.0.layers.last().expect("paint layer");
        assert!(
            paint
                .mask
                .as_ref()
                .is_some_and(|mask| *mask.get(48, 48) < 0.01),
            "a paint layer has no outline to trim, so it is erased"
        );
        assert_eq!(state.undo.len(), 1, "both edits share one undo step");
        assert!(state.undo(&mut doc));
        assert_eq!(doc.0.layers.len(), 2);
        assert!(doc.0.layers[1].mask.is_none(), "undo drops the mask");
    }

    #[test]
    fn erase_mode_keeps_the_vector_source() {
        let mut doc = panel_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::ErasePixels,
            ..Default::default()
        };
        assert_eq!(rect_trim(&mut state, &mut doc, 44.0, 52.0, false, true), 1);
        assert_eq!(doc.0.layers.len(), 1);
        assert!(matches!(
            &doc.0.layers[0].kind,
            LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Primitive(_))
        ));
        assert!(doc.0.layers[0]
            .mask
            .as_ref()
            .is_some_and(|mask| *mask.get(48, 48) < 0.01));
    }

    #[test]
    fn shift_restores_masks_instead_of_trimming() {
        let mut doc = panel_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::ErasePixels,
            ..Default::default()
        };
        rect_trim(&mut state, &mut doc, 44.0, 52.0, false, true);
        assert!(*doc.0.layers[0].mask.as_ref().unwrap().get(48, 48) < 0.01);
        rect_trim(&mut state, &mut doc, 44.0, 52.0, true, true);
        assert!(*doc.0.layers[0].mask.as_ref().unwrap().get(48, 48) > 0.99);
        assert_eq!(doc.0.layers.len(), 1, "restore never trims");
    }

    #[test]
    fn the_selection_limits_the_trim() {
        let mut doc = panel_doc();
        doc.0.layers.push(Layer::shape(
            "upper",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(48.0),
                size: Vec2::splat(40.0),
                corner_radius: 0.0,
            }),
        ));
        let mut state = EditorState {
            selected: Some(vec![1]),
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };
        rect_trim(&mut state, &mut doc, 44.0, 52.0, false, true);
        assert!(
            matches!(
                &doc.0.layers[0].kind,
                LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Primitive(_))
            ),
            "the unselected layer keeps its primitive"
        );
        assert!(doc.0.layers.iter().skip(1).all(|layer| matches!(
            &layer.kind,
            LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Path(_))
        )));
    }

    #[test]
    fn mixed_groups_are_erased_not_flattened() {
        let shape = Layer::shape(
            "shape",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(48.0),
                size: Vec2::splat(40.0),
                corner_radius: 0.0,
            }),
        );
        let paint = Layer::height_paint("paint", Grid::new(96, 96, 0.4), 1.0);
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(96)));
        doc.0.layers.push(Layer::group("mixed", vec![shape, paint]));
        let mut state = EditorState {
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };
        // Whole-stack trim: a mixed group is masked as one unit, its shape
        // child is not pulled out and flattened.
        rect_trim(&mut state, &mut doc, 44.0, 52.0, false, false);
        assert_eq!(doc.0.layers.len(), 1);
        let LayerKind::Group { children } = &doc.0.layers[0].kind else {
            panic!("the mixed group is kept")
        };
        assert_eq!(children.len(), 2, "its children are not silently flattened");
        assert!(
            doc.0.layers[0].mask.is_some(),
            "the group is erased through its mask instead"
        );
    }

    #[test]
    fn cleanup_follows_the_selection_unless_alt_is_held() {
        let empty = EditorState::default();
        assert!(
            !lasso_targets_selection(&empty, false),
            "no selection = stack"
        );
        assert!(!lasso_targets_selection(&empty, true));

        let single = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        assert!(lasso_targets_selection(&single, false));
        assert!(
            !lasso_targets_selection(&single, true),
            "Alt overrides the selection with the whole stack"
        );

        let multi = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1], vec![2, 0]],
            ..Default::default()
        };
        assert!(lasso_targets_selection(&multi, false));
        let multi_only = EditorState {
            multi_selected: vec![vec![1]],
            ..Default::default()
        };
        assert!(lasso_targets_selection(&multi_only, false));
    }

    #[test]
    fn multi_selected_layers_are_cleaned_together() {
        let mut doc = shape_doc();
        let mut upper = Layer::shape(
            "upper",
            ShapeSource::Primitive(Primitive::Ellipse {
                center: Vec2::splat(48.0),
                radii: Vec2::splat(20.0),
            }),
        );
        upper.touch();
        doc.0.layers.push(upper);
        let mut state = EditorState {
            selected: Some(vec![0]),
            multi_selected: vec![vec![1]],
            ..Default::default()
        };

        assert_eq!(
            clean_up_region(&mut state, &mut doc, &region(), false, true),
            2
        );

        assert!(doc.0.layers.iter().all(|layer| layer.mask.is_some()));
        assert!(state.status.contains("selection"), "{}", state.status);
    }

    #[test]
    fn a_selected_group_is_trimmed_without_touching_other_layers() {
        let group_shape = Layer::shape(
            "child",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(48.0),
                size: Vec2::splat(64.0),
                corner_radius: 0.0,
            }),
        );
        let other = Layer::shape(
            "other",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(20.0),
                size: Vec2::splat(16.0),
                corner_radius: 0.0,
            }),
        );
        let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(96)));
        doc.0.layers.push(Layer::group("group", vec![group_shape]));
        doc.0.layers.push(other);
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };

        rect_trim(&mut state, &mut doc, 44.0, 52.0, false, true);

        let other = doc
            .0
            .layers
            .iter()
            .find(|layer| layer.name == "other")
            .expect("the unselected layer is kept");
        assert!(
            matches!(
                &other.kind,
                LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Primitive(_))
            ),
            "the unselected layer keeps its primitive"
        );
        assert!(
            doc.0.layers.iter().any(|layer| {
                layer.name.starts_with("group")
                    && matches!(
                        &layer.kind,
                        LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Path(_))
                    )
            }),
            "the selected group is replaced by trimmed pieces"
        );
    }

    #[test]
    fn a_line_cut_splits_the_selected_shape() {
        let mut doc = panel_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };
        let draft = LassoDraft {
            points: vec![Vec2::new(48.0, -20.0)],
            hover: Some(Vec2::new(48.0, 116.0)),
        };

        assert_eq!(
            apply_lasso(
                &mut state,
                &mut doc,
                CanvasTool::LassoLine,
                &draft,
                false,
                true,
            ),
            1
        );

        assert_eq!(doc.0.layers.len(), 2, "the cut splits the panel in two");
        assert!(doc.0.layers.iter().all(|layer| matches!(
            &layer.kind,
            LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Path(_))
        )));
        assert!(state.status.starts_with("Cut"), "{}", state.status);
    }

    #[test]
    fn a_line_cut_respects_the_selection() {
        let mut doc = panel_doc();
        let mut other = Layer::shape(
            "other",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(48.0),
                size: Vec2::splat(24.0),
                corner_radius: 0.0,
            }),
        );
        other.touch();
        doc.0.layers.push(other);
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };
        let draft = LassoDraft {
            points: vec![Vec2::new(48.0, -20.0)],
            hover: Some(Vec2::new(48.0, 116.0)),
        };

        apply_lasso(
            &mut state,
            &mut doc,
            CanvasTool::LassoLine,
            &draft,
            false,
            true,
        );

        let other = doc
            .0
            .layers
            .iter()
            .find(|layer| layer.name == "other")
            .expect("the unselected layer is kept");
        assert!(matches!(
            &other.kind,
            LayerKind::Shape(shape) if matches!(shape.source, ShapeSource::Primitive(_))
        ));
    }

    #[test]
    fn a_short_line_changes_nothing() {
        let mut doc = panel_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };
        let draft = LassoDraft {
            points: vec![Vec2::splat(48.0)],
            hover: Some(Vec2::splat(48.5)),
        };

        assert_eq!(
            apply_lasso(
                &mut state,
                &mut doc,
                CanvasTool::LassoLine,
                &draft,
                false,
                true,
            ),
            0
        );
        assert!(state.status.contains("too short"), "{}", state.status);
        assert_eq!(doc.0.layers.len(), 1);
    }

    #[test]
    fn a_line_in_erase_mode_masks_a_thin_band() {
        let mut doc = panel_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::ErasePixels,
            ..Default::default()
        };
        let draft = LassoDraft {
            points: vec![Vec2::new(48.0, -20.0)],
            hover: Some(Vec2::new(48.0, 116.0)),
        };

        assert_eq!(
            apply_lasso(
                &mut state,
                &mut doc,
                CanvasTool::LassoLine,
                &draft,
                false,
                true,
            ),
            1
        );

        let mask = doc.0.layers[0].mask.as_ref().expect("mask created");
        assert!(*mask.get(48, 48) < 0.01, "the cut line is erased");
        assert!(*mask.get(20, 48) > 0.99, "outside the line stays visible");
        assert!(state.status.contains("cut line"), "{}", state.status);
    }

    #[test]
    fn freehand_loops_are_simplified_before_the_boolean() {
        let mut points = Vec::new();
        for step in 0..240 {
            let angle = std::f32::consts::TAU * step as f32 / 240.0;
            points.push(Vec2::splat(48.0) + Vec2::new(angle.cos(), angle.sin()) * 28.0);
        }
        let draft = LassoDraft {
            points: points.clone(),
            hover: None,
        };
        let source = region_source(CanvasTool::LassoFree, &draft).expect("region");
        let loops = lasso_loops(CanvasTool::LassoFree, &draft, &source).expect("loops");
        assert!(
            loops[0].len() < points.len() / 2,
            "the freehand loop should be thinned before the boolean"
        );

        let mut doc = panel_doc();
        let mut state = EditorState {
            selected: Some(vec![0]),
            lasso_mode: LassoMode::TrimShape,
            ..Default::default()
        };
        let changed = apply_lasso(
            &mut state,
            &mut doc,
            CanvasTool::LassoFree,
            &draft,
            false,
            true,
        );
        assert_eq!(changed, 1);
        assert_eq!(doc.0.layers.len(), 1);
        let LayerKind::Shape(shape) = &doc.0.layers[0].kind else {
            panic!("piece is a shape")
        };
        let ShapeSource::Path(path) = &shape.source else {
            panic!("piece is a path")
        };
        assert_eq!(path.fill_rule, FillRule::EvenOdd);
        assert!(path.subpaths.len() >= 2, "the freehand loop became a hole");
    }

    #[test]
    fn the_drag_direction_decides_cut_or_divide() {
        let draft = |delta: Vec2| LassoDraft {
            points: vec![Vec2::splat(48.0)],
            hover: Some(Vec2::splat(48.0) + delta),
        };
        // Forward drags trim.
        assert!(!lasso_divides(
            CanvasTool::LassoRect,
            &draft(Vec2::new(30.0, 4.0))
        ));
        assert!(!lasso_divides(
            CanvasTool::LassoRect,
            &draft(Vec2::new(4.0, 30.0))
        ));
        // Backward drags divide.
        assert!(lasso_divides(
            CanvasTool::LassoRect,
            &draft(Vec2::new(-30.0, 4.0))
        ));
        assert!(lasso_divides(
            CanvasTool::LassoRect,
            &draft(Vec2::new(4.0, -30.0))
        ));
        // The dominant axis wins a diagonal, with ties going horizontal.
        assert!(!lasso_divides(
            CanvasTool::LassoRect,
            &draft(Vec2::new(20.0, -8.0))
        ));
        assert!(lasso_divides(
            CanvasTool::LassoRect,
            &draft(Vec2::new(-20.0, 20.0))
        ));
        assert!(!lasso_divides(
            CanvasTool::LassoRect,
            &draft(Vec2::new(20.0, 20.0))
        ));
    }

    #[test]
    fn loop_strokes_use_the_direction_they_started_in() {
        let polygon = |points: Vec<Vec2>| LassoDraft {
            points,
            hover: None,
        };
        let clockwise = polygon(vec![
            Vec2::new(20.0, 20.0),
            Vec2::new(44.0, 20.0),
            Vec2::new(44.0, 44.0),
            Vec2::new(20.0, 44.0),
        ]);
        assert!(!lasso_divides(CanvasTool::LassoPolygon, &clockwise));

        let counter_clockwise = polygon(vec![
            Vec2::new(44.0, 20.0),
            Vec2::new(20.0, 20.0),
            Vec2::new(20.0, 44.0),
            Vec2::new(44.0, 44.0),
        ]);
        assert!(lasso_divides(CanvasTool::LassoPolygon, &counter_clockwise));

        // A freehand loop that returns to its start still reads its first leg.
        let start = Vec2::splat(48.0);
        let rightward = LassoDraft {
            points: vec![
                start,
                start + Vec2::new(40.0, 0.0),
                start + Vec2::new(40.0, 30.0),
                start + Vec2::new(0.0, 30.0),
                start,
            ],
            hover: None,
        };
        assert!(!lasso_divides(CanvasTool::LassoFree, &rightward));

        let leftward = LassoDraft {
            points: vec![
                start,
                start + Vec2::new(-40.0, 0.0),
                start + Vec2::new(-40.0, 30.0),
                start + Vec2::new(0.0, 30.0),
                start,
            ],
            hover: None,
        };
        assert!(lasso_divides(CanvasTool::LassoFree, &leftward));
    }

    /// The polygon lasso is the only lasso that places its region with clicks
    /// instead of a drag, so it needs its own pointer-driven test.
    #[cfg(test)]
    mod pointer {
        use super::*;

        const IMAGE: egui::Rect = egui::Rect {
            min: egui::pos2(20.0, 20.0),
            max: egui::pos2(220.0, 220.0),
        };
        const DOC_SIZE: f32 = 96.0;

        fn screen(doc: Vec2) -> egui::Pos2 {
            IMAGE.min + egui::vec2(doc.x, doc.y) / DOC_SIZE * IMAGE.size()
        }

        struct Harness {
            ctx: egui::Context,
            doc: DocumentRes,
            state: EditorState,
            now: f64,
            /// Space held: the next frames move the draft instead of drawing.
            move_draft: bool,
        }

        impl Harness {
            fn new() -> Self {
                Self::with_tool(CanvasTool::LassoPolygon)
            }

            fn with_tool(tool: CanvasTool) -> Self {
                let ctx = egui::Context::default();
                ctx.options_mut(|options| {
                    options.max_passes = std::num::NonZeroUsize::new(1).unwrap();
                });
                let doc = shape_doc();
                let mut state = EditorState {
                    canvas_tool: tool,
                    selected: Some(vec![0]),
                    lasso_mode: LassoMode::TrimShape,
                    ..Default::default()
                };
                // Guides would move the clicked corners onto the ellipse.
                state.smart_guides = false;
                Self {
                    ctx,
                    doc,
                    state,
                    now: 0.0,
                    move_draft: false,
                }
            }

            fn frame(&mut self, events: Vec<egui::Event>) {
                self.now += 1.0 / 60.0;
                let mut raw = egui::RawInput::default();
                raw.screen_rect = Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(240.0, 240.0),
                ));
                raw.time = Some(self.now);
                raw.predicted_dt = 1.0 / 60.0;
                raw.events = events;
                let ctx = self.ctx.clone();
                let doc = &mut self.doc;
                let state = &mut self.state;
                let move_draft = self.move_draft;
                let output = ctx.run_ui(raw, |ui| {
                    let (response, _painter) =
                        ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
                    let view = CanvasView::for_test(IMAGE, Vec2::splat(DOC_SIZE));
                    lasso_edit(ui, &response, state, doc, &view, move_draft);
                });
                output.drop_without_applying_deltas();
            }

            fn press(&mut self, at: egui::Pos2) {
                self.press_with(egui::PointerButton::Primary, at);
            }

            fn press_with(&mut self, button: egui::PointerButton, at: egui::Pos2) {
                self.frame(vec![egui::Event::PointerButton {
                    pos: at,
                    button,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }

            fn release(&mut self, at: egui::Pos2) {
                self.release_with(egui::PointerButton::Primary, at);
            }

            fn release_with(&mut self, button: egui::PointerButton, at: egui::Pos2) {
                self.frame(vec![egui::Event::PointerButton {
                    pos: at,
                    button,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }

            fn click(&mut self, at: egui::Pos2) {
                self.click_with(egui::PointerButton::Primary, at);
            }

            fn click_with(&mut self, button: egui::PointerButton, at: egui::Pos2) {
                self.frame(vec![egui::Event::PointerMoved(at)]);
                self.press_with(button, at);
                self.release_with(button, at);
            }

            /// Keep the button down without moving it.
            fn hold(&mut self, seconds: f64) {
                self.now += seconds;
                self.frame(Vec::new());
            }

            fn key(&mut self, key: egui::Key) {
                self.frame(vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }

            fn points(&self) -> usize {
                self.state
                    .lasso_draft
                    .as_ref()
                    .map_or(0, |draft| draft.points.len())
            }

            fn last_point(&self) -> Option<Vec2> {
                self.state
                    .lasso_draft
                    .as_ref()
                    .and_then(|draft| draft.points.last().copied())
            }
        }

        fn corners() -> [Vec2; 4] {
            [
                Vec2::new(20.0, 20.0),
                Vec2::new(44.0, 20.0),
                Vec2::new(44.0, 44.0),
                Vec2::new(20.0, 44.0),
            ]
        }

        #[test]
        fn clicks_place_corners_and_enter_commits() {
            let mut harness = Harness::new();
            for corner in corners() {
                harness.click(screen(corner));
            }
            assert_eq!(harness.points(), 4, "every click places one corner");
            harness.key(egui::Key::Enter);
            assert!(harness.state.lasso_draft.is_none());
            assert!(harness.state.edit_serial > 0, "the region was committed");
            assert!(
                !harness.state.status.contains("too small")
                    && !harness.state.status.contains("empty"),
                "unexpected status: {}",
                harness.state.status
            );
        }

        #[test]
        fn clicking_the_first_corner_closes_the_polygon() {
            let mut harness = Harness::new();
            for corner in corners().iter().take(3) {
                harness.click(screen(*corner));
            }
            assert_eq!(harness.points(), 3);
            harness.click(screen(corners()[0]));
            assert!(harness.state.lasso_draft.is_none(), "the click closed it");
            assert!(harness.state.edit_serial > 0);
        }

        /// egui only reports a click when the press is shorter than its click
        /// window (0.8 s). A deliberate corner often outlasts that, and the
        /// press is then a drag, which used to place nothing.
        #[test]
        fn a_long_press_still_places_a_corner() {
            let mut harness = Harness::new();
            let at = screen(Vec2::new(20.0, 20.0));
            harness.frame(vec![egui::Event::PointerMoved(at)]);
            harness.press(at);
            harness.hold(1.0);
            harness.release(at);
            assert_eq!(harness.points(), 1, "a held press must place one corner");
        }

        /// The same happens when the pointer moves more than egui's 6 px click
        /// slop while pressing: the corner belongs where the press landed.
        #[test]
        fn a_slight_drag_places_the_corner_at_the_press_point() {
            let mut harness = Harness::new();
            let at = screen(Vec2::new(20.0, 20.0));
            harness.frame(vec![egui::Event::PointerMoved(at)]);
            harness.press(at);
            harness.frame(vec![egui::Event::PointerMoved(at + egui::vec2(12.0, 0.0))]);
            harness.release(at + egui::vec2(12.0, 0.0));
            assert_eq!(harness.points(), 1, "the press must place one corner");
            let placed = harness.last_point().expect("corner");
            assert!(
                placed.distance(Vec2::new(20.0, 20.0)) < 0.5,
                "the corner follows the press, not the release: {placed:?}"
            );
        }

        /// Space drags an uncommitted polygon without adding corners, the way a
        /// marquee moves before it is applied.
        #[test]
        fn space_drags_the_uncommitted_polygon() {
            let mut harness = Harness::new();
            for corner in corners().iter().take(3) {
                harness.click(screen(*corner));
            }
            let before = harness
                .state
                .lasso_draft
                .as_ref()
                .expect("draft")
                .points
                .clone();
            assert_eq!(before.len(), 3);

            harness.move_draft = true;
            let from = egui::pos2(120.0, 120.0);
            harness.frame(vec![egui::Event::PointerMoved(from)]);
            harness.press(from);
            // Two moves of 20 screen px each: the polygon follows both.
            harness.frame(vec![egui::Event::PointerMoved(
                from + egui::vec2(20.0, 0.0),
            )]);
            harness.frame(vec![egui::Event::PointerMoved(
                from + egui::vec2(40.0, 10.0),
            )]);
            harness.release(from + egui::vec2(40.0, 10.0));

            let after = &harness
                .state
                .lasso_draft
                .as_ref()
                .expect("draft survives the move")
                .points;
            assert_eq!(after.len(), 3, "moving must not add a corner");
            let scale = 200.0 / DOC_SIZE;
            for (old, new) in before.iter().zip(after) {
                let expected = *old + Vec2::new(40.0 / scale, 10.0 / scale);
                assert!(
                    new.distance(expected) < 0.05,
                    "{old} should move to {expected}, got {new}"
                );
            }
        }

        /// Dragging a box forward (left→right) trims the region away.
        #[test]
        fn a_left_to_right_box_trims() {
            let mut harness = Harness::with_tool(CanvasTool::LassoRect);
            let from = egui::pos2(120.0, 25.0);
            let to = egui::pos2(205.0, 205.0);
            harness.frame(vec![egui::Event::PointerMoved(from)]);
            harness.press(from);
            harness.frame(vec![egui::Event::PointerMoved(to)]);
            harness.release(to);

            assert_eq!(harness.doc.0.layers.len(), 1, "one trimmed piece");
            assert!(
                !harness.state.status.contains("Divided"),
                "{}",
                harness.state.status
            );
        }

        /// Dragging the same box backwards (right→left) keeps both sides.
        #[test]
        fn a_right_to_left_box_divides() {
            let mut harness = Harness::with_tool(CanvasTool::LassoRect);
            let from = egui::pos2(205.0, 205.0);
            let to = egui::pos2(120.0, 25.0);
            harness.frame(vec![egui::Event::PointerMoved(from)]);
            harness.press(from);
            harness.frame(vec![egui::Event::PointerMoved(to)]);
            harness.release(to);

            assert_eq!(
                harness.doc.0.layers.len(),
                2,
                "the divided shape keeps both sides"
            );
            assert!(
                harness
                    .state
                    .status
                    .contains("Divided 1 shape into 2 pieces"),
                "{}",
                harness.state.status
            );
            assert!(
                !harness.state.lasso_divide,
                "committing resets the divide flag"
            );
        }

        /// A vertical forward drag also trims.
        #[test]
        fn a_top_to_bottom_box_trims() {
            let mut harness = Harness::with_tool(CanvasTool::LassoRect);
            let from = egui::pos2(120.0, 25.0);
            let to = egui::pos2(205.0, 120.0);
            harness.frame(vec![egui::Event::PointerMoved(from)]);
            harness.press(from);
            harness.frame(vec![egui::Event::PointerMoved(to)]);
            harness.release(to);

            assert_eq!(harness.doc.0.layers.len(), 1);
            assert!(
                !harness.state.status.contains("Divided"),
                "{}",
                harness.state.status
            );
        }

        /// A vertical backward drag divides the shape where the box crosses it.
        #[test]
        fn a_bottom_to_top_box_divides() {
            let mut harness = Harness::with_tool(CanvasTool::LassoRect);
            let from = egui::pos2(205.0, 120.0);
            let to = egui::pos2(120.0, 25.0);
            harness.frame(vec![egui::Event::PointerMoved(from)]);
            harness.press(from);
            harness.frame(vec![egui::Event::PointerMoved(to)]);
            harness.release(to);

            assert_eq!(harness.doc.0.layers.len(), 2);
            assert!(
                harness.state.status.contains("Divided"),
                "{}",
                harness.state.status
            );
        }

        /// The button no longer chooses the mode: a right-button drag follows
        /// the same direction rule as the left button.
        #[test]
        fn a_secondary_drag_follows_the_direction_rule() {
            let mut harness = Harness::with_tool(CanvasTool::LassoRect);
            let from = egui::pos2(120.0, 25.0);
            let to = egui::pos2(205.0, 205.0);
            harness.frame(vec![egui::Event::PointerMoved(from)]);
            harness.press_with(egui::PointerButton::Secondary, from);
            harness.frame(vec![egui::Event::PointerMoved(to)]);
            harness.release_with(egui::PointerButton::Secondary, to);

            assert_eq!(harness.doc.0.layers.len(), 1, "a forward drag trims");
            assert!(!harness.state.status.contains("Divided"));
        }

        /// The polygon uses the leg it starts with: clockwise from the
        /// top-left corner moves right, so it trims.
        #[test]
        fn a_forward_polygon_trims() {
            let mut harness = Harness::new();
            for corner in corners() {
                harness.click(screen(corner));
            }
            harness.click(screen(corners()[0]));
            assert_eq!(harness.doc.0.layers.len(), 1);
            assert!(
                !harness.state.status.contains("Divided"),
                "{}",
                harness.state.status
            );
        }

        /// Reversing the polygon starts the first leg right→left, so it divides.
        #[test]
        fn a_backward_polygon_divides() {
            let mut harness = Harness::new();
            let reversed = [corners()[1], corners()[0], corners()[3], corners()[2]];
            for corner in reversed {
                harness.click(screen(corner));
            }
            harness.click(screen(reversed[0]));
            assert!(harness.state.lasso_draft.is_none(), "the click closed it");
            assert_eq!(harness.doc.0.layers.len(), 2);
            assert!(
                harness.state.status.contains("Divided"),
                "{}",
                harness.state.status
            );
        }
    }
}
