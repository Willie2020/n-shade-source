//! Interactive 3D triplanar projection preview.
//!
//! A layer or group is evaluated in isolation, projected onto the imported
//! preview mesh through a transformed bounding box, and the resulting UV-space
//! normal map is composited back into a preview texture. The 3D material shows
//! that texture while the 2D canvas keeps the editable stack; **Apply** commits
//! the projected layer into the document.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool, Task};
use glam::{EulerRot, Quat, Vec2, Vec3};

use ns_bake::{project_triplanar, BakeMesh, TriplanarSettings};
use ns_core::{Grid, Layer, LayerId, LayerKind};

use crate::app::{DocumentRes, EditorState};

/// Minimum time between automatic preview rebuilds while a slider is dragged.
const PREVIEW_THROTTLE: Duration = Duration::from_millis(120);

/// Handle picked on the projection cage in the 3D viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionHandle {
    Move,
    AxisX,
    AxisY,
    AxisZ,
    Scale,
    Rotate,
}

#[derive(Resource)]
pub struct ProjectionRuntime {
    pub enabled: bool,
    pub source: Option<LayerId>,
    /// Bounding-box centre/size in normalized mesh space.
    pub center: Vec3,
    pub size: Vec3,
    /// Euler rotation in degrees, XYZ order.
    pub rotation_deg: Vec3,
    /// Triplanar axis sharpness (higher keeps faces crisper).
    pub sharpness: f32,
    /// Soft edge outside the bounding box, in normalized units.
    pub feather: f32,
    /// Handle currently grabbed in the viewport, if any.
    pub(crate) drag_handle: Option<ProjectionHandle>,
    pub(crate) drag_start: Vec2,
    pub(crate) drag_start_center: Vec3,
    pub(crate) drag_start_size: Vec3,
    pub(crate) drag_start_rotation: Vec3,
    pub(crate) drag_start_distance: f32,
    pub(crate) drag_start_angle: f32,

    pub mesh_path: Option<PathBuf>,
    pub mesh: Option<Arc<BakeMesh>>,
    /// True when the imported mesh uses bottom-left UVs (OBJ/FBX/DAE/USD).
    /// The projection raster must flip rows exactly like the bake does or the
    /// layer lands upside down in the 2D canvas.
    pub uv_bottom_left: bool,
    pub mesh_bounds_min: Vec3,
    pub mesh_bounds_max: Vec3,
    pub mesh_ready: bool,
    pub error: Option<String>,
    pub status: String,

    pub preview_resolution: u32,
    pub preview_texture: Option<Handle<Image>>,

    pub last_projected: Option<Arc<Grid<Vec3>>>,
    pub last_coverage: Option<Grid<f32>>,

    dirty: bool,
    serial: u64,
    last_started: Instant,
    source_grid: Option<Arc<Grid<Vec3>>>,
    source_edit_serial: u64,
    job: Option<ProjectionJob>,
}

struct ProjectionJob {
    task: Task<Result<ProjectionComputed, String>>,
    started: Instant,
}

struct ProjectionComputed {
    serial: u64,
    projected: Grid<Vec3>,
    coverage: Grid<f32>,
    preview: Grid<Vec3>,
}

impl Default for ProjectionRuntime {
    fn default() -> Self {
        Self {
            enabled: false,
            source: None,
            center: Vec3::ZERO,
            size: Vec3::ONE,
            rotation_deg: Vec3::ZERO,
            sharpness: 4.0,
            feather: 0.04,
            drag_handle: None,
            drag_start: Vec2::ZERO,
            drag_start_center: Vec3::ZERO,
            drag_start_size: Vec3::ONE,
            drag_start_rotation: Vec3::ZERO,
            drag_start_distance: 0.0,
            drag_start_angle: 0.0,
            mesh_path: None,
            mesh: None,
            uv_bottom_left: false,
            mesh_bounds_min: Vec3::splat(-0.5),
            mesh_bounds_max: Vec3::splat(0.5),
            mesh_ready: false,
            error: None,
            status: String::new(),
            preview_resolution: 512,
            preview_texture: None,
            last_projected: None,
            last_coverage: None,
            dirty: false,
            serial: 0,
            last_started: Instant::now() - PREVIEW_THROTTLE,
            source_grid: None,
            source_edit_serial: 0,
            job: None,
        }
    }
}

impl ProjectionRuntime {
    pub fn rotation(&self) -> Quat {
        Quat::from_euler(
            EulerRot::XYZ,
            self.rotation_deg.x.to_radians(),
            self.rotation_deg.y.to_radians(),
            self.rotation_deg.z.to_radians(),
        )
    }

