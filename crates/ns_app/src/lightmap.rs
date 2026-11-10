//! Background lightmap bakes.
//!
//! The UI owns a [`LightmapRuntime`]; starting a bake spawns a task on Bevy's
//! compute pool. The worker reloads the low-poly mesh and optional HDR
//! environment from disk, runs the CPU or GPU path tracer in `ns_bake`, saves either a
//! PNG or a linear EXR, and hands the preview back to the editor. This is
//! deliberately independent from the normal-map [`crate::bake`] pipeline so it
//! can be used optionally without changing existing bakes.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool, Task};
use bevy_egui::egui;

use ns_bake::{EnvironmentMap, LightmapBackend, LightmapOutput, LightmapSettings, LightmapStats};

use crate::app::{EditorState, RightTab};
use crate::bake::{bake_groups, load_bake_mesh_separated, BakeGroup, BakeSeparation};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightmapFormat {
    #[default]
    Png,
    Exr,
}

impl LightmapFormat {
    pub const ALL: [Self; 2] = [Self::Png, Self::Exr];

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG · sRGB preview",
            Self::Exr => "EXR · linear HDR",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Exr => "exr",
        }
    }
}

#[derive(Clone, Debug)]
struct LightmapRequest {
    low_path: PathBuf,
    environment_path: Option<PathBuf>,
    output_path: PathBuf,
    format: LightmapFormat,
    settings: LightmapSettings,
    /// Group assignment used when reloading the mesh. `Off` keeps the whole
    /// mesh in group 0 and `settings.group` stays `None`.
    separation: BakeSeparation,
}

struct LightmapJobOutput {
    image: LightmapOutput,
    saved_path: Option<PathBuf>,
    save_error: Option<String>,
}

pub struct LightmapResult {
    pub image: Handle<Image>,
    pub stats: LightmapStats,
    pub saved_path: Option<PathBuf>,
    pub save_error: Option<String>,
    pub backend_name: Option<String>,
    /// Number of maps the run baked. `1` for a single bake; a batch result is
    /// the last finished map.
    pub tiles_baked: usize,
    /// True when the batch separated material or object groups.
    pub separated: bool,
}

struct LightmapJob {
    task: Task<Result<LightmapJobOutput, String>>,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU32>,
    started: Instant,
}

/// One queued map. `group` is `None` for an unsplit tile bake.
#[derive(Clone, Debug)]
struct BatchJob {
    tile: u32,
    group: Option<u32>,
    group_name: Option<String>,
}

/// A run of one lightmap per queued map: a UDIM tile, or one material/object
/// group within it. Each map saves to its own file; the preview follows the
/// last finished map.
struct LightmapBatch {
    low_path: PathBuf,
    /// Empty means the per-tile default output path.
    output: PathBuf,
    /// State captured when the batch started, so edits made while it runs
    /// cannot give later tiles different values.
    settings: LightmapSettings,
    format: LightmapFormat,
    environment_path: Option<PathBuf>,
    separation: BakeSeparation,
    jobs: Vec<BatchJob>,
    next: usize,
    current: Option<BatchJob>,
    saved: Vec<PathBuf>,
    errors: Vec<String>,
}

/// A light or the sun selected by the 3D light gizmos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightSelection {
    Sun,
    Scene(usize),
}

/// The grab point of a light gizmo drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightGizmoHandle {
    Sun,
    /// Move the light (and its aim point) in the camera plane.
    Body(usize),
    /// Aim a spot or area light by moving its target.
    Target(usize),
}

