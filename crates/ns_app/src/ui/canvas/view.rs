//! 2D canvas rendering and overlays.

use super::*;
pub(crate) fn canvas_view(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    normal_id: egui::TextureId,
    height_id: egui::TextureId,
    mesh_import: &MeshImport,
) {
    state.canvas_has_focus = false;
    let mask_active = mask_edit_active(state, doc);
    // Defer full-resolution mask preview rebuilds while a pointer drag is in
    // progress. Sliders and shape drags bump the layer revision every frame;
    // recomputing and re-uploading the whole mask at that rate is what made
    // the shell stutter. The preview catches up once the pointer is released.
    let pointer_down = ui.input(|input| input.pointer.any_down());
    // Outside mask editing, the overlay shows the selected layer's effective
    // mask (painted adjustments plus clipping) so the mask can be inspected
    // while looking at the normals.
    if !mask_active && state.show_mask_overlay && !pointer_down {
        let selected = state.selected.as_deref().and_then(|path| {
            let layer = doc.0.layer_by_path(path)?;
            let clip_revision = layer
                .mask_layer
                .and_then(|id| doc.0.find_layer(id))
                .map(|source| source.content_revision())
                .unwrap_or(0);
            Some((layer.id, layer.revision, layer.mask_revision, clip_revision))
        });
        if let Some((id, revision, mask_revision, clip_revision)) = selected {
            let key: MaskOverlayKey = (
                id,
                revision,
                mask_revision,
                clip_revision,
                doc.0.size.x,
                doc.0.size.y,
            );
            if state.mask_overlay_key != Some(key) {
                state.mask_overlay_grid = doc.0.mask_preview(id);
                state.mask_overlay_key = Some(key);
                state.mask_overlay_tex_key = None;
                if state.mask_overlay_grid.is_none() {
                    state.status = "This layer has no mask to show".into();
                }
            }
        }
    }
    // Upload the overlay only when its source revisions changed; opacity is a
    // draw-time tint so dragging the slider never rebuilds the texture.
    if !mask_active
        && state.show_mask_overlay
        && !pointer_down
        && state.mask_overlay_tex_key != state.mask_overlay_key
    {
        if let Some(grid) = state.mask_overlay_grid.take() {
            let rgba = mask_overlay_rgba(&grid);
            let image =
                egui::ColorImage::from_rgba_unmultiplied([grid.w as usize, grid.h as usize], &rgba);
            match &mut state.mask_overlay_tex {
                Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                None => {
                    state.mask_overlay_tex = Some(ui.ctx().load_texture(
                        "n-shade_mask_overlay",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            state.mask_overlay_tex_key = state.mask_overlay_key;
            state.mask_overlay_grid = Some(grid);
        } else {
            // The new selection has no mask: drop the previous overlay so a
            // stale tint never lingers on the canvas.
            state.mask_overlay_tex = None;
            state.mask_overlay_tex_key = state.mask_overlay_key;
        }
    }
    let view_mask_overlay = if !mask_active && state.show_mask_overlay {
        state.mask_overlay_tex.as_ref().map(|texture| texture.id())
    } else {
        None
    };
    let mask_target = if mask_active {
        state.mask_edit_layer.and_then(|id| {
            doc.0.find_layer(id).and_then(|layer| {
                layer
                    .mask
                    .as_ref()
                    .map(|mask| (id, layer.mask_revision, mask))
            })
        })
    } else {
        None
    };
    // While a mask is open the canvas is laid out in document pixels (strokes
    // map document to mask pixels themselves), and the view mode picks
    // between the live normals, a red quick-mask tint, or the raw mask.
    let mut mask_overlay = None;
    let (texture_id, texture_size) = if let Some((id, revision, mask)) = mask_target {
        let view_mode = state.mask_view;
        let Some(mask_id) = mask_texture(ui, state, id, revision, mask, view_mode) else {
            return;
        };
        let doc_size = egui::vec2(doc.0.size.x as f32, doc.0.size.y as f32);
        match view_mode {
            MaskView::Mask => (mask_id, doc_size),
            MaskView::Overlay => {
                mask_overlay = Some(mask_id);
                (normal_id, doc_size)
            }
            MaskView::Result => (normal_id, doc_size),
        }
    } else {
        (
            if state.show_height {
                height_id
            } else {
                normal_id
            },
            egui::vec2(state.tex_dims[0] as f32, state.tex_dims[1] as f32),
        )
    };
    if texture_size.x <= 0.0 || texture_size.y <= 0.0 {
        return;
    }

    let (response, painter) =
        ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
    state.canvas_has_focus = response.has_focus();
    let rect = response.rect;
    painter.rect_filled(rect, RADIUS_XS as f32, SURFACE_LOWEST);
    draw_canvas_background(&painter, rect);

    let fit = (rect.width() / texture_size.x).min(rect.height() / texture_size.y) * 0.91;
    let mut image_rect =
        egui::Rect::from_center_size(rect.center() + state.pan, texture_size * fit * state.zoom);
    // A press on the document may start a document edit. Snapshot here, at the
    // edit site, so clicks on the surrounding stage never clone the document.
    if ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary))
        && ui.input(|input| {
            input
                .pointer
                .interact_pos()
                .is_some_and(|pos| image_rect.contains(pos))
        })
    {
        state.ensure_undo_gesture(&doc.0);
    }

    if response.hovered() {
        // Touchpad and touch gestures. Pinch and Ctrl/Cmd+wheel always zoom;
        // two-finger scrolling pans (or zooms, per Settings); Shift pans
        // sideways; a notched mouse wheel keeps zooming.
        let gesture = scroll_gesture(ui);
        gesture.remember_device(state);
        let alt = ui.input(|input| input.modifiers.alt);
        let shift = ui.input(|input| input.modifiers.shift);
        let anchor = gesture.zoom_center.or_else(|| response.hover_pos());
        if gesture.has_zoom() {
            if let Some(anchor) = anchor {
                zoom_canvas_at(state, rect.center(), anchor, gesture.zoom);
            }
        } else if gesture.has_scroll() {
            let horizontal_only = gesture.delta.x != 0.0 && gesture.delta.y == 0.0;
            let pan =
                !alt && (shift || horizontal_only || (gesture.touchpad && state.touchpad_pan));
            if pan {
                state.pan += gesture.delta;
            } else if let Some(anchor) = anchor {
                zoom_canvas_at(state, rect.center(), anchor, gesture.wheel_zoom());
            }
        }
        if gesture.touch_pan != egui::Vec2::ZERO {
            state.pan += gesture.touch_pan;
        }
        image_rect = egui::Rect::from_center_size(
            rect.center() + state.pan,
            texture_size * fit * state.zoom,
        );
    }

    let space = ui.input(|input| input.key_down(egui::Key::Space));
    // Space moves an uncommitted lasso region instead of panning the view. With
    // no region yet (or another tool) it keeps its pan meaning.
    let lasso_move = space
        && state.canvas_tool.is_lasso_tool()
        && state
            .lasso_draft
            .as_ref()
            .is_some_and(|draft| !draft.points.is_empty());
    let pan_mode = state.canvas_pan_tool || (space && !lasso_move);
    if pan_mode && response.dragged_by(egui::PointerButton::Primary) {
        state.pan += response.drag_delta();
        image_rect = egui::Rect::from_center_size(
            rect.center() + state.pan,
            texture_size * fit * state.zoom,
        );
    }

    let clipped = painter.with_clip_rect(rect);
    clipped.rect_filled(
        image_rect.expand(7.0),
        3.0,
        egui::Color32::from_black_alpha(85),
    );
    let full_uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    clipped.image(texture_id, image_rect, full_uv, egui::Color32::WHITE);
    let tint = mask_overlay_tint(state.mask_overlay_opacity);
    if let Some(overlay) = mask_overlay {
        clipped.image(overlay, image_rect, full_uv, tint);
    }
    if let Some(overlay) = view_mask_overlay {
        clipped.image(overlay, image_rect, full_uv, tint);
    }
    clipped.rect_stroke(
        image_rect,
        0.0,
        egui::Stroke::new(1.0_f32, OUTLINE_VARIANT),
        egui::StrokeKind::Outside,
    );
    draw_uv_overlay_2d(&clipped, image_rect, state, mesh_import, doc.0.active_tile);

    // M toggles the mask overlay, for quickly checking or hiding it.
    if !ui.ctx().text_edit_focused() && ui.input(|input| input.key_pressed(egui::Key::M)) {
        state.show_mask_overlay = !state.show_mask_overlay;
        state.status = if state.show_mask_overlay {
            "Mask overlay on".into()
        } else {
            "Mask overlay off".into()
        };
    }
    if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        state.canvas_drag = None;
        state.clear_snap_guides();
        state.vector_draft = None;
        state.lasso_draft = None;
        state.lasso_divide = false;
        state.node_selection.clear();
        state.node_hover = None;
        state.edge_selection = None;
        // Esc while placing the ruler puts the guide away as well.
        if state.canvas_tool == CanvasTool::Ruler {
            dismiss_ruler(state);
        }
        state.canvas_tool = CanvasTool::Select;
        state.canvas_pan_tool = false;
        if mask_active {
            end_mask_edit(state);
        }
    }

    // A click on the stage around the document clears the selection and never
    // reaches the active tool: the canvas is not a place to draw outside the
    // image.
    let clicked_background =
        !pan_mode && background_click(&response, image_rect, mask_active, state);

    if !pan_mode && !clicked_background {
        let view = CanvasView {
            image_rect,
            doc_size: Vec2::new(texture_size.x, texture_size.y),
        };
        // Ctrl/Cmd held during a drag temporarily disables smart-guide snapping.
        // Shift locks moves and duplicates to the nearest 45° ray and takes
        // priority over guides so the copy cannot drift off-axis.
        let snap_enabled = ui.input(|input| !(input.modifiers.ctrl || input.modifiers.command));
        let constrain = ui.input(|input| input.modifiers.shift);
        if state.canvas_tool == CanvasTool::Pen {
            pen_edit(ui, &response, state, doc, &view);
        } else if state.canvas_tool == CanvasTool::Polygon {
            polygon_edit(ui, &response, state, doc, &view);
        } else if state.canvas_tool == CanvasTool::Cable {
            cable_edit(ui, &response, state, doc, &view);
        } else if state.canvas_tool.is_lasso_tool() {
            lasso_edit(ui, &response, state, doc, &view, lasso_move);
        } else if state.canvas_tool == CanvasTool::Ruler {
            ruler_edit(ui, &response, state, &view);
        } else if state.canvas_tool.is_node_edit_tool() && !mask_active {
            node_edit_input(ui, &response, state, doc, &view);
        } else if state.canvas_tool == CanvasTool::Paint {
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
                    canvas_edit_start(state, doc, &view, origin, false);
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pointer) = response.interact_pointer_pos() {
                    canvas_edit_drag(state, doc, &view, pointer, snap_enabled);
                }
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                canvas_edit_stop(state, doc);
            }
            if response.clicked_by(egui::PointerButton::Primary) {
                if let Some(pointer) = response.interact_pointer_pos() {
                    paint_click(state, doc, view.screen_to_doc(pointer));
                }
            }
        } else {
            // Text is placed with a plain click, not a drag. The drag path
            // below still covers press-and-drag placement.
            if state.canvas_tool == CanvasTool::Text
                && response.clicked_by(egui::PointerButton::Primary)
            {
                if let Some(pointer) = response.interact_pointer_pos() {
                    canvas_edit_start(state, doc, &view, pointer, false);
                }
            }
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
                    // Alt turns a drag on a layer into a duplicate-and-drag.
                    let duplicate = ui.input(|input| input.modifiers.alt);
                    canvas_edit_start(state, doc, &view, origin, duplicate);
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pointer) = response.interact_pointer_pos() {
                    if state.canvas_drag.is_some() {
                        canvas_edit_drag_with_constraint(
                            state,
                            doc,
                            &view,
                            pointer,
                            snap_enabled,
                            constrain,
                        );
                    } else {
                        state.pan += response.drag_delta();
                    }
                }
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                canvas_edit_stop(state, doc);
            }
            if !mask_active && response.double_clicked() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let p = view.screen_to_doc(pointer);
                    if let Some(path) = vector_shape_at(&doc.0, state, p) {
                        state.selected = Some(path);
                        state.multi_selected.clear();
                        state.vector_tool = CanvasTool::Node;
                        state.canvas_tool = CanvasTool::Node;
                        state.node_selection.clear();
                        state.node_hover = None;
                        state.edge_selection = None;
                        state.status = NODE_HINT.into();
                    }
                }
            }
            if !mask_active && response.clicked() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let p = view.screen_to_doc(pointer);
                    if let Some(path) = top_item_at(&doc.0, state, p) {
                        if state.selected.as_deref() != Some(path.as_slice()) {
                            state.node_selection.clear();
                            state.node_hover = None;
                        }
                        // Ctrl/Cmd and Shift both toggle a shape into the canvas
                        // selection. Shift must not use the row-list range
                        // behaviour here: there is no visible row anchor, and a
                        // range replaces the existing set instead of growing it.
                        let mode = canvas_selection_mode(ui.input(|input| input.modifiers));
                        select_row(state, &path, mode);
                    }
                }
            }
        }
    }

    if response.dragged_by(egui::PointerButton::Middle) {
        state.pan += response.drag_delta();
    }

    let view = CanvasView {
        image_rect: egui::Rect::from_center_size(
            rect.center() + state.pan,
            texture_size * fit * state.zoom,
        ),
        doc_size: Vec2::new(texture_size.x, texture_size.y),
    };
    if !mask_active {
        if state.canvas_drag.is_some()
            || !state.active_guides.is_empty()
            || (state.canvas_tool == CanvasTool::LassoLine && state.lasso_draft.is_some())
        {
            draw_active_guides(&clipped, &view, &state.active_guides);
        }
        draw_canvas_overlay(&clipped, &view, &doc.0, state);
    }
    draw_create_preview(&clipped, &view, state);
    draw_vector_draft(&clipped, &view, state);
    draw_ruler(&clipped, &view, state);
    draw_lasso(&clipped, &view, state);

    if state.canvas_tool == CanvasTool::Paint {
        if let Some(pointer) = response.hover_pos() {
            let scale = view
                .doc_to_screen(Vec2::new(1.0, 0.0))
                .distance(view.doc_to_screen(Vec2::ZERO));
            let radius = (state.brush_radius_now() * scale).max(1.0);
            // The inner ring shows what the mask brush paints (white, black,
            // the Value gray, or accent for Blur) over a contrasting outer ring.
            let (outer, inner) = if mask_active {
                let inner = match state.brush_mode {
                    PaintMode::Raise => egui::Color32::from_white_alpha(235),
                    PaintMode::Lower => egui::Color32::from_black_alpha(235),
                    PaintMode::Smooth => ACCENT,
                    PaintMode::Flatten => {
                        egui::Color32::from_gray((state.brush_flatten_target * 255.0) as u8)
                    }
                };
                let outer = if state.brush_mode == PaintMode::Lower {
                    egui::Color32::from_white_alpha(170)
                } else {
                    egui::Color32::from_black_alpha(170)
                };
                (outer, inner)
            } else {
                (
                    egui::Color32::from_white_alpha(170),
                    egui::Color32::from_white_alpha(80),
                )
            };
            clipped.circle_stroke(pointer, radius, egui::Stroke::new(1.0_f32, outer));
            clipped.circle_stroke(
                pointer,
                (radius * state.brush_hardness).max(1.0),
                egui::Stroke::new(1.0_f32, inner),
            );
        }
    }

    let cursor = if pan_mode {
        if response.dragged_by(egui::PointerButton::Primary) {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        }
    } else if state.canvas_tool.is_node_edit_tool() {
        if state.node_hover.is_some() {
            egui::CursorIcon::PointingHand
        } else {
            egui::CursorIcon::Default
        }
    } else if state.canvas_tool != CanvasTool::Select {
        egui::CursorIcon::Crosshair
    } else if response
        .hover_pos()
        .is_some_and(|pointer| selected_rotation_hit(doc, state, &view, pointer))
    {
        if matches!(state.canvas_drag, Some(CanvasDrag::Rotate { .. })) {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        }
    } else {
        egui::CursorIcon::Default
    };
    let _ = response.clone().on_hover_cursor(cursor);

    // Screen-sharing tools sometimes drop the OS cursor while the app is
    // captured (especially over a custom-painted canvas). Draw a matching
    // software reticle inside the frame so the presenter's pointer stays
    // visible. The system cursor remains active, so local users still get the
    // native arrow/crosshair immediately.
    if state.show_canvas_cursor {
        let pointer = response.hover_pos().or_else(|| {
            if state.canvas_drag.is_some() {
                ui.input(|input| input.pointer.interact_pos())
            } else {
                None
            }
        });
        if let Some(pointer) = pointer {
            if rect.contains(pointer) {
                let crosshair = pan_mode || state.canvas_tool != CanvasTool::Select;
                draw_canvas_cursor(&clipped, pointer, crosshair);
            }
        }
    }
}

