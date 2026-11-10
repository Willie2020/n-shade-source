//! Blender bridge window: the one place that explains and controls the link.
//!
//! The bridge is a background service, so nothing about it is visible until
//! something asks.  The status bar carries a chip that opens this window, and
//! the window carries both the switch and the three steps needed on the Blender
//! side, so "how does this connect?" never needs a manual.

use super::*;

/// Every map the document side can serve, in menu order.
pub(crate) use crate::bridge::DOCUMENT_MAPS;

/// Chip shown in the status bar: (label, colour, tooltip).
pub(crate) fn bridge_status(bridge: &BridgeRuntime) -> (&'static str, egui::Color32, String) {
    if bridge.connected() {
        (
            "Blender · live",
            SUCCESS,
            format!(
                "Blender is connected to 127.0.0.1:{}.\n\
                 Maps follow your edits, or arrive when you press Send to Blender.",
                bridge.port
            ),
        )
    } else if bridge.listening() {
        (
            "Blender · ready",
            ACCENT,
            format!(
                "Waiting for Blender on 127.0.0.1:{}.\n\
                 Open the N-shade panel in Blender's 3D Viewport sidebar (N) and it \
                 pairs itself.\nClick for the connection guide.",
                bridge.port
            ),
        )
    } else {
        (
            "Blender · off",
            MUTED,
            "The Blender bridge is off. Click to open it and switch it on.".to_string(),
        )
    }
}

/// Popup that holds the switch, the live status and the Blender-side steps.
pub(crate) fn blender_window(
    ctx: &egui::Context,
    state: &mut EditorState,
    bridge: &mut BridgeRuntime,
    pbr: &PbrRuntime,
) {
    let mut open = state.show_bridge;
    egui::Window::new("Blender bridge")
        .id(egui::Id::new("ns_blender_bridge"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(430.0)
        .default_pos(egui::pos2(120.0, 110.0))
        .show(ctx, |ui| {
            ui.set_width(430.0);
            status_block(ui, bridge);
            ui.add_space(4.0);
            switch_block(ui, state, bridge);
            ui.separator();
            guide_block(ui, bridge);
            ui.separator();
            send_block(ui, state, bridge, pbr);
        });
    state.show_bridge = open;
}

fn status_block(ui: &mut egui::Ui, bridge: &BridgeRuntime) {
    let (label, color, _hint) = bridge_status(bridge);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.5, color);
        ui.label(egui::RichText::new(label).strong().color(color));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if bridge.listening() {
                ui.label(
                    egui::RichText::new(format!(
                        "{} frame(s) · {:.1} MB",
                        bridge.frames,
                        bridge.bytes as f64 / (1024.0 * 1024.0)
                    ))
                    .small()
                    .color(MUTED),
                );
            }
        });
    });
    if let Some(client) = &bridge.client {
        ui.label(
            egui::RichText::new(format!("Client: {client}"))
                .small()
                .color(MUTED),
        );
    }
    ui.label(
        egui::RichText::new(&bridge.status)
            .small()
            .color(if bridge.listening() { MUTED } else { FAINT }),
    );
}

fn switch_block(ui: &mut egui::Ui, state: &mut EditorState, bridge: &mut BridgeRuntime) {
    let mut enabled = bridge.enabled;
    if m3_checkbox(ui, &mut enabled, "Serve maps to Blender").changed() {
        bridge.enabled = enabled;
        state.status = if enabled {
            "Blender bridge starting on 127.0.0.1".into()
        } else {
            "Blender bridge stopped".into()
        };
    }
    ui.horizontal(|ui| {
        ui.add_enabled_ui(bridge.enabled, |ui| {
            ui.label(egui::RichText::new("Port").size(TEXT_SMALL).color(MUTED));
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
        });
        if bridge.listening() {
            ui.label(
                egui::RichText::new(format!(
                    "Pairing file: {}",
                    crate::bridge::pairing_path_display()
                ))
                .size(TEXT_CAPTION)
                .color(FAINT),
            );
        }
    });
}

