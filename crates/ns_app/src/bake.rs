//! Background normal-map bakes for imported meshes.
//!
//! The UI owns a [`BakeRuntime`]; starting a bake spawns a task on Bevy's
//! compute pool. The worker reloads the low-poly (and optional high-poly) mesh
//! from disk in raw file space, runs the CPU baker in `ns_bake`, saves a PNG
//! and hands the pixels back for previewing.

use std::collections::{BTreeSet, HashMap};
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
use glam::Vec3;

use ns_bake::{BakeMesh, BakeMode, BakeOutput, BakeSettings, BakeSpace, BakeStats};
use ns_core::{decode_normal, GreenChannel, Grid, Layer, LayerId, LayerKind};

use crate::app::{CanvasTool, DocumentRes, EditorState, RightTab, ViewportTab};
use crate::import::MeshImport;
use crate::mesh_io::{self, RawMesh};

#[derive(Clone, Debug)]
struct BakeRequest {
    low_path: PathBuf,
    high_path: Option<PathBuf>,
    output_path: PathBuf,
    settings: BakeSettings,
}

struct BakeJobOutput {
    image: BakeOutput,
    space: BakeSpace,
    saved_path: Option<PathBuf>,
    save_error: Option<String>,
}

pub struct BakeResult {
    pub image: Handle<Image>,
    pub stats: BakeStats,
    /// Share of the covered surface whose tangent-space normal is more than a
    /// few degrees off flat. `None` for object-space bakes, which have no flat
    /// reference. Near zero means the high-poly matches the low-poly.
    pub detail: Option<f32>,
    /// Space the pixels were encoded in; used to decode the map back into
    /// canonical tangent-space normals for the document.
    pub space: BakeSpace,
    pub saved_path: Option<PathBuf>,
    pub save_error: Option<String>,
    /// Number of tiles the run baked. `1` for a single-tile bake; a batch
    /// result is the last finished tile and cannot become one document layer.
    pub tiles_baked: usize,
}

/// A run of one bake per UDIM tile. Each tile saves to its own file; the
/// preview follows the last finished tile.
struct BatchRun {
    low_path: PathBuf,
    high_path: Option<PathBuf>,
    /// Empty means the per-tile default output path.
    output: PathBuf,
    /// Settings captured when the batch started, so edits made while it runs
    /// cannot give later tiles different values.
    settings: BakeSettings,
    tiles: Vec<u32>,
    next: usize,
    current: Option<u32>,
    saved: Vec<PathBuf>,
    errors: Vec<String>,
}

struct BakeJob {
    task: Task<Result<BakeJobOutput, String>>,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU32>,
    started: Instant,
}

#[derive(Resource, Default)]
pub struct BakeRuntime {
    pub settings: BakeSettings,
    pub high_path: Option<PathBuf>,
    pub output_path: String,
    pub status: String,
    pub last_error: Option<String>,
    pub result: Option<BakeResult>,
    /// Texture currently registered with egui for the preview.
    pub result_tex_id: Option<egui::TextureId>,
    /// Handle whose egui texture is registered in `result_tex_id`.
    pub result_tex_handle: Option<Handle<Image>>,
    /// Override the 3D preview material with the baked normal map.
    pub preview_on_mesh: bool,
    /// Draw the ray cage around the low-poly in the 3D preview.
    pub show_cage: bool,
    /// Set by the `--bake` automation flag; the first ready imported mesh is
    /// baked automatically.
    pub auto_start: bool,
    /// Bottom layer created by "Add baked normals to canvas", per UDIM tile.
    /// Re-sending a bake for the same tile updates that layer instead of
    /// stacking duplicates; other tiles keep their own base layer.
    pub base_layers: HashMap<u32, LayerId>,
    /// Set by the bake panel when the result should become a document layer.
    pub pending_canvas: bool,
    /// Set by `--bake-to-canvas`; sends the first finished bake to the canvas.
    pub auto_canvas: bool,
    /// Bake every UDIM tile of the imported mesh instead of the selected one.
    pub bake_all_tiles: bool,
    applied_preview_texture: Option<Handle<Image>>,
    job: Option<BakeJob>,
    batch: Option<BatchRun>,
}

impl BakeRuntime {
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

    /// Progress of the active batch as `(tiles started, tiles total)`.
    pub fn batch_progress(&self) -> Option<(usize, usize)> {
        self.batch
            .as_ref()
            .map(|batch| (batch.next.min(batch.tiles.len()), batch.tiles.len()))
    }