/// High-contrast in-frame pointer. The dark under-stroke keeps it readable on
/// light normal maps; the white stroke stays readable on dark ones.
fn draw_canvas_cursor(painter: &egui::Painter, pointer: egui::Pos2, crosshair: bool) {
    let radius = if crosshair { 12.0 } else { 8.5 };
    let gap = 2.8;
    let shadow = egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(170));
    let ink = egui::Stroke::new(1.25_f32, egui::Color32::WHITE);
    for direction in [
        egui::vec2(1.0, 0.0),
        egui::vec2(-1.0, 0.0),
        egui::vec2(0.0, 1.0),
        egui::vec2(0.0, -1.0),
    ] {
        let from = pointer + direction * gap;
        let to = pointer + direction * radius;
        painter.line_segment([from, to], shadow);
        painter.line_segment([from, to], ink);
    }
    painter.circle_filled(pointer, 3.6, egui::Color32::from_black_alpha(190));
    painter.circle_filled(pointer, 2.2, egui::Color32::WHITE);
}

/// Draw the imported mesh's UV island layout over the 2D canvas. The overlay
/// is purely visual: it never intercepts input.
pub(crate) fn draw_uv_overlay_2d(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    state: &EditorState,
    mesh_import: &MeshImport,
    active_tile: u32,
) {
    if !state.show_uv_overlay_2d || state.preview_shape != PreviewShape::Imported {
        return;
    }
    let Some(texture_id) = state.uv_tex_id else {
        return;
    };
    let alpha = (state.uv_overlay_opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    if alpha == 0 {
        return;
    }

    let udim = &mesh_import.udim;
    let (uv_rect, badge) = if udim.enabled {
        let cols = udim.cols.max(1);
        let rows = udim.rows.max(1);
        let tile = if udim.tiles.contains(&active_tile) {
            active_tile
        } else {
            udim.base_tile()
        };
        let (column, row) = udim.tile_cell(tile).unwrap_or((0, rows.saturating_sub(1)));
        let uv_rect = egui::Rect::from_min_max(
            egui::pos2(column as f32 / cols as f32, row as f32 / rows as f32),
            egui::pos2(
                (column + 1) as f32 / cols as f32,
                (row + 1) as f32 / rows as f32,
            ),
        );
        (
            uv_rect,
            Some(format!("UV {tile} · {} tiles", udim.tiles.len())),
        )
    } else {
        (
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            None,
        )
    };

    painter.image(
        texture_id,
        image_rect,
        uv_rect,
        egui::Color32::from_white_alpha(alpha),
    );

    if let Some(text) = badge {
        if image_rect.width() >= 140.0 {
            let galley = painter.layout_no_wrap(
                text,
                egui::FontId::proportional(TEXT_SMALL),
                egui::Color32::from_white_alpha(alpha.min(220)),
            );
            let chip = egui::Rect::from_min_size(
                image_rect.min + egui::vec2(8.0, 8.0),
                galley.size() + egui::vec2(10.0, 5.0),
            );
            if image_rect.contains_rect(chip) {
                painter.rect_filled(chip, 3.0, egui::Color32::from_black_alpha(150));
                painter.galley(chip.min + egui::vec2(5.0, 2.5), galley, TEXT);
            }
        }
    }
}

pub(crate) fn draw_canvas_background(painter: &egui::Painter, rect: egui::Rect) {
    let clip = painter.with_clip_rect(rect);
    let step = 28.0;
    let mut y = rect.top() + 14.0;
    while y < rect.bottom() {
        let mut x = rect.left() + 14.0;
        while x < rect.right() {
            clip.circle_filled(
                egui::pos2(x, y),
                1.0,
                state_layer(SURFACE_LOWEST, ON_SURFACE, 0.10),
            );
            x += step;
        }
        y += step;
    }
}

pub(crate) struct CanvasView {
    image_rect: egui::Rect,
    doc_size: Vec2,
}

impl CanvasView {
    /// Canvas mapping with an explicit rectangle, for tests.
    #[cfg(test)]
    pub(crate) fn for_test(image_rect: egui::Rect, doc_size: Vec2) -> Self {
        Self {
            image_rect,
            doc_size,
        }
    }

    pub(crate) fn screen_to_doc(&self, pos: egui::Pos2) -> Vec2 {
        let t = (pos - self.image_rect.min) / self.image_rect.size();
        Vec2::new(t.x * self.doc_size.x, t.y * self.doc_size.y)
    }

    pub(crate) fn doc_to_screen(&self, p: Vec2) -> egui::Pos2 {
        self.image_rect.min
            + egui::vec2(
                p.x / self.doc_size.x.max(1e-5) * self.image_rect.width(),
                p.y / self.doc_size.y.max(1e-5) * self.image_rect.height(),
            )
    }

    /// Screen pixels per document pixel, used to keep snap tolerances
    /// constant on screen at any zoom.
    pub(crate) fn scale(&self) -> f32 {
        (self.image_rect.width() / self.doc_size.x.max(1e-5)).max(1e-3)
    }
}
fn background_click(
    response: &egui::Response,
    image_rect: egui::Rect,
    mask_active: bool,
    state: &mut EditorState,
) -> bool {
    if mask_active || !response.clicked_by(egui::PointerButton::Primary) {
        return false;
    }
    let outside = response
        .interact_pointer_pos()
        .is_some_and(|pointer| !image_rect.contains(pointer));
    if !outside {
        return false;
    }
    clear_canvas_selection(state);
    true
}

/// Drop the layer and node selections. Returns true when anything was selected.
pub(crate) fn clear_canvas_selection(state: &mut EditorState) -> bool {
    let had_selection = state.selected.is_some()
        || !state.multi_selected.is_empty()
        || !state.node_selection.is_empty();
    state.selected = None;
    state.multi_selected.clear();
    state.node_selection.clear();
    state.node_hover = None;
    if had_selection {
        state.status = "Selection cleared".into();
    }
    had_selection
}

pub(crate) fn draw_canvas_overlay(
    painter: &egui::Painter,
    view: &CanvasView,
    doc: &ns_core::Document,
    state: &EditorState,
) {
    // Guides sit under the selection chrome: their dashed spine should read as
    // authoring geometry, not compete with the selected layer's highlight.
    draw_path_guides(painter, view, doc);
    // Draw the Ctrl/Cmd additions first and the primary selection last, so the
    // primary outline and its handles stay on top. Every selected layer gets a
    // highlight, matching the rows highlighted in the Layers panel.
    let multi = !state.multi_selected.is_empty();
    let per_layer_handles = state.canvas_tool == CanvasTool::Select && !multi;
    let preview = state.operation_preview.as_deref();
    for path in selection_paths(state) {
        let primary = state.selected.as_deref() == Some(path);
        if let Some(layer) = doc.layer_by_path(path) {
            let outline = preview.and_then(|preview| preview_outline(preview, path, layer));
            draw_selected_layer(
                painter,
                view,
                doc,
                state,
                layer,
                outline,
                primary,
                per_layer_handles,
            );
        }
    }
    // One box drives scaling and rotation for the whole multi-selection. The
    // per-layer boxes above stay as quiet highlights without their own handles.
    if multi && state.canvas_tool == CanvasTool::Select {
        if let Some(bounds) = selection_bounds(doc, state) {
            draw_bounds(painter, view, bounds, true, 220);
            draw_rotation_handle(painter, view, bounds);
        }
    }
}

/// Draw every visible path guide as a dashed spine.
///
/// A guide never paints, so without this overlay it would be invisible on the
/// canvas. The start dot and the hollow end dot also show which way a sweep
/// travels along the path.
fn draw_path_guides(painter: &egui::Painter, view: &CanvasView, doc: &ns_core::Document) {
    fn visit(painter: &egui::Painter, view: &CanvasView, layers: &[Layer]) {
        for layer in layers {
            if layer.visible && layer.is_path_guide() {
                if let LayerKind::Shape(shape) = &layer.kind {
                    if let ShapeSource::Path(path) = &shape.source {
                        let spine = crate::operations::guide_polyline(path);
                        if spine.len() >= 2 {
                            let points: Vec<egui::Pos2> = spine
                                .iter()
                                .map(|point| view.doc_to_screen(*point))
                                .collect();
                            painter.add(egui::Shape::dashed_line(
                                &points,
                                egui::Stroke::new(1.5_f32, PRIMARY.gamma_multiply(0.85)),
                                7.0,
                                5.0,
                            ));
                            painter.circle_filled(points[0], 3.5, PRIMARY);
                            painter.circle_stroke(
                                points[points.len() - 1],
                                3.5,
                                egui::Stroke::new(1.5_f32, PRIMARY),
                            );
                        }
                    }
                }
            }
            if let Some(children) = layer.children() {
                visit(painter, view, children);
            }
        }
    }
    visit(painter, view, &doc.layers);
}

/// Layer whose outline should stand in for `original` while a live operation
/// preview is rendered. Clip and merge replace the source with new ids, so
/// follow the layer that took its stack slot; the name check rejects a copy
/// that merely shifted into the path.
fn preview_outline<'a>(
    preview: &'a ns_core::Document,
    path: &[usize],
    original: &Layer,
) -> Option<&'a Layer> {
    if let Some(layer) = preview.find_layer(original.id) {
        return Some(layer);
    }
    let layer = preview.layer_by_path(path)?;
    let piece = layer.name.starts_with(&format!("{} ", original.name)) && layer.name.contains('/');
    (layer.name == original.name || piece).then_some(layer)
}

