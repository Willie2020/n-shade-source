//! Layer rows painted to the `.layer` component in
//! `nshade-layer-component.html`.
//!
//! The row is a 66 px border-box — 1 border + 10 pad + 44 content + 10 pad +
//! 1 border — with four parts: eye, thumbnail, name/kind and the blend tag.
//! It is drawn by hand rather than composed from widgets so the parts land on
//! the reference x positions at any row width: eye at x+11, thumb at x+39,
//! text at x+95, tag flush to x_right−11.
//!
//! Selection is the CSS paint order: background, `inset 3px 0 0` accent bar
//! clipped to the padding box, then the 1 px accent border.

use super::*;

// ── geometry (CSS box model) ────────────────────────────────────────────────
const ROW_H: f32 = 66.0;
const ROW_BORDER: f32 = 1.0;
const ROW_PAD: f32 = 10.0;
const ROW_GAP: f32 = 10.0;
const ROW_RADIUS: f32 = 10.0;
const EYE_SIZE: f32 = 18.0;
const THUMB_SIZE: f32 = 46.0;
const INDENT_PER_DEPTH: f32 = 12.0;
const CHEVRON_LANE: f32 = 12.0;
/// Lane reserved for the mask affordance. It is always kept free so that
/// revealing the control on hover never moves the blend tag.
const MASK_SIZE: f32 = 22.0;
const MASK_GAP: f32 = 8.0;
/// Lane reserved for the lock toggle, left of the mask affordance.
const LOCK_SIZE: f32 = 20.0;
const LOCK_GAP: f32 = 4.0;
const TAG_PAD_X: f32 = 8.0;
const TAG_PAD_Y: f32 = 5.0;
const TAG_RADIUS: u8 = 6;
/// Rows narrower than this keep the tag's footprint as an icon-only badge.
const NARROW_ROW: f32 = 280.0;
/// `var(--accent-line)`: accent at 35 %.
fn select_border() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(PRIMARY.r(), PRIMARY.g(), PRIMARY.b(), 89)
}