    /// Start a bake. The caller is responsible for providing a valid low-poly
    /// path and (for high-to-low mode) a high-poly path.
    pub fn start(&mut self, low_path: PathBuf, high_path: Option<PathBuf>, output_path: PathBuf) {
        self.batch = None;
        self.start_job(low_path, high_path, output_path);
    }

    /// Start one bake per UDIM tile. Tiles run sequentially so memory use stays
    /// the same as a single bake; each tile is written to its own file.
    pub fn start_batch(
        &mut self,
        low_path: PathBuf,
        high_path: Option<PathBuf>,
        tiles: Vec<u32>,
        output_path: PathBuf,
    ) {
        if tiles.is_empty() {
            self.status = "No UDIM tiles to bake".into();
            self.last_error = Some("The imported mesh has no UDIM tiles".into());
            return;
        }
        let settings = self.settings.clone();
        if settings.mode == BakeMode::HighToLow && high_path.is_none() {
            self.status = "Choose a high-poly mesh first".into();
            self.last_error = Some("High-poly mesh path is missing".into());
            return;
        }
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.pending_canvas = false;
        self.auto_canvas = false;
        // A per-tile map cannot describe the whole UDIM mesh, so never leave an
        // earlier single-tile preview overriding the canvas during a batch.
        self.preview_on_mesh = false;
        self.last_error = None;
        self.batch = Some(BatchRun {
            low_path,
            high_path: if settings.mode == BakeMode::HighToLow {
                high_path
            } else {
                None
            },
            output: output_path,
            settings,
            tiles,
            next: 0,
            current: None,
            saved: Vec::new(),
            errors: Vec::new(),
        });
        self.start_next_batch_tile();
    }

    /// Start the next queued tile, or report the batch summary when none remain.
    /// Returns true when a new job was started.
    fn start_next_batch_tile(&mut self) -> bool {
        let Some(batch) = self.batch.as_ref() else {
            return false;
        };
        if batch.next >= batch.tiles.len() {
            let Some(batch) = self.batch.take() else {
                return false;
            };
            self.status = if batch.errors.is_empty() {
                format!(
                    "Baked {} tiles · saved to {}",
                    batch.tiles.len(),
                    batch_destination(&batch)
                )
            } else {
                format!(
                    "Baked {} tiles · {} failed to save",
                    batch.tiles.len(),
                    batch.errors.len()
                )
            };
            if !batch.errors.is_empty() {
                self.last_error = Some(batch.errors.join("\n"));
            }
            return false;
        }

        let (tile, index, total, low_path, high_path, output, settings) = {
            let batch = self.batch.as_mut().expect("batch is active");
            let tile = batch.tiles[batch.next];
            batch.next += 1;
            batch.current = Some(tile);
            let mut settings = batch.settings.clone();
            settings.tile = tile;
            (
                tile,
                batch.next,
                batch.tiles.len(),
                batch.low_path.clone(),
                batch.high_path.clone(),
                batch_output_path(&batch.low_path, batch.settings.mode, &batch.output, tile),
                settings,
            )
        };
        self.settings.tile = tile;
        self.start_job_with(low_path, high_path, output, settings);
        self.status = format!("Baking tile {tile} ({index} of {total})...");
        true
    }

    fn start_job(&mut self, low_path: PathBuf, high_path: Option<PathBuf>, output_path: PathBuf) {
        let settings = self.settings.clone();
        self.start_job_with(low_path, high_path, output_path, settings);
    }

    fn start_job_with(
        &mut self,
        low_path: PathBuf,
        high_path: Option<PathBuf>,
        output_path: PathBuf,
        settings: BakeSettings,
    ) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        if settings.mode == BakeMode::HighToLow && high_path.is_none() {
            self.status = "Choose a high-poly mesh first".into();
            self.last_error = Some("High-poly mesh path is missing".into());
            return;
        }

        let output_path = ensure_png_extension(output_path);
        let high_path = if settings.mode == BakeMode::HighToLow {
            high_path
        } else {
            None
        };
        let request = BakeRequest {
            low_path,
            high_path,
            output_path,
            settings,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0));
        let task_cancel = cancel.clone();
        let task_progress = progress.clone();
        let task = AsyncComputeTaskPool::get()
            .spawn(async move { run_bake(request, task_progress, task_cancel) });

        self.job = Some(BakeJob {
            task,
            cancel,
            progress,
            started: Instant::now(),
        });
        self.status = "Preparing meshes...".into();
        self.last_error = None;
    }
}

pub struct BakePlugin;