/// Canvas clicks use toggle semantics for both Ctrl/Cmd and Shift. The Layers
/// list keeps Shift for range selection, but a canvas has no row anchor to
/// extend from, so Shift must add/remove shapes instead of replacing the set.
fn canvas_selection_mode(modifiers: egui::Modifiers) -> RowSelection {
    if modifiers.shift || modifiers.ctrl || modifiers.command {
        RowSelection::Toggle
    } else {
        RowSelection::Replace
    }
}

/// Paths to highlight on the canvas, in draw order: the Ctrl/Cmd additions
/// first and the primary selection last. A path present in both lists is drawn
/// once, as part of the primary selection.
pub(crate) fn selection_paths(state: &EditorState) -> Vec<&[usize]> {
    let primary = state.selected.as_deref();
    let mut paths: Vec<&[usize]> = state
        .multi_selected
        .iter()
        .map(Vec::as_slice)
        .filter(|path| Some(*path) != primary)
        .collect();
    if let Some(primary) = primary {
        paths.push(primary);
    }
    paths
}

/// Highlight one selected layer. The primary selection keeps the resize box and
/// rotation handle; additional selections get a quieter outline and box so a
/// multi-selection is visible on the canvas without a wall of handles.
fn draw_selected_layer(
    painter: &egui::Painter,
    view: &CanvasView,
    doc: &ns_core::Document,
    state: &EditorState,
    layer: &Layer,
    outline: Option<&Layer>,
    primary: bool,
    show_handles: bool,
) {
    if !layer.visible {
        return;
    }

    let highlight = if primary {
        SELECTION
    } else {
        SELECTION.gamma_multiply(0.88)
    };
    let stroke_width = if primary { 1.5 } else { 1.25_f32 };
    let box_alpha = if primary { 220 } else { 180 };

    match &layer.kind {
        LayerKind::Shape(_) => {
            // The evaluated preview may have clipped, merged or reflected this
            // shape, so its rendered silhouette no longer matches the source.
            // Outlining the source would read as a second, overlapping edge.
            let outline_shape = match outline.map(|layer| &layer.kind) {
                Some(LayerKind::Shape(shape)) => Some(shape),
                _ => match &layer.kind {
                    LayerKind::Shape(shape) => Some(shape),
                    _ => None,
                },
            };
            let closed = outline_shape.is_some_and(|shape| match &shape.source {
                ShapeSource::Primitive(_) => true,
                ShapeSource::Path(path) => path.subpaths.iter().all(|subpath| subpath.closed),
                ShapeSource::Text(_) => true,
                ShapeSource::Raster(_) | ShapeSource::RasterRegion(_) => false,
            });
            // Text already draws its block as the selection bounds below;
            // stroking the same rectangle twice just doubles the line.
            if let Some(shape) = outline_shape {
                if !matches!(shape.source, ShapeSource::Text(_)) {
                    for poly in shape.source.outline() {
                        if poly.len() < 2 {
                            continue;
                        }
                        let mut points: Vec<egui::Pos2> =
                            poly.iter().map(|p| view.doc_to_screen(*p)).collect();
                        if closed {
                            points.push(points[0]);
                        }
                        // Dark under-stroke first: the orange line stays visible
                        // on bright normal-map faces as well as the dark canvas.
                        painter.add(egui::Shape::line(
                            points.clone(),
                            selection_halo(stroke_width + 1.0),
                        ));
                        painter.add(egui::Shape::line(
                            points,
                            egui::Stroke::new(stroke_width, highlight),
                        ));
                    }
                }
            }

            if state.canvas_tool == CanvasTool::Select {
                if let Some(bounds) = shape_bounds(layer) {
                    draw_bounds(painter, view, bounds, show_handles, box_alpha);
                    if primary && show_handles {
                        draw_rotation_handle(painter, view, bounds);
                    }
                }
            } else if primary && state.canvas_tool.is_node_edit_tool() {
                draw_node_overlay(painter, view, doc, state);
            }
        }
        LayerKind::Group { .. } if state.canvas_tool == CanvasTool::Select => {
            if let Some(bounds) = layer_bounds(layer) {
                draw_bounds(painter, view, bounds, show_handles, box_alpha);
                if primary && show_handles {
                    draw_rotation_handle(painter, view, bounds);
                }
            }
        }
        _ => {}
    }
}