#[derive(Resource, Default)]
pub struct LightmapRuntime {
    pub settings: LightmapSettings,
    pub environment_path: Option<PathBuf>,
    /// Optional real-time PBR preview of the current lighting in the 3D
    /// viewport. Off by default; it does not affect the bake.
    pub preview_enabled: bool,
    /// Light or sun selected by the 3D light gizmos. Transient editor state.
    pub selected_light: Option<LightSelection>,
    /// Handle currently grabbed in the 3D light gizmos, if any.
    pub dragging_light: Option<LightGizmoHandle>,
    /// Last error while preparing the preview environment, if any.
    pub preview_error: Option<String>,
    /// True while the preview HDR is decoded and converted in the background.
    pub preview_loading: bool,
    pub output_path: String,
    pub format: LightmapFormat,
    pub status: String,
    pub last_error: Option<String>,
    pub result: Option<LightmapResult>,
    /// Texture currently registered with egui for the preview.
    pub result_tex_id: Option<egui::TextureId>,
    /// Handle whose egui texture is registered in `result_tex_id`.
    pub result_tex_handle: Option<Handle<Image>>,
    /// Set by the `--lightmap` automation flag; the first ready imported mesh
    /// is baked automatically.
    pub auto_start: bool,
    /// Bake every UDIM tile of the imported mesh instead of the selected one.
    pub bake_all_tiles: bool,
    /// Split the UDIM batch by material or object. Only used while the
    /// imported mesh has UDIM tiles.
    pub separation: BakeSeparation,
    job: Option<LightmapJob>,
    batch: Option<LightmapBatch>,
}

impl LightmapRuntime {
    /// Keep the 3D selection pointing at the same light after one is removed.
    pub fn light_removed(&mut self, index: usize) {
        self.selected_light = match self.selected_light {
            Some(LightSelection::Scene(selected)) if selected == index => None,
            Some(LightSelection::Scene(selected)) if selected > index => {
                Some(LightSelection::Scene(selected - 1))
            }
            other => other,
        };
        self.dragging_light = None;
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn progress(&self) -> f32 {
        self.job
            .as_ref()
            .map(|job| job.progress.load(Ordering::Relaxed) as f32 / 1000.0)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }

    pub fn elapsed(&self) -> Option<Duration> {
        self.job.as_ref().map(|job| job.started.elapsed())
    }

    pub fn cancel(&mut self) {
        self.batch = None;
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = "Cancelling...".into();
        }
    }

    /// Progress of the active batch as `(maps started, maps total)`.
    pub fn batch_progress(&self) -> Option<(usize, usize)> {
        self.batch
            .as_ref()
            .map(|batch| (batch.next.min(batch.jobs.len()), batch.jobs.len()))
    }

    /// Material or object group of the map currently being baked.
    pub fn current_group(&self) -> Option<&str> {
        self.batch.as_ref()?.current.as_ref()?.group_name.as_deref()
    }

    /// Start a bake. The low-poly path must point at a mesh with UVs.
    pub fn start(&mut self, low_path: PathBuf, output_path: PathBuf) {
        self.batch = None;
        self.settings.group = None;
        let output_path = ensure_extension(output_path, self.format);
        self.output_path = output_path.display().to_string();
        self.start_job(low_path, output_path);
    }

    /// Start one lightmap per UDIM tile, optionally split per material or
    /// object. Maps run sequentially so memory use stays the same as a single
    /// bake; each one is written to its own file.
    pub fn start_batch(
        &mut self,
        low_path: PathBuf,
        tiles: Vec<u32>,
        output_path: PathBuf,
        separation: BakeSeparation,
    ) {
        if tiles.is_empty() {
            self.status = "No UDIM tiles to bake".into();
            self.last_error = Some("The imported mesh has no UDIM tiles".into());
            return;
        }
        let jobs = if separation.is_on() {
            match bake_groups(&low_path, separation) {
                Ok(groups) => match separated_jobs(&groups, &tiles) {
                    Ok(jobs) => jobs,
                    Err(error) => {
                        self.status = "Nothing to separate".into();
                        self.last_error = Some(error);
                        return;
                    }
                },
                Err(error) => {
                    self.status = "Could not read bake groups".into();
                    self.last_error = Some(error);
                    return;
                }
            }
        } else {
            tiles
                .iter()
                .map(|&tile| BatchJob {
                    tile,
                    group: None,
                    group_name: None,
                })
                .collect()
        };
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.last_error = None;
        self.batch = Some(LightmapBatch {
            low_path,
            output: output_path,
            settings: self.settings.clone(),
            format: self.format,
            environment_path: self.environment_path.clone(),
            separation,
            jobs,
            next: 0,
            current: None,
            saved: Vec::new(),
            errors: Vec::new(),
        });
        self.start_next_batch_job();
    }

