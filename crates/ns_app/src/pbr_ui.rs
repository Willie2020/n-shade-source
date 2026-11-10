//! Optional single-image PBR panel, background scheduler and preview wiring.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool, Task};
use bevy_egui::{egui, EguiContexts, EguiTextureHandle};
use ns_core::Grid;
use ns_pbr::export::ExportOptions;
use ns_pbr::pixels::PbrImage;
use ns_pbr::{
    run_pipeline, stage_keys, MapKind, MapStore, PbrSettings, PipelineOutput, Stage, StageKey,
};

use crate::app::{normal_rgba, EditorState, MaterialPreset, PreviewHandles};
use crate::preview;
use crate::ui::{self, Icon, TEXT_CAPTION, TEXT_LABEL, TEXT_SMALL};

#[derive(Resource)]
pub struct PbrRuntime {
    pub input_path: Option<PathBuf>,
    pub input_name: String,
    pub source: Option<Arc<PbrImage>>,
    pub input_hash: Option<[u8; 32]>,
    pub settings: PbrSettings,
    pub store: MapStore,
    pub job: Option<PbrJob>,
    pub generation: u64,
    pub auto_update: bool,
    pub dirty: bool,
    pub force_generate: bool,
    pub last_change: Option<Instant>,
    pub status: String,
    pub last_error: Option<String>,
    pub last_timings: HashMap<Stage, f32>,
    pub last_keys: HashMap<Stage, StageKey>,
    pub preview_maps: bool,
    /// True while the PBR preview owns the preview material's normal, depth and
    /// MR maps. The editor's parallax settings step aside until it closes.
    pub preview_active: bool,
    pub channel: MapKind,
    pub input_tex_id: Option<egui::TextureId>,
    pub preview_tex: Option<(MapKind, egui::TextureId)>,
    pub textures: Option<PbrTextures>,
    pub material_dirty: bool,
    pub export_options: ExportOptions,
    pub export_dir: Option<PathBuf>,
    pub export_status: String,
    pub pending_input: Option<PathBuf>,
    pub auto_generate_on_load: bool,
}

impl Default for PbrRuntime {
    fn default() -> Self {
        Self {
            input_path: None,
            input_name: String::new(),
            source: None,
            input_hash: None,
            settings: PbrSettings::default(),
            store: MapStore::default(),
            job: None,
            generation: 0,
            auto_update: true,
            dirty: false,
            force_generate: false,
            last_change: None,
            status: "Load an image to begin".into(),
            last_error: None,
            last_timings: HashMap::new(),
            last_keys: HashMap::new(),
            preview_maps: false,
            preview_active: false,
            channel: MapKind::Normal,
            input_tex_id: None,
            preview_tex: None,
            textures: None,
            material_dirty: false,
            export_options: ExportOptions::default(),
            export_dir: None,
            export_status: String::new(),
            pending_input: None,
            auto_generate_on_load: false,
        }
    }
}

pub fn parse_map_kind(value: &str) -> MapKind {
    let value = value.to_ascii_lowercase();
    MapKind::ALL
        .into_iter()
        .find(|kind| {
            kind.suffix() == value
                || kind.label().to_ascii_lowercase() == value
                || format!("{:?}", kind).to_ascii_lowercase() == value
        })
        .unwrap_or(MapKind::Normal)
}

impl PbrRuntime {
    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_change = Some(Instant::now());
        self.material_dirty = true;
    }

    pub fn request_generate(&mut self) {
        self.force_generate = true;
        self.dirty = true;
        self.last_change = None;
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = "Cancelling...".into();
        }
    }

    pub fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = "Cancelling...".into();
        }
        self.force_generate = false;
        self.dirty = false;
    }

    pub fn enable_default_maps(&mut self) {
        self.settings.maps.normal = true;
        self.settings.maps.height = true;
        self.settings.maps.ao = true;
        self.settings.maps.cavity = true;
        self.settings.maps.roughness = true;
        self.settings.maps.delit = true;
        self.export_options.maps = Some(self.settings.maps);
        self.mark_dirty();
    }

    pub fn set_channel(&mut self, channel: MapKind) {
        self.channel = channel;
        self.preview_tex = None;
    }

    pub fn input_dimensions(&self) -> Option<(u32, u32)> {
        self.source.as_ref().map(|source| (source.w, source.h))
    }

    pub fn channel_dimensions(&self) -> Option<(u32, u32)> {
        let output = self.store.output(self.channel.stage())?;
        match self.channel {
            MapKind::Normal | MapKind::Height => output.height().map(|grid| (grid.w, grid.h)),
            MapKind::Displacement => output
                .displacement()
                .map(|displacement| (displacement.map.w, displacement.map.h)),
            MapKind::Delit => output.delit_image().map(|image| (image.w, image.h)),
            MapKind::Curvature => output
                .curvature()
                .map(|curvature| (curvature.map.w, curvature.map.h)),
            _ => output.scalar().map(|grid| (grid.w, grid.h)),
        }
    }

    pub fn map_status(&self, kind: MapKind) -> String {
        if !self.settings.maps.enabled(kind) {
            if kind == MapKind::Delit
                && (self.settings.maps.roughness || self.settings.maps.metallic)
            {
                return "Dependency".into();
            }
            return "Off".into();
        }
        if let Some(job) = &self.job {
            let (stage, fraction) = job.progress.get();
            if stage == Some(kind.stage()) {
                return format!("Running {:.0}%", fraction * 100.0);
            }
            return "Queued".into();
        }
        if self.store.output(kind.stage()).is_some() {
            if self.dirty {
                "Stale".into()
            } else {
                "Ready".into()
            }
        } else {
            "Not generated".into()
        }
    }

    fn schedule(&mut self) {
        let Some(source) = self.source.clone() else {
            self.status = "Load an image first".into();
            self.force_generate = false;
            return;
        };
        let Some(input_hash) = self.input_hash else {
            self.status = "Load an image first".into();
            self.force_generate = false;
            return;
        };
        let (plan, keys) = stage_keys(&self.settings, input_hash);
        if plan.is_empty() {
            self.status = "Enable at least one map".into();
            self.force_generate = false;
            self.dirty = false;
            return;
        }
        let cached = self.store.cached_for(&keys);
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(JobProgress::default());
        let settings = self.settings.clone();
        let task_cancel = cancel.clone();
        let task_progress = progress.clone();
        let task_plan = plan.clone();
        let task_keys = keys.clone();
        let task = AsyncComputeTaskPool::get().spawn(async move {
            run_pipeline(
                source,
                &settings,
                &task_plan,
                &task_keys,
                &cached,
                &task_cancel,
                &|stage, fraction| task_progress.set(stage, fraction),
            )
        });
        self.job = Some(PbrJob {
            generation,
            task,
            cancel,
            progress,
            started: Instant::now(),
        });
        self.dirty = false;
        self.force_generate = false;
        self.last_error = None;
        self.status = "Generating...".into();
        info!("PBR generation started: {:?}", plan);
    }
}