pub(crate) fn layer_row(
    ui: &mut egui::Ui,
    row: &LayerRow,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    action: &mut Option<LayerAction>,
    selection_mode: RowSelection,
    drop: &mut Option<LayerDrop>,
) {
    let Some(layer) = doc.0.layer_by_path(&row.path) else {
        return;
    };
    let id = layer.id;
    let visible = layer.visible;
    let locked = layer.locked;
    let is_group = layer.is_group();
    // A layer tagged for another UDIM tile is kept in the stack but does not
    // contribute to the canvas. Dim it like a hidden layer so a tile switch
    // does not look like the layer vanished.
    let off_tile = layer.tile.is_some_and(|tile| tile != doc.0.active_tile);
    let shown = visible && !off_tile;
    let collapsed = state.collapsed.contains(&id);
    let name = row.name.as_ref();
    let type_label = row.type_label.as_ref();
    let glyph = layer_glyph(layer);
    let blend = layer.blend;
    let vector_op = layer.vector_op;
    let id_color = layer.id_color;
    let has_clip_mask = layer.mask_layer.is_some();
    let is_mask_source = doc.0.is_mask_source(id);
    let has_paint_mask = layer.mask.is_some();
    let editing_mask = state.mask_edit_layer == Some(id);
    let paint_mask = layer.mask.as_ref();
    let mask_info = MaskRowInfo {
        enabled: layer.mask_settings.enabled,
        inverted: layer.mask_settings.invert,
        smart_count: layer
            .mask_settings
            .generators
            .iter()
            .filter(|generator| generator.enabled)
            .count(),
    };
    let (can_up, can_down) = move_flags_for(&doc.0, &row.path);

    let selected = state.selected.as_deref() == Some(row.path.as_slice());
    let multi = state
        .multi_selected
        .iter()
        .any(|path| path.as_slice() == row.path.as_slice());

    let row_width = ui.available_width();
    // The row itself is the drag handle: grabbing the name, the type line or
    // the empty space reorders the stack. The controls painted on top (eye,
    // chevron, mask, blend tag, thumbnail) are registered later, so a click on
    // one of them still goes to that control first.
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(row_width, ROW_H), egui::Sense::click_and_drag());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = response.contains_pointer();

    // ── layout ─────────────────────────────────────────────────────────────
    let cy = rect.center().y;
    let indent = (row.depth as f32 * INDENT_PER_DEPTH).min(60.0);
    // Groups get a disclosure lane before the eye; a group's own eye and its
    // children's eyes then share the same x at every depth.
    let chevron_lane = if is_group { CHEVRON_LANE } else { 0.0 };
    let eye_x = rect.left() + ROW_BORDER + ROW_PAD + indent + chevron_lane;
    let eye = egui::Rect::from_min_size(
        egui::pos2(eye_x, cy - EYE_SIZE * 0.5),
        egui::Vec2::splat(EYE_SIZE),
    );
    let chevron = is_group.then(|| {
        egui::Rect::from_min_size(
            egui::pos2(eye_x - CHEVRON_LANE, cy - 6.0),
            egui::vec2(CHEVRON_LANE, 12.0),
        )
    });
    // The 46 px thumb overflows the 44 px content box by 1 px per side, so it
    // sits at the row top + border + padding, not at the content top + 1.
    let thumb = egui::Rect::from_min_size(
        egui::pos2(eye.right() + ROW_GAP, rect.top() + ROW_BORDER + ROW_PAD),
        egui::Vec2::splat(THUMB_SIZE),
    );
    let text_x = thumb.right() + ROW_GAP;

    let narrow = rect.width() < NARROW_ROW;
    // Boolean groups advertise their live vector operation; every other layer
    // advertises how it blends into the stack.
    let tag_text = match vector_op {
        Some(op) => format!("{} {}", vector_op_operator(op), op.label()),
        None => format!("{} {}", blend_operator(blend), blend_short_label(blend)),
    };
    let tag_galley = (!narrow).then(|| ui.painter().layout_no_wrap(tag_text, mono(11.0), MUTED));
    let tag = match &tag_galley {
        Some(galley) => {
            let size = galley.size()
                + egui::vec2(
                    TAG_PAD_X * 2.0 + ROW_BORDER * 2.0,
                    TAG_PAD_Y * 2.0 + ROW_BORDER * 2.0,
                );
            egui::Rect::from_min_size(
                egui::pos2(
                    rect.right() - ROW_BORDER - ROW_PAD - size.x,
                    cy - size.y * 0.5,
                ),
                size,
            )
        }
        None => egui::Rect::from_min_size(
            egui::pos2(
                rect.right() - ROW_BORDER - ROW_PAD - MASK_SIZE,
                cy - MASK_SIZE * 0.5,
            ),
            egui::Vec2::splat(MASK_SIZE),
        ),
    };
    let mask = egui::Rect::from_min_size(
        egui::pos2(tag.left() - MASK_GAP - MASK_SIZE, cy - MASK_SIZE * 0.5),
        egui::Vec2::splat(MASK_SIZE),
    );
    let lock = egui::Rect::from_min_size(
        egui::pos2(mask.left() - LOCK_GAP - LOCK_SIZE, cy - LOCK_SIZE * 0.5),
        egui::Vec2::splat(LOCK_SIZE),
    );
    // The name always stops short of the reserved mask and lock lanes, so
    // showing those controls on hover never reflows the text.
    let text_right = (lock.left() - MASK_GAP).max(text_x + 8.0);

    // ── background and selection (CSS paint order) ─────────────────────────
    let radius = egui::CornerRadius::same(ROW_RADIUS as u8);
    if selected || multi || hovered {
        ui.painter().rect_filled(rect, radius, CONTAINER);
    }
    // Every row in the selection gets the full treatment. A weaker secondary
    // outline made Ctrl/Shift ranges hard to spot, which defeats the purpose
    // of multi-select: the anchor is named in the Properties dock, so all
    // selected rows can share the same accent bar and border.
    if selected || multi {
        // `box-shadow: inset 3px 0 0 accent` is clipped to the padding box and
        // equals A \ (A + (3, 0)). A ∩ (A + (3, 0)) is itself a rounded rect
        // with the same radius, so painting A accent then that rect in the row
        // surface leaves the exact crescent, corners included.
        let padding_box = rect.shrink(ROW_BORDER);
        let inner_radius = egui::CornerRadius::same((ROW_RADIUS - ROW_BORDER) as u8);
        ui.painter().rect_filled(padding_box, inner_radius, PRIMARY);
        ui.painter().rect_filled(
            egui::Rect::from_min_max(padding_box.min + egui::vec2(3.0, 0.0), padding_box.max),
            inner_radius,
            CONTAINER,
        );
        // The 1 px border is centred on the border-box edge.
        ui.painter().rect_stroke(
            rect.shrink(0.5),
            egui::CornerRadius::same(ROW_RADIUS as u8),
            egui::Stroke::new(1.0_f32, select_border()),
            egui::StrokeKind::Middle,
        );
    }

    // ── content ────────────────────────────────────────────────────────────
    let eye_color = if !visible {
        LAYER_DIM
    } else if selected || multi {
        PRIMARY
    } else {
        MUTED
    };
    draw_eye(ui.painter(), eye.min, eye_color, visible);
    if let Some(chevron) = chevron {
        draw_chevron(
            ui.painter(),
            chevron,
            collapsed,
            if !visible { LAYER_DIM } else { MUTED },
        );
    }

    draw_layer_thumbnail(ui.painter(), thumb, glyph, shown);
    if let Some(color) = id_color {
        // Material ID badge: the colour this layer contributes to id.png.
        let rgb = egui::Color32::from_rgb(
            (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        );
        let center = thumb.right_bottom() + egui::vec2(-4.5, -4.5);
        ui.painter().circle_filled(center, 4.5, SURFACE_LOWEST);
        ui.painter().circle_filled(center, 3.2, rgb);
    }

    let name_color = if shown { TEXT } else { LAYER_DIM };
    let kind_color = if !shown {
        LAYER_DIM
    } else if selected || multi {
        PRIMARY
    } else {
        FAINT
    };
    let text_clip = egui::Rect::from_min_max(
        egui::pos2(text_x, rect.top()),
        egui::pos2(text_right, rect.bottom()),
    );
    let text_painter = ui.painter().with_clip_rect(text_clip);
    draw_truncated_text(
        &text_painter,
        egui::pos2(text_x, cy - 9.8),
        name,
        bold(14.0),
        name_color,
        text_right - text_x,
    );
    draw_truncated_text(
        &text_painter,
        egui::pos2(text_x, cy + 10.9),
        type_label,
        mono(11.0),
        kind_color,
        text_right - text_x,
    );
    // Long names ellipsize; hovering the text exposes the full one. An
    // off-tile layer also explains why it is dimmed.
    ui.interact(
        text_clip,
        ui.make_persistent_id(("layer_name", id)),
        egui::Sense::hover(),
    )
    .on_hover_text(match layer.tile {
        Some(tile) if off_tile => format!("{name}\nTile {tile} · not shown on the active tile"),
        _ => name.to_string(),
    });

    // Blend tag: mono 11, BG fill, 1 px LINE, r6, pad 8×5.
    match &tag_galley {
        Some(galley) => {
            ui.painter()
                .rect_filled(tag, egui::CornerRadius::same(TAG_RADIUS), SURFACE_LOWEST);
            ui.painter().rect_stroke(
                tag.shrink(0.5),
                egui::CornerRadius::same(TAG_RADIUS),
                egui::Stroke::new(1.0_f32, OUTLINE_VARIANT),
                egui::StrokeKind::Middle,
            );
            ui.painter().galley(
                tag.min + egui::vec2(ROW_BORDER + TAG_PAD_X, ROW_BORDER + TAG_PAD_Y),
                galley.clone(),
                if visible { MUTED } else { LAYER_DIM },
            );
        }
        None => match vector_op {
            Some(op) => draw_icon(
                ui.painter(),
                egui::Rect::from_center_size(tag.center(), egui::Vec2::splat(14.0)),
                vector_op_icon(op),
                if visible { MUTED } else { LAYER_DIM },
            ),
            None => draw_blend_badge(
                ui.painter(),
                egui::Rect::from_center_size(tag.center(), egui::Vec2::splat(12.0)),
                blend,
                if visible { MUTED } else { LAYER_DIM },
            ),
        },
    }

    // Mask affordance sits between the name and the tag; the lane is reserved
    // above, so only its visibility changes.
    let mut mask_events = MaskRowEvents::default();
    let show_mask =
        hovered || has_paint_mask || mask_info.smart_count > 0 || is_mask_source || editing_mask;
    if show_mask {
        mask_control(
            ui,
            mask,
            ui.make_persistent_id(("layer_mask", id)),
            has_clip_mask,
            has_paint_mask,
            paint_mask,
            editing_mask,
            visible,
            mask_info,
            is_mask_source,
            &mut mask_events,
            state,
            &row.path,
        );
    }

    // Lock toggle: always visible while the layer is locked, otherwise on
    // hover. The lane is reserved above, so revealing it never moves the name.
    let mut lock_toggled = false;
    if hovered || locked {
        draw_icon(
            ui.painter(),
            egui::Rect::from_center_size(lock.center(), egui::Vec2::splat(15.0)),
            if locked { Icon::Lock } else { Icon::Unlock },
            if locked { PRIMARY } else { MUTED },
        );
        let lock_response = ui.interact(
            lock,
            ui.make_persistent_id(("layer_lock", id)),
            egui::Sense::click(),
        );
        if lock_response.clicked() {
            lock_toggled = true;
        }
        let lock_response = lock_response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(if locked {
                "Locked · click to unlock"
            } else {
                "Lock this layer · blocks moving, trimming, painting and deleting"
            });
        layer_row_context_menu(
            &lock_response,
            state,
            action,
            &row.path,
            is_group,
            locked,
            can_up,
            can_down,
            vector_op,
        );
    }

    // ── interactions ───────────────────────────────────────────────────────
    let mut select_row = false;
    let mut visibility_changed = false;
    let mut toggle_collapsed = false;
    let mut blend_change = None;
    let mut boolean_change = None;

    let eye_response = ui.interact(
        eye,
        ui.make_persistent_id(("layer_eye", id)),
        egui::Sense::click(),
    );
    if eye_response.clicked() {
        visibility_changed = true;
    }
    let eye_response = eye_response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if visible {
            "Visible · click to hide"
        } else {
            "Hidden · click to show"
        });
    layer_row_context_menu(
        &eye_response,
        state,
        action,
        &row.path,
        is_group,
        locked,
        can_up,
        can_down,
        vector_op,
    );

    if let Some(chevron) = chevron {
        let chevron = ui.interact(
            chevron,
            ui.make_persistent_id(("layer_chevron", id)),
            egui::Sense::click(),
        );
        if chevron.clicked() {
            toggle_collapsed = true;
        }
        layer_row_context_menu(
            &chevron, state, action, &row.path, is_group, locked, can_up, can_down, vector_op,
        );
    }

    // Dragging the row (or its thumbnail) reorders the stack; a plain click
    // still selects. The payload is set on both responses so whichever widget
    // wins the drag hit-test can start the drag.
    let drag_payload = LayerDragPayload {
        id,
        copy: ui.input(|input| input.modifiers.alt),
    };
    response.dnd_set_drag_payload(drag_payload);
    if response.drag_started() {
        // Keep the current selection so dragging a source layer into the
        // Properties panel of another layer does not swap the target mid-drag.
        state.node_selection.clear();
        state.node_hover = None;
    }
    draw_layer_drag_ghost(ui, &response, name);

    let thumbnail = ui.interact(
        thumb,
        ui.make_persistent_id(("layer_thumb", id)),
        egui::Sense::click_and_drag(),
    );
    if thumbnail.clicked() {
        select_row = true;
    }
    thumbnail.dnd_set_drag_payload(drag_payload);
    if thumbnail.drag_started() {
        state.node_selection.clear();
        state.node_hover = None;
    }
    draw_layer_drag_ghost(ui, &thumbnail, name);
    layer_row_context_menu(
        &thumbnail, state, action, &row.path, is_group, locked, can_up, can_down, vector_op,
    );

    let tag_response = ui.interact(
        tag,
        ui.make_persistent_id(("layer_tag", id)),
        egui::Sense::click(),
    );
    if tag_response.hovered() {
        ui.painter().rect_stroke(
            tag.shrink(0.5),
            egui::CornerRadius::same(TAG_RADIUS),
            egui::Stroke::new(1.0_f32, MUTED),
            egui::StrokeKind::Middle,
        );
    }
    if let Some(op) = vector_op {
        if let Some(chosen) = vector_op_menu(ui, id, op, &tag_response) {
            boolean_change = Some(chosen);
        }
    } else if let Some(mode) = blend_tag_menu(ui, id, blend, &tag_response) {
        blend_change = Some(mode);
    }
    let tag_response = tag_response.on_hover_text(match vector_op {
        Some(op) => format!("Boolean: {}", op.label()),
        None => format!("Blend: {} — {}", blend_label(blend), blend_hint(blend)),
    });
    layer_row_context_menu(
        &tag_response,
        state,
        action,
        &row.path,
        is_group,
        locked,
        can_up,
        can_down,
        vector_op,
    );

    if response.clicked() {
        select_row = true;
    }
    layer_row_context_menu(
        &response, state, action, &row.path, is_group, locked, can_up, can_down, vector_op,
    );

    // Rows are drop targets too: dropping near the top edge inserts the
    // dragged layer above this row, near the bottom edge below it, and a
    // group's middle band files it into the group. Dropping on a row is far
    // easier to hit than the thin strips between rows.
    let row_parent = path_parent_id(&doc.0, &row.path);
    let row_index = row.path.last().copied().unwrap_or(0);
    // Strict containment: `Response::dnd_*` also accepts pointers within
    // egui's interact radius, which would let a row swallow a release aimed at
    // the strip just outside it.
    let pointer_inside = ui
        .ctx()
        .pointer_hover_pos()
        .is_some_and(|pos| rect.contains(pos));
    let drop_target = response
        .dnd_hover_payload::<LayerDragPayload>()
        .filter(|_| pointer_inside)
        .and_then(|payload| {
            let target = row_drop_target(ui, rect, row_parent, row_index, id, is_group);
            let target_parent = match target {
                LayerDropTarget::Before { parent, .. } => parent,
                LayerDropTarget::IntoGroup(group) => Some(group),
            };
            can_drop_layer(&doc.0, payload.id, target_parent).then_some(target)
        });
    if let Some(target) = drop_target {
        match target {
            LayerDropTarget::Before { index, .. } if index == row_index => {
                draw_drop_line(ui, rect, rect.bottom() - 1.0, row.depth);
            }
            LayerDropTarget::Before { .. } => {
                draw_drop_line(ui, rect, rect.top() + 1.0, row.depth);
            }
            LayerDropTarget::IntoGroup(_) => {
                ui.painter().rect_stroke(
                    rect.shrink(0.5),
                    egui::CornerRadius::same(ROW_RADIUS as u8),
                    egui::Stroke::new(1.5_f32, ACCENT),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if let Some(payload) = response.dnd_release_payload::<LayerDragPayload>() {
            *drop = Some(LayerDrop {
                payload: LayerDragPayload {
                    copy: payload.copy || ui.input(|input| input.modifiers.alt),
                    ..*payload
                },
                target,
            });
        }
    }

    // ── commit ─────────────────────────────────────────────────────────────
    if select_row {
        // Clicking the layer thumbnail or name returns to the normal map view.
        if state.mask_edit_layer.is_some() {
            end_mask_edit(state);
            state.show_height = false;
        }
        if state.selected.as_deref() != Some(row.path.as_slice()) {
            state.node_selection.clear();
            state.node_hover = None;
        }
        super::rows::select_row(state, &row.path, selection_mode);
    }
    if toggle_collapsed && !state.collapsed.remove(&id) {
        state.collapsed.insert(id);
    }
    if mask_events.clip_toggled {
        *action = Some(LayerAction::ToggleMask);
    }
    if mask_events.edit {
        *action = Some(LayerAction::EditMask);
    }
    let row_edits = mask_events.clear_painted
        || mask_events.toggle_enabled
        || mask_events.toggle_invert
        || mask_events.add_smart.is_some()
        || blend_change.is_some()
        || boolean_change.is_some()
        || visibility_changed;
    if row_edits {
        state.ensure_undo_gesture(&doc.0);
    }
    if mask_events.clear_painted {
        if state.mask_edit_layer == Some(id) {
            end_mask_edit(state);
        }
        if let Some(layer) = doc.0.find_layer_mut(id) {
            layer.mask = None;
            layer.touch_mask();
        }
        state.mark_dirty();
    }
    if mask_events.toggle_enabled || mask_events.toggle_invert || mask_events.add_smart.is_some() {
        if let Some(layer) = doc.0.find_layer_mut(id) {
            let settings = &mut layer.mask_settings;
            if mask_events.toggle_enabled {
                settings.enabled = !settings.enabled;
                state.status = if settings.enabled {
                    format!("Mask enabled · {}", layer.name)
                } else {
                    format!("Mask disabled · {}", layer.name)
                };
            }
            if mask_events.toggle_invert {
                settings.invert = !settings.invert;
            }
            if let Some(preset) = mask_events.add_smart {
                settings.generators.push(preset_generator(preset));
                settings.enabled = true;
            }
            layer.touch_mask();
        }
        state.mark_dirty();
    }
    if let Some(mode) = blend_change {
        if let Some(layer) = doc.0.find_layer_mut(id) {
            layer.blend = mode;
            layer.touch();
        }
        state.mark_dirty();
    }
    if let Some(op) = boolean_change {
        if let Some(layer) = doc.0.find_layer_mut(id) {
            layer.vector_op = Some(op);
            layer.touch();
        }
        state.status = format!("Boolean · {}", op.label());
        state.mark_dirty();
    }
    if visibility_changed {
        if let Some(layer) = doc.0.find_layer_mut(id) {
            layer.visible = !layer.visible;
            layer.touch();
        }
        state.mark_dirty();
    }
    if lock_toggled {
        state.ensure_undo_gesture(&doc.0);
        if let Some(layer) = doc.0.find_layer_mut(id) {
            layer.locked = !layer.locked;
            state.status = if layer.locked {
                format!("Locked {}", layer.name)
            } else {
                format!("Unlocked {}", layer.name)
            };
        }
        state.mark_dirty();
    }
}

/// Where a drop on `rect` should insert the dragged layer.
///
/// The top half inserts above the row and the bottom half below it. A group
/// instead reserves its middle band for filing the layer into the group, so
/// only its edges reorder siblings. The row's own `index` is in the parent's
/// bottom-to-top order: inserting above the row means `index + 1`, below it
/// means `index`.
fn row_drop_target(
    ui: &egui::Ui,
    rect: egui::Rect,
    parent: Option<LayerId>,
    index: usize,
    id: LayerId,
    is_group: bool,
) -> LayerDropTarget {
    let above = LayerDropTarget::Before {
        parent,
        index: index + 1,
    };
    let below = LayerDropTarget::Before { parent, index };
    let Some(pointer_y) = ui.ctx().pointer_hover_pos().map(|pos| pos.y) else {
        return above;
    };
    if !is_group {
        return if pointer_y < rect.center().y {
            above
        } else {
            below
        };
    }
    // Leave the group's own edges free for plain sibling reordering.
    let edge = (rect.height() * 0.25).min(18.0);
    if pointer_y < rect.top() + edge {
        above
    } else if pointer_y > rect.bottom() - edge {
        below
    } else {
        LayerDropTarget::IntoGroup(id)
    }
}

/// The x offset where an insertion line starts for a row at `depth`.
pub(crate) fn drop_line_indent(depth: usize) -> f32 {
    ROW_BORDER + ROW_PAD + EYE_SIZE + ROW_GAP + (depth as f32 * INDENT_PER_DEPTH).min(56.0)
}

/// The 2 px insertion line shared by the row edges and the drop strips.
///
/// The indent follows the row's depth so the line starts at the layer name's
/// left edge, matching where the row itself begins at that depth.
pub(crate) fn draw_drop_line(ui: &egui::Ui, rect: egui::Rect, y: f32, depth: usize) {
    let indent = drop_line_indent(depth);
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + indent, y),
            egui::pos2(rect.right() - 6.0, y),
        ],
        egui::Stroke::new(2.0_f32, ACCENT),
    );
    ui.painter()
        .circle_filled(egui::pos2(rect.left() + indent, y), 3.0, ACCENT);
}