impl Plugin for BakePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BakeRuntime>().add_systems(
            Update,
            (
                auto_start_bake,
                poll_bake_jobs,
                apply_pending_canvas,
                sync_bake_preview_material,
            )
                .chain(),
        );
    }
}

fn sync_bake_preview_material(
    mut runtime: ResMut<BakeRuntime>,
    handles: Res<crate::app::PreviewHandles>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let desired = if runtime.preview_on_mesh {
        runtime
            .result
            .as_ref()
            .map(|result| result.image.clone())
            .unwrap_or_else(|| handles.normal_texture.clone())
    } else {
        handles.normal_texture.clone()
    };
    if runtime.applied_preview_texture.as_ref() == Some(&desired) {
        return;
    }
    if let Some(mut material) = materials.get_mut(&handles.material) {
        material.normal_map_texture = Some(desired.clone());
        runtime.applied_preview_texture = Some(desired);
    }
}

fn auto_start_bake(
    mut runtime: ResMut<BakeRuntime>,
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
        default_output_path(
            &low_path,
            runtime.settings.mode,
            runtime.settings.tile.max(1001),
            mesh_import.udim.enabled,
        )
    } else {
        PathBuf::from(runtime.output_path.trim())
    };
    let high_path = runtime.high_path.clone();
    runtime.settings.uv_bottom_left = mesh_import.uv_bottom_left;
    runtime.start(low_path, high_path, output);
    state.right_tab = RightTab::Bake;
    state.show_right_dock = true;
}

fn poll_bake_jobs(mut runtime: ResMut<BakeRuntime>, mut images: ResMut<Assets<Image>>) {
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
            let detail = match raw.stats.rays {
                0 => None,
                _ if output.space == BakeSpace::Object => None,
                _ => Some(detail_share(&raw.pixels, raw.stats.hits)),
            };
            let mut image = Image::new(
                Extent3d {
                    width: raw.width,
                    height: raw.height,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                raw.pixels,
                // Normal maps are data, not color: keep the texture linear so
                // the 3D preview material samples the encoded vectors as-is.
                TextureFormat::Rgba8Unorm,
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
            let stats = raw.stats;
            let space = output.space;
            let saved_path = output.saved_path;
            let save_error = output.save_error;
            let tiles_baked = runtime.batch.as_ref().map_or(1, |batch| batch.tiles.len());
            runtime.result = Some(BakeResult {
                image: handle,
                stats,
                detail,
                space,
                saved_path: saved_path.clone(),
                save_error: save_error.clone(),
                tiles_baked,
            });
            runtime.result_tex_id = None;
            if runtime.batch.is_some() {
                if let Some(batch) = runtime.batch.as_mut() {
                    if let Some(path) = saved_path {
                        batch.saved.push(path);
                    }
                    if let Some(error) = save_error {
                        batch.errors.push(match batch.current {
                            Some(tile) => format!("Tile {tile}: {error}"),
                            None => error,
                        });
                    }
                }
                runtime.start_next_batch_tile();
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
            runtime.pending_canvas = false;
            runtime.auto_canvas = false;
            let tile = runtime.batch.as_ref().and_then(|batch| batch.current);
            let mut errors = runtime
                .batch
                .take()
                .map(|batch| batch.errors)
                .unwrap_or_default();
            if error == "cancelled" {
                runtime.status = "Bake cancelled".into();
            } else {
                runtime.status = match tile {
                    Some(tile) => format!("Bake failed on tile {tile}"),
                    None => "Bake failed".into(),
                };
                errors.push(error);
                runtime.last_error = Some(errors.join("\n"));
            }
        }
    }
}

/// File name for one tile of a batch. An empty base path uses the same default
/// naming as a single bake; otherwise the tile number is appended before the
/// extension, replacing an existing UDIM suffix so it is never doubled.
fn batch_output_path(low: &Path, mode: BakeMode, base: &Path, tile: u32) -> PathBuf {
    if base.as_os_str().is_empty() {
        default_output_path(low, mode, tile, true)
    } else {
        output_path_for_tile(base, tile, "png")
    }
}

pub(crate) fn output_path_for_tile(path: &Path, tile: u32, extension: &str) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("output");
    let stem = strip_udim_suffix(stem);
    path.with_file_name(format!("{stem}_{tile}.{extension}"))
}

fn strip_udim_suffix(stem: &str) -> &str {
    let Some((base, suffix)) = stem.rsplit_once('_') else {
        return stem;
    };
    if suffix.len() == 4
        && suffix.bytes().all(|byte| byte.is_ascii_digit())
        && suffix.parse::<u32>().is_ok_and(|tile| tile >= 1001)
    {
        base
    } else {
        stem
    }
}