/// Selection box of a shape layer. The transform handles stay on the real
/// source layer, so dragging while a preview is live still edits the source.
fn shape_bounds(layer: &Layer) -> Option<Bounds> {
    match &layer.kind {
        LayerKind::Shape(shape) => shape.source.bounds(),
        _ => layer_bounds(layer),
    }
}

fn draw_rotation_handle(painter: &egui::Painter, view: &CanvasView, bounds: Bounds) {
    let top = view.doc_to_screen(Vec2::new(bounds.center().x, bounds.min.y));
    let handle = rotation_handle_pos(view, bounds);
    let highlight = SELECTION;
    painter.line_segment([top, handle], selection_halo(2.0));
    painter.line_segment(
        [top, handle],
        egui::Stroke::new(1.0_f32, highlight.gamma_multiply(0.8)),
    );
    painter.circle_filled(handle, 8.0, egui::Color32::WHITE);
    painter.circle_stroke(
        handle,
        8.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(30)),
    );
    painter.circle_stroke(
        handle,
        11.0,
        egui::Stroke::new(1.0_f32, highlight.gamma_multiply(0.6)),
    );
    draw_icon(
        painter,
        egui::Rect::from_center_size(handle, egui::vec2(10.0, 10.0)),
        Icon::Rotate,
        egui::Color32::from_gray(30),
    );
}

