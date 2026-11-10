//! Normal-map bake dock: high-to-low projection and low-poly bevel shading.

use std::path::PathBuf;

use super::*;
use crate::bake::default_output_path;
use ns_bake::{BakeMode, BakeSpace};

pub(crate) struct BakePanel;

impl Panel for BakePanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        bake_panel(ui, frame);
    }
}

pub(crate) fn bake_panel(ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
    let state = &mut *frame.state;
    let doc = &mut *frame.doc;
    let mesh_import = frame.mesh_import;
    let bake = &mut *frame.bake;

    let low_ready =
        mesh_import.ready && mesh_import.uv_triangles > 0 && mesh_import.source_path.is_some();

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        section_title(ui, Icon::Bake, "Normal Baker");
        ui.add_space(4.0);

        // Keep both mode labels on one line. On a minimum-width dock they do
        // not fit side by side, so stack them full-width instead of clipping.
        let spacing = 4.0;
        let button_width = ((ui.available_width() - spacing) / 2.0).max(70.0);
        let font = egui::FontId::proportional(TEXT_SMALL);
        let widest = BakeMode::ALL
            .iter()
            .map(|mode| {
                ui.painter()
                    .layout_no_wrap(mode.label().to_owned(), font.clone(), TEXT)
                    .size()
                    .x
            })
            .fold(0.0_f32, f32::max);
        let min_button_width = widest + 2.0 * ui.spacing().button_padding.x + 2.0;
        let mut mode_button = |ui: &mut egui::Ui, mode: BakeMode, width: f32| {
            let selected = bake.settings.mode == mode;
            let button = egui::Button::new(
                egui::RichText::new(mode.label())
                    .size(TEXT_SMALL)
                    .color(if selected { ON_PRIMARY } else { TEXT }),
            )
            .fill(if selected {
                PRIMARY_CONTAINER
            } else {
                INPUT_BG
            })
            // Keep one neutral border in both states: an accent border would
            // make the selected chip's fill reach the outer edge and read as
            // larger than its neighbours.
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .corner_radius(4);
            // Match the theme's control height: a smaller request lets the
            // button overflow its allocated row frame.
            let height = ui.spacing().interact_size.y;
            if ui.add_sized([width, height], button).clicked() {
                bake.settings.mode = mode;
            }
        };
        if button_width >= min_button_width {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = spacing;
                for mode in BakeMode::ALL {
                    mode_button(ui, mode, button_width);
                }
            });
        } else {
            for mode in BakeMode::ALL {
                mode_button(ui, mode, ui.available_width());
                ui.add_space(2.0);
            }
        }
        ui.label(
            egui::RichText::new(match bake.settings.mode {
                BakeMode::HighToLow => {
                    "Projects a high-poly surface onto the low-poly UVs with two-sided cage rays."
                }
                BakeMode::Bevel => {
                    "Synthesizes rounded-edge normals from the low-poly geometry only."
                }
            })
            .size(TEXT_CAPTION)
            .color(MUTED),
        );

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Low-poly mesh")
                .size(TEXT_SMALL)
                .color(MUTED),
        );
        if mesh_import.loading {
            ui.label(
                egui::RichText::new("Importing...")
                    .size(TEXT_SMALL)
                    .color(ACCENT),
            );
        } else {
            // Same drop zone as the Mesh and Lightmap tabs, so importing a
            // low-poly mesh looks and behaves the same wherever it appears.
            let mesh_title = state
                .imported_mesh
                .clone()
                .unwrap_or_else(|| "Import low-poly mesh".to_string());
            let mesh_subtitle = if state.imported_mesh.is_some() {
                format!(
                    "{} tris · {} UV tris · click to replace",
                    mesh_import.triangles, mesh_import.uv_triangles
                )
            } else {
                "OBJ, FBX, glTF/GLB or USD with UVs · click or drop a file".to_string()
            };
            if dashed_drop_zone(ui, Icon::Upload, &mesh_title, &mesh_subtitle)
                .on_hover_text("Import the low-poly mesh the normal map is baked onto")
                .clicked()
            {
                open_mesh_dialog(state);
            }
        }
        if mesh_import.ready && mesh_import.uv_triangles == 0 {
            ui.label(
                egui::RichText::new("The low-poly mesh has no UV coordinates.")
                    .size(TEXT_CAPTION)
                    .color(WARNING),
            );
        }

        if bake.settings.mode == BakeMode::HighToLow {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("High poly")
                    .size(TEXT_SMALL)
                    .color(MUTED),
            );
            if let Some(path) = &bake.high_path {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("high poly");
                ui.add(
                    egui::Label::new(egui::RichText::new(name).size(TEXT_SMALL).color(TEXT))
                        .truncate(),
                )
                .on_hover_text(path.display().to_string());
            } else {
                ui.label(
                    egui::RichText::new("No high-poly mesh selected")
                        .size(TEXT_SMALL)
                        .color(MUTED),
                );
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let clear_width = if bake.high_path.is_some() { 56.0 } else { 0.0 };
                let choose_width = (ui.available_width() - clear_width).max(70.0);
                if ui
                    .add_sized(
                        [choose_width, 25.0],
                        egui::Button::new(
                            egui::RichText::new("Choose high-poly mesh...").size(TEXT_LABEL),
                        ),
                    )
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(
                            "3D meshes",
                            &[
                                "glb", "gltf", "fbx", "obj", "dae", "usd", "usda", "usdc", "usdz",
                            ],
                        )
                        .pick_file()
                    {
                        bake.high_path = Some(path);
                    }
                }
                if bake.high_path.is_some()
                    && ui
                        .add_sized(
                            [clear_width - 4.0, 25.0],
                            egui::Button::new(egui::RichText::new("Clear").size(TEXT_LABEL)),
                        )
                        .clicked()
                {
                    bake.high_path = None;
                }
            });
            ui.label(
                egui::RichText::new("Both meshes must share the same file-space coordinates.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }
    });

    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        section_title(ui, Icon::Settings, "Bake Settings");
        ui.add_space(4.0);

        let resolution_label = format!("{} px", bake.settings.resolution);
        labeled_combo(
            ui,
            "Resolution",
            "bake_resolution",
            &resolution_label,
            |ui| {
                for resolution in [512u32, 1024, 2048, 4096] {
                    ui.selectable_value(
                        &mut bake.settings.resolution,
                        resolution,
                        format!("{resolution} px"),
                    );
                }
            },
        );

        let anti_alias = bake.settings.anti_alias;
        let anti_alias_label = format!("{anti_alias}× ({} samples)", anti_alias * anti_alias);
        labeled_combo(ui, "Anti-alias", "bake_aa", &anti_alias_label, |ui| {
            for factor in [1u32, 2, 4] {
                ui.selectable_value(
                    &mut bake.settings.anti_alias,
                    factor,
                    format!("{factor}× ({} samples)", factor * factor),
                );
            }
        });
        let effective_aa = bake.settings.effective_aa();
        if effective_aa < bake.settings.anti_alias {
            ui.label(
                egui::RichText::new(format!("Limited to {effective_aa}× at this resolution"))
                    .size(TEXT_CAPTION)
                    .color(WARNING),
            );
        }

        labeled_combo(
            ui,
            "Output space",
            "bake_space",
            bake.settings.space.label(),
            |ui| {
                for space in BakeSpace::ALL {
                    ui.selectable_value(&mut bake.settings.space, space, space.label());
                }
            },
        );

        if mesh_import.udim.enabled {
            if !mesh_import.udim.tiles.contains(&bake.settings.tile) {
                bake.settings.tile = mesh_import.udim.base_tile();
            }
            ui.checkbox(&mut bake.bake_all_tiles, "Bake all UDIM tiles");
            if bake.bake_all_tiles {
                ui.label(
                    egui::RichText::new(format!(
                        "{} tiles · one PNG per tile, baked in sequence",
                        mesh_import.udim.tiles.len()
                    ))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            } else {
                let tile_label = bake.settings.tile.to_string();
                labeled_combo(ui, "UDIM tile", "bake_tile", &tile_label, |ui| {
                    for tile in &mesh_import.udim.tiles {
                        ui.selectable_value(&mut bake.settings.tile, *tile, tile.to_string());
                    }
                });
                ui.label(
                    egui::RichText::new("One tile per bake; enable batch to bake every tile.")
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            }
        }

        ui.add_space(3.0);
        match bake.settings.mode {
            BakeMode::HighToLow => {
                // Cage offset is a percentage of the mesh extent; 0–5 % with
                // four decimals covers tight surface contact and fine tuning.
                slider_row(
                    ui,
                    "Cage %",
                    &mut bake.settings.cage_offset_pct,
                    0.0..=5.0,
                    0.001,
                    4,
                );
                slider_row(
                    ui,
                    "Ray dist %",
                    &mut bake.settings.ray_distance_pct,
                    0.1..=50.0,
                    0.1,
                    1,
                );
                ui.horizontal(|ui| {
                    if toggle_switch(ui, &mut bake.show_cage).changed() {
                        state.status = if bake.show_cage {
                            "Bake cage shown in the 3D preview".into()
                        } else {
                            "Bake cage hidden".into()
                        };
                    }
                    ui.label(
                        egui::RichText::new("Show cage")
                            .size(TEXT_LABEL)
                            .color(TEXT),
                    );
                });
                if bake.show_cage {
                    ui.label(
                        egui::RichText::new(
                            "Teal hull marks where the cage rays start, at the current distance.",
                        )
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                    );
                }
            }
            BakeMode::Bevel => {
                slider_row(
                    ui,
                    "Radius %",
                    &mut bake.settings.bevel_radius_pct,
                    0.01..=2.0,
                    0.01,
                    2,
                );
                slider_row(
                    ui,
                    "Falloff",
                    &mut bake.settings.bevel_falloff,
                    0.25..=4.0,
                    0.01,
                    2,
                );
            }
        }
        // Applies to both modes; object-space bakes keep their raw vectors.
        slider_row(
            ui,
            "Intensity",
            &mut bake.settings.normal_intensity,
            0.0..=4.0,
            0.01,
            2,
        );
        ui.label(
            egui::RichText::new(
                "1.00 is the ray result; higher exaggerates the relief, 0 flattens the map.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        // Padding is a float setting; the bake snaps it to whole texels.
        slider_row(
            ui,
            "Padding (px)",
            &mut bake.settings.padding,
            0.0..=64.0,
            0.5,
            2,
        );
        let padding_texels = bake.settings.padding.round().clamp(0.0, 256.0);
        if (bake.settings.padding - padding_texels).abs() > 0.001 {
            ui.label(
                egui::RichText::new(format!("Bakes with {padding_texels:.0} px of padding."))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Output PNG")
                .size(TEXT_SMALL)
                .color(MUTED),
        );
        ui.horizontal(|ui| {
            // TextEdit adds its own 4 px side margins on top of `desired_width`.
            let field_width =
                (ui.available_width() - 74.0 - ui.spacing().item_spacing.x - 8.0).max(70.0);
            ui.add(
                egui::TextEdit::singleline(&mut bake.output_path)
                    .desired_width(field_width)
                    .hint_text("output/bakes/..."),
            );
            if ui
                .add_sized(
                    [74.0, 24.0],
                    egui::Button::new(egui::RichText::new("Browse...").size(TEXT_SMALL)),
                )
                .clicked()
            {
                let default_name = mesh_import
                    .source_path
                    .as_deref()
                    .map(|path| {
                        default_output_path(
                            path,
                            bake.settings.mode,
                            bake.settings.tile.max(1001),
                            mesh_import.udim.enabled,
                        )
                    })
                    .and_then(|path| path.file_name().map(|name| name.to_owned()))
                    .and_then(|name| name.to_str().map(str::to_string))
                    .unwrap_or_else(|| "normal.png".to_string());
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PNG", &["png"])
                    .set_file_name(default_name)
                    .save_file()
                {
                    bake.output_path = path.display().to_string();
                }
            }
        });

        ui.add_space(5.0);
        let batch_tiles = if bake.bake_all_tiles && mesh_import.udim.enabled {
            mesh_import.udim.tiles.clone()
        } else {
            Vec::new()
        };
        if bake.is_running() {
            let progress = bake.progress();
            let progress_text = match bake.batch_progress() {
                Some((index, total)) => format!(
                    "Tile {} · {index} of {total} · {:.0}%",
                    bake.settings.tile,
                    progress * 100.0
                ),
                None => format!("Baking... {:.0}%", progress * 100.0),
            };
            ui.add(egui::ProgressBar::new(progress).text(progress_text));
            if let Some(elapsed) = bake.elapsed() {
                ui.label(
                    egui::RichText::new(format!("{:.1}s elapsed", elapsed.as_secs_f32()))
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            }
            if ui
                .add_sized(
                    [ui.available_width(), 24.0],
                    egui::Button::new(egui::RichText::new("Cancel").size(TEXT_LABEL)),
                )
                .clicked()
            {
                bake.cancel();
            }
        } else {
            let can_bake =
                low_ready && (bake.settings.mode == BakeMode::Bevel || bake.high_path.is_some());
            let label = if batch_tiles.len() > 1 {
                format!("Bake {} tiles", batch_tiles.len())
            } else {
                "Bake normal map".to_string()
            };
            let button = egui::Button::new(egui::RichText::new(label).strong().color(ON_PRIMARY))
                .fill(PRIMARY_CONTAINER)
                .stroke(egui::Stroke::NONE)
                .corner_radius(4);
            let response = ui
                .add_enabled_ui(can_bake, |ui| {
                    ui.add_sized([ui.available_width(), 28.0], button)
                })
                .inner;
            if response.clicked() {
                if let Some(low_path) = mesh_import.source_path.clone() {
                    let high_path = bake.high_path.clone();
                    bake.settings.uv_bottom_left = mesh_import.uv_bottom_left;
                    if batch_tiles.is_empty() {
                        let output = if bake.output_path.trim().is_empty() {
                            default_output_path(
                                &low_path,
                                bake.settings.mode,
                                bake.settings.tile.max(1001),
                                mesh_import.udim.enabled,
                            )
                        } else {
                            PathBuf::from(bake.output_path.trim())
                        };
                        bake.start(low_path, high_path, output);
                    } else {
                        bake.start_batch(
                            low_path,
                            high_path,
                            batch_tiles,
                            PathBuf::from(bake.output_path.trim()),
                        );
                    }
                }
            }
            if !can_bake {
                ui.label(
                    egui::RichText::new(if !low_ready {
                        "Import a low-poly mesh with UVs first."
                    } else {
                        "Choose a high-poly mesh for this mode."
                    })
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            }
        }
        if !bake.status.is_empty() {
            ui.label(
                egui::RichText::new(&bake.status)
                    .size(TEXT_SMALL)
                    .color(MUTED),
            );
        }
        if let Some(error) = &bake.last_error {
            ui.label(egui::RichText::new(error).size(TEXT_SMALL).color(WARNING));
        }
    });

    let result_tile = bake
        .result
        .as_ref()
        .map_or(1001, |result| result.stats.tile);
    let has_base = bake
        .base_layers
        .get(&result_tile)
        .and_then(|id| doc.0.find_layer(*id))
        .is_some();
    let mut preview_on_mesh = bake.preview_on_mesh;
    let mut pending_canvas = false;
    if let (Some(result), Some(texture)) = (&bake.result, bake.result_tex_id) {
        ui.add_space(8.0);
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
            section_title(ui, Icon::Grid, "Baked Map");
            ui.add_space(3.0);

            // Report what the bake actually found. A flat result is usually an
            // input problem (high matching low, or rays never reaching it), not
            // a missing feature, so say which one it is.
            // Two rays are cast per texel (front and back) and at most one can
            // hit, so coverage is hits over half the ray count.
            let hit_ratio = if result.stats.rays > 0 {
                Some((result.stats.hits as f32 * 2.0 / result.stats.rays as f32).clamp(0.0, 1.0))
            } else {
                None
            };
            let mut summary = String::new();
            if let Some(ratio) = hit_ratio {
                summary.push_str(&format!("Rays hit {:.0}%", ratio * 100.0));
            }
            if let Some(detail) = result.detail {
                if !summary.is_empty() {
                    summary.push_str(" · ");
                }
                summary.push_str(&format!("detail on {:.1}% of the surface", detail * 100.0));
            }
            if !summary.is_empty() {
                ui.label(
                    egui::RichText::new(summary)
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            }
            if hit_ratio.is_some_and(|ratio| ratio < 0.35) {
                ui.label(
                    egui::RichText::new(
                        "Few rays reached the high-poly. Raise Cage % / Ray dist %, or check both files share scale and position.",
                    )
                    .size(TEXT_CAPTION)
                    .color(WARNING),
                );
            } else if result.detail.is_some_and(|detail| detail < 0.10) {
                ui.label(
                    egui::RichText::new(
                        "Only a small part of the surface carries detail. Check that the high-poly holds the extra relief and sits inside the cage; Intensity only scales what is there.",
                    )
                    .size(TEXT_CAPTION)
                    .color(WARNING),
                );
            }

            if result.tiles_baked > 1 {
                let text = if bake.is_running() {
                    format!(
                        "Baking {} tiles. The preview follows the last finished one (tile {}).",
                        result.tiles_baked, result.stats.tile
                    )
                } else {
                    format!(
                        "Batch baked {} tiles. The preview shows the last one (tile {}); each tile is saved to its own PNG.",
                        result.tiles_baked, result.stats.tile
                    )
                };
                ui.label(
                    egui::RichText::new(text)
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            }

            ui.checkbox(
                &mut preview_on_mesh,
                "Apply baked map to the 3D preview",
            );
            ui.label(
                egui::RichText::new(if has_base {
                    "3D preview only; overrides the canvas, including layers above the bake."
                } else {
                    "Overrides the document normal map while enabled."
                })
                .size(TEXT_CAPTION)
                .color(MUTED),
            );

            ui.add_space(5.0);
            if result.space == BakeSpace::Object {
                ui.label(
                    egui::RichText::new(
                        "Object-space bakes cannot become a tangent-space layer. Re-bake in a tangent space to mix it on the canvas.",
                    )
                    .size(TEXT_CAPTION)
                    .color(WARNING),
                );
            } else {
                let tile = result.stats.tile;
                let per_tile = mesh_import.udim.enabled;
                let label = match (has_base, per_tile) {
                    (true, true) => format!("Update baked layer for tile {tile}"),
                    (false, true) => format!("Add tile {tile} normals to canvas"),
                    (true, false) => "Update baked base layer".to_string(),
                    (false, false) => "Add baked normals to canvas".to_string(),
                };
                let button = egui::Button::new(
                    egui::RichText::new(label).size(TEXT_LABEL).color(TEXT),
                )
                .fill(CONTAINER_HIGH)
                .stroke(egui::Stroke::new(1.0_f32, OUTLINE_VARIANT))
                .corner_radius(RADIUS_SM);
                if ui
                    .add_sized([ui.available_width(), 27.0], button)
                    .on_hover_text(
                        "Inserts the bake at the bottom of the stack for this tile so shapes and paint blend over it.",
                    )
                    .clicked()
                {
                    pending_canvas = true;
                    state.status = "Adding baked normals to the canvas...".into();
                }
                ui.label(
                    egui::RichText::new(if per_tile {
                        "The layer is tagged for this tile; switch tiles above the canvas to author another one."
                    } else if has_base {
                        "Refreshes the existing bottom layer with this bake."
                    } else {
                        "The bake becomes the base surface; new layers mix above it."
                    })
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            }

            ui.add_space(12.0);
            let max_width = ui.available_width();
            let mut width = max_width;
            let mut height = width;
            if height > 190.0 {
                height = 190.0;
                width = height;
            }
            ui.add(egui::Image::new(egui::load::SizedTexture::new(
                texture,
                egui::vec2(width, height),
            )));
            let stats = result.stats;
            ui.label(
                egui::RichText::new(format!(
                    "{}×{} · {}× AA · {:.0} ms",
                    stats.resolution, stats.resolution, stats.effective_aa, stats.elapsed_ms
                ))
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
            if stats.high_triangles > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "{} low tris · {} high tris",
                        stats.low_triangles, stats.high_triangles
                    ))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{} rays · {} hits · {} misses",
                        stats.rays, stats.hits, stats.misses
                    ))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            }
            if let Some(path) = &result.saved_path {
                ui.add_sized(
                    [ui.available_width(), 15.0],
                    egui::Label::new(
                        egui::RichText::new(format!("Saved {}", path.display()))
                            .size(TEXT_CAPTION)
                            .color(SUCCESS),
                    )
                    .truncate(),
                );
            }
            if let Some(error) = &result.save_error {
                ui.label(egui::RichText::new(error).size(TEXT_CAPTION).color(WARNING));
            }
        });
    }
    bake.preview_on_mesh = preview_on_mesh;
    if pending_canvas {
        bake.pending_canvas = true;
    }
}