fn batch_destination(batch: &BatchRun) -> String {
    batch
        .saved
        .first()
        .and_then(|path| path.parent())
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the output folder".to_string())
}

/// Share of the covered surface whose tangent-space normal is more than ~6°
/// off flat. A bake that reports almost none usually means the high-poly and
/// the low-poly describe the same surface, so there is nothing to transfer.
fn detail_share(pixels: &[u8], hits: u64) -> f32 {
    if hits == 0 || pixels.is_empty() {
        return 0.0;
    }
    let mut detailed = 0usize;
    for pixel in pixels.chunks_exact(4) {
        let x = pixel[0] as f32 / 127.5 - 1.0;
        let y = pixel[1] as f32 / 127.5 - 1.0;
        if x * x + y * y > 0.0025 {
            detailed += 1;
        }
    }
    (detailed as f32 / hits as f32).clamp(0.0, 1.0)
}

/// Decode a baked RGBA8 normal map into canonical (OpenGL Y+) tangent-space
/// normals. Object-space bakes cannot be used as a tangent-space layer.
fn decode_baked_normals(
    data: &[u8],
    width: u32,
    height: u32,
    space: BakeSpace,
) -> Option<Grid<Vec3>> {
    if width == 0 || height == 0 || data.len() < width as usize * height as usize * 4 {
        return None;
    }
    let convention = if space == BakeSpace::TangentDirectX {
        GreenChannel::DirectX
    } else {
        GreenChannel::OpenGl
    };
    Some(Grid::from_fn_par(width, height, |x, y| {
        let index = ((y as usize * width as usize) + x as usize) * 4;
        let encoded = Vec3::new(
            data[index] as f32 / 255.0,
            data[index + 1] as f32 / 255.0,
            data[index + 2] as f32 / 255.0,
        );
        decode_normal(encoded, convention)
    }))
}

/// Insert or refresh the baked normals as the bottom layer for one tile.
/// Returns true when the document changed.
fn install_baked_normals(
    runtime: &mut BakeRuntime,
    doc: &mut DocumentRes,
    state: &mut EditorState,
    normal: Grid<Vec3>,
    tile: u32,
    udim: bool,
) -> bool {
    state.checkpoint_document(&doc.0);
    let existing = runtime
        .base_layers
        .get(&tile)
        .copied()
        .and_then(|id| doc.0.find_layer_mut(id));
    let updated = if let Some(layer) = existing {
        layer.kind = LayerKind::ImportedNormal { normal };
        layer.name = if udim {
            format!("Baked normals {tile}")
        } else {
            "Baked normals".into()
        };
        layer.tile = udim.then_some(tile);
        layer.touch();
        true
    } else {
        let mut layer = Layer::imported_normal(
            if udim {
                format!("Baked normals {tile}")
            } else {
                "Baked normals".into()
            },
            normal,
        );
        layer.tile = udim.then_some(tile);
        let id = layer.id;
        doc.0.insert_at(&[], 0, layer);
        runtime.base_layers.insert(tile, id);
        state.selected = Some(vec![0]);
        state.multi_selected.clear();
        state.node_selection.clear();
        state.node_hover = None;
        false
    };
    // Show the tile the bake belongs to, otherwise a per-tile layer would be
    // inserted into a canvas where it does not contribute.
    doc.0.active_tile = tile;

    // The document now owns the bake, so the 3D material must follow the
    // evaluated canvas again or newly authored layers stay invisible.
    runtime.preview_on_mesh = false;
    runtime.pending_canvas = false;
    runtime.auto_canvas = false;
    runtime.status = if updated {
        if udim {
            format!("Updated the baked base layer for tile {tile}")
        } else {
            "Updated the baked base layer".into()
        }
    } else if udim {
        format!("Added baked normals for tile {tile} as the base layer")
    } else {
        "Added baked normals as the base layer".into()
    };
    state.viewport_tab = ViewportTab::TwoD;
    state.show_height = false;
    state.canvas_tool = CanvasTool::Select;
    state.canvas_drag = None;
    state.status = runtime.status.clone();
    state.mark_dirty();
    true
}

