//! Top bar, tool rail, and status bar panels.

use super::*;

pub(crate) struct TopBarPanel;

impl Panel for TopBarPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        top_bar(
            ui,
            frame.state,
            frame.doc,
            frame.cache,
            frame.mesh_import,
            &*frame.lightmap,
            &*frame.bake,
        );
    }
}

pub(crate) struct ToolRailPanel;

impl Panel for ToolRailPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        tool_rail(ui, frame.state);
    }
}

pub(crate) struct StatusBarPanel;

impl Panel for StatusBarPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        status_bar(ui, frame.state, &frame.system_stats, frame.bridge);
    }
}

/// Floating diagnostics panel toggled from the status bar.
pub(crate) fn perf_window(
    ctx: &egui::Context,
    state: &mut EditorState,
    doc: &Document,
    gpu_eval_ms: Option<f64>,
    gpu_height_ms: Option<f64>,
    gpu_upload_ms: Option<f64>,
) {
    if !state.show_perf_hud {
        return;
    }
    let mut open = state.show_perf_hud;
    egui::Window::new("Performance")
        .id(egui::Id::new("ns_perf_hud"))
        .open(&mut open)
        .resizable(false)
        .default_width(360.0)
        // Start below the top bar so the menu remains reachable.
        .default_pos(egui::pos2(88.0, 76.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (label, color) = if state.using_gpu {
                    ("GPU path", SUCCESS)
                } else {
                    ("CPU fallback", WARNING)
                };
                ui.label(egui::RichText::new(label).strong().color(color));
                if state.using_gpu {
                    ui.label(
                        egui::RichText::new("shape stack + precomputed textures")
                            .size(TEXT_LABEL)
                            .color(MUTED),
                    );
                }
            });

            if !state.using_gpu {
                if state.gate_rejects.is_empty() {
                    ui.label(
                        egui::RichText::new("No reason was classified; check the log.")
                            .color(MUTED),
                    );
                } else {
                    for reason in &state.gate_rejects {
                        ui.label(
                            egui::RichText::new(format!("• {}", reason.label()))
                                .color(TEXT)
                                .strong(),
                        )
                        .on_hover_text(reason.hint());
                    }
                }
            }

            ui.separator();
            perf_stat_row(ui, "Scene build", &state.perf_scene_build_ms, "ms");
            perf_stat_row(ui, "CPU eval", &state.perf_cpu_eval_ms, "ms");
            perf_optional_row(
                ui,
                "GPU eval",
                gpu_eval_ms,
                "ms",
                "GPU timestamp query for the shape-stack dispatch; n/a when the backend lacks timestamp support.",
            );
            perf_optional_row(
                ui,
                "GPU height",
                gpu_height_ms,
                "ms",
                "GPU timestamp query for the height-integration passes.",
            );
            perf_optional_row(
                ui,
                "GPU upload",
                gpu_upload_ms,
                "ms",
                "CPU time spent packing and writing buffers for the last GPU scene update.",
            );
            let frame_ms = ctx.input(|input| input.stable_dt) * 1000.0;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Frame").color(MUTED));
                ui.label(format!("{frame_ms:.2} ms"));
            });

            ui.separator();
            let cache_bytes = doc.estimated_cache_bytes();
            let cache_budget = ns_core::DEFAULT_EVAL_CACHE_BUDGET;
            let cache_ratio = cache_bytes as f32 / cache_budget.max(1) as f32;
            let cache_color = if cache_ratio >= 1.0 { WARNING } else { TEXT };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Eval cache").color(MUTED));
                ui.label(
                    egui::RichText::new(format!(
                        "{} / {} MiB",
                        format_mib(cache_bytes),
                        format_mib(cache_budget)
                    ))
                    .color(cache_color),
                );
            })
            .response
            .on_hover_text(
                "Estimated bytes for caching every visible layer's normal + height. \
                 Above the budget the CPU evaluator streams instead.",
            );

            let undo_bytes = state.undo_bytes() as u64;
            let undo_budget = state.undo_budget() as u64;
            let undo_color = if undo_bytes >= undo_budget {
                WARNING
            } else {
                TEXT
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Undo + redo").color(MUTED));
                ui.label(
                    egui::RichText::new(format!(
                        "{} / {} MiB",
                        format_mib(undo_bytes),
                        format_mib(undo_budget)
                    ))
                    .color(undo_color),
                );
            })
            .response
            .on_hover_text("Shared document allocations are counted once.");

            ui.separator();
            ui.label(
                egui::RichText::new(format!(
                    "Document {} × {} · {} visible layers",
                    doc.size.x,
                    doc.size.y,
                    doc.visible_layer_count()
                ))
                .size(TEXT_LABEL)
                .color(MUTED),
            );
        });
    state.show_perf_hud = open;
}

fn perf_optional_row(ui: &mut egui::Ui, label: &str, value: Option<f64>, unit: &str, hint: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(MUTED));
        match value {
            Some(value) => {
                ui.label(format!("{value:.2} {unit}"));
            }
            None => {
                ui.label(egui::RichText::new("n/a").color(FAINT));
            }
        }
    })
    .response
    .on_hover_text(hint);
}

fn perf_stat_row(ui: &mut egui::Ui, label: &str, stat: &PerfStat, unit: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(MUTED));
        match (stat.last(), stat.mean(), stat.p95()) {
            (Some(last), Some(mean), Some(p95)) => {
                ui.label(format!(
                    "{last:.2} {unit}  ·  mean {mean:.2}  ·  p95 {p95:.2}"
                ));
            }
            _ => {
                ui.label(egui::RichText::new("no samples yet").color(FAINT));
            }
        }
        if stat.len() > 1 {
            ui.label(
                egui::RichText::new(format!("({} samples)", stat.len()))
                    .size(TEXT_LABEL)
                    .color(FAINT),
            );
        }
    });
}

fn format_mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_048_576.0)
}

/// Every top-bar dropdown shares this content width so the menu bar reads as
/// one system instead of sizing each popup to its longest label.
const MENU_WIDTH: f32 = 232.0;