/// Single-line text truncated to `width`, centred on `center_y`.
fn draw_truncated_text(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    width: f32,
) {
    if width <= 0.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(anchor.x, anchor.y - galley.size().y * 0.5),
        galley,
        color,
    );
}

/// Eye icon from the reference SVG (`M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12z`
/// plus `circle(12,12,3)`), expanded to four absolute cubics and flattened into
/// one closed polyline so the joins do not show. A slash marks a hidden layer.
fn draw_eye(painter: &egui::Painter, origin: egui::Pos2, color: egui::Color32, visible: bool) {
    let k = EYE_SIZE / 24.0;
    let v = |x: f32, y: f32| egui::vec2(x, y);
    let segments = [
        [v(2.0, 12.0), v(2.0, 12.0), v(6.0, 5.0), v(12.0, 5.0)],
        [v(12.0, 5.0), v(18.0, 5.0), v(22.0, 12.0), v(22.0, 12.0)],
        [v(22.0, 12.0), v(22.0, 12.0), v(18.0, 19.0), v(12.0, 19.0)],
        [v(12.0, 19.0), v(6.0, 19.0), v(2.0, 12.0), v(2.0, 12.0)],
    ];
    let mut points = Vec::new();
    for segment in segments {
        for step in 0..24 {
            let t = step as f32 / 24.0;
            let u = 1.0 - t;
            let point = segment[0] * (u * u * u)
                + segment[1] * (3.0 * u * u * t)
                + segment[2] * (3.0 * u * t * t)
                + segment[3] * (t * t * t);
            points.push(origin + point * k);
        }
    }
    let stroke = egui::Stroke::new(1.6 * k, color);
    painter.add(egui::Shape::closed_line(points, stroke));
    painter.circle_stroke(origin + egui::vec2(12.0, 12.0) * k, 3.0 * k, stroke);
    if !visible {
        painter.line_segment(
            [
                origin + egui::vec2(3.5, 3.5) * k,
                origin + egui::vec2(20.5, 20.5) * k,
            ],
            stroke,
        );
    }
}