fn guide_block(ui: &mut egui::Ui, bridge: &BridgeRuntime) {
    subheading(ui, "Connect Blender 4.3 or newer");
    let addon = addon_dir();
    ui.label(
        egui::RichText::new("1.  Preferences → Get Extensions → Install from Disk")
            .size(TEXT_LABEL)
            .color(TEXT),
    );
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        match &addon {
            Some(path) => {
                if ui.add(ghost_button("Copy add-on folder")).clicked() {
                    ui.ctx().copy_text(path.display().to_string());
                }
                if ui.add(ghost_button("Open folder")).clicked() {
                    open_folder(path);
                }
                ui.label(
                    egui::RichText::new(
                        path.file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| "n_shade_bridge".into()),
                    )
                    .size(TEXT_CAPTION)
                    .color(FAINT),
                );
            }
            None => {
                ui.label(
                    egui::RichText::new("add-on folder not found next to the app")
                        .size(TEXT_CAPTION)
                        .color(WARNING),
                );
            }
        }
    });
    ui.label(
        egui::RichText::new("2.  Enable “N-shade Bridge”, then 3D Viewport → N → N-shade")
            .size(TEXT_LABEL)
            .color(TEXT),
    );
    ui.label(
        egui::RichText::new("3.  The panel pairs and connects on its own while this says ready")
            .size(TEXT_LABEL)
            .color(if bridge.listening() { TEXT } else { MUTED }),
    );
    if !bridge.listening() {
        ui.label(
            egui::RichText::new("Switch the bridge on above first.")
                .size(TEXT_CAPTION)
                .color(WARNING),
        );
    }
    if let Some(client) = &bridge.client {
        ui.label(
            egui::RichText::new(format!("Connected: {client}"))
                .size(TEXT_CAPTION)
                .color(SUCCESS),
        );
    }
}

fn send_block(
    ui: &mut egui::Ui,
    state: &EditorState,
    bridge: &mut BridgeRuntime,
    pbr: &PbrRuntime,
) {
    subheading(ui, "Send maps now");
    ui.label(
        egui::RichText::new(
            "Blender pulls on its own. This pushes on demand, even in manual mode.",
        )
        .size(TEXT_CAPTION)
        .color(MUTED),
    );
    let live = bridge.listening();
    let mut all: Vec<MapId> = DOCUMENT_MAPS.to_vec();
    all.extend(crate::bridge::pbr_available(pbr));
    let mut selected = crate::bridge::document_selection(state);
    selected.extend(crate::bridge::pbr_selection(pbr));

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                live && !all.is_empty(),
                ghost_button(format!("Send all ({})", all.len())),
            )
            .on_hover_text("Every map this document and the PBR panel can produce")
            .clicked()
        {
            bridge.publish(all);
        }
        if ui
            .add_enabled(
                live && !selected.is_empty(),
                ghost_button(format!("Send selected ({})", selected.len())),
            )
            .on_hover_text("The maps ticked in the export card and the PBR panel")
            .clicked()
        {
            bridge.publish(selected);
        }
    });
    if !live {
        ui.label(
            egui::RichText::new("Switch the bridge on to send.")
                .size(TEXT_CAPTION)
                .color(FAINT),
        );
    }
}

/// `blender/n_shade_bridge`, next to the executable or in the source tree.
fn addon_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    // Portable layout: N-shade/blender/n_shade_bridge next to the executable.
    candidates.push(dir.join("blender").join("n_shade_bridge"));
    candidates.push(dir.join("n_shade_bridge"));
    // Running out of a source tree (target/release, target/debug): walk up a
    // few levels instead of baking the build machine's absolute path into the
    // binary with env!("CARGO_MANIFEST_DIR").
    for ancestor in dir.ancestors().skip(1).take(3) {
        candidates.push(ancestor.join("blender").join("n_shade_bridge"));
    }
    candidates.into_iter().find(|path| path.is_dir())
}

fn open_folder(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let command = "explorer";
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = "xdg-open";
    let _ = std::process::Command::new(command).arg(path).spawn();
}