pub struct PbrJob {
    pub generation: u64,
    pub task: Task<ns_pbr::Result<PipelineOutput>>,
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<JobProgress>,
    pub started: Instant,
}

#[derive(Default)]
pub struct JobProgress {
    stage: Mutex<Option<Stage>>,
    fraction: AtomicU32,
}

impl JobProgress {
    fn set(&self, stage: Stage, fraction: f32) {
        if let Ok(mut current) = self.stage.lock() {
            *current = Some(stage);
        }
        self.fraction.store(
            (fraction.clamp(0.0, 1.0) * 1000.0) as u32,
            Ordering::Relaxed,
        );
    }

    pub fn get(&self) -> (Option<Stage>, f32) {
        let stage = self.stage.lock().ok().and_then(|current| *current);
        let fraction = self.fraction.load(Ordering::Relaxed) as f32 / 1000.0;
        (stage, fraction)
    }
}

pub struct PbrTextures {
    pub normal: Handle<Image>,
    pub height: Handle<Image>,
    pub displacement: Handle<Image>,
    pub ao: Handle<Image>,
    pub cavity: Handle<Image>,
    pub curvature: Handle<Image>,
    pub edge_wear: Handle<Image>,
    pub roughness: Handle<Image>,
    pub metallic: Handle<Image>,
    pub base_color: Handle<Image>,
    pub packed_mr: Handle<Image>,
    pub flat_normal: Handle<Image>,
    pub input: Handle<Image>,
}

pub struct PbrPlugin;

impl Plugin for PbrPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PbrRuntime>()
            .add_systems(Startup, setup_pbr_textures.after(preview::setup_scene))
            .add_systems(
                Update,
                (handle_pbr_requests, poll_pbr_jobs, auto_schedule_pbr).chain(),
            )
            .add_systems(
                Update,
                sync_pbr_material.after(preview::apply_preview_style),
            );
    }
}

fn setup_pbr_textures(mut runtime: ResMut<PbrRuntime>, mut images: ResMut<Assets<Image>>) {
    let textures = PbrTextures {
        normal: images.add(neutral_image(
            [128, 128, 255, 255],
            TextureFormat::Rgba8Unorm,
        )),
        height: images.add(neutral_image([0, 0, 0, 255], TextureFormat::Rgba8Unorm)),
        displacement: images.add(neutral_image(
            [128, 128, 128, 255],
            TextureFormat::Rgba8Unorm,
        )),
        ao: images.add(neutral_image(
            [255, 255, 255, 255],
            TextureFormat::Rgba8Unorm,
        )),
        cavity: images.add(neutral_image([0, 0, 0, 255], TextureFormat::Rgba8Unorm)),
        curvature: images.add(neutral_image(
            [128, 128, 128, 255],
            TextureFormat::Rgba8Unorm,
        )),
        edge_wear: images.add(neutral_image([0, 0, 0, 255], TextureFormat::Rgba8Unorm)),
        roughness: images.add(neutral_image(
            [128, 128, 128, 255],
            TextureFormat::Rgba8Unorm,
        )),
        metallic: images.add(neutral_image([0, 0, 0, 255], TextureFormat::Rgba8Unorm)),
        base_color: images.add(neutral_image(
            [180, 180, 180, 255],
            TextureFormat::Rgba8UnormSrgb,
        )),
        packed_mr: images.add(neutral_image([255, 128, 0, 255], TextureFormat::Rgba8Unorm)),
        flat_normal: images.add(neutral_image(
            [128, 128, 255, 255],
            TextureFormat::Rgba8Unorm,
        )),
        input: images.add(neutral_image(
            [24, 24, 24, 255],
            TextureFormat::Rgba8UnormSrgb,
        )),
    };
    runtime.textures = Some(textures);
}

fn neutral_image(color: [u8; 4], format: TextureFormat) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &color,
        format,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

fn update_image(
    images: &mut Assets<Image>,
    handle: &Handle<Image>,
    data: Vec<u8>,
    width: u32,
    height: u32,
) {
    if let Some(mut image) = images.get_mut(handle) {
        if image.width() != width || image.height() != height {
            image.texture_descriptor.size = Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            };
        }
        image.data = Some(data);
    }
}