    pub fn enable_for(&mut self, id: LayerId) {
        self.enabled = true;
        self.error = None;
        self.source = Some(id);
        self.source_grid = None;
        self.last_projected = None;
        self.last_coverage = None;
        self.drag_handle = None;
        self.mark_dirty();
    }

    pub fn disable(&mut self) {
        self.enabled = false;
        self.dirty = false;
        self.job = None;
        self.drag_handle = None;
        self.status.clear();
    }

    pub fn mark_dirty(&mut self) {
        self.serial = self.serial.wrapping_add(1);
        self.dirty = true;
    }

    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn fit_to_mesh(&mut self) {
        if !self.mesh_ready {
            return;
        }
        self.center = (self.mesh_bounds_min + self.mesh_bounds_max) * 0.5;
        self.size = (self.mesh_bounds_max - self.mesh_bounds_min)
            .abs()
            .max(Vec3::splat(0.03));
        self.rotation_deg = Vec3::ZERO;
        self.mark_dirty();
    }

    pub fn reset_bounds(&mut self) {
        self.center = Vec3::ZERO;
        self.size = Vec3::ONE;
        self.rotation_deg = Vec3::ZERO;
        self.mark_dirty();
    }

    fn preview_size(&self) -> u32 {
        self.preview_resolution.clamp(64, 1024)
    }
}

pub struct ProjectionPlugin;

impl Plugin for ProjectionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProjectionRuntime>().add_systems(
            Update,
            (update_projection_preview, drive_projection_material).chain(),
        );
    }
}

/// Point the 3D material at the projected preview while the tool is active and
/// restore the live normal map when it closes. The evaluator keeps updating the
/// shared normal texture underneath, so leaving the tool has nothing to undo.
fn drive_projection_material(
    runtime: Res<ProjectionRuntime>,
    handles: Option<Res<crate::app::PreviewHandles>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(handles) = handles else {
        return;
    };
    let Some(mut material) = materials.get_mut(&handles.material) else {
        return;
    };
    let wanted = if runtime.enabled {
        runtime
            .preview_texture
            .clone()
            .unwrap_or_else(|| handles.normal_texture.clone())
    } else {
        handles.normal_texture.clone()
    };
    if material.normal_map_texture.as_ref() != Some(&wanted) {
        material.normal_map_texture = Some(wanted);
    }
}

#[allow(clippy::too_many_arguments)]
fn update_projection_preview(
    mut runtime: ResMut<ProjectionRuntime>,
    doc: Res<DocumentRes>,
    state: Res<EditorState>,
    mesh_import: Res<crate::import::MeshImport>,
    mut images: ResMut<Assets<Image>>,
) {
    poll_projection_job(&mut runtime, &mut images);

    if !runtime.enabled {
        return;
    }
    let Some(selected) = state
        .selected
        .as_deref()
        .and_then(|path| doc.0.layer_by_path(path))
    else {
        runtime.error = Some("Select a layer or group to project".into());
        return;
    };
    if runtime.source != Some(selected.id) {
        runtime.source = Some(selected.id);
        runtime.source_grid = None;
        runtime.last_projected = None;
        runtime.last_coverage = None;
        runtime.mark_dirty();
    }
    if !supports_projection(selected) || selected.is_path_guide() {
        // Do not leave a stale projection on the model when the selection
        // moves to a layer that cannot be projected.
        runtime.preview_texture = None;
        runtime.last_projected = None;
        runtime.error = Some("Select a shape, group or normal layer to project".into());
        return;
    }

    sync_mesh(&mut runtime, &mesh_import);

    if runtime.mesh_ready
        && (runtime.source_edit_serial != state.edit_serial || runtime.source_grid.is_none())
    {
        runtime.source_edit_serial = state.edit_serial;
        runtime.source_grid = doc.0.evaluate_layer_normal(selected.id).map(Arc::new);
        runtime.mark_dirty();
    }

    if runtime.source_grid.is_none() {
        runtime.error = Some("This layer has no normal-map content to project".into());
        return;
    }
    if runtime.job.is_some() {
        return;
    }
    if !runtime.dirty {
        return;
    }
    if runtime.last_started.elapsed() < PREVIEW_THROTTLE {
        return;
    }
    if !start_projection_job(&mut runtime, &doc, selected.id) {
        return;
    }
    runtime.last_started = Instant::now();
}