    /// Start the next queued map, or report the batch summary when none remain.
    /// Returns true when a new job was started.
    fn start_next_batch_job(&mut self) -> bool {
        let Some(batch) = self.batch.as_ref() else {
            return false;
        };
        let noun = if batch.separation.is_on() {
            "maps"
        } else {
            "tiles"
        };
        if batch.next >= batch.jobs.len() {
            let Some(batch) = self.batch.take() else {
                return false;
            };
            self.status = if batch.errors.is_empty() {
                format!(
                    "Baked {} {noun} · saved to {}",
                    batch.jobs.len(),
                    lightmap_batch_destination(&batch)
                )
            } else {
                format!(
                    "Baked {} {noun} · {} failed to save",
                    batch.jobs.len(),
                    batch.errors.len()
                )
            };
            if !batch.errors.is_empty() {
                self.last_error = Some(batch.errors.join("\n"));
            }
            return false;
        }

        let (job, index, total, low_path, output, settings, format, environment_path, separation) = {
            let batch = self.batch.as_mut().expect("batch is active");
            let job = batch.jobs[batch.next].clone();
            batch.next += 1;
            batch.current = Some(job.clone());
            let mut settings = batch.settings.clone();
            settings.tile = job.tile;
            settings.group = job.group;
            (
                job.clone(),
                batch.next,
                batch.jobs.len(),
                batch.low_path.clone(),
                lightmap_batch_output_path(
                    &batch.low_path,
                    batch.format,
                    &batch.output,
                    job.tile,
                    job.group_name.as_deref(),
                ),
                settings,
                batch.format,
                batch.environment_path.clone(),
                batch.separation,
            )
        };
        self.settings.tile = job.tile;
        self.settings.group = job.group;
        self.start_job_with(
            low_path,
            output,
            settings,
            format,
            environment_path,
            separation,
        );
        self.status = match &job.group_name {
            Some(group) => format!(
                "Tracing {group} · tile {} ({index} of {total})...",
                job.tile
            ),
            None => format!("Tracing tile {} ({index} of {total})...", job.tile),
        };
        true
    }

    fn start_job(&mut self, low_path: PathBuf, output_path: PathBuf) {
        let settings = self.settings.clone();
        let format = self.format;
        let environment_path = self.environment_path.clone();
        self.start_job_with(
            low_path,
            output_path,
            settings,
            format,
            environment_path,
            BakeSeparation::Off,
        );
    }

    fn start_job_with(
        &mut self,
        low_path: PathBuf,
        output_path: PathBuf,
        settings: LightmapSettings,
        format: LightmapFormat,
        environment_path: Option<PathBuf>,
        separation: BakeSeparation,
    ) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let request = LightmapRequest {
            low_path,
            environment_path,
            output_path,
            format,
            settings,
            separation,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0));
        let task_cancel = cancel.clone();
        let task_progress = progress.clone();
        let task = AsyncComputeTaskPool::get()
            .spawn(async move { run_bake(request, task_progress, task_cancel) });

        self.job = Some(LightmapJob {
            task,
            cancel,
            progress,
            started: Instant::now(),
        });
        self.status = "Loading lightmap inputs...".into();
        self.last_error = None;
    }
}

pub struct LightmapPlugin;

impl Plugin for LightmapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightmapRuntime>()
            .add_systems(Update, (auto_start_lightmap, poll_lightmap_jobs).chain());
    }
}