fn handle_pbr_requests(mut runtime: ResMut<PbrRuntime>, mut images: ResMut<Assets<Image>>) {
    let Some(path) = runtime.pending_input.take() else {
        return;
    };
    runtime.status = format!("Loading {}...", path.display());
    match ns_pbr::pixels::load(&path) {
        Ok(source) => {
            let hash = ns_pbr::hash_image(&source);
            let (width, height) = (source.w, source.h);
            if let Some(textures) = runtime.textures.as_ref() {
                update_image(
                    &mut images,
                    &textures.input,
                    source.to_srgb_rgba8(),
                    width,
                    height,
                );
            }
            if let Some(job) = runtime.job.take() {
                job.cancel.store(true, Ordering::Relaxed);
            }
            runtime.input_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("image")
                .to_string();
            runtime.input_path = Some(path);
            runtime.source = Some(Arc::new(source));
            runtime.input_hash = Some(hash);
            runtime.store.clear();
            runtime.last_keys.clear();
            runtime.generation = runtime.generation.wrapping_add(1);
            runtime.input_tex_id = None;
            runtime.preview_tex = None;
            runtime.material_dirty = true;
            runtime.dirty = true;
            runtime.last_change = Some(Instant::now());
            runtime.last_error = None;
            runtime.status = format!("Loaded {width}x{height} — ready to generate");
            if runtime.auto_generate_on_load {
                runtime.auto_generate_on_load = false;
                runtime.enable_default_maps();
                runtime.force_generate = true;
                runtime.last_change = None;
            }
        }
        Err(error) => {
            runtime.last_error = Some(error.to_string());
            runtime.status = format!("Could not load image: {error}");
        }
    }
}

fn poll_pbr_jobs(mut runtime: ResMut<PbrRuntime>, mut images: ResMut<Assets<Image>>) {
    let Some(mut job) = runtime.job.take() else {
        return;
    };
    let Some(result) = block_on(poll_once(&mut job.task)) else {
        runtime.job = Some(job);
        return;
    };
    if job.generation == runtime.generation {
        match result {
            Ok(output) => {
                for (key, output) in &output.new_entries {
                    runtime.store.insert(*key, output.clone());
                }
                runtime.store.set_current(&output.keys);
                runtime.last_keys = output.keys.clone();
                runtime.last_timings = output.timings_ms.clone();
                runtime.last_error = None;
                refresh_textures(&mut runtime, &output, &mut images);
                let total_ms: f32 = output.timings_ms.values().sum();
                runtime.status = format!(
                    "Ready · {} stages · {:.0} ms",
                    output.stages.len(),
                    total_ms
                );
                info!(
                    "PBR generation ready: {} stages in {:.0} ms ({:?})",
                    output.stages.len(),
                    total_ms,
                    output.timings_ms
                );
                runtime.dirty = false;
            }
            Err(ns_pbr::PbrError::Cancelled) => {
                runtime.status = "Cancelled".into();
            }
            Err(error) => {
                runtime.last_error = Some(error.to_string());
                runtime.status = format!("PBR generation failed: {error}");
            }
        }
    }
    if runtime.force_generate || (runtime.auto_update && runtime.dirty) {
        runtime.last_change = None;
    }
}

fn auto_schedule_pbr(mut runtime: ResMut<PbrRuntime>) {
    let has_maps = runtime.settings.maps.enabled_iter().next().is_some();
    if runtime.input_hash.is_none() || !has_maps {
        return;
    }
    let wants = runtime.force_generate || (runtime.auto_update && runtime.dirty);
    if !wants {
        return;
    }
    if runtime.job.is_some() {
        if let Some(job) = &runtime.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        runtime.status = "Cancelling...".into();
        return;
    }
    let debounce_ready = runtime.force_generate
        || runtime
            .last_change
            .map(|changed| changed.elapsed().as_millis() >= 220)
            .unwrap_or(true);
    if debounce_ready {
        runtime.schedule();
    }
}

fn refresh_textures(runtime: &mut PbrRuntime, output: &PipelineOutput, images: &mut Assets<Image>) {
    let Some(textures) = runtime.textures.as_ref() else {
        return;
    };
    for kind in MapKind::ALL {
        if !runtime.settings.maps.enabled(kind) && runtime.channel != kind {
            continue;
        }
        let Some(stage) = output.map(kind) else {
            continue;
        };
        match kind {
            MapKind::Normal => {
                if let Some(normal) = stage.normal() {
                    let data = normal_rgba(normal);
                    update_image(images, &textures.normal, data, normal.w, normal.h);
                }
            }
            MapKind::Height => {
                if let Some(height) = stage.height() {
                    update_image(
                        images,
                        &textures.height,
                        scalar_rgba(height),
                        height.w,
                        height.h,
                    );
                }
            }
            MapKind::Displacement => {
                if let Some(displacement) = stage.displacement() {
                    update_image(
                        images,
                        &textures.displacement,
                        scalar_rgba(&displacement.map),
                        displacement.map.w,
                        displacement.map.h,
                    );
                }
            }
            MapKind::Ao => {
                if let Some(grid) = stage.scalar() {
                    update_image(images, &textures.ao, scalar_rgba(grid), grid.w, grid.h);
                }
            }
            MapKind::Cavity => {
                if let Some(grid) = stage.scalar() {
                    update_image(images, &textures.cavity, scalar_rgba(grid), grid.w, grid.h);
                }
            }
            MapKind::Curvature => {
                if let Some(curvature) = stage.curvature() {
                    update_image(
                        images,
                        &textures.curvature,
                        scalar_rgba(&curvature.map),
                        curvature.map.w,
                        curvature.map.h,
                    );
                }
            }
            MapKind::EdgeWear => {
                if let Some(grid) = stage.scalar() {
                    update_image(
                        images,
                        &textures.edge_wear,
                        scalar_rgba(grid),
                        grid.w,
                        grid.h,
                    );
                }
            }
            MapKind::Roughness => {
                if let Some(grid) = stage.scalar() {
                    update_image(
                        images,
                        &textures.roughness,
                        scalar_rgba(grid),
                        grid.w,
                        grid.h,
                    );
                }
            }
            MapKind::Metallic => {
                if let Some(grid) = stage.scalar() {
                    update_image(
                        images,
                        &textures.metallic,
                        scalar_rgba(grid),
                        grid.w,
                        grid.h,
                    );
                }
            }
            MapKind::Delit => {
                if let Some(color) = stage.delit_image() {
                    update_image(
                        images,
                        &textures.base_color,
                        color.to_srgb_rgba8(),
                        color.w,
                        color.h,
                    );
                }
            }
        }
    }

    let roughness = output
        .output(Stage::Roughness)
        .and_then(|stage| stage.scalar());
    let metallic = output
        .output(Stage::Metallic)
        .and_then(|stage| stage.scalar());
    if let Some(dimensions) = roughness
        .map(|grid| (grid.w, grid.h))
        .or_else(|| metallic.map(|grid| (grid.w, grid.h)))
    {
        let (width, height) = dimensions;
        let default_rough = Grid::new(width, height, 0.5);
        let default_metal = Grid::new(width, height, 0.0);
        let rough = roughness.unwrap_or(&default_rough);
        let metal = metallic.unwrap_or(&default_metal);
        update_image(
            images,
            &textures.packed_mr,
            packed_mr_rgba(rough, metal),
            width,
            height,
        );
    }
    runtime.preview_tex = None;
    runtime.material_dirty = true;
}

