//! Mesh import and preview-appearance dock content.

use super::*;

pub(crate) struct MeshPanel;

impl Panel for MeshPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        mesh_card(ui, frame.state, frame.mesh_import);
    }
}

pub(crate) fn mesh_card(ui: &mut egui::Ui, state: &mut EditorState, mesh_import: &MeshImport) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        ui.horizontal(|ui| {
            section_title(ui, Icon::Cube, "Preview Mesh");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Fit, false, true, "Reset the 3D camera").clicked() {
                    reset_camera(state);
                }
            });
        });
        ui.add_space(4.0);

        let import_title = state.imported_mesh.as_deref().unwrap_or("Import mesh...");
        let import_hint = if state.imported_mesh.is_some() {
            "Click to replace · glTF/GLB/FBX/OBJ/DAE/USD"
        } else {
            "glTF/GLB/FBX/OBJ/DAE/USD with UVs · click or drop a file"
        };
        if dashed_drop_zone(ui, Icon::Upload, import_title, import_hint)
            .on_hover_text("Import glTF/GLB/FBX/OBJ/DAE/USD, or drop a mesh on the window")
            .clicked()
        {
            open_mesh_dialog(state);
        }

        ui.add_space(2.0);
        ui.label(egui::RichText::new("Mesh").size(TEXT_SMALL).color(MUTED));
        egui::ComboBox::from_id_salt("preview_mesh")
            .width(ui.available_width())
            .selected_text(state.preview_shape.label())
            .show_ui(ui, |ui| {
                for shape in PreviewShape::BUILTIN {
                    ui.selectable_value(&mut state.preview_shape, shape, shape.label());
                }
                if state.imported_mesh.is_some() {
                    ui.selectable_value(
                        &mut state.preview_shape,
                        PreviewShape::Imported,
                        PreviewShape::Imported.label(),
                    );
                }
            });

        if slider_row(ui, "Tiling", &mut state.tiling, 1.0..=4.0, 0.01, 2) {
            state.mark_eval_dirty();
        }

        ui.add_space(3.0);
        let uv_ready = state.preview_shape == PreviewShape::Imported && state.uv_tex_id.is_some();
        let uv_label = if state.uv_overlay_triangles > 0 {
            format!("UV overlay 3D ({} tris)", state.uv_overlay_triangles)
        } else {
            "UV overlay in 3D view".to_string()
        };
        ui.add_enabled(
            uv_ready,
            egui::Checkbox::new(&mut state.show_uv_overlay, uv_label),
        )
        .on_disabled_hover_text("Import a mesh with UV coordinates first");
        ui.add_enabled(
            uv_ready,
            egui::Checkbox::new(&mut state.show_uv_overlay_2d, "UV overlay on 2D canvas"),
        )
        .on_disabled_hover_text("Import a mesh with UV coordinates first");
        if uv_ready {
            slider_row(
                ui,
                "UV opacity",
                &mut state.uv_overlay_opacity,
                0.05..=1.0,
                0.01,
                2,
            );
        }

        if let Some(name) = &state.imported_mesh {
            let width = ui.available_width();
            ui.add_sized(
                [width, 15.0],
                egui::Label::new(egui::RichText::new(name).size(TEXT_SMALL).color(MUTED))
                    .truncate(),
            );
        }
        if !state.import_status.is_empty() {
            let width = ui.available_width();
            ui.add_sized(
                [width, 15.0],
                egui::Label::new(
                    egui::RichText::new(&state.import_status)
                        .size(TEXT_SMALL)
                        .color(MUTED),
                )
                .truncate(),
            );
        }
        if mesh_import.udim.enabled {
            let width = ui.available_width();
            ui.add_sized(
                [width, 15.0],
                egui::Label::new(
                    egui::RichText::new(mesh_import.udim.summary())
                        .size(TEXT_SMALL)
                        .color(ACCENT),
                )
                .truncate(),
            );
        }
        if uv_ready {
            ui.add_space(3.0);
            let bake = egui::Button::new(egui::RichText::new("Bake normals...").size(TEXT_LABEL))
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0_f32, OUTLINE_VARIANT))
                .corner_radius(RADIUS_SM);
            if ui.add_sized([ui.available_width(), 24.0], bake).clicked() {
                state.right_tab = RightTab::Bake;
                state.show_right_dock = true;
            }
        }
    });

    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        section_title(ui, Icon::Settings, "Appearance");
        ui.add_space(4.0);

        labeled_combo(
            ui,
            "Material",
            "material_preset",
            state.material_preset.label(),
            |ui| {
                for preset in MaterialPreset::ALL {
                    ui.selectable_value(&mut state.material_preset, preset, preset.label());
                }
            },
        );
        labeled_combo(
            ui,
            "Lighting",
            "lighting_preset",
            state.lighting_preset.label(),
            |ui| {
                for preset in LightingPreset::ALL {
                    ui.selectable_value(&mut state.lighting_preset, preset, preset.label());
                }
            },
        );
        labeled_combo(
            ui,
            "Environment",
            "environment_preset",
            state.environment_preset.label(),
            |ui| {
                for preset in EnvironmentPreset::ALL {
                    ui.selectable_value(&mut state.environment_preset, preset, preset.label());
                }
            },
        );
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            toggle_switch(ui, &mut state.auto_rotate);
            ui.label(egui::RichText::new("Auto-rotate preview").size(TEXT_LABEL));
        });
    });

    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        section_title(ui, Icon::Rotate, "Camera");
        ui.add_space(4.0);

        let mut yaw = state.orbit_yaw.to_degrees();
        if slider_row(ui, "Yaw", &mut yaw, -180.0..=180.0, 1.0, 0) {
            state.orbit_yaw = yaw.to_radians();
        }
        let mut pitch = state.orbit_pitch.to_degrees();
        if slider_row(ui, "Pitch", &mut pitch, -75.0..=75.0, 1.0, 0) {
            state.orbit_pitch = pitch.to_radians();
        }
        slider_row(
            ui,
            "Distance",
            &mut state.orbit_distance,
            1.8..=8.0,
            0.05,
            1,
        );
        ui.add_space(3.0);
        if text_button(ui, "Reset camera", ui.available_width(), 28.0, false).clicked() {
            reset_camera(state);
        }
    });
}