fn apply_pending_canvas(
    mut runtime: ResMut<BakeRuntime>,
    mut doc: ResMut<DocumentRes>,
    mut state: ResMut<EditorState>,
    images: Res<Assets<Image>>,
    mesh_import: Res<MeshImport>,
) {
    if !runtime.pending_canvas && !runtime.auto_canvas {
        return;
    }
    let Some(result) = runtime.result.as_ref() else {
        return;
    };
    let (image_handle, space) = (result.image.clone(), result.space);
    if space == BakeSpace::Object {
        runtime.pending_canvas = false;
        runtime.auto_canvas = false;
        runtime.last_error = Some(
            "Object-space bakes cannot be used as a tangent-space layer. Bake in a tangent space."
                .into(),
        );
        state.status = runtime.last_error.clone().unwrap_or_default();
        return;
    }
    let Some(image) = images.get(&image_handle) else {
        return;
    };
    let Some(data) = image.data.as_ref() else {
        runtime.pending_canvas = false;
        runtime.auto_canvas = false;
        runtime.last_error = Some("The baked image is no longer readable on the CPU".into());
        state.status = runtime.last_error.clone().unwrap_or_default();
        return;
    };
    let Some(normal) = decode_baked_normals(data, image.width(), image.height(), space) else {
        runtime.pending_canvas = false;
        runtime.auto_canvas = false;
        runtime.last_error = Some("The baked image has invalid pixel data".into());
        state.status = runtime.last_error.clone().unwrap_or_default();
        return;
    };
    let tile = runtime
        .result
        .as_ref()
        .map_or(1001, |result| result.stats.tile);
    if install_baked_normals(
        &mut runtime,
        &mut doc,
        &mut state,
        normal,
        tile,
        mesh_import.udim.enabled,
    ) {
        runtime.last_error = None;
    }
}

pub fn default_output_path(low: &Path, mode: BakeMode, tile: u32, udim: bool) -> PathBuf {
    let directory = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("output")
        .join("bakes");
    let stem = low
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("mesh");
    let tile_suffix = if udim {
        format!("_{tile}")
    } else {
        String::new()
    };
    directory.join(format!("{stem}_{}{tile_suffix}.png", mode.suffix()))
}

fn ensure_png_extension(path: PathBuf) -> PathBuf {
    let is_png = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"));
    if is_png {
        path
    } else {
        path.with_extension("png")
    }
}

fn run_bake(
    request: BakeRequest,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
) -> Result<BakeJobOutput, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    progress.store(20, Ordering::Relaxed);
    let low =
        load_bake_mesh(&request.low_path, true).map_err(|error| format!("Low poly: {error}"))?;
    progress.store(80, Ordering::Relaxed);

    let high = if request.settings.mode == BakeMode::HighToLow {
        let path = request
            .high_path
            .as_ref()
            .ok_or_else(|| "High-poly mesh path is missing".to_string())?;
        let mesh = load_bake_mesh(path, false).map_err(|error| format!("High poly: {error}"))?;
        progress.store(140, Ordering::Relaxed);
        Some(mesh)
    } else {
        progress.store(140, Ordering::Relaxed);
        None
    };

    let output = ns_bake::bake(&low, high, &request.settings, &progress, &cancel)
        .map_err(|error| error.to_string())?;
    progress.store(980, Ordering::Relaxed);

    let (saved_path, save_error) = match save_png(&request.output_path, &output) {
        Ok(path) => (Some(path), None),
        Err(error) => (None, Some(error)),
    };
    progress.store(1000, Ordering::Relaxed);

    Ok(BakeJobOutput {
        image: output,
        space: request.settings.space,
        saved_path,
        save_error,
    })
}

/// How a UDIM lightmap batch separates its output.
///
/// The bake still uses every triangle as an occluder; the split only decides
/// which triangles are rasterized into each map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BakeSeparation {
    /// One map per tile, covering every triangle.
    #[default]
    Off,
    /// One map per material and tile.
    Material,
    /// One map per object and tile.
    Object,
}

impl BakeSeparation {
    pub const ALL: [Self; 3] = [Self::Off, Self::Material, Self::Object];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Material => "Per material",
            Self::Object => "Per object",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Off => {
                "One map per tile; every triangle in the tile shares it. Choose Per material or \
                 Per object to write separate maps."
            }
            Self::Material => {
                "One lightmap per material and tile. Meshes without material names fall back \
                 to their object name. Other materials still cast shadows into each map."
            }
            Self::Object => {
                "One lightmap per object and tile. Other objects still cast shadows into each \
                 map. Instanced objects share one group when they carry the same name."
            }
        }
    }

    pub fn is_on(self) -> bool {
        self != Self::Off
    }
}

