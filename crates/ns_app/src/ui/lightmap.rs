//! Lightmap dock: sun + HDR environment path tracing with optional denoising.

use std::path::PathBuf;

use super::*;
use crate::bake::BakeSeparation;
use crate::lightmap::default_lightmap_output_path;
use ns_bake::{
    BackendUsed, DenoiserUsed, LightmapBackend, LightmapDenoiser, LightmapLightKind,
    LightmapLightingMode, LightmapPipeline, LightmapSceneLight, OidnDevice, PngBitDepth,
};

pub(crate) struct LightmapPanel;

impl Panel for LightmapPanel {
    fn show(&self, ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
        lightmap_panel(ui, frame);
    }
}

pub(crate) fn lightmap_panel(ui: &mut egui::Ui, frame: &mut UiFrame<'_>) {
    let state = &mut *frame.state;
    let mesh_import = frame.mesh_import;
    let runtime = &mut *frame.lightmap;

    let low_ready =
        mesh_import.ready && mesh_import.uv_triangles > 0 && mesh_import.source_path.is_some();

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        let quality = quality_label(runtime);
        section_title_with_pill(ui, Icon::Sun, "Lightmap Baker", Some((quality, true)));
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "White diffuse bake with sun, scene-light and environment shadows. CPU or GPU path tracing.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );

        ui.add_space(4.0);
        labeled_combo(
            ui,
            "Pipeline",
            "lightmap_pipeline",
            runtime.settings.pipeline.label(),
            |ui| {
                for pipeline in LightmapPipeline::ALL {
                    ui.selectable_value(&mut runtime.settings.pipeline, pipeline, pipeline.label())
                        .on_hover_text(match pipeline {
                            LightmapPipeline::Legacy => {
                                "Reproduces older bakes exactly. Use it to compare or to keep an
                                 existing look."
                            }
                            LightmapPipeline::V2 => {
                                "Owen-Sobol sampling, edge extension, chart-aware denoising,
                                 TPDF dither and geometric-normal offsets."
                            }
                        });
                }
            },
        );
        ui.add_space(4.0);
        preset_row(ui, runtime);

        ui.add_space(6.0);
        labeled_combo(
            ui,
            "Mode",
            "lightmap_lighting",
            runtime.settings.lighting.label(),
            |ui| {
                for mode in LightmapLightingMode::ALL {
                    ui.selectable_value(&mut runtime.settings.lighting, mode, mode.label())
                        .on_hover_text(mode.hint());
                }
            },
        );
        ui.label(
            egui::RichText::new(runtime.settings.lighting.hint())
                .size(TEXT_CAPTION)
                .color(MUTED),
        );
        if runtime.settings.lighting != LightmapLightingMode::Full {
            ui.label(
                egui::RichText::new(
                    "Second Life: bake this mode for Advanced Lighting, then multiply the \
                     result over the diffuse texture. The live preview still shows sun + \
                     environment; the mode applies to the saved map.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        }

        ui.add_space(4.0);
        let advanced_body = |ui: &mut egui::Ui| {
            labeled_combo(
                ui,
                "PNG depth",
                "lightmap_png_depth",
                runtime.settings.png_bit_depth.label(),
                |ui| {
                    for depth in PngBitDepth::ALL {
                        ui.selectable_value(
                            &mut runtime.settings.png_bit_depth,
                            depth,
                            depth.label(),
                        )
                        .on_hover_text(
                            "16-bit keeps dark falloff smooth in the saved PNG. EXR is always \
                             float.",
                        );
                    }
                },
            );
            slider_row(
                ui,
                "Rolloff",
                &mut runtime.settings.highlight_rolloff,
                0.0..=0.95,
                0.01,
                2,
            );
            ui.label(
                egui::RichText::new(
                    "Rolloff is a soft shoulder at 1 - amount; 0 hard-clips. The clipping \
                     report still counts values above 1.0.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
            ui.add_enabled_ui(runtime.settings.lighting.is_ambient_occlusion(), |ui| {
                slider_row(
                    ui,
                    "AO distance",
                    &mut runtime.settings.ao_max_distance,
                    0.0..=2.0,
                    0.005,
                    3,
                );
            });
            ui.label(
                egui::RichText::new(
                    "AO distance is in normalized mesh units; 0 keeps the v1 infinite, binary \
                     ray. Values above 0 fade distant blockers instead of turning them black.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        };
        collapsible_section(
            ui,
            "lightmap_advanced",
            Icon::Settings,
            "Advanced",
            None,
            false,
            advanced_body,
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
            // Reference: the drop zone doubles as the mesh field, so the name
            // and triangle counts live in its label instead of extra rows.
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
                .on_hover_text("Import the low-poly mesh the lightmap is baked onto")
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
    });

    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        section_title_with_pill(
            ui,
            Icon::Eye,
            "Live Preview",
            Some((
                if runtime.preview_enabled { "On" } else { "Off" },
                runtime.preview_enabled,
            )),
        );
        ui.add_space(4.0);

        let mut enabled = runtime.preview_enabled;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("3D lighting preview")
                    .size(TEXT_LABEL)
                    .color(TEXT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = ui.add_enabled_ui(low_ready, |ui| toggle_switch(ui, &mut enabled));
                if response.inner.changed() {
                    runtime.preview_enabled = enabled && low_ready;
                    runtime.preview_error = None;
                }
            });
        });
        ui.label(
            egui::RichText::new(
                "Shows the imported mesh with the current sun, scene lights and HDR environment in the 3D \
                 viewport. Off by default. Uses a neutral white material, shadow maps and \
                 image-based lighting; it updates as you edit. This is a rasterized PBR \
                 approximation, not the path-traced bake. Disable to return to the normal-map \
                 preview.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        if runtime.preview_enabled && runtime.preview_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Loading HDR environment...")
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            });
        }
        if runtime.preview_enabled && low_ready && runtime.environment_path.is_none() {
            ui.label(
                egui::RichText::new(
                    "No HDR loaded: the solid ambient fill is used instead of an environment.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        }
        if !low_ready {
            ui.label(
                egui::RichText::new("Import a low-poly mesh with UVs to use the live preview.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }
        if let Some(error) = &runtime.preview_error {
            ui.label(egui::RichText::new(error).size(TEXT_CAPTION).color(WARNING));
        }
    });

    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        section_title(ui, Icon::Sun, "Lighting");
        ui.add_space(4.0);

        if runtime.settings.lighting.uses_sun() {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Directional sun")
                        .size(TEXT_LABEL)
                        .color(TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    toggle_switch(ui, &mut runtime.settings.sun_enabled);
                });
            });
            if runtime.settings.sun_enabled {
                slider_row(
                    ui,
                    "Azimuth",
                    &mut runtime.settings.sun_azimuth_deg,
                    0.0..=360.0,
                    1.0,
                    0,
                );
                slider_row(
                    ui,
                    "Elevation",
                    &mut runtime.settings.sun_elevation_deg,
                    0.0..=90.0,
                    1.0,
                    0,
                );
                slider_row(
                    ui,
                    "Sun size",
                    &mut runtime.settings.sun_size_deg,
                    0.1..=15.0,
                    0.1,
                    1,
                );
                slider_row(
                    ui,
                    "Intensity",
                    &mut runtime.settings.sun_intensity,
                    0.0..=10.0,
                    0.01,
                    2,
                );
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    field_label(ui, "Sun color");
                    ui.color_edit_button_rgb(&mut runtime.settings.sun_color);
                });
            }
        } else {
            ui.label(
                egui::RichText::new("Direct sun is not part of this bake mode.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }

        ui.add_space(12.0);
        if runtime.settings.lighting.uses_environment() {
            ui.label(egui::RichText::new("Environment").size(TEXT_SMALL).color(MUTED));

            let environment_name = runtime
                .environment_path
                .as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("Solid ambient fill");
            ui.add(
                egui::Label::new(egui::RichText::new(environment_name).size(TEXT_SMALL).color(TEXT))
                    .truncate(),
            )
            .on_hover_text(
                runtime
                    .environment_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Used when no HDR environment is loaded.".into()),
            );
            let has_environment = runtime.environment_path.is_some();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let choose_width = if has_environment {
                    (ui.available_width() - 64.0).max(80.0)
                } else {
                    ui.available_width()
                };
                if ui
                    .add_sized(
                        [choose_width, 25.0],
                        egui::Button::new(egui::RichText::new("Choose HDR/EXR...").size(TEXT_SMALL)),
                    )
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("HDR environments", &["hdr", "exr", "png", "jpg", "jpeg"])
                        .pick_file()
                    {
                        runtime.environment_path = Some(path);
                    }
                }
                if has_environment
                    && ui
                        .add_sized(
                            [60.0, 25.0],
                            egui::Button::new(egui::RichText::new("Clear").size(TEXT_SMALL)),
                        )
                        .clicked()
                {
                    runtime.environment_path = None;
                }
            });
            if has_environment {
                slider_row(
                    ui,
                    "HDR strength",
                    &mut runtime.settings.environment_strength,
                    0.0..=8.0,
                    0.01,
                    2,
                );
                slider_row(
                    ui,
                    "Env fill",
                    &mut runtime.settings.environment_fill,
                    0.0..=4.0,
                    0.01,
                    2,
                );
                ui.label(
                    egui::RichText::new(
                        "The HDR is path-traced directly; Env fill adds an unshadowed artistic lift scaled by its average brightness. 0 keeps the HDR physically accurate.",
                    )
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_sized(
                    [68.0, 22.0],
                    egui::Label::new(
                        egui::RichText::new(if has_environment {
                            "Fill color"
                        } else {
                            "Ambient color"
                        })
                        .size(TEXT_LABEL)
                        .color(MUTED),
                    ),
                );
                ui.color_edit_button_rgb(&mut runtime.settings.ambient_color);
            });
            slider_row(
                ui,
                if has_environment {
                    "Fill strength"
                } else {
                    "Ambient"
                },
                &mut runtime.settings.ambient_intensity,
                0.0..=4.0,
                0.01,
                2,
            );
            if !has_environment {
                ui.label(
                    egui::RichText::new(
                        "Solid ambient is an unshadowed fill. Load an HDR for environment occlusion.",
                    )
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            }
        } else {
            ui.label(
                egui::RichText::new("Environment and ambient fill are ignored in this bake mode.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
        }

        ui.add_space(10.0);
        ui.checkbox(&mut state.show_light_gizmos, "3D light gizmos")
            .on_hover_text(
                "Show draggable light bodies, aim arrows and rotation rings in the 3D viewport. \
                 Also shown while the live lighting preview is on.",
            );
    });

    ui.add_space(8.0);
    scene_lights_card(ui, runtime);

    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        section_title(ui, Icon::Settings, "Quality");
        ui.add_space(4.0);

        labeled_combo(
            ui,
            "Backend",
            "lightmap_backend",
            runtime.settings.backend.label(),
            |ui| {
                for backend in LightmapBackend::ALL {
                    ui.selectable_value(
                        &mut runtime.settings.backend,
                        backend,
                        backend.label(),
                    );
                }
            },
        );
        ui.label(
            egui::RichText::new(
                "GPU (wgpu) runs on RTX, GTX and Intel GPUs. Auto tries it and falls back to CPU.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );

        let resolution_label = format!("{} px", runtime.settings.resolution);
        labeled_combo(
            ui,
            "Resolution",
            "lightmap_resolution",
            &resolution_label,
            |ui| {
                for resolution in [256u32, 512, 1024, 2048, 4096] {
                    ui.selectable_value(
                        &mut runtime.settings.resolution,
                        resolution,
                        format!("{resolution} px"),
                    );
                }
            },
        );

        let samples_label = format!("{} spp", runtime.settings.samples);
        labeled_combo(ui, "Samples", "lightmap_samples", &samples_label, |ui| {
            for samples in [16u32, 32, 64, 128, 256, 512, 1024] {
                ui.selectable_value(
                    &mut runtime.settings.samples,
                    samples,
                    format!("{samples} spp"),
                );
            }
        });
        ui.label(
            egui::RichText::new(
                "128+ spp is recommended for clean shadows and HDR environments; denoising cannot fully recover very low sample counts.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        if runtime.settings.samples < 64 {
            ui.label(
                egui::RichText::new(
                    "Low sample count: expect visible blotches. Raise Samples to 128+ for final bakes.",
                )
                .size(TEXT_CAPTION)
                .color(WARNING),
            );
        }
        if runtime.settings.pipeline == LightmapPipeline::V2
            && !runtime.settings.samples.is_power_of_two()
        {
            ui.label(
                egui::RichText::new(
                    "V2 stratifies best with power-of-two sample counts (32, 64, 128, 256...).",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        }

        if runtime.settings.lighting.uses_bounces() {
            let mut bounces = runtime.settings.bounces as f32;
            if slider_row(ui, "Bounces", &mut bounces, 0.0..=6.0, 1.0, 0) {
                runtime.settings.bounces = bounces.round() as u32;
            }
        } else {
            ui.label(
                egui::RichText::new(
                    if runtime.settings.lighting.is_ambient_occlusion() {
                        "Ambient occlusion uses one hemisphere visibility ray per sample; bounces do not apply."
                    } else {
                        "Direct mode has no inter-surface bounces."
                    },
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        }
        let mut padding = runtime.settings.padding as f32;
        if slider_row(ui, "Padding", &mut padding, 0.0..=64.0, 1.0, 0) {
            runtime.settings.padding = padding.round() as u32;
        }

        labeled_combo(
            ui,
            "Denoiser",
            "lightmap_denoiser",
            runtime.settings.denoiser.label(),
            |ui| {
                for denoiser in LightmapDenoiser::ALL {
                    ui.selectable_value(&mut runtime.settings.denoiser, denoiser, denoiser.label());
                }
            },
        );
        if matches!(
            runtime.settings.denoiser,
            LightmapDenoiser::Auto | LightmapDenoiser::Optix | LightmapDenoiser::Oidn
        ) {
            ui.label(
                egui::RichText::new(
                    "OpenImageDenoise's RTLightmap filter usually gives the cleanest low-sample \
                     lightmaps; OptiX is faster on RTX. Both need enough samples, so 128+ spp \
                     remains the baseline for final bakes.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        }
        if runtime.settings.denoiser == LightmapDenoiser::Auto
            && !ns_bake::optix_available()
            && !ns_bake::oidn_available()
        {
            ui.label(
                egui::RichText::new(
                    "Neither OptiX nor OpenImageDenoise was found; the built-in edge-aware denoiser will be used.",
                )
                .size(TEXT_CAPTION)
                .color(WARNING),
            );
        }
        if runtime.settings.denoiser == LightmapDenoiser::Oidn && !ns_bake::oidn_available() {
            ui.label(
                egui::RichText::new(
                    "OpenImageDenoise was not found. The portable build ships it next to N-shade.exe; otherwise install OIDN or use the built-in denoiser.",
                )
                .size(TEXT_CAPTION)
                .color(WARNING),
            );
        }
        if runtime.settings.denoiser == LightmapDenoiser::Optix {
            if ns_bake::optix_available() {
                ui.label(
                    egui::RichText::new(
                        "OptiX HDR denoising with normal and albedo guides runs on the NVIDIA GPU.",
                    )
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            } else {
                ui.label(
                    egui::RichText::new(
                        "NVIDIA OptiX runtime was not found (nvoptix/CUDA). The built-in denoiser will be used; install or update the NVIDIA driver.",
                    )
                    .size(TEXT_CAPTION)
                    .color(WARNING),
                );
            }
        }
        ui.label(
            egui::RichText::new(
                "RTX: OptiX uses the installed NVIDIA driver runtime (nvoptix + CUDA) with normal and albedo guides; the OptiX SDK is not required. OpenImageDenoise with NVIDIA CUDA is an alternative when an OIDN build with CUDA is installed. DLSS is a real-time upscaler, not an offline bake denoiser.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        if matches!(
            runtime.settings.denoiser,
            LightmapDenoiser::Oidn | LightmapDenoiser::Auto
        ) {
            labeled_combo(
                ui,
                "OIDN device",
                "lightmap_oidn_device",
                runtime.settings.oidn_device.label(),
                |ui| {
                    for device in OidnDevice::ALL {
                        ui.selectable_value(
                            &mut runtime.settings.oidn_device,
                            device,
                            device.label(),
                        );
                    }
                },
            );
            ui.label(
                egui::RichText::new(
                    "CUDA targets NVIDIA RTX/GTX; SYCL targets Intel GPUs. Auto falls back to CPU when a device is unavailable.",
                )
                .size(TEXT_CAPTION)
                .color(MUTED),
            );
        }
        if matches!(
            runtime.settings.denoiser,
            LightmapDenoiser::Builtin | LightmapDenoiser::Auto
        ) {
            let mut passes = runtime.settings.denoise_passes as f32;
            if slider_row(ui, "Denoise passes", &mut passes, 1.0..=6.0, 1.0, 0) {
                runtime.settings.denoise_passes = passes.round() as u32;
            }
        }
        slider_row(
            ui,
            "Exposure",
            &mut runtime.settings.exposure,
            -4.0..=4.0,
            0.05,
            2,
        );

        if mesh_import.udim.enabled {
            if !mesh_import.udim.tiles.contains(&runtime.settings.tile) {
                runtime.settings.tile = mesh_import.udim.base_tile();
            }
            ui.checkbox(&mut runtime.bake_all_tiles, "Bake all UDIM tiles");
            labeled_combo(
                ui,
                "Separation",
                "lightmap_separation",
                runtime.separation.label(),
                |ui| {
                    for separation in BakeSeparation::ALL {
                        ui.selectable_value(
                            &mut runtime.separation,
                            separation,
                            separation.label(),
                        )
                        .on_hover_text(separation.hint());
                    }
                },
            );
            ui.label(
                egui::RichText::new(runtime.separation.hint())
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            if runtime.bake_all_tiles {
                ui.label(
                    egui::RichText::new(format!(
                        "{} tiles · one {} per tile, baked in sequence",
                        mesh_import.udim.tiles.len(),
                        runtime.format.extension().to_ascii_uppercase()
                    ))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            } else {
                let tile_label = runtime.settings.tile.to_string();
                labeled_combo(ui, "UDIM tile", "lightmap_tile", &tile_label, |ui| {
                    for tile in &mesh_import.udim.tiles {
                        ui.selectable_value(&mut runtime.settings.tile, *tile, tile.to_string());
                    }
                });
                ui.label(
                    egui::RichText::new("One tile per bake; enable batch to bake every tile.")
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            }
        }
    });

    ui.add_space(8.0);
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        section_title(ui, Icon::Download, "Lightmap Output");
        ui.add_space(4.0);

        labeled_combo(
            ui,
            "Format",
            "lightmap_format",
            runtime.format.label(),
            |ui| {
                for format in LightmapFormat::ALL {
                    ui.selectable_value(&mut runtime.format, format, format.label());
                }
            },
        );
        ui.label(
            egui::RichText::new("Exposure affects the PNG preview only; EXR stays linear.")
                .size(TEXT_CAPTION)
                .color(MUTED),
        );

        ui.label(
            egui::RichText::new("Output file")
                .size(TEXT_SMALL)
                .color(MUTED),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // TextEdit adds its own 4 px side margins on top of `desired_width`.
            let field_width =
                (ui.available_width() - 74.0 - ui.spacing().item_spacing.x - 8.0).max(70.0);
            ui.add(
                egui::TextEdit::singleline(&mut runtime.output_path)
                    .desired_width(field_width)
                    .hint_text("output/lightmaps/..."),
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
                        default_lightmap_output_path(
                            path,
                            runtime.format,
                            runtime.settings.tile.max(1001),
                            mesh_import.udim.enabled,
                        )
                    })
                    .and_then(|path| path.file_name().map(|name| name.to_owned()))
                    .and_then(|name| name.to_str().map(str::to_string))
                    .unwrap_or_else(|| format!("lightmap.{}", runtime.format.extension()));
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        if runtime.format == LightmapFormat::Exr {
                            "OpenEXR"
                        } else {
                            "PNG"
                        },
                        &[runtime.format.extension()],
                    )
                    .set_file_name(default_name)
                    .save_file()
                {
                    runtime.output_path = path.display().to_string();
                }
            }
        });

        ui.add_space(5.0);
        let separate = runtime.separation.is_on() && mesh_import.udim.enabled;
        let batch_tiles = if runtime.bake_all_tiles && mesh_import.udim.enabled {
            mesh_import.udim.tiles.clone()
        } else {
            Vec::new()
        };
        if runtime.is_running() {
            let progress = runtime.progress();
            let progress_text = match runtime.batch_progress() {
                Some((index, total)) => match runtime.current_group() {
                    Some(group) => format!(
                        "{group} · tile {} · {index} of {total} · {:.0}%",
                        runtime.settings.tile,
                        progress * 100.0
                    ),
                    None => format!(
                        "Tile {} · {index} of {total} · {:.0}%",
                        runtime.settings.tile,
                        progress * 100.0
                    ),
                },
                None => format!("Tracing... {:.0}%", progress * 100.0),
            };
            ui.add(egui::ProgressBar::new(progress).text(progress_text));
            if let Some(elapsed) = runtime.elapsed() {
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
                runtime.cancel();
            }
        } else {
            let label = if separate {
                "Bake separated maps".to_string()
            } else if batch_tiles.len() > 1 {
                format!("Bake {} tiles", batch_tiles.len())
            } else {
                "Bake lightmap".to_string()
            };
            let button = egui::Button::new(egui::RichText::new(label).strong().color(ON_PRIMARY))
                .fill(PRIMARY_CONTAINER)
                .stroke(egui::Stroke::NONE)
                .corner_radius(4);
            let response = ui
                .add_enabled_ui(low_ready, |ui| {
                    ui.add_sized([ui.available_width(), 28.0], button)
                })
                .inner;
            if response.clicked() {
                if let Some(low_path) = mesh_import.source_path.clone() {
                    runtime.settings.uv_bottom_left = mesh_import.uv_bottom_left;
                    if separate {
                        let tiles = if batch_tiles.is_empty() {
                            vec![runtime.settings.tile.max(1001)]
                        } else {
                            batch_tiles.clone()
                        };
                        runtime.start_batch(
                            low_path,
                            tiles,
                            PathBuf::from(runtime.output_path.trim()),
                            runtime.separation,
                        );
                    } else if batch_tiles.is_empty() {
                        let output = if runtime.output_path.trim().is_empty() {
                            default_lightmap_output_path(
                                &low_path,
                                runtime.format,
                                runtime.settings.tile.max(1001),
                                mesh_import.udim.enabled,
                            )
                        } else {
                            PathBuf::from(runtime.output_path.trim())
                        };
                        runtime.start(low_path, output);
                    } else {
                        runtime.start_batch(
                            low_path,
                            batch_tiles,
                            PathBuf::from(runtime.output_path.trim()),
                            BakeSeparation::Off,
                        );
                    }
                }
            }
            if !low_ready {
                ui.label(
                    egui::RichText::new("Import a low-poly mesh with UVs first.")
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            }
        }
        if !runtime.status.is_empty() {
            ui.label(
                egui::RichText::new(&runtime.status)
                    .size(TEXT_SMALL)
                    .color(MUTED),
            );
        }
        if let Some(error) = &runtime.last_error {
            ui.label(egui::RichText::new(error).size(TEXT_SMALL).color(WARNING));
        }
    });

    if let (Some(result), Some(texture)) = (&runtime.result, runtime.result_tex_id) {
        ui.add_space(8.0);
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
            section_title(ui, Icon::Grid, "Baked Lightmap");
            ui.add_space(3.0);

            let max_width = ui.available_width().max(64.0);
            let side = max_width.min(280.0);
            egui::Frame::new()
                .fill(CONTAINER_HIGH)
                .stroke(egui::Stroke::NONE)
                .corner_radius(egui::CornerRadius::same(RADIUS_SM))
                .inner_margin(egui::Margin::same(3))
                .show(ui, |ui| {
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(
                        texture,
                        egui::vec2(side, side),
                    )));
                });

            let stats = result.stats;
            if result.tiles_baked > 1 {
                let text = if result.separated {
                    if runtime.is_running() {
                        format!(
                            "Baking {} maps (per material or object and tile). The preview follows the last finished one.",
                            result.tiles_baked
                        )
                    } else {
                        format!(
                            "Batch baked {} maps. The preview shows the last one (tile {}); each material or object group is saved to its own file.",
                            result.tiles_baked, runtime.settings.tile
                        )
                    }
                } else if runtime.is_running() {
                    format!(
                        "Baking {} tiles. The preview follows the last finished one (tile {}).",
                        result.tiles_baked, runtime.settings.tile
                    )
                } else {
                    format!(
                        "Batch baked {} tiles. The preview shows the last one (tile {}); each tile is saved to its own file.",
                        result.tiles_baked, runtime.settings.tile
                    )
                };
                ui.label(
                    egui::RichText::new(text)
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            } else if result.separated {
                ui.label(
                    egui::RichText::new(
                        "Separated bake: this map contains only the selected group's UV islands. \
                         Other groups still cast shadows.",
                    )
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            }
            if let Some(backend_name) = &result.backend_name {
                ui.label(
                    egui::RichText::new(backend_name)
                        .size(TEXT_CAPTION)
                        .color(MUTED),
                );
            }
            let mut summary = format!(
                "{} · {}×{} · {} spp",
                stats.lighting.label(),
                stats.resolution,
                stats.resolution,
                stats.samples
            );
            if stats.lighting.uses_bounces() {
                summary.push_str(&format!(" · {} bounces", stats.bounces));
            }
            summary.push_str(&format!(" · {:.1}s", stats.elapsed_ms / 1000.0));
            summary.push_str(&format!(" · {}", stats.pipeline.label()));
            ui.label(
                egui::RichText::new(summary)
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            if stats.rays > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "{} traced texels · {} rays · {} geometry hits",
                        stats.texels, stats.rays, stats.hits
                    ))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            } else {
                ui.label(
                    egui::RichText::new(format!(
                        "{} traced texels · GPU compute path tracing",
                        stats.texels
                    ))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            }
            if stats.charts > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "{} charts · {} edge texels",
                        stats.charts, stats.edge_texels
                    ))
                    .size(TEXT_CAPTION)
                    .color(MUTED),
                );
            }
            if stats.clipped > 0 {
                let covered = stats.texels.max(1);
                let fraction = stats.clipped as f32 / covered as f32;
                if fraction >= 0.02 {
                    ui.label(
                        egui::RichText::new(format!(
                            "{:.0}% of texels clip to white in the PNG preview. Lower Exposure or light intensity, or save EXR to keep HDR values.",
                            fraction * 100.0
                        ))
                        .size(TEXT_CAPTION)
                        .color(WARNING),
                    );
                }
            }
            if runtime.settings.backend == LightmapBackend::Gpu
                && stats.backend != BackendUsed::Gpu
            {
                ui.label(
                    egui::RichText::new(
                        "The GPU backend was unavailable; the CPU fallback produced this bake.",
                    )
                    .size(TEXT_CAPTION)
                    .color(WARNING),
                );
            }
            ui.label(
                egui::RichText::new(format!("Denoised with {}", stats.denoiser.label()))
                    .size(TEXT_CAPTION)
                    .color(if stats.denoiser == DenoiserUsed::None {
                        MUTED
                    } else {
                        SUCCESS
                    }),
            );
            if runtime.settings.denoiser == LightmapDenoiser::Auto
                && stats.denoiser == DenoiserUsed::Builtin
            {
                ui.label(
                    egui::RichText::new(
                        "OptiX and OpenImageDenoise were unavailable; the built-in denoiser produced this bake.",
                    )
                    .size(TEXT_CAPTION)
                    .color(WARNING),
                );
            }
            if runtime.settings.denoiser == LightmapDenoiser::Oidn
                && !stats.denoiser.is_oidn()
            {
                ui.label(
                    egui::RichText::new("OpenImageDenoise was unavailable; used the built-in denoiser.")
                        .size(TEXT_CAPTION)
                        .color(WARNING),
                );
            }
            if runtime.settings.denoiser == LightmapDenoiser::Optix
                && stats.denoiser != DenoiserUsed::Optix
            {
                ui.label(
                    egui::RichText::new(
                        "NVIDIA OptiX was unavailable; the built-in denoiser produced this bake.",
                    )
                    .size(TEXT_CAPTION)
                    .color(WARNING),
                );
            }
            if runtime.settings.denoiser == LightmapDenoiser::Oidn
                && stats.denoiser.is_oidn()
                && runtime.settings.oidn_device == OidnDevice::Cuda
                && stats.denoiser != DenoiserUsed::OidnCuda
            {
                ui.label(
                    egui::RichText::new(format!(
                        "CUDA was unavailable; OIDN used {}. Install an OIDN build with CUDA support to use the RTX GPU.",
                        stats.denoiser.label()
                    ))
                    .size(TEXT_CAPTION)
                    .color(WARNING),
                );
            }
            if runtime.settings.denoiser == LightmapDenoiser::Oidn
                && stats.denoiser.is_oidn()
                && runtime.settings.oidn_device == OidnDevice::Sycl
                && stats.denoiser != DenoiserUsed::OidnSycl
            {
                ui.label(
                    egui::RichText::new(format!(
                        "Intel SYCL was unavailable; OIDN used {}. Install an OIDN build with SYCL support for the Intel GPU.",
                        stats.denoiser.label()
                    ))
                    .size(TEXT_CAPTION)
                    .color(WARNING),
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
}

/// Quality preset represented by the current samples/resolution/bounces, or
/// `None` for a custom combination. Shared by the header pill and the
/// segmented control so the two can never disagree.
fn quality_preset(runtime: &LightmapRuntime) -> Option<usize> {
    let settings = &runtime.settings;
    if settings.resolution == 512 && settings.samples == 64 && settings.bounces == 1 {
        Some(0)
    } else if settings.resolution == 1024 && settings.samples == 128 && settings.bounces == 2 {
        Some(1)
    } else if settings.resolution == 2048 && settings.samples == 512 && settings.bounces == 3 {
        Some(2)
    } else {
        None
    }
}

fn quality_label(runtime: &LightmapRuntime) -> &'static str {
    match quality_preset(runtime) {
        Some(0) => "Quick",
        Some(1) => "Balanced",
        Some(2) => "Final",
        _ => "Custom",
    }
}

fn preset_row(ui: &mut egui::Ui, runtime: &mut LightmapRuntime) {
    let mut active = quality_preset(runtime);

    if let Some(index) = segmented_control(
        ui,
        "lightmap_quality",
        &["Quick", "Balanced", "Final"],
        active,
    ) {
        let (resolution, samples, bounces) = match index {
            0 => (512, 64, 1),
            1 => (1024, 128, 2),
            _ => (2048, 512, 3),
        };
        runtime.settings.resolution = resolution;
        runtime.settings.samples = samples;
        runtime.settings.bounces = bounces;
        active = Some(index);
    }

    ui.label(
        egui::RichText::new(match active {
            Some(0) => "Low power. Good for checking layout and UVs.",
            Some(1) => "A clean result in reasonable time.",
            Some(2) => "Takes longer, gives the cleanest shadows.",
            _ => "Custom resolution, samples or bounces.",
        })
        .size(TEXT_CAPTION)
        .color(INSPECTOR_MUTED),
    );
}

/// Spot, area and point lights authored for the bake.
///
/// Positions and sizes are in normalized mesh space, matching the 3D preview's
/// normalization. The bake maps them back into raw mesh coordinates, so the
/// same rig works for any source unit.
fn scene_lights_card(ui: &mut egui::Ui, runtime: &mut LightmapRuntime) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = CARD_ROW_GAP;
        let light_count = runtime.settings.lights.len();
        let light_pill = format!(
            "{light_count} light{}",
            if light_count == 1 { "" } else { "s" }
        );
        section_title_with_pill(
            ui,
            Icon::Sun,
            "Scene Lights",
            Some((&light_pill, light_count > 0)),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Point, spot and area lights for dark corners. They are path-traced into the \
                 bake and approximated in the 3D preview. Positions are mesh-relative: the \
                 model is centered on the origin with its longest axis at 1 unit. Click a light \
                 in the 3D view to select it, drag its icon to move it, or drag its aim arrow to \
                 rotate it.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
        if !runtime.settings.lighting.uses_lights() {
            ui.label(
                egui::RichText::new(
                    "Scene lights are baked in Full, Direct and Indirect modes; AO ignores them.",
                )
                .size(TEXT_CAPTION)
                .color(WARNING),
            );
        }

        ui.add_space(6.0);
        let spacing = ui.spacing().item_spacing.x;
        let button_width = ((ui.available_width() - spacing * 2.0) / 3.0).max(56.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = spacing;
            for kind in LightmapLightKind::ALL {
                let add = egui::Button::new(
                    egui::RichText::new(format!("+ {}", kind.label())).size(TEXT_SMALL),
                )
                .fill(INPUT_BG)
                .stroke(egui::Stroke::new(1.0_f32, BORDER))
                .corner_radius(RADIUS_XS);
                if ui.add_sized([button_width, 23.0], add).clicked() {
                    let count = runtime
                        .settings
                        .lights
                        .iter()
                        .filter(|light| light.kind == kind)
                        .count();
                    let mut light = LightmapSceneLight::new(kind);
                    light.name = format!("{} {}", kind.label(), count + 1);
                    let index = runtime.settings.lights.len();
                    runtime.settings.lights.push(light);
                    runtime.selected_light = Some(LightSelection::Scene(index));
                }
            }
        });

        if runtime.settings.lights.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("No scene lights. The sun and HDR still light the bake.")
                    .size(TEXT_CAPTION)
                    .color(MUTED),
            );
            return;
        }

        let mut remove = None;
        let mut select = None;
        for (index, light) in runtime.settings.lights.iter_mut().enumerate() {
            ui.add_space(10.0);
            let selected = runtime.selected_light == Some(LightSelection::Scene(index));
            ui.horizontal(|ui| {
                if selected {
                    ui.label(egui::RichText::new("●").size(TEXT_SMALL).color(ACCENT));
                }
                let name = ui.add(
                    egui::Label::new(
                        egui::RichText::new(&light.name)
                            .size(TEXT_LABEL)
                            .color(if selected { ACCENT_TEXT } else { TEXT }),
                    )
                    .sense(egui::Sense::click()),
                );
                if name.clicked() {
                    select = Some(index);
                }
                name.on_hover_text("Select this light in the 3D view");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("x")
                        .on_hover_text("Delete this light")
                        .clicked()
                    {
                        remove = Some(index);
                    }
                    ui.checkbox(&mut light.visible_in_view, "Preview")
                        .on_hover_text(
                        "Show the light in the 3D preview. Uncheck it to hide the light and its \
                         marker from the viewport; it still bakes.",
                    );
                    ui.checkbox(&mut light.enabled, "Bake")
                        .on_hover_text("Include this light in the lightmap bake.");
                });
            });

            light_vector_row(ui, "Position", &mut light.position, 0.01);
            if light.kind != LightmapLightKind::Point {
                light_vector_row(ui, "Aim", &mut light.target, 0.01);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                field_label(ui, "Color");
                ui.color_edit_button_rgb(&mut light.color);
            });
            slider_row(ui, "Intensity", &mut light.intensity, 0.0..=20.0, 0.05, 2);
            slider_row(ui, "Range", &mut light.range, 0.0..=20.0, 0.05, 2);
            match light.kind {
                LightmapLightKind::Point => {}
                LightmapLightKind::Spot => {
                    slider_row(ui, "Cone", &mut light.spot_angle_deg, 5.0..=170.0, 1.0, 0);
                    slider_row(ui, "Softness", &mut light.spot_blend, 0.0..=1.0, 0.01, 2);
                }
                LightmapLightKind::Area => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        field_label(ui, "Size");
                        for (axis, value) in ["W ", "H "].iter().zip(&mut light.area_size) {
                            ui.add(
                                egui::DragValue::new(value)
                                    .speed(0.01)
                                    .range(0.01..=10.0)
                                    .prefix(*axis)
                                    .max_decimals(2),
                            );
                        }
                    });
                    ui.checkbox(&mut light.two_sided, "Two-sided")
                        .on_hover_text("Emit from both faces of the rectangle.");
                }
            }
        }
        if let Some(index) = remove {
            runtime.settings.lights.remove(index);
            runtime.light_removed(index);
        }
        if let Some(index) = select {
            runtime.selected_light = Some(LightSelection::Scene(index));
        }

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Bake includes the light even when Preview is off; Preview can also show a \
                 light that is excluded from the bake.",
            )
            .size(TEXT_CAPTION)
            .color(MUTED),
        );
    });
}

fn light_vector_row(ui: &mut egui::Ui, label: &str, value: &mut [f32; 3], speed: f32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_sized(
            [FIELD_LABEL_WIDTH, 20.0],
            egui::Label::new(egui::RichText::new(label).size(TEXT_CAPTION).color(MUTED)),
        );
        for (axis, component) in ["X ", "Y ", "Z "].iter().zip(value.iter_mut()) {
            ui.add(
                egui::DragValue::new(component)
                    .speed(speed)
                    .range(-20.0..=20.0)
                    .prefix(*axis)
                    .max_decimals(2),
            );
        }
    });
}