/// Full-height menu-bar tab. When `active`, the tab gets the panel tone and a
/// muted primary underline at the bar's bottom edge. `content_height` is the
/// height of the top-bar content row (where the label is centered);
/// `bar_height` is the visible bar height, so the active fill can reach the
/// bottom edge and merge into the dropdown panel below it.
fn menu_tab(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    content_height: f32,
    bar_height: f32,
) -> egui::Response {
    // Reserve a paint slot before the button draws so the active title can get
    // a tonal fill without covering its label.
    let background = ui.painter().add(egui::Shape::Noop);
    let mut button = egui::Button::new(egui::RichText::new(label).size(14.0))
        .min_size(egui::vec2(0.0, content_height))
        .corner_radius(egui::CornerRadius::same(2));
    if active {
        // Keep egui's hover fill from covering the active title fill.
        button = button.fill(egui::Color32::TRANSPARENT);
    }
    let response = ui.add(button);
    if active {
        let rect = response.rect;
        let tab_bottom = rect.bottom() + (bar_height - content_height).max(0.0);
        // Round only the top corners: the bottom edge stays square so the tab
        // merges into the dropdown panel below it, like the reference mock.
        ui.painter().set(
            background,
            egui::Shape::rect_filled(
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, tab_bottom)),
                egui::CornerRadius {
                    nw: 6,
                    ne: 6,
                    sw: 0,
                    se: 0,
                },
                CONTAINER_HIGH,
            ),
        );
        let underline = egui::Rect::from_min_size(
            egui::pos2(rect.left(), tab_bottom - 2.0),
            egui::vec2(rect.width(), 2.0),
        );
        ui.painter().rect_filled(
            underline,
            egui::CornerRadius::ZERO,
            state_layer(CONTAINER_HIGH, PRIMARY, 0.72),
        );
    }
    response
}

/// Flat, panel-toned rows shared by every top-bar dropdown. Call this inside
/// the popup frame before laying out items, so the animated menus and the
/// native Tools popup read as one system.
fn style_menu_popup(ui: &mut egui::Ui) {
    // The reference menu has no per-item chips, only a hover highlight and
    // hairline separators.
    let visuals = ui.visuals_mut();
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.inactive.fg_stroke.color = ON_SURFACE_VAR;
    visuals.widgets.hovered.weak_bg_fill = hover_layer(CONTAINER_HIGH);
    visuals.widgets.hovered.bg_fill = hover_layer(CONTAINER_HIGH);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.hovered.fg_stroke.color = TEXT;
    visuals.widgets.active.weak_bg_fill = state_layer(CONTAINER_HIGH, ON_SURFACE, PRESS_ALPHA);
    visuals.widgets.active.bg_fill = state_layer(CONTAINER_HIGH, ON_SURFACE, PRESS_ALPHA);
    visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
    // Checked rows (the View tab, for example) use a tonal chip instead of
    // the full-primary text-selection fill so they read as state, not a
    // button.
    visuals.selection.bg_fill = state_layer(CONTAINER_HIGH, PRIMARY, 0.16);
    visuals.selection.stroke = egui::Stroke::new(1.5_f32, ON_SECONDARY_CONTAINER);

    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    ui.set_width(MENU_WIDTH);
    ui.spacing_mut().item_spacing.y = 2.0;
    // 10 px keeps the long View menu inside a 780 px-tall window even with
    // the tabs spanning the full bar height.
    ui.spacing_mut().button_padding = egui::vec2(10.0, 10.0);
    // The View menu's opacity slider would otherwise overflow the shared
    // width and stretch that one panel out of scale.
    ui.spacing_mut().slider_width = 92.0;
}

/// Top-bar menu with a short drop animation.
///
/// egui's `menu_button` opens instantly. This draws the same popup in an
/// `Area` and eases it down from under the button while it fades in. The
/// content closure returns `true` when an item asked to close the menu.
fn animated_menu_button(
    ui: &mut egui::Ui,
    id_salt: &str,
    label: &str,
    content_height: f32,
    bar_height: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> bool,
) {
    let id = ui.make_persistent_id(("top_menu", id_salt));
    let mut open = ui.data(|data| data.get_temp::<bool>(id).unwrap_or(false));
    let response = menu_tab(ui, label, open, content_height, bar_height);

    if response.clicked() {
        open = !open;
        if open {
            let now = ui.input(|input| input.time);
            ui.data_mut(|data| {
                data.insert_temp(id.with("opened"), now);
                data.insert_temp(id.with("rect"), egui::Rect::NOTHING);
            });
        }
    }

    // Clicking anywhere outside the popup, or pressing Escape, closes it.
    if open {
        let pointer = ui.input(|input| input.pointer.interact_pos());
        let clicked = ui.input(|input| input.pointer.any_click());
        let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
        let menu_rect = ui.data(|data| data.get_temp::<egui::Rect>(id.with("rect")));
        let inside_menu = menu_rect
            .zip(pointer)
            .is_some_and(|(rect, position)| rect.contains(position));
        let inside_button = pointer.is_some_and(|position| response.rect.contains(position));
        if escape || (clicked && !inside_menu && !inside_button) {
            open = false;
        }
    }

    if open {
        let now = ui.input(|input| input.time);
        let opened_at = ui
            .data(|data| data.get_temp::<f64>(id.with("opened")))
            .unwrap_or(now);
        let progress = ((now - opened_at) as f32 / 0.13).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        // Start slightly above the resting spot and slide down into place.
        // The panel hangs flush from the bar's bottom edge, like the reference
        // mock, not from the shorter content-row button.
        let tab_bottom = response.rect.bottom() + (bar_height - content_height).max(0.0);
        let origin =
            egui::pos2(response.rect.left(), tab_bottom) - egui::vec2(0.0, (1.0 - eased) * 6.0);

        let menu = egui::Area::new(id.with("popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(origin)
            .show(ui.ctx(), |ui| {
                ui.set_opacity(eased);
                egui::Frame::menu(ui.style())
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 8,
                        top: 16,
                        bottom: 12,
                    })
                    .show(ui, |ui| {
                        style_menu_popup(ui);
                        // Justify rows so hover and selected fills span the
                        // panel instead of hugging the label.
                        ui.with_layout(
                            egui::Layout::top_down_justified(egui::Align::LEFT),
                            add_contents,
                        )
                        .inner
                    })
                    .inner
            });

        ui.data_mut(|data| {
            data.insert_temp(id.with("rect"), menu.response.rect);
            data.insert_temp(id.with("opened"), opened_at);
        });
        if menu.inner {
            open = false;
        }
    }

    ui.data_mut(|data| data.insert_temp(id, open));
}