/// One separated lightmap target: a material or object and the UDIM tiles it
/// actually occupies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BakeGroup {
    /// Index used by [`ns_bake::BakeMesh::triangle_group`].
    pub index: u32,
    pub name: String,
    /// Sorted UDIM tiles with at least one triangle in this group.
    pub tiles: Vec<u32>,
}

pub(crate) fn load_bake_mesh(path: &Path, require_uvs: bool) -> Result<BakeMesh, String> {
    load_bake_mesh_separated(path, require_uvs, BakeSeparation::Off).map(|(mesh, _groups)| mesh)
}

/// Load the bake mesh and, when `separation` is on, assign a stable group index
/// to every triangle. Group names are returned in first-seen order so the UI
/// and the background worker agree without sharing state.
pub(crate) fn load_bake_mesh_separated(
    path: &Path,
    require_uvs: bool,
    separation: BakeSeparation,
) -> Result<(BakeMesh, Vec<String>), String> {
    let raw = mesh_io::load_any(path)?;
    let mut merged = BakeMesh::default();
    let mut used = 0usize;
    let mut names: Vec<String> = Vec::new();
    let mut index_by_name: HashMap<String, u32> = HashMap::new();
    for mesh in &raw {
        if require_uvs {
            let Some(uvs) = &mesh.uvs else {
                continue;
            };
            if uvs.len() != mesh.positions.len() {
                continue;
            }
        }
        let group = separation.is_on().then(|| {
            let mut name = match separation {
                BakeSeparation::Material => mesh
                    .material
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| mesh.object_name().to_string()),
                BakeSeparation::Object | BakeSeparation::Off => mesh.object_name().to_string(),
            };
            if name.trim().is_empty() {
                name = format!("Group {}", names.len() + 1);
            }
            *index_by_name.entry(name.clone()).or_insert_with(|| {
                let index = names.len() as u32;
                names.push(name);
                index
            })
        });
        append_raw(&mut merged, mesh, require_uvs, group);
        used += 1;
    }
    if used == 0 {
        return Err(if require_uvs {
            "The file has no UV-mapped meshes".into()
        } else {
            "The file contains no meshes".into()
        });
    }
    if merged.indices.len() < 3 {
        return Err("The file contains no triangles".into());
    }
    Ok((merged, names))
}

/// List the material or object groups of a mesh and the UDIM tiles each one
/// uses. Empty when `separation` is off or the file exposes no names.
pub(crate) fn bake_groups(
    path: &Path,
    separation: BakeSeparation,
) -> Result<Vec<BakeGroup>, String> {
    if !separation.is_on() {
        return Ok(Vec::new());
    }
    let (mesh, names) = load_bake_mesh_separated(path, true, separation)?;
    let mut tiles: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); names.len()];
    let offset = uv_offset(&mesh.uvs);
    for (triangle_index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        let Some(local) = triangle_uvs(&mesh, [triangle[0], triangle[1], triangle[2]]) else {
            continue;
        };
        let centroid = (local[0] + local[1] + local[2]) / 3.0;
        let u = (centroid.x - offset.x).floor().max(0.0) as i64;
        let v = (centroid.y - offset.y).floor().max(0.0) as i64;
        let tile = (1001 + u + 10 * v).max(1) as u32;
        if let Some(group) = tiles.get_mut(mesh.triangle_group(triangle_index) as usize) {
            group.insert(tile);
        }
    }
    Ok(names
        .into_iter()
        .enumerate()
        .filter_map(|(index, name)| {
            let tiles = tiles.get(index)?;
            (!tiles.is_empty()).then(|| BakeGroup {
                index: index as u32,
                name,
                tiles: tiles.iter().copied().collect(),
            })
        })
        .collect())
}

fn uv_offset(uvs: &[[f32; 2]]) -> glam::Vec2 {
    let mut min = glam::Vec2::splat(f32::INFINITY);
    for uv in uvs {
        let uv = glam::Vec2::from_array(*uv);
        if uv.is_finite() {
            min = min.min(uv);
        }
    }
    if !min.is_finite() {
        return glam::Vec2::ZERO;
    }
    glam::Vec2::new(
        if min.x >= 1000.0 { min.x.floor() } else { 0.0 },
        if min.y >= 1000.0 { min.y.floor() } else { 0.0 },
    )
}

fn triangle_uvs(mesh: &BakeMesh, triangle: [u32; 3]) -> Option<[glam::Vec2; 3]> {
    let mut uvs = [glam::Vec2::ZERO; 3];
    for (slot, index) in triangle.into_iter().enumerate() {
        let uv = mesh.uvs.get(index as usize)?;
        let uv = glam::Vec2::from_array(*uv);
        if !uv.is_finite() {
            return None;
        }
        uvs[slot] = uv;
    }
    Some(uvs)
}

