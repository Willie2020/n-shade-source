//! Settings window.

use super::*;
pub(crate) fn settings_window(
    ctx: &egui::Context,
    state: &mut EditorState,
    bridge: &mut BridgeRuntime,
) {
    let mut open = state.show_settings;
    let mut dirty = false;
    // The sections outgrow a 780 px display, especially after Recovery was
    // added. Cap the content and scroll rather than letting the last rows fall
    // off the bottom of the screen.
    let max_height = (ctx.viewport_rect().height() - 120.0).max(240.0);
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        // An auto-sized window settles on the first scroll-area frame and
        // clips the next section mid-row. Fill the allowed height instead so
        // the whole panel scrolls as one list.
        .default_height(max_height)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // View toggles live in the View menu; Settings owns the
                    // preferences that are not part of the current viewport.
                    ui.label(
                        egui::RichText::new(
                            "View overlays, zoom, camera and dock visibility are in the View menu.",
                        )
                        .small()
                        .color(MUTED),
                    );
                    ui.separator();
                    ui.label(egui::RichText::new("Input").strong());
                    ui.checkbox(
                        &mut state.touchpad_pan,
                        "Two-finger touchpad scroll pans the 2D canvas",
                    )
                    .on_hover_text(
                        "Off: two-finger scrolling zooms instead. Ctrl/Cmd+wheel and pinch \
                         gestures always zoom; Shift+scroll pans sideways.",
                    );
                    ui.checkbox(&mut state.smart_guides, "Smart guides")
                        .on_hover_text(
                            "Snap the pen, moved shapes and duplicate drags to nearby \
                             vertices, edges, centers, axes and the canvas. Shift still fixes \
                             pen angles; Ctrl/Cmd disables snapping during a drag.",
                        );
                    pen_toggle_row(ui, state);
                    let pen_label = if state.pen_detected {
                        "Pressure-capable pen detected"
                    } else {
                        "No pen seen yet — touch the canvas with the stylus"
                    };
                    ui.label(egui::RichText::new(pen_label).small().color(MUTED));
                    if !state.pen_detected {
                        ui.label(
                            egui::RichText::new(
                                "If the tablet driver has a Windows Ink switch, turn it on: \
                                 Wintab-only mode cannot report pressure.",
                            )
                            .small()
                            .color(FAINT),
                        );
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Recovery").strong());
                    ui.checkbox(&mut state.autosave_enabled, "Autosave unsaved work")
                        .on_hover_text(format!(
                            "Writes a recovery snapshot every {} seconds while the project \
                             has unsaved changes. Snapshots are written atomically, so a \
                             crash or power loss cannot corrupt them or the project file.",
                            crate::autosave::INTERVAL_SECS as u32
                        ));
                    ui.add_enabled_ui(state.autosave_enabled, |ui| {
                        ui.add(
                            egui::Slider::new(
                                &mut state.autosave_backups,
                                crate::autosave::MIN_BACKUPS..=crate::autosave::MAX_BACKUPS,
                            )
                            .text("Backups"),
                        )
                        .on_hover_text(
                            "How many recovery snapshots to keep. Older snapshots rotate \
                             out once the limit is reached.",
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "Saved to {}",
                                crate::autosave::autosave_dir(state.project_path.as_deref())
                                    .display()
                            ))
                            .small()
                            .color(MUTED),
                        );
                        if let Some(error) = &state.autosave_error {
                            ui.label(egui::RichText::new(error).small().color(WARNING));
                        } else if state.autosave_status.is_empty() {
                            ui.label(egui::RichText::new("No autosave yet").small().color(MUTED));
                        } else {
                            ui.label(
                                egui::RichText::new(&state.autosave_status)
                                    .small()
                                    .color(MUTED),
                            );
                        }
                    });
                    ui.separator();
                    ui.label(egui::RichText::new("Evaluator").strong());
                    if ui
                        .checkbox(
                            &mut state.integrate_height,
                            "Derive preview height from normals (GPU FFT)",
                        )
                        .on_hover_text(
                            "Integrates the evaluated normal map into the 2D height view \
                             and preview with a GPU Frankot–Chellappa FFT. Export height \
                             has its own Match height to normals option.",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                    ui.separator();
                    ui.label(if state.using_gpu {
                        "GPU compute"
                    } else {
                        "CPU fallback"
                    });
                    if state.integrate_height {
                        ui.label(
                            egui::RichText::new(if state.using_gpu {
                                "Height: GPU FFT integration"
                            } else {
                                "Height: CPU FFT integration"
                            })
                            .small()
                            .color(ACCENT),
                        );
                    }
                    ui.label(format!("Last evaluation: {:.1} ms", state.last_eval_ms));
                    ui.label(
                        egui::RichText::new("256 MiB cache budget with streaming fallback")
                            .small()
                            .color(MUTED),
                    );
                    ui.separator();
                    ui.label(egui::RichText::new("3D Preview").strong());
                    ui.label(
                        egui::RichText::new(
                            "Height and tessellation shape the 3D preview only; the document \
                             and exports are unchanged.",
                        )
                        .small()
                        .color(MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Height").size(TEXT_SMALL).color(MUTED))
                            .on_hover_text(
                                "How the evaluated height map is applied to the preview surface: \
                                 Relief and Occlusion use parallax mapping, Off keeps a purely \
                                 normal-mapped surface.",
                            );
                        let mut mode = state.preview_parallax;
                        egui::ComboBox::from_id_salt("preview_parallax")
                            .width(ui.available_width())
                            .selected_text(mode.label())
                            .show_ui(ui, |ui| {
                                for option in PreviewParallax::ALL {
                                    ui.selectable_value(&mut mode, option, option.label());
                                }
                            });
                        if mode != state.preview_parallax {
                            state.preview_parallax = mode;
                            // The CPU fallback only computes height when a
                            // consumer asks for it, so request a fresh frame.
                            dirty = true;
                        }
                    });
                    if state.preview_parallax != PreviewParallax::Off {
                        ui.add(
                            egui::Slider::new(&mut state.preview_height_scale, 0.0..=4.0)
                                .text("Height scale"),
                        )
                        .on_hover_text(
                            "Multiplies the document height scale when converting the height \
                             map into parallax depth. 0 flattens the surface; 2 doubles it.",
                        );
                    }
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Tessellation")
                                .size(TEXT_SMALL)
                                .color(MUTED),
                        )
                        .on_hover_text(
                            "Subdivides the built-in preview meshes. Higher levels smooth the \
                             rounded cube, cylinder and plane silhouettes at grazing angles. \
                             Imported meshes keep their own topology.",
                        );
                        egui::ComboBox::from_id_salt("preview_tessellation")
                            .width(ui.available_width())
                            .selected_text(state.preview_tessellation.label())
                            .show_ui(ui, |ui| {
                                for option in PreviewTessellation::ALL {
                                    ui.selectable_value(
                                        &mut state.preview_tessellation,
                                        option,
                                        option.label(),
                                    );
                                }
                            });
                    });
                    ui.separator();
                    bridge_section(ui, state, bridge);
                });
        });
    state.show_settings = open;
    if dirty {
        state.mark_eval_dirty();
    }
}