pub(crate) fn top_bar(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    doc: &mut DocumentRes,
    cache: &mut EvalCache,
    mesh_import: &crate::import::MeshImport,
    lightmap: &crate::lightmap::LightmapRuntime,
    bake: &crate::bake::BakeRuntime,
) {
    let width = ui.available_width();
    let tight = width < 980.0;
    let tiny = width < 720.0;
    let compact_docks = width < 860.0;
    let (left_visible, right_visible) = if compact_docks {
        match (state.show_layers_dock, state.show_right_dock) {
            (true, true) => {
                if state.right_tab == RightTab::Properties {
                    (true, false)
                } else {
                    (false, true)
                }
            }
            (layers, right) => (layers, right),
        }
    } else {
        (state.show_layers_dock, state.show_right_dock)
    };

    // The bar frame paints one outer-margin's worth past the panel's clip rect,
    // so derive the visible height instead of trusting `available_height()`.
    let bar_height = (ui.clip_rect().bottom() - ui.cursor().top()).max(28.0);
    // The frame's inner margin makes the content row shorter than the visible
    // bar. Labels center in this row; the active tab fill still reaches the
    // bar's bottom edge via `bar_height`.
    let content_height = ui.available_height().clamp(28.0, bar_height);
    let save_context = crate::project_data::SaveContext {
        mesh_import,
        lightmap,
        bake,
    };

    ui.horizontal_centered(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(9.0, 6.0);
        ui.spacing_mut().interact_size.y = 28.0;
        ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        ui.visuals_mut().widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
        ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
        draw_brand_logo(ui);
        if !tiny {
            ui.add_space(7.0);
            // The box must be as tall as the two labels (16 px and 10.5 px
            // lines), otherwise the block overflows downwards and the whole
            // pair sits low in the row.
            let title_box = if tight {
                egui::vec2(76.0, 20.0)
            } else {
                egui::vec2(118.0, 32.0)
            };
            ui.allocate_ui_with_layout(
                title_box,
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(
                        egui::RichText::new("N-shade")
                            .size(16.0)
                            .strong()
                            .color(TEXT),
                    );
                    if !tight {
                        ui.label(
                            egui::RichText::new("NORMAL MAP STUDIO")
                                .size(10.5)
                                .color(FAINT),
                        );
                    }
                },
            );
            if !tiny {
                // Reference badge: mono, accent on a 14% wash with a 1px accent
                // border. Painted by hand because a `Frame` added to a row
                // with `Align::Center` is stretched to the full row height,
                // which turns the chip into a full-height slab.
                let galley = ui.painter().layout_no_wrap(
                    concat!("V", env!("CARGO_PKG_VERSION")).to_owned(),
                    mono(10.0),
                    PRIMARY,
                );
                let (rect, _) = ui.allocate_exact_size(
                    galley.size() + egui::vec2(12.0, 4.0),
                    egui::Sense::hover(),
                );
                ui.painter().rect(
                    rect,
                    egui::CornerRadius::same(RADIUS_XS),
                    ACCENT_WASH,
                    egui::Stroke::new(1.0_f32, PRIMARY.gamma_multiply(0.30)),
                    egui::StrokeKind::Inside,
                );
                ui.painter()
                    .galley(rect.center() - galley.size() / 2.0, galley, PRIMARY);
            }
        }
        ui.add_space(8.0);
        animated_menu_button(ui, "file", "File", content_height, bar_height, |ui| {
            let mut close = false;
            if ui
                .button("New project")
                .on_hover_text("Start a blank project (Ctrl+N)")
                .clicked()
            {
                new_project(state, doc, cache);
                close = true;
            }
            if ui
                .button("Open project...")
                .on_hover_text("Open a .nsproj project (Ctrl+O)")
                .clicked()
            {
                open_project_dialog(state, doc, cache);
                close = true;
            }
            if ui
                .button("Save project")
                .on_hover_text("Save the current project (Ctrl+S)")
                .clicked()
            {
                save_project_dialog(state, doc, false, Some(&save_context));
                close = true;
            }
            if ui
                .button("Save project as...")
                .on_hover_text("Save under a new name (Ctrl+Shift+S)")
                .clicked()
            {
                save_project_dialog(state, doc, true, Some(&save_context));
                close = true;
            }
            if ui
                .button("Save legacy document (v1)...")
                .on_hover_text(
                    "Write a plain .nsproj document for older N-shade builds; mesh, \
                     editor and lightmap state are not included",
                )
                .clicked()
            {
                save_legacy_project_dialog(state, doc);
                close = true;
            }
            if ui
                .button("Recover latest autosave")
                .on_hover_text(
                    "Open the newest recovery snapshot for this project. Recovered work \
                     stays unsaved until you save it to the project file.",
                )
                .clicked()
            {
                recover_latest_autosave(state, doc, cache);
                close = true;
            }
            ui.separator();
            if ui.button("Import mesh...").clicked() {
                open_mesh_dialog(state);
                close = true;
            }
            if ui.button("Import SVG...").clicked() {
                open_svg_dialog(state);
                close = true;
            }
            if ui.button("Import photo...").clicked() {
                open_photo_dialog(state);
                close = true;
            }
            if ui.button("Import alpha...").clicked() {
                open_alpha_dialog(state);
                close = true;
            }
            if ui.button("Import normal map...").clicked() {
                open_normal_dialog(state);
                close = true;
            }
            if ui.button("Export normal + height").clicked() {
                export_project(state, doc, cache);
                close = true;
            }
            ui.separator();
            if ui.button("Reset demo scene").clicked() {
                doc.0 = demo_document(doc.0.size.x).into();
                state.collapsed.clear();
                state.multi_selected.clear();
                state.selected = doc.0.layers.len().checked_sub(1).map(|index| vec![index]);
                state.mark_dirty();
                close = true;
            }
            ui.separator();
            if ui.button("Quit").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            close
        });
        animated_menu_button(ui, "edit", "Edit", content_height, bar_height, |ui| {
            let mut close = false;
            if ui.button("Undo").clicked() {
                state.undo(doc);
                close = true;
            }
            if ui.button("Redo").clicked() {
                state.redo(doc);
                close = true;
            }
            ui.separator();
            let has_selection = state.selected.is_some();
            if ui
                .add_enabled(has_selection, egui::Button::new("Copy"))
                .on_hover_text("Ctrl/Cmd+C")
                .clicked()
            {
                copy_selected_layers(state, doc);
                close = true;
            }
            if ui
                .add_enabled(
                    !state.layer_clipboard.is_empty(),
                    egui::Button::new("Paste"),
                )
                .on_hover_text("Ctrl/Cmd+V")
                .clicked()
            {
                paste_layers(state, doc);
                close = true;
            }
            ui.separator();
            if ui
                .add_enabled(has_selection, egui::Button::new("Duplicate"))
                .clicked()
            {
                state.checkpoint_document(&doc.0);
                duplicate_selected_layers(state, doc);
                close = true;
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Duplicate array..."))
                .clicked()
            {
                state.show_duplicate_array = true;
                close = true;
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Group"))
                .on_hover_text("Ctrl/Cmd+G")
                .clicked()
            {
                state.checkpoint_document(&doc.0);
                apply_layer_action(LayerAction::GroupSelected, state, doc);
                close = true;
            }
            ui.separator();
            if ui.button("Settings...").clicked() {
                state.show_settings = true;
                close = true;
            }
            close
        });
        animated_menu_button(ui, "view", "View", content_height, bar_height, |ui| {
            let mut close = false;
            for tab in [ViewportTab::ThreeD, ViewportTab::TwoD] {
                if ui
                    .selectable_label(state.viewport_tab == tab && !state.split_view, tab.label())
                    .clicked()
                {
                    state.split_view = false;
                    state.viewport_tab = tab;
                    close = true;
                }
            }
            if ui
                .checkbox(&mut state.split_view, "Split 3D / 2D")
                .changed()
            {
                if state.split_view && state.dirty {
                    state.request_eval_now();
                }
                state.status = if state.split_view {
                    "Split view · drag the divider to resize".into()
                } else {
                    state.viewport_tab.label().into()
                };
            }
            ui.separator();
            if ui.button("Fit 2D view").clicked() {
                state.zoom = 1.0;
                state.pan = egui::Vec2::ZERO;
                close = true;
            }
            if ui.button("100%").clicked() {
                state.zoom = 1.0;
                close = true;
            }
            if ui.button("200%").clicked() {
                state.zoom = 2.0;
                close = true;
            }
            if ui.button("Center 2D view").clicked() {
                state.pan = egui::Vec2::ZERO;
                close = true;
            }
            if ui.button("Reset 3D camera").clicked() {
                reset_camera(state);
                close = true;
            }
            ui.separator();
            ui.checkbox(&mut state.auto_rotate, "Auto-rotate 3D preview");
            ui.checkbox(&mut state.show_uv_overlay, "UV overlay in 3D view");
            ui.checkbox(&mut state.show_uv_overlay_2d, "UV overlay on 2D canvas");
            ui.checkbox(&mut state.show_canvas_cursor, "Visible cursor on 2D canvas")
                .on_hover_text(
                    "Draw a high-contrast reticle at the canvas pointer so screen \
                     sharing and streaming tools that omit the OS cursor still show it",
                );
            ui.add(egui::Slider::new(&mut state.uv_overlay_opacity, 0.05..=1.0).text("UV opacity"));
            ui.separator();
            ui.checkbox(&mut state.show_layers_dock, "Layers panel");
            ui.checkbox(&mut state.show_right_dock, "Properties panel");
            ui.separator();
            if ui
                .button("Keyboard shortcuts...")
                .on_hover_text("Reference for every shortcut in the app (?)")
                .clicked()
            {
                toggle_shortcuts(ui.ctx());
                close = true;
            }
            close
        });
        // Settings is a text entry in the menu row: the icon shortcuts for the
        // bake/lightmap/PBR panels were redundant with the right-dock tabs.
        // Settings is a plain toggle in the menu row, drawn with the same tab
        // treatment as the menus while its window is open.
        let settings_open = state.show_settings;
        if menu_tab(ui, "Settings", settings_open, content_height, bar_height)
            .on_hover_text("Open the settings window")
            .clicked()
        {
            state.show_settings = !state.show_settings;
        }
        // Optional generators live in their own window: they never edit the
        // document, so they stay out of the tool rail and the docks.
        // An open native menu highlights its button (and focus keeps it
        // active), which egui paints lighter than the custom tabs. Match the
        // tab tone while the menu is open; there is still no underline here.
        ui.scope(|ui| {
            let visuals = ui.visuals_mut();
            for widget in [&mut visuals.widgets.hovered, &mut visuals.widgets.active] {
                widget.weak_bg_fill = CONTAINER_HIGH;
                widget.bg_fill = CONTAINER_HIGH;
            }
            egui::containers::menu::MenuButton::from_button(
                egui::Button::new(egui::RichText::new("Tools").size(14.0))
                    .min_size(egui::vec2(0.0, content_height))
                    .corner_radius(egui::CornerRadius::same(2)),
            )
            .ui(ui, |ui| {
                    style_menu_popup(ui);
                    // The native frame's margin is 8 px all round; pad the top
                    // so the first row sits at the same 16 px as the animated
                    // menus.
                    ui.add_space(8.0);
                    if ui
                        .button("Sci-Fi texture generator...")
                        .on_hover_text(
                            "Procedural panel textures in a separate window. Generates and saves images only; the document is untouched.",
                        )
                        .clicked()
                    {
                        state.scifi.open = true;
                        ui.close();
                    }
                    ui.add_space(4.0);
            });
        });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            if icon_button(
                ui,
                Icon::PanelRight,
                right_visible,
                true,
                if right_visible {
                    "Hide the right panel"
                } else {
                    "Show the right panel"
                },
            )
            .clicked()
            {
                state.show_right_dock = !right_visible;
                if state.show_right_dock && compact_docks {
                    state.show_layers_dock = false;
                }
            }
            if icon_button(
                ui,
                Icon::PanelLeft,
                left_visible,
                true,
                if left_visible {
                    "Hide the layers panel"
                } else {
                    "Show the layers panel"
                },
            )
            .clicked()
            {
                state.show_layers_dock = !left_visible;
                if state.show_layers_dock && compact_docks {
                    state.show_right_dock = false;
                }
            }
            if !tight {
                ui.add_space(10.0);
                let can_redo = !state.redo.is_empty();
                if icon_button(
                    ui,
                    Icon::Redo,
                    false,
                    can_redo,
                    if can_redo {
                        "Redo (Ctrl+Shift+Z / Ctrl+Y)"
                    } else {
                        "Nothing to redo"
                    },
                )
                .clicked()
                {
                    state.redo(doc);
                }
                let can_undo = !state.undo.is_empty();
                if icon_button(
                    ui,
                    Icon::Undo,
                    false,
                    can_undo,
                    if can_undo {
                        "Undo (Ctrl+Z)"
                    } else {
                        "Nothing to undo"
                    },
                )
                .clicked()
                {
                    state.undo(doc);
                }
            }
        });
    });
}