fn scalar_rgba(grid: &Grid<f32>) -> Vec<u8> {
    let mut data = vec![0u8; grid.data.len() * 4];
    for (index, value) in grid.data.iter().enumerate() {
        let byte = (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let base = index * 4;
        data[base] = byte;
        data[base + 1] = byte;
        data[base + 2] = byte;
        data[base + 3] = 255;
    }
    data
}

fn packed_mr_rgba(roughness: &Grid<f32>, metallic: &Grid<f32>) -> Vec<u8> {
    let mut data = vec![0u8; roughness.data.len() * 4];
    for (index, (rough, metal)) in roughness.data.iter().zip(metallic.data.iter()).enumerate() {
        let base = index * 4;
        data[base] = 255;
        data[base + 1] = (rough.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        data[base + 2] = (metal.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        data[base + 3] = 255;
    }
    data
}

fn map_handle(textures: &PbrTextures, kind: MapKind) -> Handle<Image> {
    match kind {
        MapKind::Normal => textures.normal.clone(),
        MapKind::Height => textures.height.clone(),
        MapKind::Displacement => textures.displacement.clone(),
        MapKind::Ao => textures.ao.clone(),
        MapKind::Cavity => textures.cavity.clone(),
        MapKind::Curvature => textures.curvature.clone(),
        MapKind::EdgeWear => textures.edge_wear.clone(),
        MapKind::Roughness => textures.roughness.clone(),
        MapKind::Metallic => textures.metallic.clone(),
        MapKind::Delit => textures.base_color.clone(),
    }
}

pub fn ensure_texture_ids(contexts: &mut EguiContexts, runtime: &mut PbrRuntime) {
    let Some(textures) = runtime.textures.as_ref() else {
        return;
    };
    if runtime.input_tex_id.is_none() {
        runtime.input_tex_id =
            Some(contexts.add_image(EguiTextureHandle::Strong(textures.input.clone())));
    }
    if runtime
        .preview_tex
        .as_ref()
        .map(|(kind, _)| *kind != runtime.channel)
        .unwrap_or(true)
    {
        let id = contexts.add_image(EguiTextureHandle::Strong(map_handle(
            textures,
            runtime.channel,
        )));
        runtime.preview_tex = Some((runtime.channel, id));
    }
}

pub(crate) fn sync_pbr_material(
    mut runtime: ResMut<PbrRuntime>,
    state: Res<EditorState>,
    handles: Res<PreviewHandles>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut was_active: Local<bool>,
    mut last_preset: Local<Option<MaterialPreset>>,
) {
    let preset_changed = *last_preset != Some(state.material_preset);
    *last_preset = Some(state.material_preset);
    let ready = runtime.store.output(Stage::Geometry).is_some()
        || runtime.store.output(Stage::Delit).is_some()
        || runtime.store.output(Stage::Ao).is_some();
    let should_apply = runtime.preview_maps && ready;
    if !should_apply {
        runtime.preview_active = false;
        if *was_active {
            if let Some(mut material) = materials.get_mut(&handles.material) {
                let (color, metallic, roughness) = preset_values(state.material_preset);
                material.base_color = color;
                material.metallic = metallic;
                material.perceptual_roughness = roughness;
                material.base_color_texture = None;
                material.normal_map_texture = Some(handles.normal_texture.clone());
                material.metallic_roughness_texture = None;
                material.occlusion_texture = None;
                material.depth_map = None;
                material.parallax_depth_scale = 0.0;
            }
            *was_active = false;
        }
        return;
    }
    if *was_active && !runtime.material_dirty && !preset_changed {
        return;
    }
    *was_active = true;
    runtime.material_dirty = false;
    let Some(textures) = runtime.textures.as_ref() else {
        runtime.preview_active = false;
        return;
    };
    let Some(mut material) = materials.get_mut(&handles.material) else {
        return;
    };
    material.base_color = Color::WHITE;
    material.base_color_texture = runtime
        .settings
        .maps
        .delit
        .then(|| textures.base_color.clone());
    material.normal_map_texture = Some(if runtime.settings.maps.normal {
        textures.normal.clone()
    } else {
        textures.flat_normal.clone()
    });
    let use_mr = runtime.settings.maps.roughness || runtime.settings.maps.metallic;
    material.metallic_roughness_texture = use_mr.then(|| textures.packed_mr.clone());
    material.metallic = if runtime.settings.maps.metallic {
        1.0
    } else {
        0.0
    };
    material.perceptual_roughness = if use_mr { 1.0 } else { 0.5 };
    material.occlusion_texture = runtime.settings.maps.ao.then(|| textures.ao.clone());
    material.depth_map = if runtime.settings.maps.height {
        Some(textures.height.clone())
    } else if runtime.settings.maps.displacement {
        Some(textures.displacement.clone())
    } else {
        None
    };
    material.parallax_depth_scale = 0.04;
    runtime.preview_active = true;
}

fn preset_values(preset: MaterialPreset) -> (Color, f32, f32) {
    match preset {
        MaterialPreset::MatteGray => (Color::srgb(0.40, 0.43, 0.48), 0.1, 0.55),
        MaterialPreset::Clay => (Color::srgb(0.62, 0.38, 0.29), 0.0, 0.82),
        MaterialPreset::Steel => (Color::srgb(0.55, 0.59, 0.64), 0.88, 0.27),
    }
}

fn open_input_dialog(runtime: &mut PbrRuntime) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Images", &["png", "jpg", "jpeg", "tga", "exr"])
        .pick_file()
    {
        runtime.pending_input = Some(path);
        runtime.status = "Loading image...".into();
    }
}

fn status_color(status: &str) -> egui::Color32 {
    if status.starts_with("Ready") {
        ui::SUCCESS
    } else if status.starts_with("Running") {
        ui::ACCENT
    } else if status.starts_with("Queued") {
        ui::MUTED
    } else if status.starts_with("Stale") || status.starts_with("Cancelling") {
        ui::WARNING
    } else {
        ui::MUTED
    }
}

/// Docked single-image PBR content. The caller owns the surrounding scroll
/// area so the panel can live in a resizable sidebar.
pub fn pbr_panel(
    ui: &mut egui::Ui,
    runtime: &mut PbrRuntime,
    bridge: &mut crate::bridge::BridgeRuntime,
) {
    input_card(ui, runtime);
    ui.add_space(8.0);
    maps_card(ui, runtime);
    ui.add_space(8.0);
    preview_card(ui, runtime);
    ui.add_space(8.0);
    export_card(ui, runtime, bridge);
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "Classical backend only. Roughness and metallic are estimates; \
             back them with authored masks when accuracy matters.",
        )
        .size(TEXT_SMALL)
        .color(ui::MUTED),
    );
}