fn draw_bounds(
    painter: &egui::Painter,
    view: &CanvasView,
    bounds: Bounds,
    handles: bool,
    alpha: u8,
) {
    let min = view.doc_to_screen(bounds.min);
    let max = view.doc_to_screen(bounds.max);
    let rect = egui::Rect::from_min_max(min, max);
    painter.rect_stroke(rect, 0.0, selection_halo(2.0), egui::StrokeKind::Outside);
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0_f32, SELECTION.gamma_multiply(alpha as f32 / 255.0)),
        egui::StrokeKind::Outside,
    );
    if !handles {
        return;
    }
    for (_, p) in handle_positions(bounds) {
        let handle_rect = egui::Rect::from_center_size(view.doc_to_screen(p), egui::vec2(8.0, 8.0));
        painter.rect_filled(handle_rect, 1.0, egui::Color32::WHITE);
        painter.rect_stroke(
            handle_rect,
            1.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(30)),
            egui::StrokeKind::Outside,
        );
    }
}

/// Halo shared by every selection outline. The dark under-stroke is what makes
/// the warm orange read on a white highlight as well as on the dark canvas.
fn selection_halo(width: f32) -> egui::Stroke {
    egui::Stroke::new(width, SELECTION_HALO.gamma_multiply(0.85))
}