fn draw_chevron(painter: &egui::Painter, rect: egui::Rect, collapsed: bool, color: egui::Color32) {
    let c = rect.center();
    let points = if collapsed {
        vec![
            c + egui::vec2(-1.8, -3.4),
            c + egui::vec2(2.2, 0.0),
            c + egui::vec2(-1.8, 3.4),
        ]
    } else {
        vec![
            c + egui::vec2(-3.4, -1.8),
            c + egui::vec2(0.0, 2.2),
            c + egui::vec2(3.4, -1.8),
        ]
    };
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.4_f32, color)));
}

/// Blend menu anchored to the tag, with the mode badge beside each label.
fn blend_tag_menu(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    blend: BlendMode,
    response: &egui::Response,
) -> Option<BlendMode> {
    let popup_id = ui.make_persistent_id(("blend_combo", id_salt));
    let mut chosen = None;
    egui::Popup::menu(response)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(172.0);
            ui.label(
                egui::RichText::new("Blend mode")
                    .size(TEXT_SMALL)
                    .color(MUTED),
            );
            for mode in BLEND_MODES {
                ui.horizontal(|ui| {
                    let (badge_rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    draw_blend_badge(
                        ui.painter(),
                        badge_rect,
                        mode,
                        if mode == blend { ACCENT } else { MUTED },
                    );
                    if ui
                        .selectable_label(mode == blend, blend_label(mode))
                        .on_hover_text(blend_hint(mode))
                        .clicked()
                    {
                        chosen = Some(mode);
                        ui.close();
                    }
                });
            }
        });
    chosen
}