fn auto_start_lightmap(
    mut runtime: ResMut<LightmapRuntime>,
    mesh_import: Res<crate::import::MeshImport>,
    mut state: ResMut<EditorState>,
) {
    if !runtime.auto_start || !mesh_import.ready {
        return;
    }
    let Some(low_path) = mesh_import.source_path.clone() else {
        return;
    };
    runtime.auto_start = false;
    if mesh_import.udim.enabled && !mesh_import.udim.tiles.contains(&runtime.settings.tile) {
        runtime.settings.tile = mesh_import.udim.base_tile();
    }
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
    runtime.settings.uv_bottom_left = mesh_import.uv_bottom_left;
    runtime.start(low_path, output);
    state.right_tab = RightTab::Lightmap;
    state.show_right_dock = true;
}

fn poll_lightmap_jobs(mut runtime: ResMut<LightmapRuntime>, mut images: ResMut<Assets<Image>>) {
    let Some(mut job) = runtime.job.take() else {
        return;
    };
    let elapsed = job.started.elapsed();
    let Some(result) = block_on(poll_once(&mut job.task)) else {
        runtime.job = Some(job);
        return;
    };

    match result {
        Ok(output) => {
            let raw = output.image;
            let stats = raw.stats;
            let backend_name = raw.backend_name.clone();
            let mut image = Image::new(
                Extent3d {
                    width: raw.width,
                    height: raw.height,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                raw.pixels,
                // The preview bytes are already sRGB encoded.
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::ClampToEdge,
                address_mode_v: ImageAddressMode::ClampToEdge,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                ..default()
            });
            let handle = images.add(image);
            let saved_path = output.saved_path;
            let save_error = output.save_error;
            let (tiles_baked, separated) = runtime.batch.as_ref().map_or((1, false), |batch| {
                (batch.jobs.len(), batch.separation.is_on())
            });
            runtime.result = Some(LightmapResult {
                image: handle,
                stats,
                saved_path: saved_path.clone(),
                save_error: save_error.clone(),
                backend_name,
                tiles_baked,
                separated,
            });
            runtime.result_tex_id = None;
            if runtime.batch.is_some() {
                if let Some(batch) = runtime.batch.as_mut() {
                    if let Some(path) = saved_path {
                        batch.saved.push(path);
                    }
                    if let Some(error) = save_error {
                        batch.errors.push(match &batch.current {
                            Some(job) => match &job.group_name {
                                Some(group) => format!("{group} tile {}: {error}", job.tile),
                                None => format!("Tile {}: {error}", job.tile),
                            },
                            None => error,
                        });
                    }
                }
                runtime.start_next_batch_job();
            } else {
                runtime.status = format!(
                    "Baked {}×{} in {:.1}s",
                    stats.resolution,
                    stats.resolution,
                    elapsed.as_secs_f32()
                );
                runtime.last_error = None;
            }
        }
        Err(error) => {
            let tile = runtime
                .batch
                .as_ref()
                .and_then(|batch| batch.current.as_ref())
                .map(|job| job.tile);
            let mut errors = runtime
                .batch
                .take()
                .map(|batch| batch.errors)
                .unwrap_or_default();
            if error == "cancelled" {
                runtime.status = "Lightmap cancelled".into();
            } else {
                eprintln!("Lightmap bake failed: {error}");
                runtime.status = match tile {
                    Some(tile) => format!("Lightmap failed on tile {tile}"),
                    None => "Lightmap failed".into(),
                };
                errors.push(error);
                runtime.last_error = Some(errors.join("\n"));
            }
        }
    }
}

/// Queue one map per group and tile. A group is skipped when it has no
/// triangles in the requested tiles, which keeps a mixed-material mesh from
/// writing empty maps for tiles it never uses.
fn separated_jobs(groups: &[BakeGroup], tiles: &[u32]) -> Result<Vec<BatchJob>, String> {
    let wanted: BTreeSet<u32> = tiles.iter().copied().collect();
    let mut used_names = HashSet::new();
    let mut jobs = Vec::new();
    for group in groups {
        let file_name = unique_group_file_name(&group.name, &mut used_names);
        for tile in &group.tiles {
            if wanted.contains(tile) {
                jobs.push(BatchJob {
                    tile: *tile,
                    group: Some(group.index),
                    group_name: Some(file_name.clone()),
                });
            }
        }
    }
    if jobs.is_empty() {
        return Err("No material or object groups use the selected UDIM tiles".into());
    }
    Ok(jobs)
}