fn input_card(ui: &mut egui::Ui, runtime: &mut PbrRuntime) {
    ui::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui::section_title(ui, Icon::Pbr, "Source Image");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if let Some(id) = runtime.input_tex_id {
                let (w, h) = runtime.input_dimensions().unwrap_or((1, 1));
                let aspect = w as f32 / h.max(1) as f32;
                let height = 46.0;
                let width = (height * aspect).clamp(24.0, 82.0);
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(id, egui::vec2(width, height)))
                        .corner_radius(3.0),
                );
            }
            ui.vertical(|ui| {
                if let Some((w, h)) = runtime.input_dimensions() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&runtime.input_name)
                                .size(TEXT_LABEL)
                                .color(ui::TEXT),
                        )
                        .truncate(),
                    );
                    ui.label(
                        egui::RichText::new(format!("{w} × {h}"))
                            .size(TEXT_SMALL)
                            .color(ui::MUTED),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("PNG, JPEG, TGA or EXR")
                            .size(TEXT_SMALL)
                            .color(ui::MUTED),
                    );
                }
            });
        });
        ui.add_space(4.0);
        let load = egui::Button::new(egui::RichText::new("Load image...").size(TEXT_LABEL))
            .corner_radius(4);
        if ui.add_sized([ui.available_width(), 24.0], load).clicked() {
            open_input_dialog(runtime);
        }

        ui.add_space(4.0);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Quality")
                    .size(TEXT_SMALL)
                    .color(ui::MUTED),
            );
            egui::ComboBox::from_id_salt("pbr_quality")
                .width((ui.available_width() - 8.0).max(80.0))
                .selected_text(runtime.settings.quality.label())
                .show_ui(ui, |ui| {
                    for quality in ns_pbr::Quality::ALL {
                        changed |= ui
                            .selectable_value(
                                &mut runtime.settings.quality,
                                quality,
                                quality.label(),
                            )
                            .changed();
                    }
                });
        });
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .checkbox(&mut runtime.settings.seamless, "Seamless (wrap)")
                .changed();
            changed |= ui
                .checkbox(&mut runtime.settings.denoise, "Denoise")
                .changed();
        });
        changed |= ui
            .checkbox(
                &mut runtime.preview_maps,
                "Use generated maps in the 3D preview",
            )
            .changed();
        if changed {
            runtime.mark_dirty();
            runtime.material_dirty = true;
        }
        if !runtime.status.is_empty() {
            ui.label(egui::RichText::new(&runtime.status).size(TEXT_SMALL).color(
                if runtime.last_error.is_some() {
                    ui::WARNING
                } else {
                    ui::MUTED
                },
            ));
        }
    });
}