pub(crate) fn draw_create_preview(painter: &egui::Painter, view: &CanvasView, state: &EditorState) {
    let Some(CanvasDrag::Create {
        tool,
        start,
        current,
        ..
    }) = &state.canvas_drag
    else {
        return;
    };
    if *tool == CanvasTool::Select {
        return;
    }
    let bounds = Bounds {
        min: start.min(*current),
        max: start.max(*current),
    };
    if bounds.size().min_element() < 1.0 {
        return;
    }
    let center = bounds.center();
    let size = bounds.size();
    let primitive = match tool {
        CanvasTool::Rectangle => Primitive::Rect {
            center,
            size,
            corner_radius: size.min_element() * state.rect_corner.clamp(0.0, 0.5),
        },
        CanvasTool::Ellipse => Primitive::Ellipse {
            center,
            radii: size * 0.5,
        },
        CanvasTool::Star => Primitive::Star {
            center,
            inner_radius: size.min_element() * 0.22,
            outer_radius: size.min_element() * 0.5,
            points: 5,
            rotation: -std::f32::consts::FRAC_PI_2,
        },
        CanvasTool::Select
        | CanvasTool::Text
        | CanvasTool::Ruler
        | CanvasTool::LassoRect
        | CanvasTool::LassoEllipse
        | CanvasTool::LassoPolygon
        | CanvasTool::LassoFree
        | CanvasTool::LassoLine
        | CanvasTool::Polygon
        | CanvasTool::Cable
        | CanvasTool::Pen
        | CanvasTool::Node
        | CanvasTool::AddAnchor
        | CanvasTool::DeleteAnchor
        | CanvasTool::ConvertAnchor
        | CanvasTool::Paint => return,
    };
    let source = ShapeSource::Primitive(primitive);
    for poly in source.outline() {
        if poly.len() < 2 {
            continue;
        }
        let mut points: Vec<egui::Pos2> = poly.iter().map(|p| view.doc_to_screen(*p)).collect();
        points.push(points[0]);
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.5_f32, PRIMARY),
        ));
    }
}