fn unique_group_file_name(name: &str, used: &mut HashSet<String>) -> String {
    let base = sanitize_group_name(name);
    let mut candidate = base.clone();
    let mut suffix = 2;
    while !used.insert(candidate.clone()) {
        candidate = format!("{base}_{suffix}");
        suffix += 1;
    }
    candidate
}

/// Make a group name safe as a filename component while keeping non-ASCII
/// letters so material names in other scripts stay readable.
fn sanitize_group_name(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    let mut last_underscore = false;
    for character in name.chars() {
        if character.is_alphanumeric() || character == '-' || character == '_' {
            output.push(character);
            last_underscore = character == '_';
        } else if !last_underscore {
            output.push('_');
            last_underscore = true;
        }
    }
    let trimmed = output.trim_matches('_');
    if trimmed.is_empty() {
        "group".into()
    } else {
        trimmed.to_string()
    }
}

/// File name for one map of a batch. An empty base path uses the default
/// naming; otherwise the tile number is appended before the extension,
/// replacing an existing UDIM suffix so it is never doubled. Separated maps add
/// the group before the tile number.
fn lightmap_batch_output_path(
    low: &Path,
    format: LightmapFormat,
    base: &Path,
    tile: u32,
    group: Option<&str>,
) -> PathBuf {
    let path = if base.as_os_str().is_empty() {
        default_lightmap_output_path(low, format, tile, true)
    } else {
        crate::bake::output_path_for_tile(base, tile, format.extension())
    };
    match group {
        Some(group) => insert_group_suffix(path, group, tile),
        None => path,
    }
}

fn insert_group_suffix(path: PathBuf, group: &str, tile: u32) -> PathBuf {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("png")
        .to_string();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("lightmap")
        .to_string();
    let tile_suffix = format!("_{tile}");
    let stem = stem.strip_suffix(&tile_suffix).unwrap_or(&stem);
    let group = sanitize_group_name(group);
    path.with_file_name(format!("{stem}_{group}{tile_suffix}.{extension}"))
}

fn lightmap_batch_destination(batch: &LightmapBatch) -> String {
    batch
        .saved
        .first()
        .and_then(|path| path.parent())
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the output folder".to_string())
}

pub fn default_lightmap_output_path(
    low: &Path,
    format: LightmapFormat,
    tile: u32,
    udim: bool,
) -> PathBuf {
    let directory = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("output")
        .join("lightmaps");
    let stem = low
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("mesh");
    let tile_suffix = if udim {
        format!("_{tile}")
    } else {
        String::new()
    };
    directory.join(format!(
        "{stem}_lightmap{tile_suffix}.{}",
        format.extension()
    ))
}

fn ensure_extension(path: PathBuf, format: LightmapFormat) -> PathBuf {
    let matches = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(format.extension()));
    if matches {
        path
    } else {
        path.with_extension(format.extension())
    }
}