/// Blender bridge controls: on/off, port, and what the listener is doing.
fn bridge_section(ui: &mut egui::Ui, state: &mut EditorState, bridge: &mut BridgeRuntime) {
    ui.label(egui::RichText::new("Blender Bridge").strong());
    ui.checkbox(&mut bridge.enabled, "Serve maps to Blender")
        .on_hover_text(
            "Opens a loopback listener that the N-shade add-on inside Blender pulls \
             normal, height and PBR maps from. Nothing is uploaded and the socket \
             only listens on 127.0.0.1. Sweep a slider here and the Blender \
             viewport follows within a frame.",
        );
    ui.add_enabled_ui(bridge.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Port").small().color(MUTED));
            let mut port = i32::from(bridge.port);
            if ui
                .add(
                    egui::DragValue::new(&mut port)
                        .range(1024..=65_535)
                        .speed(1.0),
                )
                .on_hover_text("Changing the port restarts the listener")
                .changed()
            {
                bridge.port = port.clamp(1024, 65_535) as u16;
            }
            if bridge.listening() && bridge.connected() {
                ui.label(
                    egui::RichText::new("client connected")
                        .small()
                        .color(ACCENT),
                );
            }
        });
        ui.label(egui::RichText::new(&bridge.status).small().color(MUTED));
        if bridge.listening() {
            ui.label(
                egui::RichText::new(format!(
                    "{} frame(s), {:.1} MB served",
                    bridge.frames,
                    bridge.bytes as f64 / (1024.0 * 1024.0)
                ))
                .small()
                .color(FAINT),
            );
            ui.label(
                egui::RichText::new(format!(
                    "Pairing file: {}",
                    crate::bridge::pairing_path_display()
                ))
                .small()
                .color(FAINT),
            );
        }
    });
    ui.label(
        egui::RichText::new(
            "Blender 4.3+: install blender/n_shade_bridge, then open the 3D Viewport \
             sidebar (N) → N-shade. The panel pairs itself.",
        )
        .small()
        .color(FAINT),
    );
    if ui.add(ghost_button("How to connect…")).clicked() {
        state.show_bridge = true;
    }
}