fn maps_card(ui: &mut egui::Ui, runtime: &mut PbrRuntime) {
    ui::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui::section_title(ui, Icon::Grid, "Maps");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let _ = ui.checkbox(&mut runtime.auto_update, "Auto-update");
            });
        });
        ui.add_space(3.0);

        for kind in MapKind::ALL {
            let mut enabled = runtime.settings.maps.enabled(kind);
            let status = runtime.map_status(kind);
            ui.horizontal(|ui| {
                if ui
                    .checkbox(
                        &mut enabled,
                        egui::RichText::new(kind.label()).size(12.0).color(ui::TEXT),
                    )
                    .changed()
                {
                    runtime.settings.maps.set(kind, enabled);
                    let mut selection =
                        runtime.export_options.maps.unwrap_or(runtime.settings.maps);
                    selection.set(kind, enabled);
                    runtime.export_options.maps = Some(selection);
                    runtime.mark_dirty();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(status.clone())
                            .size(TEXT_SMALL)
                            .color(status_color(&status)),
                    );
                });
            });
            if enabled || kind == MapKind::Metallic {
                egui::CollapsingHeader::new(egui::RichText::new("Settings").size(TEXT_SMALL))
                    .id_salt(format!("pbr_settings_{}", kind.suffix()))
                    .default_open(false)
                    .show(ui, |ui| {
                        let mut changed = false;
                        map_settings(ui, kind, &mut runtime.settings, &mut changed);
                        if changed {
                            runtime.mark_dirty();
                        }
                    });
            }
            ui.add_space(2.0);
        }

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    runtime.source.is_some(),
                    egui::Button::new(egui::RichText::new("Generate enabled").size(12.0)),
                )
                .clicked()
            {
                runtime.request_generate();
            }
            if runtime.job.is_some() && ui.button("Cancel").clicked() {
                runtime.cancel();
            }
        });
        if let Some(job) = runtime.job.as_ref() {
            let (stage, fraction) = job.progress.get();
            let text = stage
                .map(|stage| format!("{} {:.0}%", stage.label(), fraction * 100.0))
                .unwrap_or_else(|| format!("{:.0}%", fraction * 100.0));
            let elapsed = job.started.elapsed().as_secs_f32();
            let text = format!("{text} · {elapsed:.1}s");
            ui.add(
                egui::ProgressBar::new(fraction)
                    .text(egui::RichText::new(text).size(TEXT_CAPTION))
                    .desired_width(ui.available_width()),
            );
        }

        if !runtime.last_timings.is_empty() {
            egui::CollapsingHeader::new(egui::RichText::new("Stage timings").size(TEXT_SMALL))
                .id_salt("pbr_timings")
                .show(ui, |ui| {
                    let mut total = 0.0;
                    for stage in Stage::ALL {
                        if let Some(ms) = runtime.last_timings.get(&stage) {
                            total += *ms;
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(stage.label())
                                        .size(TEXT_SMALL)
                                        .color(ui::MUTED),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{ms:.0} ms"))
                                                .size(TEXT_SMALL)
                                                .color(ui::TEXT),
                                        );
                                    },
                                );
                            });
                        }
                    }
                    ui.label(
                        egui::RichText::new(format!("Total {total:.0} ms"))
                            .size(TEXT_SMALL)
                            .color(ui::ACCENT),
                    );
                });
        }
    });
}