fn run_bake(
    request: LightmapRequest,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
) -> Result<LightmapJobOutput, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    progress.store(20, Ordering::Relaxed);
    let low = load_bake_mesh_separated(&request.low_path, true, request.separation)
        .map(|(mesh, _groups)| mesh)
        .map_err(|error| format!("Low poly: {error}"))?;
    progress.store(80, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    let environment = match &request.environment_path {
        Some(path) => {
            let map = load_environment(path)?;
            progress.store(140, Ordering::Relaxed);
            Some(map)
        }
        None => None,
    };

    let output = match request.settings.backend {
        LightmapBackend::Cpu => ns_bake::bake_lightmap(
            &low,
            environment.as_ref(),
            &request.settings,
            &progress,
            &cancel,
        ),
        LightmapBackend::Gpu => ns_bake::bake_lightmap_gpu(
            &low,
            environment.as_ref(),
            &request.settings,
            &progress,
            &cancel,
        ),
        LightmapBackend::Auto => match ns_bake::bake_lightmap_gpu(
            &low,
            environment.as_ref(),
            &request.settings,
            &progress,
            &cancel,
        ) {
            Ok(output) => Ok(output),
            Err(ns_bake::BakeError::Cancelled) => Err(ns_bake::BakeError::Cancelled),
            Err(error) => {
                eprintln!("Lightmap GPU backend unavailable; falling back to CPU: {error}");
                ns_bake::bake_lightmap(
                    &low,
                    environment.as_ref(),
                    &request.settings,
                    &progress,
                    &cancel,
                )
            }
        },
    }
    .map_err(|error| error.to_string())?;
    progress.store(980, Ordering::Relaxed);

    let (saved_path, save_error) =
        match save_lightmap(&request.output_path, request.format, &output) {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(error)),
        };
    progress.store(1000, Ordering::Relaxed);

    Ok(LightmapJobOutput {
        image: output,
        saved_path,
        save_error,
    })
}

pub(crate) fn load_environment(path: &Path) -> Result<EnvironmentMap, String> {
    load_environment_limited(path, None)
}

/// Load an HDR/EXR (or LDR fallback) environment, optionally downsampling its
/// longest side first.
///
/// The live preview only builds a 512 px cubemap, so decoding an 8K HDR at
/// full resolution wastes hundreds of MiB and makes the first preview frame
/// feel frozen. The bake keeps the full map; the preview caps it.
pub(crate) fn load_environment_limited(
    path: &Path,
    max_side: Option<u32>,
) -> Result<EnvironmentMap, String> {
    let mut image = ns_core::io::open_image_limited(path)
        .map_err(|error| format!("Could not load HDR environment {}: {error}", path.display()))?;
    if let Some(max_side) = max_side {
        let (width, height) = (image.width(), image.height());
        let longest = width.max(height);
        if longest > max_side {
            let scale = max_side as f32 / longest as f32;
            let target_width = ((width as f32 * scale).round() as u32).max(1);
            let target_height = ((height as f32 * scale).round() as u32).max(1);
            image = image.resize(
                target_width,
                target_height,
                image::imageops::FilterType::Triangle,
            );
        }
    }
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return Err("The HDR environment has no pixels".into());
    }
    let linear = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("hdr") || extension.eq_ignore_ascii_case("exr")
        });
    let rgb = image.to_rgb32f();
    let mut pixels = Vec::with_capacity(width as usize * height as usize);
    for pixel in rgb.pixels() {
        let [r, g, b] = pixel.0;
        if linear {
            pixels.push([r, g, b]);
        } else {
            pixels.push([srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)]);
        }
    }
    EnvironmentMap::new(width, height, pixels).map_err(|error| error.to_string())
}