/// Boolean menu anchored to the row tag: the live vector operations a compound
/// group can use. Non-boolean layers keep the blend menu instead.
fn vector_op_menu(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    current: VectorOp,
    response: &egui::Response,
) -> Option<VectorOp> {
    let popup_id = ui.make_persistent_id(("vector_op_menu", id_salt));
    let mut chosen = None;
    egui::Popup::menu(response)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(172.0);
            ui.label(
                egui::RichText::new("Boolean operation")
                    .size(TEXT_SMALL)
                    .color(MUTED),
            );
            for op in VectorOp::ALL {
                ui.horizontal(|ui| {
                    let (icon_rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    draw_icon(
                        ui.painter(),
                        icon_rect,
                        vector_op_icon(op),
                        if op == current { ACCENT } else { MUTED },
                    );
                    if ui.selectable_label(op == current, op.label()).clicked() {
                        chosen = Some(op);
                        ui.close();
                    }
                });
            }
        });
    chosen
}
#[allow(clippy::too_many_arguments)]
fn layer_row_context_menu(
    response: &egui::Response,
    state: &mut EditorState,
    action: &mut Option<LayerAction>,
    path: &[usize],
    is_group: bool,
    locked: bool,
    can_up: bool,
    can_down: bool,
    vector_op: Option<VectorOp>,
) {
    response.context_menu(|ui| {
        ensure_row_selected(state, path);
        layer_row_menu(
            ui, state, action, path, is_group, locked, can_up, can_down, vector_op,
        );
    });
}

/// Mask state shown on the row thumbnail.
#[derive(Clone, Copy)]
struct MaskRowInfo {
    enabled: bool,
    inverted: bool,
    smart_count: usize,
}