fn map_settings(ui: &mut egui::Ui, kind: MapKind, settings: &mut PbrSettings, changed: &mut bool) {
    match kind {
        MapKind::Normal => {
            *changed |= ui
                .add(egui::Slider::new(&mut settings.normal.strength, 0.0..=4.0).text("Strength"))
                .changed();
            *changed |= ui
                .checkbox(&mut settings.normal.direct_x, "DirectX green (export)")
                .changed();
        }
        MapKind::Height => {
            *changed |= ui.checkbox(&mut settings.height.invert, "Invert").changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.height.height_scale_px, 1.0..=32.0)
                        .text("Height scale (px)"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.height.drift_removal, 0.0..=1.0)
                        .text("Drift removal"),
                )
                .changed();
            ui.label(
                egui::RichText::new("Band weights · fine to coarse")
                    .size(TEXT_CAPTION)
                    .color(ui::MUTED),
            );
            ui.horizontal(|ui| {
                for weight in &mut settings.height.band_weights {
                    *changed |= ui
                        .add(
                            egui::DragValue::new(weight)
                                .speed(0.01)
                                .range(-1.0..=1.0)
                                .fixed_decimals(2),
                        )
                        .changed();
                }
            });
        }
        MapKind::Displacement => {
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.displacement.midlevel, 0.0..=1.0)
                        .text("Midlevel"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.displacement.amount_m, 0.0..=0.1)
                        .text("Amount (m)"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.displacement.drift_removal, 0.0..=1.0)
                        .text("Drift removal"),
                )
                .changed();
        }
        MapKind::Ao => {
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.ao.small_radius_px, 1.0..=32.0)
                        .text("Small radius"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.ao.large_radius_px, 8.0..=128.0)
                        .text("Large radius"),
                )
                .changed();
            *changed |= ui
                .add(egui::Slider::new(&mut settings.ao.strength, 0.0..=2.0).text("Strength"))
                .changed();
        }
        MapKind::Cavity => {
            *changed |= ui
                .add(egui::Slider::new(&mut settings.cavity.lo, 0.0..=0.5).text("Low"))
                .changed();
            *changed |= ui
                .add(egui::Slider::new(&mut settings.cavity.hi, 0.2..=1.0).text("High"))
                .changed();
            *changed |= ui
                .checkbox(&mut settings.cavity.invert, "Invert (white = open)")
                .changed();
        }
        MapKind::Curvature => {
            *changed |= ui
                .add(egui::Slider::new(&mut settings.curvature.gain, 0.0..=1.0).text("Gain"))
                .changed();
        }
        MapKind::EdgeWear => {
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.edge_wear.intensity, 0.0..=2.0)
                        .text("Intensity"),
                )
                .changed();
            *changed |= ui
                .add(egui::Slider::new(&mut settings.edge_wear.breakup, 0.0..=1.0).text("Breakup"))
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.edge_wear.contrast, 0.0..=1.0).text("Contrast"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.edge_wear.ao_influence, 0.0..=1.0)
                        .text("AO influence"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.edge_wear.scratch_amount, 0.0..=1.0)
                        .text("Scratches"),
                )
                .changed();
        }
        MapKind::Roughness => {
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.roughness.k_hf, -1.0..=1.0)
                        .text("Detail influence"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.roughness.k_cav, -1.0..=1.0)
                        .text("Cavity influence"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.roughness.k_wear, -1.0..=1.0)
                        .text("Wear influence"),
                )
                .changed();
            *changed |= ui
                .checkbox(&mut settings.roughness.use_cavity, "Use cavity")
                .changed();
            *changed |= ui
                .checkbox(&mut settings.roughness.use_wear, "Use edge wear")
                .changed();
            let mut class = settings.roughness.class;
            let previous_class = class;
            egui::ComboBox::from_id_salt("pbr_rough_class")
                .width(ui.available_width())
                .selected_text(class.label())
                .show_ui(ui, |ui| {
                    for option in ns_pbr::MaterialClass::ALL {
                        *changed |= ui
                            .selectable_value(&mut class, option, option.label())
                            .changed();
                    }
                });
            settings.roughness.class = class;
            if class != previous_class {
                settings.roughness.k_wear = class.range().3;
                *changed = true;
            }
            let mut mode = settings.roughness.mode;
            egui::ComboBox::from_id_salt("pbr_rough_mode")
                .width(ui.available_width())
                .selected_text(mode.label())
                .show_ui(ui, |ui| {
                    for option in ns_pbr::RoughnessMode::ALL {
                        *changed |= ui
                            .selectable_value(&mut mode, option, option.label())
                            .changed();
                    }
                });
            settings.roughness.mode = mode;
            if mode == ns_pbr::RoughnessMode::Constant {
                *changed |= ui
                    .add(
                        egui::Slider::new(&mut settings.roughness.constant, 0.0..=1.0)
                            .text("Constant"),
                    )
                    .changed();
            }
        }
        MapKind::Metallic => {
            let mut mode = settings.metallic.mode;
            egui::ComboBox::from_id_salt("pbr_metal_mode")
                .width(ui.available_width())
                .selected_text(mode.label())
                .show_ui(ui, |ui| {
                    for option in ns_pbr::MetallicMode::ALL {
                        *changed |= ui
                            .selectable_value(&mut mode, option, option.label())
                            .changed();
                    }
                });
            settings.metallic.mode = mode;
            if mode == ns_pbr::MetallicMode::Constant {
                *changed |= ui
                    .add(
                        egui::Slider::new(&mut settings.metallic.constant, 0.0..=1.0)
                            .text("Constant"),
                    )
                    .changed();
            }
            if mode == ns_pbr::MetallicMode::AutoSegment {
                *changed |= ui
                    .add(
                        egui::Slider::new(&mut settings.metallic.threshold, 0.05..=0.95)
                            .text("Threshold"),
                    )
                    .changed();
            }
            ui.label(
                egui::RichText::new(
                    "Metallic is the least reliable estimate; prefer None or Constant.",
                )
                .size(TEXT_CAPTION)
                .color(ui::WARNING),
            );
        }
        MapKind::Delit => {
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.delit.sigma_fraction, 0.02..=0.30)
                        .text("Flatten sigma"),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::Slider::new(&mut settings.delit.highlight_percentile, 0.95..=0.9999)
                        .text("Highlight clamp"),
                )
                .changed();
        }
    }
}

fn preview_card(ui: &mut egui::Ui, runtime: &mut PbrRuntime) {
    ui::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui::section_title(ui, Icon::Cube, "Map Preview");
        ui.add_space(3.0);
        let mut channel = runtime.channel;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Channel")
                    .size(TEXT_SMALL)
                    .color(ui::MUTED),
            );
            egui::ComboBox::from_id_salt("pbr_channel")
                .width((ui.available_width() - 4.0).max(80.0))
                .selected_text(channel.label())
                .show_ui(ui, |ui| {
                    for kind in MapKind::ALL {
                        if ui
                            .selectable_value(&mut channel, kind, kind.label())
                            .changed()
                        {
                            runtime.set_channel(channel);
                        }
                    }
                });
        });
        ui.add_space(4.0);
        let current = runtime
            .preview_tex
            .as_ref()
            .filter(|(kind, _)| *kind == runtime.channel)
            .map(|(_, id)| *id);
        let dimensions = runtime
            .channel_dimensions()
            .or_else(|| runtime.input_dimensions());
        if let (Some(id), Some((w, h))) = (current, dimensions) {
            let aspect = w as f32 / h.max(1) as f32;
            let height = (ui.available_width().max(160.0) / aspect).min(300.0);
            let width = height * aspect;
            ui.add(
                egui::Image::new(egui::load::SizedTexture::new(id, egui::vec2(width, height)))
                    .corner_radius(6.0),
            );
            ui.label(
                egui::RichText::new(format!("{} · {w}×{h}", runtime.channel.label()))
                    .size(TEXT_SMALL)
                    .color(ui::MUTED),
            );
        } else {
            ui.label(
                egui::RichText::new("Generate the selected map to see a preview.")
                    .size(TEXT_SMALL)
                    .color(ui::MUTED),
            );
        }
    });
}