fn srgb_to_linear(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn save_lightmap(
    path: &Path,
    format: LightmapFormat,
    output: &LightmapOutput,
) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
    }
    match format {
        LightmapFormat::Png => {
            if let Some(pixels) = &output.pixels16 {
                // 16-bit PNG keeps the dark falloff smooth when the engine
                // accepts more than 8 bits per channel.
                let image = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(
                    output.width,
                    output.height,
                    pixels.clone(),
                )
                .ok_or_else(|| "Baked lightmap has invalid dimensions".to_string())?;
                image
                    .save_with_format(path, image::ImageFormat::Png)
                    .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
            } else {
                let image =
                    image::RgbaImage::from_raw(output.width, output.height, output.pixels.clone())
                        .ok_or_else(|| "Baked lightmap has invalid dimensions".to_string())?;
                image
                    .save_with_format(path, image::ImageFormat::Png)
                    .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
            }
        }
        LightmapFormat::Exr => {
            let pixel_count = output.width as usize * output.height as usize;
            let mut data = Vec::with_capacity(pixel_count * 4);
            for index in 0..pixel_count {
                let alpha = output.pixels[index * 4 + 3] as f32 / 255.0;
                data.push(output.hdr[index * 3]);
                data.push(output.hdr[index * 3 + 1]);
                data.push(output.hdr[index * 3 + 2]);
                data.push(alpha);
            }
            let image = image::Rgba32FImage::from_raw(output.width, output.height, data)
                .ok_or_else(|| "Baked lightmap has invalid dimensions".to_string())?;
            image
                .save_with_format(path, image::ImageFormat::OpenExr)
                .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_lightmap_output_paths_end_with_the_tile_number() {
        let low = Path::new("C:/work/panel.obj");
        assert_eq!(
            lightmap_batch_output_path(low, LightmapFormat::Png, Path::new(""), 1002, None)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("panel_lightmap_1002.png")
        );
        assert_eq!(
            lightmap_batch_output_path(
                low,
                LightmapFormat::Exr,
                Path::new("C:/out/panel.exr"),
                1002,
                None,
            )
            .file_name()
            .and_then(|name| name.to_str()),
            Some("panel_1002.exr")
        );
    }

    #[test]
    fn separated_batch_paths_put_the_group_before_the_tile() {
        let low = Path::new("C:/work/panel.obj");
        assert_eq!(
            lightmap_batch_output_path(
                low,
                LightmapFormat::Png,
                Path::new(""),
                1002,
                Some("Body"),
            )
            .file_name()
            .and_then(|name| name.to_str()),
            Some("panel_lightmap_Body_1002.png")
        );
        assert_eq!(
            lightmap_batch_output_path(
                low,
                LightmapFormat::Exr,
                Path::new("C:/out/panel_1002.exr"),
                1002,
                Some("Body / Head"),
            )
            .file_name()
            .and_then(|name| name.to_str()),
            Some("panel_Body_Head_1002.exr")
        );
    }

    #[test]
    fn separated_jobs_only_queue_tiles_a_group_uses() {
        let groups = vec![
            BakeGroup {
                index: 0,
                name: "Body".into(),
                tiles: vec![1001, 1002],
            },
            BakeGroup {
                index: 1,
                name: "Head".into(),
                tiles: vec![1002],
            },
        ];

        let jobs = separated_jobs(&groups, &[1002]).expect("groups use tile 1002");
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|job| job.tile == 1002));
        assert_eq!(jobs[0].group, Some(0));
        assert_eq!(jobs[1].group, Some(1));

        let error = separated_jobs(&groups, &[1003]).expect_err("no group uses 1003");
        assert!(error.contains("No material or object groups"));
    }

    #[test]
    fn removing_a_light_keeps_the_3d_selection_valid() {
        let mut runtime = LightmapRuntime {
            selected_light: Some(LightSelection::Scene(2)),
            dragging_light: Some(LightGizmoHandle::Body(2)),
            ..Default::default()
        };

        runtime.light_removed(0);
        assert_eq!(runtime.selected_light, Some(LightSelection::Scene(1)));
        assert_eq!(runtime.dragging_light, None);

        runtime.selected_light = Some(LightSelection::Scene(1));
        runtime.light_removed(1);
        assert_eq!(runtime.selected_light, None);

        runtime.selected_light = Some(LightSelection::Sun);
        runtime.light_removed(0);
        assert_eq!(runtime.selected_light, Some(LightSelection::Sun));
    }

    #[test]
    fn preview_loader_caps_the_longest_side() {
        let path =
            std::env::temp_dir().join(format!("n_shade_preview_limit_{}.png", std::process::id()));
        let image = image::RgbImage::from_fn(8, 4, |x, y| image::Rgb([x as u8, y as u8, 128]));
        image.save(&path).expect("write test image");

        let full = load_environment(&path).expect("load full environment");
        assert_eq!((full.width, full.height), (8, 4));

        let capped = load_environment_limited(&path, Some(4)).expect("load capped environment");
        assert_eq!((capped.width, capped.height), (4, 2));

        let _ = std::fs::remove_file(path);
    }
}