/// Mask actions requested from the row thumbnail and its menu.
#[derive(Default)]
struct MaskRowEvents {
    clip_toggled: bool,
    clear_painted: bool,
    edit: bool,
    toggle_enabled: bool,
    toggle_invert: bool,
    add_smart: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
fn mask_control(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    widget_id: egui::Id,
    clipped: bool,
    has_paint_mask: bool,
    paint_mask: Option<&Grid<f32>>,
    editing: bool,
    visible: bool,
    info: MaskRowInfo,
    is_mask_source: bool,
    events: &mut MaskRowEvents,
    state: &mut EditorState,
    path: &[usize],
) {
    let response = ui.interact(rect, widget_id, egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand(1.5),
            egui::CornerRadius::same(RADIUS_XS),
            HOVER_BG,
        );
    }
    let has_mask = has_paint_mask || info.smart_count > 0 || is_mask_source;
    draw_mask_thumbnail(
        ui.painter(),
        rect,
        clipped,
        paint_mask,
        editing,
        visible,
        info,
        is_mask_source,
    );
    let mut tip = if editing {
        "Editing this mask. Click the layer thumbnail to return.".to_string()
    } else if has_paint_mask {
        "Painted mask. Click to edit.".to_string()
    } else if clipped {
        "Clipping mask. Click to paint an extra mask.".to_string()
    } else if info.smart_count > 0 {
        "Smart mask. Click to paint on top of it.".to_string()
    } else {
        "No mask. Click to paint one; right-click for smart masks.".to_string()
    };
    if info.smart_count > 0 {
        tip.push_str(&format!(
            "\n{} smart mask(s) from the layers below.",
            info.smart_count
        ));
    }
    if has_mask {
        tip.push_str(if info.enabled {
            "\nShift-click to disable."
        } else {
            "\nDisabled. Shift-click to enable."
        });
    }
    let response = response.on_hover_text(tip);
    if response.clicked() {
        ensure_row_selected(state, path);
        if ui.input(|input| input.modifiers.shift) && has_mask {
            events.toggle_enabled = true;
        } else {
            events.edit = true;
        }
    }
    let response = response.context_menu(|ui| {
        ensure_row_selected(state, path);
        if ui
            .button(if has_paint_mask {
                "Edit painted mask..."
            } else {
                "Paint mask..."
            })
            .clicked()
        {
            events.edit = true;
            ui.close();
        }
        if has_mask
            && ui
                .button(if info.enabled {
                    "Disable mask"
                } else {
                    "Enable mask"
                })
                .clicked()
        {
            events.toggle_enabled = true;
            ui.close();
        }
        if has_paint_mask {
            if ui
                .button(if info.inverted {
                    "Un-invert painted mask"
                } else {
                    "Invert painted mask"
                })
                .clicked()
            {
                events.toggle_invert = true;
                ui.close();
            }
            if ui.button("Delete painted mask").clicked() {
                events.clear_painted = true;
                ui.close();
            }
        }
        ui.separator();
        ui.menu_button("Add smart mask", |ui| {
            for (index, (label, tip)) in SMART_PRESETS.iter().enumerate() {
                if ui.button(*label).on_hover_text(*tip).clicked() {
                    events.add_smart = Some(index);
                    ui.close();
                }
            }
        });
        if ui
            .button(if clipped {
                "Remove clipping mask"
            } else {
                "Clip to layer below"
            })
            .clicked()
        {
            events.clip_toggled = true;
            ui.close();
        }
    });
    let _ = response;
}

#[allow(clippy::too_many_arguments)]
fn draw_mask_thumbnail(
    painter: &egui::Painter,
    rect: egui::Rect,
    clipped: bool,
    mask: Option<&Grid<f32>>,
    editing: bool,
    visible: bool,
    info: MaskRowInfo,
    is_mask_source: bool,
) {
    let has_any = mask.is_some() || clipped || info.smart_count > 0 || is_mask_source;
    if !has_any {
        // Empty slot: a dashed placeholder, no filled box and no outline.
        let dashed = egui::Stroke::new(1.2_f32, MUTED.gamma_multiply(0.55));
        let r = rect.shrink(3.0);
        let points = vec![
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
            r.left_top(),
        ];
        painter.add(egui::Shape::dashed_line(&points, dashed, 3.0, 2.5));
        return;
    }
    painter.rect_filled(rect, egui::CornerRadius::same(RADIUS_SM), CONTAINER_HIGHEST);
    let inner = rect.shrink(2.0);
    let cell = inner.width() / 2.0;
    for row in 0..2 {
        for column in 0..2 {
            if (row + column) % 2 == 0 {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        inner.min + egui::vec2(column as f32 * cell, row as f32 * cell),
                        egui::vec2(cell, cell),
                    ),
                    0.0,
                    CONTAINER_HIGH,
                );
            }
        }
    }

    if let Some(grid) = mask {
        // Coarse but real preview of the painted mask.
        let cells = 8usize;
        let cell = inner.width() / cells as f32;
        for y in 0..cells {
            for x in 0..cells {
                let sx = ((x as f32 + 0.5) / cells as f32 * grid.w as f32)
                    .floor()
                    .clamp(0.0, (grid.w - 1) as f32) as i32;
                let sy = ((y as f32 + 0.5) / cells as f32 * grid.h as f32)
                    .floor()
                    .clamp(0.0, (grid.h - 1) as f32) as i32;
                let mut value = grid.sample(sx, sy, Wrap::Clamp).clamp(0.0, 1.0);
                if info.inverted {
                    value = 1.0 - value;
                }
                let shade = if visible {
                    (value * 235.0 + 20.0) as u8
                } else {
                    (value * 120.0 + 18.0) as u8
                };
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        inner.min + egui::vec2(x as f32 * cell, y as f32 * cell),
                        egui::vec2(cell, cell),
                    ),
                    0.0,
                    egui::Color32::from_gray(shade),
                );
            }
        }
    } else if clipped {
        let white = if visible {
            egui::Color32::from_gray(214)
        } else {
            egui::Color32::from_gray(120)
        };
        painter.rect_filled(inner, 1.0, white);
        painter.circle_filled(
            inner.center(),
            inner.width() * 0.24,
            egui::Color32::from_gray(28),
        );
    } else if info.smart_count > 0 {
        // Smart-only mask: a soft ramp hints that it is generated.
        let steps = 8;
        let cell = inner.width() / steps as f32;
        for i in 0..steps {
            let t = (i as f32 + 0.5) / steps as f32;
            let shade = if visible {
                40.0 + t * 190.0
            } else {
                30.0 + t * 90.0
            };
            painter.rect_filled(
                egui::Rect::from_min_size(
                    inner.min + egui::vec2(i as f32 * cell, 0.0),
                    egui::vec2(cell + 0.5, inner.height()),
                ),
                0.0,
                egui::Color32::from_gray(shade as u8),
            );
        }
    } else {
        draw_icon(
            painter,
            egui::Rect::from_center_size(inner.center(), egui::Vec2::splat(13.0)),
            Icon::Mask,
            MUTED.gamma_multiply(0.5),
        );
    }

    if info.smart_count > 0 {
        // Accent badge: the mask also reads the surface below.
        let badge = egui::pos2(rect.right() - 4.5, rect.top() + 4.5);
        painter.circle_filled(badge, 3.5, SURFACE_LOWEST);
        painter.circle_filled(badge, 2.5, ACCENT);
    }
    let has_mask = mask.is_some() || info.smart_count > 0;
    if has_mask && !info.enabled {
        // Disabled: dim and cross out, as in other editors.
        painter.rect_filled(inner, 1.0, egui::Color32::from_black_alpha(120));
        let stroke = egui::Stroke::new(2.0_f32, ERROR);
        painter.line_segment([inner.left_top(), inner.right_bottom()], stroke);
        painter.line_segment([inner.right_top(), inner.left_bottom()], stroke);
    }

    if is_mask_source {
        // Bound as another layer's mask: it only contributes coverage.
        let badge = rect.left_bottom() + egui::vec2(3.0, -3.0);
        painter.circle_filled(badge, 3.2, CONTAINER_LOW);
        painter.circle_filled(badge, 2.4, PRIMARY);
    }
    // No resting outline. Editing is the one active state that keeps a ring.
    if editing {
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(RADIUS_SM),
            egui::Stroke::new(2.0_f32, PRIMARY),
            egui::StrokeKind::Inside,
        );
    }
}