pub(crate) fn tool_rail(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.vertical_centered(|ui| {
        if rail_tool(
            ui,
            state.canvas_tool == CanvasTool::Select,
            "select",
            "Select",
            "Select and transform (V)",
        )
        .clicked()
        {
            activate_tool(state, CanvasTool::Select);
        }

        // Shape group: left-click arms the last shape chosen in the flyout,
        // right-click shows every primitive in one menu.
        let shape_tool = state.shape_tool;
        let response = rail_tool(
            ui,
            state.canvas_tool.is_shape_tool(),
            "shapes",
            "Shapes",
            &format!(
                "Shapes · {} (R/O/S/G) · right-click for rectangle, ellipse, star and polygon",
                shape_tool.label()
            ),
        );
        rail_flyout_marker(ui, response.rect, state.canvas_tool.is_shape_tool());
        if response.clicked() {
            activate_tool(state, shape_tool);
        }
        response.context_menu(|ui| {
            ui.label(
                egui::RichText::new("SHAPE TOOLS")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            for tool in CanvasTool::SHAPE_TOOLS {
                // Show the armed shape as selected; while another tool is in use
                // the menu still marks the one the tile would arm.
                let selected = if state.canvas_tool.is_shape_tool() {
                    state.canvas_tool == tool
                } else {
                    state.shape_tool == tool
                };
                ui.horizontal(|ui| {
                    let (icon_rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    draw_icon(
                        ui.painter(),
                        icon_rect,
                        shape_tool_icon(tool),
                        if selected { ACCENT } else { MUTED },
                    );
                    ui.add_space(2.0);
                    if ui.selectable_label(selected, tool.label()).clicked() {
                        state.shape_tool = tool;
                        activate_tool(state, tool);
                        ui.close();
                    }
                });
            }
            ui.separator();
            ui.label(
                egui::RichText::new(
                    "Rectangle, ellipse and star drag out from a corner. Polygon places \
                     corners on click; Enter or a click on the first corner closes it.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        });

        if rail_tool(
            ui,
            state.canvas_tool == CanvasTool::Text,
            "text",
            "Text",
            "Text (T) · click to place, then edit content and font in Properties",
        )
        .clicked()
        {
            activate_tool(state, CanvasTool::Text);
        }

        ui.add_space(10.0);

        // Vector group: left-click uses the last chosen vector tool,
        // right-click opens the anchor/pen flyout.
        let glyph = rail_vector_glyph(state.vector_tool);
        let vector_label = if state.vector_tool == CanvasTool::Cable {
            "Cable"
        } else {
            "Pen"
        };
        let response = rail_tool(
            ui,
            state.canvas_tool.is_vector_tool(),
            glyph,
            vector_label,
            "Vector tools (P) · right-click for Pen, Cable and anchor tools",
        );
        // Flyout marker tucked into the tile corner, as in a toolbox flyout.
        rail_flyout_marker(ui, response.rect, state.canvas_tool.is_vector_tool());
        if response.clicked() {
            activate_tool(state, state.vector_tool);
        }
        response.context_menu(|ui| {
            ui.label(
                egui::RichText::new("VECTOR TOOLS")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            for tool in CanvasTool::VECTOR_TOOLS {
                let selected = state.vector_tool == tool;
                ui.horizontal(|ui| {
                    let (icon_rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    draw_icon(
                        ui.painter(),
                        icon_rect,
                        vector_tool_icon(tool),
                        if selected { ACCENT } else { MUTED },
                    );
                    ui.add_space(2.0);
                    if ui.selectable_label(selected, tool.label()).clicked() {
                        activate_tool(state, tool);
                        ui.close();
                    }
                });
            }
            ui.separator();
            ui.label(
                egui::RichText::new(
                    "Cable turns a custom vector path into a smooth tube. Draw the centreline \
                     with the Pen first, then click that path with the Cable tool.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        });

        // Lasso group: one tile for the cleanup lassos; right-click picks the
        // region type, the tile runs the last one chosen.
        let lasso_label = format!(
            "Lasso (L) · {} · drag to clean the layer (Shift restores, Space moves, drag right/down to cut, left/up to divide) · right-click the tile for more",
            state.lasso_tool.label()
        );
        let response = rail_tool(
            ui,
            state.canvas_tool.is_lasso_tool(),
            "lasso",
            "Lasso",
            &lasso_label,
        );
        rail_flyout_marker(ui, response.rect, state.canvas_tool.is_lasso_tool());
        if response.clicked() {
            activate_tool(state, state.lasso_tool);
        }
        response.context_menu(|ui| {
            ui.label(
                egui::RichText::new("LASSO TOOLS · CLEANUP")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            for tool in CanvasTool::LASSO_TOOLS {
                let selected = state.lasso_tool == tool;
                ui.horizontal(|ui| {
                    let (icon_rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    draw_icon(
                        ui.painter(),
                        icon_rect,
                        lasso_tool_icon(tool),
                        if selected { ACCENT } else { MUTED },
                    );
                    ui.add_space(2.0);
                    if ui.selectable_label(selected, tool.label()).clicked() {
                        state.lasso_tool = tool;
                        activate_tool(state, tool);
                        ui.close();
                    }
                });
            }
            ui.separator();
            ui.label(
                egui::RichText::new(
                    "Hides the normal in the drawn region on the selected layer, group \
                     or selection (whole stack when nothing is selected) through \
                     painted masks, so shapes and their edges go flat.\n\
                     Alt targets the whole stack; Shift draws it back.\n\
                     The Polygon Lasso snaps its corners to the smart guides.\n\
                     Nothing is rasterized.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        });

        ui.add_space(10.0);

        if rail_tool(
            ui,
            state.canvas_tool == CanvasTool::Paint,
            "paint",
            "Paint",
            "Paint (B) · height or mask brush on the 2D canvas",
        )
        .clicked()
        {
            activate_tool(state, CanvasTool::Paint);
        }
        if rail_tool(
            ui,
            state.canvas_tool == CanvasTool::Ruler,
            "ruler",
            "Ruler",
            "Ruler · drag to set an angle, Shift snaps the drawing to it; click again to put it away",
        )
        .clicked()
        {
            toggle_ruler_tool(state);
        }
        ui.add_space(10.0);
        if rail_tool(
            ui,
            state.canvas_pan_tool,
            "pan",
            "Pan",
            "Pan 2D canvas (H, or hold Space)",
        )
        .clicked()
        {
            state.canvas_pan_tool = !state.canvas_pan_tool;
            state.vector_draft = None;
            state.lasso_draft = None;
            state.viewport_tab = ViewportTab::TwoD;
        }
    });

    ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        if rail_tool(
            ui,
            state.viewport_tab == ViewportTab::TwoD,
            "grid2d",
            "2D",
            if state.split_view {
                "Focus the 2D pane"
            } else {
                "2D canvas"
            },
        )
        .clicked()
        {
            state.viewport_tab = ViewportTab::TwoD;
        }
        if rail_tool(
            ui,
            state.viewport_tab == ViewportTab::ThreeD,
            "cube",
            "3D",
            if state.split_view {
                "Focus the 3D pane"
            } else {
                "3D view"
            },
        )
        .clicked()
        {
            state.viewport_tab = ViewportTab::ThreeD;
        }
        ui.add_space(10.0);
    });
}
pub(crate) fn vector_tool_icon(tool: CanvasTool) -> Icon {
    match tool {
        CanvasTool::Pen => Icon::Pen,
        CanvasTool::Cable => Icon::Cable,
        CanvasTool::Node => Icon::NodeEdit,
        CanvasTool::AddAnchor => Icon::AddAnchor,
        CanvasTool::DeleteAnchor => Icon::DeleteAnchor,
        CanvasTool::ConvertAnchor => Icon::ConvertPoint,
        _ => Icon::Pen,
    }
}

/// Glyph for a primitive shape in the Shapes flyout.
pub(crate) fn shape_tool_icon(tool: CanvasTool) -> Icon {
    match tool {
        CanvasTool::Ellipse => Icon::Ellipse,
        CanvasTool::Star => Icon::Star,
        CanvasTool::Polygon => Icon::Polygon,
        _ => Icon::Rectangle,
    }
}

/// Rail glyph for a vector sub-tool, drawn from the editor's icon paths with
/// the same stroke weight as the sprite-derived tiles.
fn rail_vector_glyph(tool: CanvasTool) -> &'static str {
    match tool {
        CanvasTool::Cable => "cable",
        CanvasTool::Node => "node",
        CanvasTool::AddAnchor => "add_anchor",
        CanvasTool::DeleteAnchor => "delete_anchor",
        CanvasTool::ConvertAnchor => "convert_point",
        _ => "pen",
    }
}

/// Glyph for a lasso variant in the rail flyout.
pub(crate) fn lasso_tool_icon(tool: CanvasTool) -> Icon {
    match tool {
        CanvasTool::LassoRect => Icon::Rectangle,
        CanvasTool::LassoEllipse => Icon::Ellipse,
        CanvasTool::LassoPolygon => Icon::LassoPolygon,
        CanvasTool::LassoFree => Icon::LassoFree,
        CanvasTool::LassoLine => Icon::LassoLine,
        _ => Icon::Lasso,
    }
}

/// Rail tile behaviour for the ruler: unlike the other tools it toggles, so a
/// second click puts the tool and its guide away instead of re-arming them.
pub(crate) fn toggle_ruler_tool(state: &mut EditorState) {
    if state.canvas_tool == CanvasTool::Ruler {
        // Off: the guide goes with the tool, so nothing lingers on the canvas.
        dismiss_ruler(state);
        state.canvas_tool = CanvasTool::Select;
        return;
    }
    activate_tool(state, CanvasTool::Ruler);
}

pub(crate) fn activate_tool(state: &mut EditorState, tool: CanvasTool) {
    let previous = state.canvas_tool;
    if state.canvas_tool != tool {
        state.node_selection.clear();
        state.node_hover = None;
    }
    state.canvas_tool = tool;
    state.canvas_drag = None;
    state.clear_snap_guides();
    // Pen and Polygon share the draft. Switching between them drops the draft so
    // a half-drawn Bézier path is never committed as a polygon (or vice versa).
    let keeps_draft = previous == tool && matches!(tool, CanvasTool::Pen | CanvasTool::Polygon);
    if !keeps_draft {
        state.vector_draft = None;
    }
    if !tool.is_lasso_tool() {
        state.lasso_draft = None;
        state.lasso_divide = false;
    }
    if !tool.is_node_edit_tool() {
        state.edge_selection = None;
    }
    if tool.is_vector_tool() {
        state.vector_tool = tool;
    }
    if tool.is_lasso_tool() {
        state.lasso_tool = tool;
    }
    if tool.is_shape_tool() {
        state.shape_tool = tool;
    }
    let status = match tool {
        CanvasTool::Pen => {
            "Pen: click to place nodes; guides snap to nearby vertices, axes and parallel/perpendicular edges (Shift = fixed 45° or the ruler angle); drag for handles, click the first node to close, Enter or double-click ends an open cut line"
                .to_string()
        }
        CanvasTool::Node => NODE_HINT.to_string(),
        CanvasTool::AddAnchor => "Add anchor: click a path segment".to_string(),
        CanvasTool::DeleteAnchor => "Delete anchor: click an anchor".to_string(),
        CanvasTool::ConvertAnchor => "Convert anchor: click to toggle corner/smooth".to_string(),
        CanvasTool::Paint => {
            if state.mask_edit_layer.is_some() {
                "Paint mask: drag on the 2D canvas".to_string()
            } else {
                state.show_height = true;
                "Paint height: drag on the 2D canvas".to_string()
            }
        }
        CanvasTool::Select => {
            "Select and transform shapes · drag to move, Alt+drag (or Alt-drag a layer's grip) to duplicate"
                .to_string()
        }
        CanvasTool::Rectangle => "Rectangle: drag on the 2D canvas".to_string(),
        CanvasTool::Ellipse => "Ellipse: drag on the 2D canvas".to_string(),
        CanvasTool::Star => "Star: drag on the 2D canvas".to_string(),
        CanvasTool::Polygon => {
            "Polygon: click to place corners; click the first corner or press Enter to close · smart guides snap · right-click the Shapes tile for other shapes"
                .to_string()
        }
        CanvasTool::Cable => {
            "Cable: click a vector path (draw it with the Pen) to turn it into a tube · width, height and ends in Properties"
                .to_string()
        }
        CanvasTool::Text => {
            "Text: click the canvas to place live text, then edit content, font, size, tracking and alignment in Properties"
                .to_string()
        }
        CanvasTool::Ruler => {
            "Ruler: drag to set an angle, click to move it; Shift while drawing snaps to it, right-click clears"
                .to_string()
        }
        CanvasTool::LassoRect | CanvasTool::LassoEllipse => {
            format!(
                "{} · {}: drag a box (follows the selection · Alt = whole stack · Shift restores) · right-click the tile for other lassos",
                tool.label(),
                state.lasso_mode.label()
            )
        }
        CanvasTool::LassoPolygon => {
            format!(
                "Polygon lasso · {}: click points (smart guides snap each corner), Enter or click the first point to commit · follows the selection · Alt = whole stack · Shift restores · right-click cancels",
                state.lasso_mode.label()
            )
        }
        CanvasTool::LassoFree => {
            format!(
                "Freehand lasso · {}: drag a loop (follows the selection · Alt = whole stack · Shift restores)",
                state.lasso_mode.label()
            )
        }
        CanvasTool::LassoLine => format!(
            "Line lasso · {}: drag a straight cut · smart guides snap the ends · Shift = 45°/ruler · follows the selection (Alt = whole stack)",
            state.lasso_mode.label()
        ),
    };
    state.status = status;
    state.canvas_pan_tool = false;
    state.viewport_tab = ViewportTab::TwoD;
}

pub(crate) fn status_bar(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    system_stats: &SystemStats,
    bridge: &mut BridgeRuntime,
) {
    let compact = ui.available_width() < 720.0;
    let stats_compact = ui.available_width() < 1000.0;
    let view_label = if state.split_view {
        if state.viewport_tab == ViewportTab::ThreeD {
            "Split · 3D"
        } else {
            "Split · 2D"
        }
    } else {
        state.viewport_tab.label()
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(view_label).size(TEXT_LABEL).color(TEXT))
            .on_hover_text("Active viewport");
        status_separator(ui);
        ui.label(
            egui::RichText::new(format!("{} × {}", state.tex_dims[0], state.tex_dims[1]))
                .size(TEXT_LABEL)
                .color(MUTED),
        );
        if !compact && (state.viewport_tab == ViewportTab::TwoD || state.split_view) {
            status_separator(ui);
            ui.label(
                egui::RichText::new(format!("{:.0}%", state.zoom * 100.0))
                    .size(TEXT_LABEL)
                    .color(MUTED),
            );
            status_separator(ui);
            ui.label(
                egui::RichText::new(if state.mask_edit_layer.is_some() {
                    "Mask"
                } else if state.show_height {
                    "Height"
                } else {
                    "Normal"
                })
                .size(TEXT_LABEL)
                .color(TEXT),
            );
            status_separator(ui);
            ui.label(
                egui::RichText::new(state.canvas_tool.label())
                    .size(TEXT_LABEL)
                    .color(MUTED),
            );
        }
        if !compact {
            status_separator(ui);
            let status = if state.dirty {
                "Updating preview..."
            } else if state.status.is_empty() {
                "Ready"
            } else {
                state.status.as_str()
            };
            let color = if state.status.starts_with("Export failed") {
                DANGER
            } else if state.dirty {
                ACCENT
            } else if status == "Ready" {
                MUTED
            } else {
                TEXT
            };
            let width = (ui.available_width() * 0.42).max(80.0);
            ui.add_sized(
                [width, 18.0],
                egui::Label::new(egui::RichText::new(status).size(TEXT_LABEL).color(color))
                    .truncate(),
            )
            .on_hover_text(status);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (color, text, hint) = if state.using_gpu {
                (
                    SUCCESS,
                    if state.integrate_height {
                        "GPU · FFT".to_string()
                    } else {
                        "GPU".to_string()
                    },
                    "The preview is evaluated on the GPU. Raster, smoothed, text, \
                     photo and painted-mask layers are precomputed once on the CPU \
                     and sampled by the shader."
                        .to_string(),
                )
            } else {
                let reason_labels: Vec<&str> = state
                    .gate_rejects
                    .iter()
                    .map(|reason| reason.label())
                    .collect();
                let text = if compact || reason_labels.is_empty() {
                    format!("CPU · {:.0} ms", state.last_eval_ms)
                } else {
                    format!(
                        "CPU · {:.0} ms · {}",
                        state.last_eval_ms,
                        reason_labels.len()
                    )
                };
                let hint = if reason_labels.is_empty() {
                    "CPU evaluator: the GPU scene could not be packed. Open the \
                     performance HUD for timing and path details."
                        .to_string()
                } else {
                    let mut hint = String::from("CPU fallback reasons:\n");
                    for reason in &state.gate_rejects {
                        hint.push_str("• ");
                        hint.push_str(reason.label());
                        hint.push_str(" — ");
                        hint.push_str(reason.hint());
                        hint.push('\n');
                    }
                    hint.trim_end().to_string()
                };
                (WARNING, text, hint)
            };
            ui.label(egui::RichText::new(text).size(TEXT_LABEL).color(color))
                .on_hover_text(hint);
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.0, color);

            status_separator(ui);
            // The bridge is a background service: without a chip here nothing
            // on screen hints that it exists.
            let (label, color, hint) = bridge_status(bridge);
            if ui
                .selectable_label(
                    state.show_bridge,
                    egui::RichText::new(label).size(TEXT_LABEL).color(color),
                )
                .on_hover_text(hint)
                .clicked()
            {
                state.show_bridge = true;
            }

            status_separator(ui);
            if ui
                .selectable_label(
                    state.show_perf_hud,
                    egui::RichText::new("Perf").size(TEXT_LABEL),
                )
                .on_hover_text(
                    "Show evaluation timings, cache/undo budgets, and GPU fallback reasons",
                )
                .clicked()
            {
                state.show_perf_hud = !state.show_perf_hud;
            }

            if let Some(gpu) = &system_stats.gpu {
                status_separator(ui);
                let utilization = gpu
                    .utilization
                    .map(|value| format!("{value:.0}%"))
                    .unwrap_or_else(|| "n/a".to_string());
                let vram_percent = if gpu.total_bytes > 0 {
                    ((gpu.used_bytes as f64 / gpu.total_bytes as f64) * 100.0).round() as u32
                } else {
                    0
                };
                let (vram_color, advice) = resource_color(vram_percent, "VRAM");
                if stats_compact {
                    ui.label(
                        egui::RichText::new(format!("GPU {utilization}"))
                            .size(TEXT_LABEL)
                            .color(MUTED),
                    )
                    .on_hover_text("GPU utilization");
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "GPU {utilization} · VRAM {}/{} GB",
                            format_gb(gpu.used_bytes),
                            format_gb(gpu.total_bytes)
                        ))
                        .size(TEXT_LABEL)
                        .color(vram_color),
                    )
                    .on_hover_text(advice);
                }
            } else if !stats_compact {
                status_separator(ui);
                ui.label(egui::RichText::new("GPU n/a").size(TEXT_LABEL).color(MUTED));
            }

            if let Some(used) = system_stats.process_ram {
                status_separator(ui);
                let total = system_stats.system_ram.unwrap_or(0);
                let percent = ram_percent(used, total);
                let (ram_color, advice) = resource_color(percent, "RAM");
                // Say "App": the number is this process's working set, never
                // system-wide usage. The total only appears in the tooltip so
                // the bar cannot be mistaken for an OS memory meter.
                let label = if stats_compact {
                    format!("App {}", format_memory_short(used))
                } else {
                    format!("App RAM {}", format_memory(used))
                };
                let response =
                    ui.label(egui::RichText::new(label).size(TEXT_LABEL).color(ram_color));
                if percent >= 85 {
                    // Icon + text, so the warning never relies on colour alone.
                    let (icon_rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    draw_warning_glyph(ui.painter(), icon_rect, ram_color);
                }
                let detail = if total > 0 {
                    format!(
                        "This app's resident memory (working set): {}\n{percent}% of the {} GB \
                         installed system RAM. The figure is N-shade only, not system-wide usage.\n\
                         {advice}",
                        format_memory(used),
                        format_gb(total)
                    )
                } else {
                    format!(
                        "This app's resident memory (working set): {}\n{advice}",
                        format_memory(used)
                    )
                };
                let _ = response.on_hover_text(detail);
            }
        });
    });
}

fn format_gb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_073_741_824.0)
}

/// App memory in the wide status bar: MB below a gigabyte, otherwise GB.
fn format_memory(bytes: u64) -> String {
    let bytes = bytes as f64;
    if bytes >= 1_073_741_824.0 {
        format!("{:.1} GB", bytes / 1_073_741_824.0)
    } else {
        format!("{:.0} MB", bytes / 1_048_576.0)
    }
}

/// App memory in the compact status bar, where horizontal space is scarce.
fn format_memory_short(bytes: u64) -> String {
    let bytes = bytes as f64;
    if bytes >= 1_073_741_824.0 {
        format!("{:.1}G", bytes / 1_073_741_824.0)
    } else {
        format!("{:.0}M", bytes / 1_048_576.0)
    }
}

/// Share of physical RAM used by this process, for the warning colour.
fn ram_percent(used: u64, total: u64) -> u32 {
    if total == 0 {
        return 0;
    }
    ((used as f64 / total as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u32
}

/// Resource readouts turn amber at 85% and red at 95%, always with advice so
/// the colour is never the only signal (WCAG 1.4.1).
fn resource_color(percent: u32, name: &str) -> (egui::Color32, String) {
    if percent >= 95 {
        (
            DANGER,
            format!("{name} is almost full. Close other apps or export at a smaller size."),
        )
    } else if percent >= 85 {
        (
            WARNING,
            format!("{name} is getting high. Consider closing other apps before a large bake."),
        )
    } else {
        (MUTED, format!("{name} in use"))
    }
}

/// Small filled triangle used beside critical resource warnings.
fn draw_warning_glyph(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let c = rect.center();
    let points = vec![
        egui::pos2(c.x, c.y - 5.0),
        egui::pos2(c.x + 5.5, c.y + 4.5),
        egui::pos2(c.x - 5.5, c.y + 4.5),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ruler() -> RulerState {
        RulerState {
            origin: Vec2::splat(10.0),
            angle: 0.5,
        }
    }

    #[test]
    fn the_ruler_tile_turns_the_tool_and_its_guide_off() {
        let mut state = EditorState {
            canvas_tool: CanvasTool::Ruler,
            ruler: Some(ruler()),
            ..Default::default()
        };
        toggle_ruler_tool(&mut state);
        assert_eq!(
            state.canvas_tool,
            CanvasTool::Select,
            "the tile toggles off"
        );
        assert!(state.ruler.is_none(), "the guide goes with it");
        assert!(state.status.contains("Ruler cleared"));

        // Clicking again arms the ruler again, without a guide until dragged.
        toggle_ruler_tool(&mut state);
        assert_eq!(state.canvas_tool, CanvasTool::Ruler);
        assert!(state.ruler.is_none());
    }

    #[test]
    fn the_ruler_tile_activates_the_tool_from_another_tool() {
        let mut state = EditorState {
            canvas_tool: CanvasTool::Pen,
            ruler: Some(ruler()),
            ..Default::default()
        };
        toggle_ruler_tool(&mut state);
        assert_eq!(state.canvas_tool, CanvasTool::Ruler);
        assert!(state.ruler.is_some(), "the existing guide is kept");
    }

    #[test]
    fn the_shapes_tile_remembers_the_last_shape() {
        let mut state = EditorState::default();
        activate_tool(&mut state, CanvasTool::Polygon);
        assert_eq!(state.canvas_tool, CanvasTool::Polygon);
        assert_eq!(state.shape_tool, CanvasTool::Polygon);

        // Leaving the shape group keeps the remembered shape for the tile.
        activate_tool(&mut state, CanvasTool::Select);
        assert_eq!(state.shape_tool, CanvasTool::Polygon);

        activate_tool(&mut state, CanvasTool::Star);
        assert_eq!(state.shape_tool, CanvasTool::Star);
        assert!(CanvasTool::SHAPE_TOOLS
            .iter()
            .all(|tool| tool.is_shape_tool()));
        assert!(!CanvasTool::Select.is_shape_tool());
        assert!(!CanvasTool::Text.is_shape_tool());
    }

    #[test]
    fn cable_lives_in_the_vector_flyout() {
        assert!(CanvasTool::Cable.is_vector_tool());
        assert!(!CanvasTool::Cable.is_shape_tool());
        assert!(CanvasTool::VECTOR_TOOLS.contains(&CanvasTool::Cable));
        assert!(!CanvasTool::SHAPE_TOOLS.contains(&CanvasTool::Cable));
    }

    #[test]
    fn switching_between_pen_and_polygon_drops_the_draft() {
        let mut state = EditorState {
            canvas_tool: CanvasTool::Pen,
            vector_draft: Some(VectorDraft {
                nodes: vec![Node::corner(Vec2::ZERO)],
                ..Default::default()
            }),
            ..Default::default()
        };
        activate_tool(&mut state, CanvasTool::Polygon);
        assert!(
            state.vector_draft.is_none(),
            "a pen draft must not become a polygon"
        );
    }

    #[test]
    fn memory_labels_show_the_app_usage() {
        const MIB: u64 = 1_048_576;
        const GIB: u64 = 1_073_741_824;
        assert_eq!(format_memory(512 * MIB), "512 MB");
        assert_eq!(format_memory(GIB * 3 / 2), "1.5 GB");
        assert_eq!(format_memory_short(512 * MIB), "512M");
        assert_eq!(format_memory_short(GIB * 3 / 2), "1.5G");
        assert_eq!(ram_percent(GIB, 8 * GIB), 13);
        assert_eq!(ram_percent(0, 0), 0);
        assert_eq!(ram_percent(8 * GIB, 4 * GIB), 100);
    }
}