fn export_card(
    ui: &mut egui::Ui,
    runtime: &mut PbrRuntime,
    bridge: &mut crate::bridge::BridgeRuntime,
) {
    ui::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui::section_title(ui, Icon::Download, "Export");
        ui.add_space(4.0);
        let folder = egui::Button::new(egui::RichText::new("Choose folder...").size(TEXT_LABEL))
            .corner_radius(4);
        if ui.add_sized([ui.available_width(), 24.0], folder).clicked() {
            if let Some(directory) = rfd::FileDialog::new().pick_folder() {
                runtime.export_dir = Some(directory);
            }
        }
        if let Some(directory) = &runtime.export_dir {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(directory.display().to_string())
                        .size(TEXT_CAPTION)
                        .color(ui::MUTED),
                )
                .truncate(),
            );
        }
        ui.label(
            egui::RichText::new("Resolution")
                .size(TEXT_SMALL)
                .color(ui::MUTED),
        );
        let resolution_label = if runtime.export_options.resolution == 0 {
            "Native (working size)".to_string()
        } else {
            format!("{} px", runtime.export_options.resolution)
        };
        egui::ComboBox::from_id_salt("pbr_export_resolution")
            .width(ui.available_width())
            .selected_text(resolution_label)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut runtime.export_options.resolution,
                    0,
                    "Native (working size)",
                );
                for size in [512u32, 1024, 2048, 4096] {
                    ui.selectable_value(
                        &mut runtime.export_options.resolution,
                        size,
                        format!("{size} px"),
                    );
                }
            });
        ui.label(
            egui::RichText::new("Textures")
                .size(TEXT_SMALL)
                .color(ui::MUTED),
        );
        {
            let settings_maps = runtime.settings.maps;
            let selection = runtime.export_options.maps.get_or_insert(settings_maps);
            let enabled_maps: Vec<MapKind> = runtime.settings.enabled_maps().collect();
            if enabled_maps.is_empty() {
                ui.label(
                    egui::RichText::new("Enable maps in the Maps section first")
                        .size(TEXT_CAPTION)
                        .color(ui::MUTED),
                );
            } else {
                ui.horizontal_wrapped(|ui| {
                    for kind in enabled_maps {
                        let mut checked = selection.enabled(kind);
                        if ui.checkbox(&mut checked, kind.label()).changed() {
                            selection.set(kind, checked);
                        }
                    }
                });
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut runtime.export_options.sixteen_bit, "16-bit");
            ui.checkbox(&mut runtime.export_options.packed_gltf, "glTF MR");
            ui.checkbox(&mut runtime.export_options.packed_orm, "ORM");
            ui.checkbox(&mut runtime.export_options.packed_unity, "Unity mask");
        });
        let any_selected = runtime
            .export_options
            .maps
            .unwrap_or(runtime.settings.maps)
            .enabled_iter()
            .next()
            .is_some();
        let packed = runtime.export_options.packed_gltf
            || runtime.export_options.packed_orm
            || runtime.export_options.packed_unity;
        let can_export = runtime.export_dir.is_some()
            && runtime.input_path.is_some()
            && !runtime.store.is_empty()
            && runtime.job.is_none()
            && !runtime.dirty
            && (any_selected || packed);
        if ui
            .add_enabled(
                can_export,
                egui::Button::new(egui::RichText::new("Export selected maps").size(12.0)),
            )
            .clicked()
        {
            if let (Some(directory), Some(path)) = (&runtime.export_dir, &runtime.input_path) {
                let stem = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("texture");
                match ns_pbr::export::export_maps(
                    directory,
                    stem,
                    &runtime.settings,
                    &runtime.store,
                    &runtime.export_options,
                ) {
                    Ok(report) => {
                        runtime.export_status = if report.errors.is_empty() {
                            format!("Exported {} files", report.files.len())
                        } else {
                            format!(
                                "Exported {} files; {} skipped",
                                report.files.len(),
                                report.errors.len()
                            )
                        };
                    }
                    Err(error) => runtime.export_status = format!("Export failed: {error}"),
                }
            }
        }
        if !runtime.export_status.is_empty() {
            ui.label(
                egui::RichText::new(&runtime.export_status)
                    .size(TEXT_SMALL)
                    .color(ui::MUTED),
            );
        }
        send_to_blender(ui, runtime, bridge);
    });
}

/// Hands the PBR maps to Blender without touching the disk. Mirrors the export
/// card's map selection, plus an "all generated" escape hatch.
fn send_to_blender(
    ui: &mut egui::Ui,
    runtime: &mut PbrRuntime,
    bridge: &mut crate::bridge::BridgeRuntime,
) {
    let selected = crate::bridge::pbr_selection(runtime);
    let live = bridge.enabled && bridge.listening();
    ui.horizontal(|ui| {
        let label = match selected.len() {
            0 => "Send to Blender".to_string(),
            count => format!("Send {count} to Blender"),
        };
        let send = ui.add_enabled(
            live && !selected.is_empty(),
            egui::Button::new(egui::RichText::new(label).size(TEXT_SMALL)),
        );
        if send
            .on_hover_text(
                "Push the textures ticked above into the running Blender session. \
                 Streaming images of the same names are replaced in place.",
            )
            .clicked()
        {
            let count = bridge.publish(selected);
            runtime.export_status = format!("Queued {count} map(s) for Blender");
        }
        let all = ui.add_enabled(
            live,
            egui::Button::new(egui::RichText::new("All").size(TEXT_SMALL)),
        );
        if all
            .on_hover_text("Send every PBR map that has finished generating")
            .clicked()
        {
            let generated = crate::bridge::pbr_available(runtime);
            let count = bridge.publish(generated);
            runtime.export_status = format!("Queued {count} map(s) for Blender");
        }
        if !live {
            ui.label(
                egui::RichText::new("bridge off")
                    .size(TEXT_CAPTION)
                    .color(ui::MUTED),
            )
            .on_hover_text("Enable the Blender bridge in Settings");
        }
    });
}