/// Full-width-looking blend selector used by the panel header.
pub(crate) fn blend_mode_combo(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    blend: BlendMode,
    width: f32,
    height: f32,
    color: egui::Color32,
    framed: bool,
) -> (Option<BlendMode>, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let hovered = response.hovered();
    // A field: a shade below the row surface with a hairline, matching the
    // reference's bordered dropdowns. Rows and the header share the look; the
    // header adds a chevron to say it opens a menu.
    let base = state_layer(CONTAINER, SURFACE_LOWEST, 0.45);
    let fill = if hovered { hover_layer(base) } else { base };
    let radius = egui::CornerRadius::same(RADIUS_SM);
    ui.painter().rect_filled(rect, radius, fill);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0_f32, CONTROL_BORDER),
        egui::StrokeKind::Inside,
    );

    // Wide chips spell the operator out in mono (`= Derivative`), exactly like
    // the reference tag, instead of reserving 12 px for a drawn badge.
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 8.0, rect.top()),
        egui::pos2(
            rect.right() - if framed { 16.0 } else { 8.0 },
            rect.bottom(),
        ),
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        format!("{} {}", blend_operator(blend), blend_short_label(blend)),
        mono(11.0),
        color,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width().max(8.0));
    let galley = ui.painter().layout_job(job);
    ui.painter().with_clip_rect(text_rect).galley(
        egui::pos2(text_rect.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );

    let c = egui::pos2(rect.right() - 9.0, rect.center().y);
    if framed {
        let chevron = if hovered { ACCENT } else { MUTED };
        ui.painter().add(egui::Shape::line(
            vec![
                egui::pos2(c.x - 3.0, c.y - 2.0),
                egui::pos2(c.x, c.y + 1.5),
                egui::pos2(c.x + 3.0, c.y - 2.0),
            ],
            egui::Stroke::new(1.3_f32, chevron),
        ));
    }

    let response = response.on_hover_text(format!(
        "Blend: {} — {}",
        blend_label(blend),
        blend_hint(blend)
    ));
    let chosen = blend_tag_menu(ui, id_salt, blend, &response);
    (chosen, response)
}