fn sync_mesh(runtime: &mut ProjectionRuntime, mesh_import: &crate::import::MeshImport) {
    if runtime.mesh_path == mesh_import.source_path && runtime.mesh.is_some() {
        return;
    }
    runtime.mesh_path = mesh_import.source_path.clone();
    runtime.mesh = None;
    runtime.mesh_ready = false;
    runtime.uv_bottom_left = mesh_import.uv_bottom_left;
    runtime.mesh_bounds_min = Vec3::splat(-0.5);
    runtime.mesh_bounds_max = Vec3::splat(0.5);
    let Some(path) = &runtime.mesh_path else {
        runtime.error = Some("Import a UV-mapped mesh in the Mesh tab".into());
        return;
    };
    runtime.status = "Loading projection mesh...".into();
    match crate::bake::load_bake_mesh(path, true) {
        Ok(mesh) => {
            let (min, max) = mesh_bounds(&mesh);
            runtime.mesh = Some(Arc::new(mesh));
            runtime.mesh_ready = true;
            runtime.error = None;
            runtime.status = "Projection mesh ready".into();
            if let Some(extent) = (max - min).max_element().is_finite().then(|| 1.0) {
                let _ = extent;
            }
            let center = (min + max) * 0.5;
            let extent = (max - min).max_element().max(1e-5);
            runtime.mesh_bounds_min = (min - center) / extent;
            runtime.mesh_bounds_max = (max - center) / extent;
            runtime.center = (runtime.mesh_bounds_min + runtime.mesh_bounds_max) * 0.5;
            runtime.size = (runtime.mesh_bounds_max - runtime.mesh_bounds_min)
                .abs()
                .max(Vec3::splat(0.03));
            runtime.source_grid = None;
            runtime.mark_dirty();
        }
        Err(error) => {
            runtime.error = Some(error.clone());
            runtime.status = error;
        }
    }
}

fn mesh_bounds(mesh: &BakeMesh) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for position in &mesh.positions {
        let p = Vec3::from_array(*position);
        if !p.is_finite() {
            continue;
        }
        min = min.min(p);
        max = max.max(p);
    }
    if !min.is_finite() || !max.is_finite() {
        return (Vec3::splat(-0.5), Vec3::splat(0.5));
    }
    (min, max)
}

fn projection_settings(runtime: &ProjectionRuntime, resolution: u32) -> TriplanarSettings {
    TriplanarSettings {
        resolution,
        padding: 6,
        uv_bottom_left: runtime.uv_bottom_left,
        center: runtime.center.to_array(),
        size: runtime.size.to_array(),
        rotation: runtime.rotation().to_array(),
        sharpness: runtime.sharpness,
        edge_feather: runtime.feather,
        uv_window: [0.0, 0.0, 1.0, 1.0],
    }
}

fn start_projection_job(
    runtime: &mut ProjectionRuntime,
    doc: &DocumentRes,
    source_id: LayerId,
) -> bool {
    let (Some(mesh), Some(source_grid)) = (runtime.mesh.clone(), runtime.source_grid.clone())
    else {
        return false;
    };
    let Some(source_path) = doc.0.find_path(source_id) else {
        runtime.error = Some("The projection source is no longer in the stack".into());
        return false;
    };
    let Some(source_layer) = doc.0.layer_by_path(&source_path).cloned() else {
        return false;
    };
    let mut preview_doc = doc.0.clone();
    let preview_size = runtime.preview_size();
    preview_doc.size = glam::UVec2::splat(preview_size);
    let settings = projection_settings(runtime, preview_size);
    let serial = runtime.serial;
    let name = if source_layer.name.is_empty() {
        "Projection".to_string()
    } else {
        format!("{} projection", source_layer.name)
    };
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let output = project_triplanar(&mesh, &settings, |uv| sample_normal(&source_grid, uv))
            .map_err(|error| error.to_string())?;
        let width = output.width;
        let height = output.height;
        let projected = Grid::from_fn(width, height, |x, y| {
            Vec3::from_array(output.normals[(y * width + x) as usize])
        });
        let coverage = Grid::from_fn(width, height, |x, y| {
            output.coverage[(y * width + x) as usize]
        });
        let mut layer = Layer::imported_normal(name, projected.clone());
        layer.opacity = source_layer.opacity;
        layer.blend = source_layer.blend;
        layer.mask = Some(coverage.clone());
        layer.touch_mask();

        let (index, parent) = source_path
            .split_last()
            .map(|(index, parent)| (*index, parent.to_vec()))
            .ok_or_else(|| "The projection source has no stack position".to_string())?;
        preview_doc.remove_at(&source_path);
        preview_doc.insert_at(&parent, index, layer);
        let preview = preview_doc.evaluate();
        Ok(ProjectionComputed {
            serial,
            projected,
            coverage,
            preview,
        })
    });
    runtime.job = Some(ProjectionJob {
        task,
        started: Instant::now(),
    });
    runtime.error = None;
    runtime.status = "Projecting triplanar normals...".into();
    true
}