fn append_raw(merged: &mut BakeMesh, mesh: &RawMesh, keep_uvs: bool, group: Option<u32>) {
    let base = merged.positions.len() as u32;
    let normals = if mesh.normals.len() == mesh.positions.len() {
        mesh.normals.clone()
    } else {
        mesh_io::smooth_normals(&mesh.positions, &mesh.indices)
    };
    merged.positions.extend_from_slice(&mesh.positions);
    merged.normals.extend_from_slice(&normals);
    if keep_uvs {
        if let Some(uvs) = &mesh.uvs {
            merged.uvs.extend_from_slice(uvs);
        }
    }
    for triangle in mesh.indices.chunks_exact(3) {
        if triangle
            .iter()
            .all(|index| (*index as usize) < mesh.positions.len())
        {
            merged.indices.extend_from_slice(&[
                base + triangle[0],
                base + triangle[1],
                base + triangle[2],
            ]);
            if let Some(group) = group {
                merged.triangle_groups.push(group);
            }
        }
    }
}

fn save_png(path: &Path, output: &BakeOutput) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
    }
    let image = image::RgbaImage::from_raw(output.width, output.height, output.pixels.clone())
        .ok_or_else(|| "Baked image has invalid dimensions".to_string())?;
    image
        .save_with_format(path, image::ImageFormat::Png)
        .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;
    use ns_core::{encode_normal, Document};

    #[test]
    fn batch_output_paths_end_with_the_tile_number() {
        let base = Path::new("C:/out/panel.png");
        assert_eq!(
            output_path_for_tile(base, 1001, "png")
                .file_name()
                .and_then(|name| name.to_str()),
            Some("panel_1001.png")
        );
        // A path that already carries this tile's suffix is not doubled.
        assert_eq!(
            output_path_for_tile(Path::new("C:/out/panel_1001.png"), 1001, "png")
                .file_name()
                .and_then(|name| name.to_str()),
            Some("panel_1001.png")
        );
        // The previous tile's suffix is replaced.
        assert_eq!(
            output_path_for_tile(Path::new("C:/out/panel_1001.png"), 1002, "png")
                .file_name()
                .and_then(|name| name.to_str()),
            Some("panel_1002.png")
        );

        let low = Path::new("C:/work/panel.obj");
        assert_eq!(
            batch_output_path(low, BakeMode::HighToLow, Path::new(""), 1003)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("panel_normal_high_1003.png")
        );
    }

    #[test]
    fn detail_share_counts_sloped_texels() {
        let flat = [128u8, 128, 255, 255];
        let mut pixels = Vec::new();
        for _ in 0..4 {
            pixels.extend_from_slice(&flat);
        }
        assert_eq!(detail_share(&pixels, 4), 0.0);

        pixels[0..4].copy_from_slice(&[255, 128, 255, 255]);
        assert!((detail_share(&pixels, 4) - 0.25).abs() < 1e-5);
        // No hits means no surface to describe.
        assert_eq!(detail_share(&pixels, 0), 0.0);
    }

    fn pixel(normal: Vec3, space: BakeSpace) -> [u8; 4] {
        let convention = if space == BakeSpace::TangentDirectX {
            GreenChannel::DirectX
        } else {
            GreenChannel::OpenGl
        };
        let encoded = encode_normal(normal, convention);
        [
            (encoded.x * 255.0).round() as u8,
            (encoded.y * 255.0).round() as u8,
            (encoded.z * 255.0).round() as u8,
            255,
        ]
    }

    #[test]
    fn directx_baked_pixels_decode_back_to_canonical_normals() {
        let normal = Vec3::new(0.25, 0.5, 0.829).normalize();
        let data = pixel(normal, BakeSpace::TangentDirectX);
        let grid =
            decode_baked_normals(&data, 1, 1, BakeSpace::TangentDirectX).expect("valid pixels");
        assert!((*grid.get(0, 0) - normal).length() < 0.02);
    }

    #[test]
    fn baked_normals_become_the_bottom_layer_and_refresh_in_place() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(4)));
        doc.0.layers.push(Layer::flat("top"));
        let mut state = EditorState::default();
        let mut runtime = BakeRuntime {
            preview_on_mesh: true,
            ..default()
        };

        let first = Grid::from_fn(4, 4, |_, _| Vec3::new(0.0, 0.6, 0.8).normalize());
        assert!(install_baked_normals(
            &mut runtime,
            &mut doc,
            &mut state,
            first,
            1001,
            false
        ));
        assert_eq!(doc.0.layers.len(), 2);
        assert!(matches!(
            doc.0.layers[0].kind,
            LayerKind::ImportedNormal { .. }
        ));
        assert_eq!(doc.0.layers[0].name, "Baked normals");
        assert_eq!(state.selected, Some(vec![0]));
        assert_eq!(state.viewport_tab, ViewportTab::TwoD);
        assert!(!runtime.preview_on_mesh);
        assert_eq!(state.undo.len(), 1);

        let id = runtime
            .base_layers
            .get(&1001)
            .copied()
            .expect("base layer id");
        assert!(install_baked_normals(
            &mut runtime,
            &mut doc,
            &mut state,
            Grid::new(4, 4, Vec3::Z),
            1001,
            false
        ));
        assert_eq!(doc.0.layers.len(), 2, "re-sending must not stack copies");
        assert_eq!(doc.0.layers[0].id, id);
        assert_eq!(state.undo.len(), 2);
    }

    #[test]
    fn per_tile_bakes_keep_separate_base_layers() {
        let mut doc = DocumentRes::new(Document::new(UVec2::splat(4)));
        let mut state = EditorState::default();
        let mut runtime = BakeRuntime::default();
        let normal = Grid::from_fn(4, 4, |_, _| Vec3::new(0.0, 0.6, 0.8).normalize());

        assert!(install_baked_normals(
            &mut runtime,
            &mut doc,
            &mut state,
            normal.clone(),
            1001,
            true
        ));
        assert!(install_baked_normals(
            &mut runtime,
            &mut doc,
            &mut state,
            normal.clone(),
            1002,
            true
        ));

        assert_eq!(doc.0.layers.len(), 2);
        assert_eq!(runtime.base_layers.len(), 2);
        assert_eq!(doc.0.layers[0].name, "Baked normals 1002");
        assert_eq!(doc.0.layers[0].tile, Some(1002));
        assert_eq!(doc.0.active_tile, 1002, "the baked tile becomes visible");

        // Re-sending tile 1001 refreshes that layer instead of stacking a third.
        assert!(install_baked_normals(
            &mut runtime,
            &mut doc,
            &mut state,
            normal,
            1001,
            true
        ));
        assert_eq!(doc.0.layers.len(), 2);
        assert_eq!(doc.0.active_tile, 1001);
    }

    #[test]
    fn separation_groups_use_material_and_object_names() {
        let directory = std::env::temp_dir().join(format!(
            "n-shade_separation_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&directory).expect("create temp folder");
        let obj = directory.join("panel.obj");
        std::fs::write(
            directory.join("panel.mtl"),
            "newmtl Skin\nKd 1 1 1\nnewmtl Hair\nKd 1 1 1\n",
        )
        .expect("write MTL");
        // Body uses tile 1001, Head uses tile 1002, so each group only queues the
        // tile it actually occupies.
        std::fs::write(
            &obj,
            "mtllib panel.mtl\n\
             v 0 0 0\nv 1 0 0\nv 0 1 0\n\
             v 2 0 0\nv 3 0 0\nv 2 1 0\n\
             vt 0 0\nvt 1 0\nvt 0 1\n\
             vt 1 0\nvt 2 0\nvt 1 1\n\
             usemtl Skin\no Body\nf 1/1 2/2 3/3\n\
             usemtl Hair\no Head\nf 4/4 5/5 6/6\n",
        )
        .expect("write OBJ");

        let objects = bake_groups(&obj, BakeSeparation::Object).expect("object groups");
        assert_eq!(
            objects
                .iter()
                .map(|group| group.name.as_str())
                .collect::<Vec<_>>(),
            ["Body", "Head"]
        );
        assert_eq!(objects[0].tiles, vec![1001]);
        assert_eq!(objects[1].tiles, vec![1002]);

        let materials = bake_groups(&obj, BakeSeparation::Material).expect("material groups");
        assert_eq!(
            materials
                .iter()
                .map(|group| group.name.as_str())
                .collect::<Vec<_>>(),
            ["Skin", "Hair"]
        );

        let (mesh, names) = load_bake_mesh_separated(&obj, true, BakeSeparation::Object)
            .expect("grouped bake mesh");
        assert_eq!(names, ["Body", "Head"]);
        assert_eq!(mesh.triangle_groups, vec![0, 1]);

        let _ = std::fs::remove_dir_all(directory);
    }
}