pub(crate) fn draw_vector_draft(painter: &egui::Painter, view: &CanvasView, state: &EditorState) {
    let Some(draft) = &state.vector_draft else {
        return;
    };
    let accent = PRIMARY;
    let stroke = egui::Stroke::new(1.5_f32, accent);

    let mut nodes = draft.nodes.clone();
    if let Some(hover) = draft.hover {
        if draft.drag_index.is_none() && !nodes.is_empty() {
            nodes.push(Node::corner(hover));
        }
    }
    if nodes.len() >= 2 {
        let preview = Path {
            subpaths: vec![SubPath {
                nodes,
                closed: false,
            }],
            fill_rule: FillRule::NonZero,
        };
        for polyline in ns_core::path::flatten_path(&preview) {
            if polyline.len() < 2 {
                continue;
            }
            let points: Vec<egui::Pos2> = polyline
                .iter()
                .map(|point| view.doc_to_screen(*point))
                .collect();
            painter.add(egui::Shape::line(points, stroke));
        }
    }

    for (index, node) in draft.nodes.iter().enumerate() {
        let p = view.doc_to_screen(node.pos);
        let rect = egui::Rect::from_center_size(p, egui::vec2(7.0, 7.0));
        painter.rect_filled(rect, 1.0, egui::Color32::WHITE);
        painter.rect_stroke(
            rect,
            1.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(30)),
            egui::StrokeKind::Outside,
        );
        if index == 0 && draft.nodes.len() >= 2 {
            // Armed: a click here closes the loop, so say so before the click.
            if draft.close_armed {
                painter.circle_filled(p, 8.0, accent.gamma_multiply(0.35));
                painter.circle_stroke(p, 8.0, egui::Stroke::new(2.0_f32, accent));
            } else {
                painter.circle_stroke(p, 8.0, egui::Stroke::new(1.0_f32, accent));
            }
        }
    }

    if let Some(node) = draft.drag_index.and_then(|index| draft.nodes.get(index)) {
        let p = view.doc_to_screen(node.pos);
        for handle in [node.handle_in, node.handle_out] {
            if handle.length_squared() > 1.0 {
                let h = view.doc_to_screen(node.pos + handle);
                painter.line_segment([p, h], egui::Stroke::new(1.0_f32, accent));
                painter.circle_filled(h, 3.0, accent);
            }
        }
    }

    // Alignment guides sit on top of the draft so the constraint that placed
    // the pending node is unmistakable.
    draw_smart_guides(painter, view, draft);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_the_canvas_selection_drops_every_selection_kind() {
        let mut state = EditorState {
            selected: Some(vec![1]),
            multi_selected: vec![vec![0]],
            node_selection: vec![NodeRef {
                subpath: 0,
                node: 0,
            }],
            node_hover: Some(NodeRef {
                subpath: 0,
                node: 1,
            }),
            ..Default::default()
        };
        assert!(clear_canvas_selection(&mut state));
        assert!(state.selected.is_none());
        assert!(state.multi_selected.is_empty());
        assert!(state.node_selection.is_empty());
        assert!(state.node_hover.is_none());
        // Nothing left to clear: an empty click reports no change.
        assert!(!clear_canvas_selection(&mut state));
    }

    #[test]
    fn selection_paths_include_every_selected_layer() {
        let state = EditorState {
            selected: Some(vec![1]),
            multi_selected: vec![vec![0], vec![2]],
            ..Default::default()
        };
        let paths: Vec<Vec<usize>> = selection_paths(&state)
            .into_iter()
            .map(<[usize]>::to_vec)
            .collect();
        // Additions first, primary last so its handles sit on top.
        assert_eq!(paths, vec![vec![0], vec![2], vec![1]]);
    }

    #[test]
    fn selection_paths_draw_the_primary_once() {
        let state = EditorState {
            selected: Some(vec![1]),
            multi_selected: vec![vec![0], vec![1]],
            ..Default::default()
        };
        let paths: Vec<Vec<usize>> = selection_paths(&state)
            .into_iter()
            .map(<[usize]>::to_vec)
            .collect();
        assert_eq!(paths, vec![vec![0], vec![1]]);
    }

    #[test]
    fn canvas_shift_click_toggles_and_never_uses_row_ranges() {
        assert_eq!(
            canvas_selection_mode(egui::Modifiers {
                shift: true,
                ..Default::default()
            }),
            RowSelection::Toggle
        );
        assert_eq!(
            canvas_selection_mode(egui::Modifiers {
                command: true,
                ..Default::default()
            }),
            RowSelection::Toggle
        );
        assert_eq!(
            canvas_selection_mode(egui::Modifiers::default()),
            RowSelection::Replace
        );
    }

    #[test]
    fn shift_click_accumulates_more_than_two_canvas_shapes() {
        let mut state = EditorState::default();
        let shift = canvas_selection_mode(egui::Modifiers {
            shift: true,
            ..Default::default()
        });
        select_row(&mut state, &[0], shift);
        select_row(&mut state, &[1], shift);
        select_row(&mut state, &[2], shift);
        assert_eq!(state.selected, Some(vec![2]));
        assert_eq!(state.multi_selected, vec![vec![0], vec![1]]);

        // Clicking a selected shape again removes only that shape.
        select_row(&mut state, &[1], shift);
        assert_eq!(state.selected, Some(vec![2]));
        assert_eq!(state.multi_selected, vec![vec![0]]);
    }

    #[test]
    fn selection_paths_without_a_primary_keep_the_additions() {
        let state = EditorState {
            selected: None,
            multi_selected: vec![vec![0], vec![2]],
            ..Default::default()
        };
        assert_eq!(selection_paths(&state).len(), 2);
    }

    #[test]
    fn preview_outline_follows_a_clipped_source_piece() {
        let original = ns_core::Layer::shape(
            "panel",
            ns_core::ShapeSource::Primitive(ns_core::Primitive::Rect {
                center: glam::Vec2::splat(32.0),
                size: glam::Vec2::splat(20.0),
                corner_radius: 2.0,
            }),
        );
        let piece = ns_core::Layer::shape(
            "panel",
            ns_core::ShapeSource::Primitive(ns_core::Primitive::Rect {
                center: glam::Vec2::splat(32.0),
                size: glam::Vec2::splat(10.0),
                corner_radius: 2.0,
            }),
        );
        let copy = ns_core::Layer::shape(
            "panel 001",
            ns_core::ShapeSource::Primitive(ns_core::Primitive::Rect {
                center: glam::Vec2::splat(32.0),
                size: glam::Vec2::splat(10.0),
                corner_radius: 2.0,
            }),
        );
        let mut preview = ns_core::Document::new(glam::UVec2::splat(64));
        preview.layers = vec![piece.clone(), copy];

        // The source id vanished with the cut; the piece keeps the same name
        // and stack slot, so its outline replaces the stale source outline.
        let outline = preview_outline(&preview, &[0], &original).expect("piece is outlined");
        assert_eq!(outline.id, piece.id);
    }

    #[test]
    fn preview_outline_ignores_a_copy_shifted_into_the_path() {
        let original = ns_core::Layer::shape(
            "panel",
            ns_core::ShapeSource::Primitive(ns_core::Primitive::Rect {
                center: glam::Vec2::splat(32.0),
                size: glam::Vec2::splat(20.0),
                corner_radius: 2.0,
            }),
        );
        let copy = ns_core::Layer::shape(
            "panel 001",
            ns_core::ShapeSource::Primitive(ns_core::Primitive::Rect {
                center: glam::Vec2::splat(32.0),
                size: glam::Vec2::splat(20.0),
                corner_radius: 2.0,
            }),
        );
        let mut preview = ns_core::Document::new(glam::UVec2::splat(64));
        preview.layers = vec![copy];

        assert!(preview_outline(&preview, &[0], &original).is_none());
    }
}