fn poll_projection_job(runtime: &mut ProjectionRuntime, images: &mut Assets<Image>) {
    let Some(mut job) = runtime.job.take() else {
        return;
    };
    let elapsed = job.started.elapsed();
    let Some(result) = block_on(poll_once(&mut job.task)) else {
        runtime.job = Some(job);
        return;
    };
    match result {
        Ok(computed) => {
            if computed.serial != runtime.serial {
                // A newer transform/source was requested while this task ran.
                runtime.dirty = true;
                return;
            }
            upload_preview(runtime, images, &computed.preview);
            runtime.last_projected = Some(Arc::new(computed.projected));
            runtime.last_coverage = Some(computed.coverage);
            runtime.dirty = false;
            runtime.error = None;
            runtime.status = format!(
                "Projection preview · {:.0} ms",
                elapsed.as_secs_f32() * 1000.0
            );
        }
        Err(error) => {
            runtime.error = Some(error);
            runtime.status = "Projection failed".into();
            runtime.dirty = false;
        }
    }
}

fn upload_preview(
    runtime: &mut ProjectionRuntime,
    images: &mut Assets<Image>,
    preview: &Grid<Vec3>,
) {
    let rgba = crate::app::normal_rgba(preview);
    let image = normal_preview_image(preview.w, preview.h, rgba);
    match &runtime.preview_texture {
        Some(handle) => {
            if let Some(mut existing) = images.get_mut(handle) {
                *existing = image;
            }
        }
        None => runtime.preview_texture = Some(images.add(image)),
    }
}