/// Field-style boolean selector used by the Layers panel header when the
/// selected layer is a live boolean group.
pub(crate) fn vector_op_combo(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    op: VectorOp,
    width: f32,
    height: f32,
    color: egui::Color32,
    framed: bool,
) -> (Option<VectorOp>, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let hovered = response.hovered();
    let base = state_layer(CONTAINER, SURFACE_LOWEST, 0.45);
    let fill = if hovered { hover_layer(base) } else { base };
    let radius = egui::CornerRadius::same(RADIUS_SM);
    ui.painter().rect_filled(rect, radius, fill);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0_f32, CONTROL_BORDER),
        egui::StrokeKind::Inside,
    );

    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 8.0, rect.top()),
        egui::pos2(
            rect.right() - if framed { 16.0 } else { 8.0 },
            rect.bottom(),
        ),
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        format!("{} {}", vector_op_operator(op), op.label()),
        mono(11.0),
        color,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width().max(8.0));
    let galley = ui.painter().layout_job(job);
    ui.painter().with_clip_rect(text_rect).galley(
        egui::pos2(text_rect.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );

    if framed {
        let c = egui::pos2(rect.right() - 9.0, rect.center().y);
        ui.painter().add(egui::Shape::line(
            vec![
                egui::pos2(c.x - 3.0, c.y - 2.0),
                egui::pos2(c.x, c.y + 1.5),
                egui::pos2(c.x + 3.0, c.y - 2.0),
            ],
            egui::Stroke::new(1.3_f32, if hovered { ACCENT } else { MUTED }),
        ));
    }

    let response = response.on_hover_text(format!("Boolean: {}", op.label()));
    let chosen = vector_op_menu(ui, id_salt, op, &response);
    (chosen, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: egui::Pos2 = egui::pos2(20.0, 20.0);
    const WIDTH: f32 = 400.0;

    fn press(pos: egui::Pos2) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn release(pos: egui::Pos2) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn row_rect(index: usize) -> egui::Rect {
        egui::Rect::from_min_size(
            ORIGIN + egui::vec2(0.0, index as f32 * ROW_H),
            egui::vec2(WIDTH, ROW_H),
        )
    }

    fn thumb_center(index: usize) -> egui::Pos2 {
        row_rect(index).min
            + egui::vec2(
                ROW_BORDER + ROW_PAD + EYE_SIZE + ROW_GAP + THUMB_SIZE * 0.5,
                ROW_BORDER + ROW_PAD + THUMB_SIZE * 0.5,
            )
    }

    /// The name/type text, past the thumbnail and all the way left of the tag.
    fn name_center(index: usize) -> egui::Pos2 {
        row_rect(index).min
            + egui::vec2(
                ROW_BORDER + ROW_PAD + EYE_SIZE + ROW_GAP + THUMB_SIZE + 70.0,
                ROW_H * 0.5,
            )
    }

    fn strip_center(row_count: usize) -> egui::Pos2 {
        ORIGIN + egui::vec2(WIDTH * 0.5, row_count as f32 * ROW_H + 3.0)
    }

    fn eye_center(index: usize) -> egui::Pos2 {
        row_rect(index).min + egui::vec2(ROW_BORDER + ROW_PAD + EYE_SIZE * 0.5, ROW_H * 0.5)
    }

    /// Draws the rows top-to-bottom plus the root drop strip under them at a
    /// known rect, so a simulated pointer can be aimed at any part of a row.
    struct Harness {
        ctx: egui::Context,
        doc: DocumentRes,
        state: EditorState,
        rows: Vec<LayerRow>,
        drop: Option<LayerDrop>,
    }

    impl Harness {
        /// `layers` is bottom-to-top, like `Document::layers`; the rows are
        /// built in display order.
        fn new(layers: Vec<Layer>) -> Self {
            let ctx = egui::Context::default();
            ctx.options_mut(|options| {
                options.max_passes = std::num::NonZeroUsize::new(1).unwrap();
            });
            let mut doc = DocumentRes::new(Document::new(glam::UVec2::splat(64)));
            let rows = (0..layers.len())
                .rev()
                .map(|index| LayerRow {
                    path: vec![index],
                    depth: 0,
                    name: layers[index].name.as_str().into(),
                    type_label: "Layer".into(),
                })
                .collect();
            doc.0.layers = layers;
            Self {
                ctx,
                doc,
                state: EditorState::default(),
                rows,
                drop: None,
            }
        }

        fn flat(names: &[&str]) -> Self {
            Self::new(names.iter().map(|name| Layer::flat(*name)).collect())
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(420.0, 400.0),
            ));
            raw.events = events;
            let ctx = self.ctx.clone();
            let doc = &mut self.doc;
            let state = &mut self.state;
            let rows = &self.rows;
            let drop = &mut self.drop;
            let output = ctx.run_ui(raw, |ui| {
                let rect = egui::Rect::from_min_size(ORIGIN, egui::vec2(WIDTH, 400.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                child.spacing_mut().item_spacing.y = 0.0;
                let mut action = None;
                for row in rows {
                    layer_row(
                        &mut child,
                        row,
                        state,
                        doc,
                        &mut action,
                        RowSelection::Replace,
                        drop,
                    );
                }
                layer_drop_strip(&mut child, doc, None, 0, 0, 6.0, drop);
            });
            output.drop_without_applying_deltas();
        }

        /// Hover, press, drag `from` → `to`, and release.
        fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(from)]);
            self.frame(vec![press(from)]);
            self.frame(vec![egui::Event::PointerMoved(to)]);
            self.frame(vec![release(to)]);
        }

        /// Hover, press and release without moving: a click.
        fn click(&mut self, at: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(at)]);
            self.frame(vec![press(at)]);
            self.frame(vec![release(at)]);
        }

        fn apply(&mut self) -> bool {
            let Some(drop) = self.drop.take() else {
                return false;
            };
            apply_layer_drop(&mut self.state, &mut self.doc, drop)
        }

        fn names(&self) -> Vec<&str> {
            self.doc
                .0
                .layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect()
        }
    }

    #[test]
    fn dragging_the_thumbnail_reorders_the_stack() {
        let mut harness = Harness::flat(&["bottom", "top"]);
        harness.drag(thumb_center(0), strip_center(2));
        assert!(harness.apply(), "thumbnail drag must move the layer");
        assert_eq!(harness.names(), vec!["top", "bottom"]);
    }

    #[test]
    fn dragging_the_row_body_reorders_the_stack() {
        let mut harness = Harness::flat(&["bottom", "top"]);
        harness.drag(name_center(0), strip_center(2));
        assert!(
            harness.apply(),
            "dragging the row body must move the layer, not just the 46 px thumbnail"
        );
        assert_eq!(harness.names(), vec!["top", "bottom"]);
    }

    #[test]
    fn dropping_on_a_rows_bottom_half_moves_the_layer_below_it() {
        let mut harness = Harness::flat(&["bottom", "middle", "top"]);
        let below_middle = row_rect(1).center().y + ROW_H * 0.25;
        harness.drag(name_center(0), egui::pos2(WIDTH * 0.5, below_middle));
        assert!(harness.apply());
        // Displayed top-to-bottom: middle, top, bottom.
        assert_eq!(harness.names(), vec!["bottom", "top", "middle"]);
    }

    #[test]
    fn dropping_on_a_rows_top_half_moves_the_layer_above_it() {
        let mut harness = Harness::flat(&["bottom", "middle", "top"]);
        let above_middle = row_rect(1).top() + 4.0;
        // The bottom row is dragged above the middle one.
        harness.drag(name_center(2), egui::pos2(WIDTH * 0.5, above_middle));
        assert!(harness.apply());
        // Displayed top-to-bottom: top, bottom, middle.
        assert_eq!(harness.names(), vec!["middle", "bottom", "top"]);
    }

    #[test]
    fn dropping_on_a_group_row_files_the_layer_into_it() {
        let group = Layer::group("group", vec![Layer::flat("inside")]);
        let mut harness = Harness::new(vec![group, Layer::flat("top")]);
        let middle = row_rect(1).center().y;
        harness.drag(name_center(0), egui::pos2(WIDTH * 0.5, middle));
        assert!(harness.apply());
        assert_eq!(harness.names(), vec!["group"]);
        let children = harness.doc.0.layers[0].children().unwrap();
        let names: Vec<&str> = children.iter().map(|layer| layer.name.as_str()).collect();
        assert_eq!(names, vec!["inside", "top"]);
    }

    #[test]
    fn a_group_rows_edges_reorder_siblings_instead_of_filing_in() {
        let group = Layer::group("group", vec![Layer::flat("inside")]);
        let mut harness = Harness::new(vec![group, Layer::flat("middle"), Layer::flat("top")]);
        let group_top_edge = row_rect(2).top() + 4.0;
        harness.drag(name_center(0), egui::pos2(WIDTH * 0.5, group_top_edge));
        assert!(harness.apply());
        // Displayed top-to-bottom: top, middle, group — nothing filed into it.
        assert_eq!(harness.names(), vec!["group", "top", "middle"]);
        assert_eq!(harness.doc.0.layers[0].children().unwrap().len(), 1);
    }

    #[test]
    fn clicking_the_row_body_still_selects_it() {
        let mut harness = Harness::flat(&["bottom", "top"]);
        harness.click(name_center(0));
        assert_eq!(harness.state.selected, Some(vec![1]));
        assert!(harness.drop.is_none(), "a click is not a drop");
    }

    #[test]
    fn clicking_the_eye_still_toggles_visibility_without_selecting() {
        let mut harness = Harness::flat(&["bottom", "top"]);
        harness.click(eye_center(0));
        assert!(
            !harness.doc.0.layers[1].visible,
            "the eye must hide the layer"
        );
        assert_eq!(
            harness.state.selected, None,
            "the eye must keep its click instead of selecting the row"
        );
    }
}