fn normal_preview_image(width: u32, height: u32, rgba: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8Unorm,
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

fn sample_normal(grid: &Grid<Vec3>, uv: [f32; 2]) -> [f32; 3] {
    if grid.w == 0 || grid.h == 0 {
        return [0.0, 0.0, 1.0];
    }
    let w = grid.w as f32;
    let h = grid.h as f32;
    let x = (uv[0].clamp(0.0, 1.0) * w - 0.5).clamp(0.0, (grid.w - 1) as f32);
    let y = (uv[1].clamp(0.0, 1.0) * h - 0.5).clamp(0.0, (grid.h - 1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(grid.w - 1);
    let y1 = (y0 + 1).min(grid.h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let a = *grid.get(x0, y0);
    let b = *grid.get(x1, y0);
    let c = *grid.get(x0, y1);
    let d = *grid.get(x1, y1);
    let top = a.lerp(b, fx);
    let bottom = c.lerp(d, fx);
    top.lerp(bottom, fy).normalize_or(Vec3::Z).to_array()
}

/// Commit the latest projected normal map into the document. The source layer
/// or group is hidden and a UV-space projected layer is inserted above it.
pub(crate) fn apply_projection_now(
    state: &mut EditorState,
    doc: &mut DocumentRes,
    runtime: &mut ProjectionRuntime,
) -> bool {
    if runtime.is_busy() || runtime.dirty {
        state.status = "Projection preview is still updating".into();
        return false;
    }
    let Some(source_id) = runtime.source else {
        state.status = "No projection source".into();
        return false;
    };
    if runtime.last_projected.is_none() {
        state.status = "No projection preview to apply".into();
        return false;
    }
    let Some(source_path) = doc.0.find_path(source_id) else {
        state.status = "The projection source is no longer in the stack".into();
        return false;
    };
    let Some(source) = doc.0.layer_by_path(&source_path).cloned() else {
        return false;
    };
    let (Some(mesh), Some(source_grid)) = (runtime.mesh.clone(), runtime.source_grid.clone())
    else {
        state.status = "The projection mesh or source is no longer available".into();
        return false;
    };
    let Some((index, parent)) = source_path
        .split_last()
        .map(|(index, parent)| (*index, parent.to_vec()))
    else {
        return false;
    };
    // The live preview is deliberately small; bake at document resolution so
    // the layer that lands in the 2D canvas keeps every detail.
    let resolution = doc.0.size.x.max(doc.0.size.y).clamp(64, 4096);
    let settings = projection_settings(runtime, resolution);
    let output = match project_triplanar(&mesh, &settings, |uv| sample_normal(&source_grid, uv)) {
        Ok(output) => output,
        Err(error) => {
            state.status = format!("Projection failed: {error}");
            return false;
        }
    };
    let width = output.width;
    let height = output.height;
    let projected = Grid::from_fn(width, height, |x, y| {
        Vec3::from_array(output.normals[(y * width + x) as usize])
    });
    let coverage = Grid::from_fn(width, height, |x, y| {
        output.coverage[(y * width + x) as usize]
    });
    let name = if source.name.is_empty() {
        "Projected normals".to_string()
    } else {
        format!("{} projected", source.name)
    };
    state.checkpoint_document(&doc.0);
    let mut layer = Layer::imported_normal(name, projected);
    layer.opacity = source.opacity;
    layer.blend = source.blend;
    layer.mask = Some(coverage);
    layer.touch_mask();
    if let Some(original) = doc.0.find_layer_mut(source_id) {
        original.visible = false;
        original.touch();
    }
    if !doc.0.insert_at(&parent, index + 1, layer) {
        state.status = "Could not insert the projected layer".into();
        return false;
    }
    let mut new_path = parent;
    new_path.push(index + 1);
    state.selected = Some(new_path);
    state.multi_selected.clear();
    state.mark_dirty();
    runtime.disable();
    state.status = "Projection applied · source hidden, projected layer selected".into();
    true
}

/// True when a projection can be previewed for this layer kind.
pub(crate) fn supports_projection(layer: &Layer) -> bool {
    matches!(
        layer.kind,
        LayerKind::Shape(_)
            | LayerKind::Group { .. }
            | LayerKind::ImportedNormal { .. }
            | LayerKind::PhotoHeight(_)
            | LayerKind::HeightPaint { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;

    #[test]
    fn fit_uses_normalized_mesh_bounds() {
        let mut runtime = ProjectionRuntime {
            mesh_ready: true,
            mesh_bounds_min: Vec3::new(-0.5, -0.25, -0.1),
            mesh_bounds_max: Vec3::new(0.5, 0.25, 0.1),
            ..Default::default()
        };
        runtime.fit_to_mesh();
        assert!((runtime.center - Vec3::ZERO).length() < 1e-6);
        assert!((runtime.size - Vec3::new(1.0, 0.5, 0.2)).length() < 1e-6);
        assert!(runtime.dirty);
    }

    /// Unit quad covering UV 0–1, facing +Z. Apply re-projects at document
    /// resolution, so a real mesh has to be present.
    fn quad_mesh() -> BakeMesh {
        BakeMesh {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            indices: vec![0, 1, 2, 0, 2, 3],
            ..Default::default()
        }
    }

    #[test]
    fn applying_projection_hides_the_source_and_inserts_a_layer() {
        let mut doc = DocumentRes::new(ns_core::Document::new(UVec2::splat(64)));
        let source = Layer::shape(
            "source",
            ns_core::ShapeSource::Primitive(ns_core::Primitive::Rect {
                center: Vec2::splat(32.0),
                size: Vec2::splat(20.0),
                corner_radius: 2.0,
            }),
        );
        let source_id = source.id;
        doc.0.layers.push(source);
        let mut state = EditorState {
            selected: Some(vec![0]),
            ..Default::default()
        };
        let mut runtime = ProjectionRuntime {
            enabled: true,
            source: Some(source_id),
            last_projected: Some(Arc::new(Grid::new(64, 64, Vec3::Z))),
            last_coverage: Some(Grid::new(64, 64, 0.5)),
            mesh: Some(Arc::new(quad_mesh())),
            mesh_ready: true,
            source_grid: Some(Arc::new(Grid::new(64, 64, Vec3::Z))),
            ..Default::default()
        };
        assert!(apply_projection_now(&mut state, &mut doc, &mut runtime));
        assert_eq!(doc.0.layers.len(), 2);
        assert!(
            !doc.0.layers[0].visible,
            "the source is hidden, not removed"
        );
        let LayerKind::ImportedNormal { normal } = &doc.0.layers[1].kind else {
            panic!("the projected layer must be an imported normal layer");
        };
        assert_eq!((normal.w, normal.h), (64, 64), "apply uses document size");
        assert_eq!(doc.0.layers[1].name, "source projected");
        assert_eq!(state.selected, Some(vec![1]));
        assert!(!runtime.enabled);
    }
}
