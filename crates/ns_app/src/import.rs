//! Preview-mesh import and UV layout overlays.
//!
//! glTF/GLB files load through Bevy's asset server and are staged into
//! `assets/imports/<name>/` so external buffers resolve. FBX, OBJ, COLLADA
//! and USD files are parsed by `mesh_io` and converted to Bevy meshes
//! directly; all backends then share tangent generation, UDIM analysis and
//! the UV-island overlay. The scene is spawned under an outer entity that can
//! be hidden and rotated with the built-in preview shapes.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::asset::{AssetId, LoadState};
use bevy::gltf::GltfAssetLabel;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::mesh::VertexAttributeValues;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::FileDragAndDrop;
use bevy::world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady};

use crate::app::{EditorState, PreviewHandles, PreviewShape};
use crate::mesh_io;
use crate::preview::RotatingPreview;

const UV_CANVAS_SIZE: usize = 1024;
const UV_CANVAS_MAX: usize = 2048;
const UV_CANVAS_CELL_MAX: usize = 1024;
const UV_INSET_SIZE: usize = 384;
const LOAD_TIMEOUT_FRAMES: u32 = 1200;

/// Detected UDIM layout for an imported mesh.
#[derive(Clone, Debug, Default)]
pub struct UdimInfo {
    pub enabled: bool,
    pub tiles: Vec<u32>,
    pub cols: u32,
    pub rows: u32,
    pub min_tile_u: i32,
    pub max_tile_v: i32,
}

impl UdimInfo {
    pub fn summary(&self) -> String {
        if !self.enabled {
            return "UV 0–1".into();
        }
        match (self.tiles.first(), self.tiles.last(), self.tiles.len()) {
            (Some(first), Some(_), 1) => format!("UDIM {first}"),
            (Some(first), Some(last), count) => format!("UDIM {first}–{last} · {count} tiles"),
            _ => "UDIM".into(),
        }
    }

    pub fn tile_number(&self, column: u32, row: u32) -> u32 {
        let tile_u = self.min_tile_u + column as i32;
        let tile_v = self.max_tile_v - row as i32;
        (1001 + tile_u + 10 * tile_v).max(1) as u32
    }

    /// Grid cell (column, row) occupied by a UDIM tile number in the layout
    /// texture. Row 0 is the top of the texture, matching `tile_number`.
    pub fn tile_cell(&self, tile: u32) -> Option<(u32, u32)> {
        if !self.enabled {
            return (tile == 1001).then_some((0, 0));
        }
        let index = tile.checked_sub(1001)?;
        let tile_u = (index % 10) as i32;
        let tile_v = (index / 10) as i32;
        let column = tile_u - self.min_tile_u;
        let row = self.max_tile_v - tile_v;
        if column < 0 || row < 0 || column >= self.cols as i32 || row >= self.rows as i32 {
            return None;
        }
        Some((column as u32, row as u32))
    }

    /// Lowest tile number present in the detected range.
    pub fn base_tile(&self) -> u32 {
        self.tiles.first().copied().unwrap_or(1001)
    }
}

/// UDIM tile numbers of a native mesh file, for headless exports.
///
/// Mirrors the triangle-centroid detection in `analyze_uvs` but works on raw
/// file-space vertices, so `--export-tiles all --mesh model.dae` can split an
/// export without spinning up Bevy's asset server.
pub fn udim_tiles_for_mesh(path: &Path) -> Vec<u32> {
    let Ok(meshes) = mesh_io::load_native(path) else {
        return Vec::new();
    };
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    let mut has_uvs = false;
    for mesh in &meshes {
        let Some(uvs) = &mesh.uvs else {
            continue;
        };
        has_uvs = true;
        for uv in uvs {
            if uv[0].is_finite() && uv[1].is_finite() {
                min = min.min(Vec2::from_array(*uv));
                max = max.max(Vec2::from_array(*uv));
            }
        }
    }
    if !has_uvs || !min.is_finite() {
        return Vec::new();
    }

    let offset = Vec2::new(
        if min.x >= 1000.0 { min.x.floor() } else { 0.0 },
        if min.y >= 1000.0 { min.y.floor() } else { 0.0 },
    );
    let mut cells = BTreeSet::<(i32, i32)>::new();
    for mesh in &meshes {
        let Some(uvs) = &mesh.uvs else {
            continue;
        };
        for triangle in mesh.indices.chunks_exact(3) {
            let Some(ua) = uvs.get(triangle[0] as usize) else {
                continue;
            };
            let Some(ub) = uvs.get(triangle[1] as usize) else {
                continue;
            };
            let Some(uc) = uvs.get(triangle[2] as usize) else {
                continue;
            };
            let u = ((ua[0] + ub[0] + uc[0]) / 3.0) - offset.x;
            let v = ((ua[1] + ub[1] + uc[1]) / 3.0) - offset.y;
            if u.is_finite() && v.is_finite() {
                cells.insert((tile_index(u), tile_index(v)));
            }
        }
    }

    let normalized_min = min - offset;
    let normalized_max = max - offset;
    let enabled = offset != Vec2::ZERO
        || cells.len() > 1
        || normalized_min.x < -1e-4
        || normalized_min.y < -1e-4
        || normalized_max.x > 1.0 + 1e-4
        || normalized_max.y > 1.0 + 1e-4;
    if !enabled {
        return Vec::new();
    }
    cells
        .iter()
        .map(|(u, v)| (1001 + u + 10 * v).max(1) as u32)
        .collect()
}

#[derive(Clone, Debug)]
struct UvAnalysis {
    udim: UdimInfo,
    offset: Vec2,
}

impl Default for UvAnalysis {
    fn default() -> Self {
        Self {
            udim: UdimInfo::default(),
            offset: Vec2::ZERO,
        }
    }
}

/// Asset root used by both Bevy and the import staging step.
pub fn assets_dir() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if cwd.join("Cargo.toml").is_file() {
        return cwd.join("assets");
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("assets")))
        .unwrap_or_else(|| cwd.join("assets"))
}

/// One preview-mesh triangle in normalized mesh space, with the UVs the
/// material samples. The 3D sweep-path editor raycasts these to snap a path
/// node to the actual surface instead of an arbitrary screen plane.
#[derive(Clone, Copy, Debug)]
pub struct SurfaceTriangle {
    pub positions: [[f32; 3]; 3],
    pub uvs: [[f32; 2]; 3],
}

/// Runtime state for the currently imported preview mesh.
#[derive(Resource, Default)]
pub struct MeshImport {
    pub assets_dir: PathBuf,
    /// Original file the current preview was imported from, used by baking to
    /// reload raw file-space geometry.
    pub source_path: Option<PathBuf>,
    pub root: Option<Entity>,
    pub scene: Option<Entity>,
    /// Scene handle while a glTF/GLB is loading, so a load failure can be
    /// reported instead of leaving the panel on "Loading..." forever.
    pending_scene: Option<Handle<WorldAsset>>,
    /// Full-resolution UV wireframe used by the 2D canvas overlay.
    pub uv_texture: Option<Handle<Image>>,
    /// Small borders-only UV layout used by the 3D viewport inset.
    pub uv_inset_texture: Option<Handle<Image>>,
    /// Normalized preview triangles used by the 3D sweep-path editor.
    pub surface: Arc<Vec<SurfaceTriangle>>,
    /// World rotation of the preview root, sampled every frame. Auto-rotate
    /// turns the imported mesh, so surface handles and raycasts must use the
    /// same angle the camera sees.
    pub surface_rotation: Quat,
    /// True when the imported format uses bottom-left UVs (OBJ/FBX/DAE/USD).
    pub uv_bottom_left: bool,
    pub udim: UdimInfo,
    pub triangles: usize,
    pub uv_triangles: usize,
    pub ready: bool,
    pub loading: bool,
    frames_waiting: u32,
}

/// Outer pivot entity for an imported mesh. Carries `PreviewShape::Imported`
/// and `RotatingPreview`, so the existing visibility/rotation systems apply.
#[derive(Component)]
pub struct ImportedMeshRoot;

/// Marks the inner `SceneRoot` entity whose transform is normalized on import.
#[derive(Component)]
pub struct ImportedSceneRoot;

pub struct MeshImportPlugin;

impl Plugin for MeshImportPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(MeshImport {
            assets_dir: assets_dir(),
            ..default()
        })
        .add_systems(
            Update,
            (
                handle_file_drops,
                handle_import_requests,
                fail_stalled_import,
                finalize_imported_mesh,
                sync_surface_rotation,
            )
                .chain(),
        )
        .add_observer(on_scene_ready);
    }
}

pub fn is_mesh_path(path: &Path) -> bool {
    mesh_io::is_mesh_path(path)
}

pub fn is_svg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
}

pub fn is_project_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("nsproj"))
}

pub fn is_photo_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "tga" | "exr" | "hdr" | "bmp" | "tif" | "tiff" | "webp"
            )
        })
}

fn handle_file_drops(
    mut events: MessageReader<FileDragAndDrop>,
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<EditorState>,
) {
    // Shift routes dropped images through the alpha importer and Ctrl through
    // the normal-map importer. The event carries no modifier state, so read the
    // keyboard at drop time.
    let alpha = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let normal = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    for event in events.read() {
        if let FileDragAndDrop::DroppedFile { path_buf, .. } = event {
            if is_project_path(path_buf) {
                state.project_open_request = Some(path_buf.clone());
                state.status = format!("Opening project {}...", display_name(path_buf));
            } else if is_mesh_path(path_buf) {
                state.import_status = format!("Queued {}", display_name(path_buf));
                state.import_request = Some(path_buf.clone());
            } else if is_svg_path(path_buf) {
                state.svg_import_request = Some(path_buf.clone());
                state.status = format!("Importing SVG {}...", display_name(path_buf));
            } else if is_photo_path(path_buf) {
                if normal {
                    state.normal_import_request = Some(path_buf.clone());
                    state.status = format!("Importing normal map {}...", display_name(path_buf));
                } else if alpha {
                    state.alpha_import_request = Some(path_buf.clone());
                    state.status = format!("Importing alpha {}...", display_name(path_buf));
                } else {
                    state.photo_import_request = Some(path_buf.clone());
                    state.status = format!("Importing photo {}...", display_name(path_buf));
                }
            } else {
                state.import_status = "Drop a .glb, .gltf, .fbx, .obj, .dae, .usd, .svg, .nsproj \
                    or image file. Hold Shift while dropping an image to import it as an alpha mask, \
                    or Ctrl to import it as a normal map."
                    .into();
            }
        }
    }
}

fn handle_import_requests(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    handles: Res<PreviewHandles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut import: ResMut<MeshImport>,
    mut state: ResMut<EditorState>,
) {
    let Some(path) = state.import_request.take() else {
        return;
    };
    import.source_path = Some(path.clone());

    // Drop the previous instance before staging the new one.
    if let Some(root) = import.root.take() {
        commands.entity(root).despawn();
    }
    import.scene = None;
    import.pending_scene = None;
    import.uv_texture = None;
    import.uv_inset_texture = None;
    import.surface = Arc::new(Vec::new());
    import.surface_rotation = Quat::IDENTITY;
    import.udim = UdimInfo::default();
    import.triangles = 0;
    import.uv_triangles = 0;
    import.ready = false;
    import.loading = true;
    import.frames_waiting = 0;
    state.uv_overlay_triangles = 0;
    state.uv_bottom_left = false;
    import.uv_bottom_left = false;

    let mesh_name = display_name(&path);

    if mesh_io::is_gltf_path(&path) {
        if !mesh_io::gltf_has_scene(&path) {
            // Mesh-only GLBs have no `Scene0` asset for Bevy's loader to
            // return. Convert their geometry through the native mesh path so
            // they import exactly like OBJ/FBX instead of failing outright.
            state.uv_bottom_left = false;
            import.uv_bottom_left = false;
            match mesh_io::load_any(&path) {
                Ok(raw_meshes) => {
                    let triangle_total: usize =
                        raw_meshes.iter().map(|mesh| mesh.triangle_count()).sum();
                    let mesh_count = raw_meshes.len();
                    let (root, scene) = spawn_raw_scene(
                        &mut commands,
                        &mut meshes,
                        handles.material.clone(),
                        raw_meshes,
                    );
                    import.root = Some(root);
                    import.scene = Some(scene);
                    state.imported_mesh = Some(mesh_name.clone());
                    state.preview_shape = PreviewShape::Imported;
                    state.import_status = format!("Preparing {mesh_name}...");
                    info!(
                        "imported mesh-only glTF {mesh_name}: {mesh_count} submeshes, \
                         {triangle_total} triangles"
                    );
                }
                Err(error) => {
                    import.loading = false;
                    state.import_status = format!("Import failed: {error}");
                    state.imported_mesh = None;
                    state.preview_shape = PreviewShape::Cube;
                    warn!("mesh import failed: {error}");
                }
            }
            return;
        }
        match stage_gltf(&path, &import.assets_dir) {
            Ok(asset_path) => {
                let scene: Handle<WorldAsset> =
                    asset_server.load(GltfAssetLabel::Scene(0).from_asset(asset_path));
                import.pending_scene = Some(scene.clone());
                let root = commands
                    .spawn((
                        ImportedMeshRoot,
                        PreviewShape::Imported,
                        Visibility::Hidden,
                        InheritedVisibility::default(),
                        ViewVisibility::default(),
                        Transform::default(),
                        RotatingPreview,
                        Name::new("Imported preview mesh"),
                    ))
                    .with_children(|parent| {
                        parent.spawn((
                            WorldAssetRoot(scene),
                            ImportedSceneRoot,
                            Visibility::Inherited,
                            InheritedVisibility::default(),
                            ViewVisibility::default(),
                            Transform::default(),
                        ));
                    })
                    .id();
                import.root = Some(root);
                state.imported_mesh = Some(mesh_name.clone());
                state.preview_shape = PreviewShape::Imported;
                state.import_status = format!("Loading {mesh_name}...");
                info!("importing glTF mesh {mesh_name} from staged asset");
            }
            Err(error) => {
                import.loading = false;
                state.import_status = format!("Import failed: {error}");
                state.imported_mesh = None;
                state.preview_shape = PreviewShape::Cube;
                warn!("mesh import failed: {error}");
            }
        }
        return;
    }

    let Some(format) = mesh_io::mesh_format(&path) else {
        import.loading = false;
        state.import_status = format!("Unsupported mesh format: {mesh_name}");
        return;
    };
    let bottom_left = if format == mesh_io::MeshFormat::NsMesh {
        mesh_io::nsmesh_uv_bottom_left(&path)
    } else {
        true
    };
    state.uv_bottom_left = bottom_left;
    import.uv_bottom_left = bottom_left;

    match mesh_io::load_native(&path) {
        Ok(raw_meshes) => {
            let triangle_total: usize = raw_meshes.iter().map(|mesh| mesh.triangle_count()).sum();
            let mesh_count = raw_meshes.len();
            let (root, scene) = spawn_raw_scene(
                &mut commands,
                &mut meshes,
                handles.material.clone(),
                raw_meshes,
            );
            import.root = Some(root);
            import.scene = Some(scene);
            state.imported_mesh = Some(mesh_name.clone());
            state.preview_shape = PreviewShape::Imported;
            state.import_status = format!("Preparing {mesh_name}...");
            info!(
                "imported {format:?} mesh {mesh_name}: {mesh_count} submeshes, {triangle_total} triangles"
            );
        }
        Err(error) => {
            import.loading = false;
            state.import_status = format!("Import failed: {error}");
            state.imported_mesh = None;
            state.preview_shape = PreviewShape::Cube;
            warn!("mesh import failed: {error}");
        }
    }
}

/// Spawn the scene shape shared by native meshes and mesh-only glTF files.
///
/// The outer entity carries the preview flags every importer shares; the inner
/// root is the one `finalize_imported_mesh` normalizes to the unit cube.
fn spawn_raw_scene(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    raw_meshes: Vec<mesh_io::RawMesh>,
) -> (Entity, Entity) {
    let mut scene = Entity::PLACEHOLDER;
    let root = commands
        .spawn((
            ImportedMeshRoot,
            PreviewShape::Imported,
            Visibility::Hidden,
            InheritedVisibility::default(),
            ViewVisibility::default(),
            Transform::default(),
            RotatingPreview,
            Name::new("Imported preview mesh"),
        ))
        .with_children(|parent| {
            scene = parent
                .spawn((
                    ImportedSceneRoot,
                    Visibility::Inherited,
                    InheritedVisibility::default(),
                    ViewVisibility::default(),
                    Transform::default(),
                    Name::new("Imported mesh root"),
                ))
                .with_children(|parent| {
                    for raw in raw_meshes {
                        let name = raw.name.clone();
                        let handle = meshes.add(raw_to_bevy_mesh(raw));
                        parent.spawn((
                            Mesh3d(handle),
                            MeshMaterial3d(material.clone()),
                            Transform::default(),
                            Name::new(name),
                        ));
                    }
                })
                .id();
        })
        .id();
    (root, scene)
}

/// Convert an engine-agnostic mesh into the Bevy mesh expected by the preview.
fn raw_to_bevy_mesh(raw: mesh_io::RawMesh) -> Mesh {
    use bevy::render::mesh::PrimitiveTopology;

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, raw.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, raw.normals);
    if let Some(uvs) = raw.uvs {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    }
    mesh.insert_indices(bevy::render::mesh::Indices::U32(raw.indices));
    mesh
}

/// Report a glTF/GLB load failure instead of waiting forever for a scene
/// instance that will never spawn. Native formats parse synchronously and never
/// set `pending_scene`.
fn fail_stalled_import(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut import: ResMut<MeshImport>,
    mut state: ResMut<EditorState>,
) {
    if !import.loading || import.scene.is_some() {
        return;
    }
    let Some(handle) = import.pending_scene.clone() else {
        return;
    };
    let Some(LoadState::Failed(error)) = asset_server.get_load_state(handle.id()) else {
        return;
    };

    import.loading = false;
    import.ready = true;
    import.pending_scene = None;
    if let Some(root) = import.root.take() {
        commands.entity(root).despawn();
    }
    state.imported_mesh = None;
    state.preview_shape = PreviewShape::Cube;
    state.import_status = format!("Import failed: {error}");
    warn!("glTF import failed: {error}");
}

#[allow(clippy::too_many_arguments)]
fn on_scene_ready(
    trigger: On<WorldInstanceReady>,
    mut commands: Commands,
    handles: Res<PreviewHandles>,
    children: Query<&Children>,
    mesh_entities: Query<&Mesh3d>,
    scene_roots: Query<(), With<ImportedSceneRoot>>,
    mut import: ResMut<MeshImport>,
    mut state: ResMut<EditorState>,
) {
    let root = trigger.event().entity;
    if scene_roots.get(root).is_err() {
        return;
    }

    import.scene = Some(root);
    import.pending_scene = None;
    import.ready = false;
    import.frames_waiting = 0;
    state.import_status = "Preparing imported mesh...".into();

    for descendant in children.iter_descendants(root) {
        if mesh_entities.get(descendant).is_ok() {
            commands
                .entity(descendant)
                .insert(MeshMaterial3d(handles.material.clone()));
        }
    }
    info!("imported scene ready; applying normal-map material");
}

#[allow(clippy::too_many_arguments)]
fn finalize_imported_mesh(
    mut commands: Commands,
    mut import: ResMut<MeshImport>,
    mut state: ResMut<EditorState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    children: Query<&Children>,
    mesh_query: Query<(&Mesh3d, &GlobalTransform)>,
) {
    if import.ready {
        return;
    }
    let Some(scene) = import.scene else {
        return;
    };
    import.frames_waiting += 1;

    let entries: Vec<(Entity, Handle<Mesh>, GlobalTransform)> = children
        .iter_descendants(scene)
        .filter_map(|entity| {
            mesh_query
                .get(entity)
                .ok()
                .map(|(mesh, transform)| (entity, mesh.0.clone(), *transform))
        })
        .collect();

    if entries.is_empty() {
        if import.frames_waiting > LOAD_TIMEOUT_FRAMES {
            import.ready = true;
            import.loading = false;
            state.import_status = "Import failed: the file contains no meshes.".into();
            state.imported_mesh = None;
            state.preview_shape = PreviewShape::Cube;
        }
        return;
    }

    // The scene and its meshes are loaded asynchronously; wait until every
    // mesh asset is available before generating tangents and UV data.
    let all_loaded = entries
        .iter()
        .all(|(_, handle, _)| meshes.get(handle).is_some());
    if !all_loaded {
        if import.frames_waiting > LOAD_TIMEOUT_FRAMES {
            import.ready = true;
            import.loading = false;
            state.import_status = "Import timed out while loading mesh data.".into();
            state.imported_mesh = None;
            state.preview_shape = PreviewShape::Cube;
        }
        return;
    }

    // Detect UDIMs before generating tangents. Offset-style UDIM UVs (for
    // example 1001–1002 in U) are shifted back to 0–1 so the repeat sampler
    // and the UV overlay operate on local tile coordinates.
    let analysis = analyze_uvs(&entries, &meshes);
    if analysis.offset != Vec2::ZERO {
        normalize_offset_uvs(&entries, &mut meshes, analysis.offset);
    }
    import.udim = analysis.udim.clone();

    let mut unique_meshes = HashSet::new();
    let mut triangles = 0usize;
    for (_, handle, _) in &entries {
        if !unique_meshes.insert(handle.id()) {
            continue;
        }
        let Some(mut mesh) = meshes.get_mut(handle) else {
            continue;
        };
        triangles += triangle_count(&mesh);
        if mesh.attribute(Mesh::ATTRIBUTE_TANGENT).is_none()
            && mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_some()
        {
            // glTF tangents are optional; normal mapping requires them.
            let _ = mesh.generate_tangents();
        }
    }

    let bounds = combined_bounds(&entries, &meshes);
    let mut normalization = (Vec3::ZERO, 1.0);
    if let Some((min, max)) = bounds {
        let center = (min + max) * 0.5;
        let extent = (max - min).max_element().max(1e-5);
        // Keep a margin in the tall, narrow 3D viewport.
        let scale = 1.0 / extent;
        normalization = (center, scale);
        commands
            .entity(scene)
            .insert(Transform::from_scale(Vec3::splat(scale)).with_translation(-center * scale));
    }

    // Keep the normalized preview surface for the 3D sweep-path editor. The
    // positions use the same centre/scale as the scene transform above, so a
    // raycast hit maps back to exactly the point the artist sees.
    let (center, scale) = normalization;
    let mut surface = Vec::new();
    for (_, handle, transform) in &entries {
        let Some(mesh) = meshes.get(handle) else {
            continue;
        };
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            continue;
        };
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            continue;
        };
        let indices = mesh_indices(mesh);
        for triangle in indices.chunks_exact(3) {
            let (a, b, c) = (
                triangle[0] as usize,
                triangle[1] as usize,
                triangle[2] as usize,
            );
            let (Some(pa), Some(pb), Some(pc)) =
                (positions.get(a), positions.get(b), positions.get(c))
            else {
                continue;
            };
            let (Some(ua), Some(ub), Some(uc)) = (uvs.get(a), uvs.get(b), uvs.get(c)) else {
                continue;
            };
            let normalize = |p: &[f32; 3]| {
                let world = transform.transform_point(Vec3::from_array(*p));
                ((world - center) * scale).to_array()
            };
            surface.push(SurfaceTriangle {
                positions: [normalize(pa), normalize(pb), normalize(pc)],
                uvs: [*ua, *ub, *uc],
            });
        }
    }
    import.surface = Arc::new(surface);

    let layout = build_uv_layout(&entries, &meshes, &import.udim, import.uv_bottom_left);
    let udim_suffix = if import.udim.enabled {
        format!(" · {}", import.udim.summary())
    } else {
        String::new()
    };
    if let Some(layout) = layout {
        let uv_triangles = layout.triangles;
        import.uv_texture = Some(images.add(layout.canvas));
        import.uv_inset_texture = Some(images.add(layout.inset));
        import.uv_triangles = uv_triangles;
        state.uv_overlay_triangles = uv_triangles;
        state.import_status = format!(
            "Imported {}: {} triangles, UV overlay ready{udim_suffix}",
            state.imported_mesh.as_deref().unwrap_or("mesh"),
            triangles
        );
    } else {
        import.uv_texture = None;
        import.uv_inset_texture = None;
        import.uv_triangles = 0;
        state.uv_overlay_triangles = 0;
        state.import_status = format!(
            "Imported {}: {} triangles (no UV coordinates)",
            state.imported_mesh.as_deref().unwrap_or("mesh"),
            triangles
        );
    }

    import.triangles = triangles;
    import.loading = false;
    import.ready = true;
    info!(
        "imported mesh ready: {} triangles, {} UV triangles ({})",
        triangles,
        import.uv_triangles,
        import.udim.summary()
    );
}

/// Keep the sweep-path editor in the same orientation the camera sees.
/// Auto-rotate turns the imported preview root every frame; handles and rays
/// have to follow it or a drag lands somewhere else on the model.
fn sync_surface_rotation(
    roots: Query<&Transform, With<ImportedMeshRoot>>,
    mut import: ResMut<MeshImport>,
) {
    let Ok(transform) = roots.single() else {
        return;
    };
    if import.surface_rotation != transform.rotation {
        import.surface_rotation = transform.rotation;
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mesh")
        .to_string()
}

/// Stage a glTF/GLB into Bevy's asset root under a content-addressed name.
///
/// The staged path is lowercased for loader matching, changes whenever the
/// source bytes change (so re-importing an edited file does not return the
/// cached previous asset), and keeps two differently located meshes that share
/// a name from overwriting each other.
fn stage_gltf(source: &Path, assets_dir: &Path) -> Result<String, String> {
    if !source.is_file() {
        return Err(format!("{} is not a file", source.display()));
    }
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension != "glb" && extension != "gltf" {
        return Err("glTF staging only accepts .glb and .gltf files".into());
    }

    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("mesh");
    let folder = sanitize_name(stem);
    let target_dir = assets_dir.join("imports").join(&folder);
    std::fs::create_dir_all(&target_dir)
        .map_err(|error| format!("could not create {}: {error}", target_dir.display()))?;

    // Bevy matches loader extensions case-sensitively (`gltf`/`glb`), so a
    // `Model.GLB` must be staged as `.glb` or the asset server finds no loader.
    // The content hash also makes a re-import of an edited file a new asset
    // path instead of returning the cached previous load, and keeps two meshes
    // that share a name from overwriting each other.
    let fingerprint = file_fingerprint(source)?;
    let target_name = format!("{folder}-{fingerprint:016x}.{extension}");
    let target_file = target_dir.join(&target_name);
    std::fs::copy(source, &target_file).map_err(|error| {
        format!(
            "could not copy {} to {}: {error}",
            source.display(),
            target_file.display()
        )
    })?;

    if extension == "gltf" {
        copy_gltf_resources(source, &target_dir)?;
    }

    Ok(format!("imports/{folder}/{target_name}"))
}

/// Stable content hash for staging. Streaming keeps multi-hundred-megabyte
/// GLBs out of memory and still distinguishes an edited file from the previous
/// import.
fn file_fingerprint(path: &Path) -> Result<u64, String> {
    use std::hash::Hasher;
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.write(&buffer[..read]);
    }
    Ok(hasher.finish())
}

fn copy_gltf_resources(source: &Path, target_dir: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(source)
        .map_err(|error| format!("could not read {}: {error}", source.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("invalid glTF JSON: {error}"))?;
    let source_dir = source.parent().unwrap_or_else(|| Path::new("."));

    for key in ["buffers", "images"] {
        let Some(items) = json.get(key).and_then(|value| value.as_array()) else {
            continue;
        };
        for item in items {
            let Some(uri) = item.get("uri").and_then(|value| value.as_str()) else {
                continue;
            };
            if uri.starts_with("data:") || uri.starts_with("http://") || uri.starts_with("https://")
            {
                continue;
            }
            let relative = uri.split(['?', '#']).next().unwrap_or(uri);
            let relative = relative.replace("%20", " ");
            let relative = Path::new(&relative);
            if relative.is_absolute()
                || relative.components().any(|component| {
                    matches!(component, Component::ParentDir | Component::Prefix(_))
                })
            {
                continue;
            }
            let from = source_dir.join(relative);
            if !from.is_file() {
                continue;
            }
            let to = target_dir.join(relative);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
            }
            std::fs::copy(&from, &to)
                .map_err(|error| format!("could not copy resource {}: {error}", from.display()))?;
        }
    }
    Ok(())
}

fn sanitize_name(name: &str) -> String {
    let mut cleaned: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|character| character == '.') {
        cleaned = "mesh".into();
    }
    cleaned
}

fn triangle_count(mesh: &Mesh) -> usize {
    mesh.indices()
        .map(|indices| indices.len() / 3)
        .unwrap_or_else(|| mesh.count_vertices() / 3)
}

fn mesh_indices(mesh: &Mesh) -> Vec<u32> {
    match mesh.indices() {
        Some(bevy::render::mesh::Indices::U32(indices)) => indices.clone(),
        Some(bevy::render::mesh::Indices::U16(indices)) => {
            indices.iter().map(|index| *index as u32).collect()
        }
        None => (0..mesh.count_vertices() as u32).collect(),
    }
}

/// Exact integer UV boundaries belong to the lower tile (UDIM tiles commonly
/// run 0..1, 1..2; vertex values of exactly 1.0 are shared visual seams).
fn tile_index(value: f32) -> i32 {
    if value > 0.0 {
        (value - 1e-5).floor() as i32
    } else {
        value.floor() as i32
    }
}

fn combined_bounds(
    entries: &[(Entity, Handle<Mesh>, GlobalTransform)],
    meshes: &Assets<Mesh>,
) -> Option<(Vec3, Vec3)> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for (_, handle, transform) in entries {
        let Some(mesh) = meshes.get(handle) else {
            continue;
        };
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            continue;
        };
        for position in positions {
            let world = transform.transform_point(Vec3::from_array(*position));
            min = min.min(world);
            max = max.max(world);
        }
    }
    (min.is_finite() && max.is_finite() && min.x <= max.x).then_some((min, max))
}

/// Inspect UVs across the imported meshes and classify the layout.
///
/// Both common UDIM encodings are handled: UVs laid out in 0..N tiles and
/// offset UVs (for example U 1001..1002). Offset UVs are later normalized so
/// the repeat sampler can use the same local 0–1 coordinates per tile.
fn analyze_uvs(
    entries: &[(Entity, Handle<Mesh>, GlobalTransform)],
    meshes: &Assets<Mesh>,
) -> UvAnalysis {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    let mut has_uvs = false;
    let mut seen = HashSet::<AssetId<Mesh>>::new();

    for (_, handle, _) in entries {
        if !seen.insert(handle.id()) {
            continue;
        }
        let Some(mesh) = meshes.get(handle) else {
            continue;
        };
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            continue;
        };
        has_uvs = true;
        for uv in uvs {
            if uv[0].is_finite() && uv[1].is_finite() {
                min = min.min(Vec2::from_array(*uv));
                max = max.max(Vec2::from_array(*uv));
            }
        }
    }
    if !has_uvs || !min.is_finite() {
        return UvAnalysis::default();
    }

    // Some DCCs encode the UDIM number directly in U (and occasionally V).
    // Shifting by the integer base keeps float precision high and lets the
    // Repeat sampler operate on local tile coordinates.
    let offset = Vec2::new(
        if min.x >= 1000.0 { min.x.floor() } else { 0.0 },
        if min.y >= 1000.0 { min.y.floor() } else { 0.0 },
    );

    let mut tiles = BTreeSet::<(i32, i32)>::new();
    seen.clear();
    for (_, handle, _) in entries {
        if !seen.insert(handle.id()) {
            continue;
        }
        let Some(mesh) = meshes.get(handle) else {
            continue;
        };
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            continue;
        };
        let indices = mesh_indices(mesh);
        for triangle in indices.chunks_exact(3) {
            let Some((ua, ub, uc)) = uv_indices(uvs, [triangle[0], triangle[1], triangle[2]])
            else {
                continue;
            };
            // Use the triangle centroid rather than vertices: vertices on an
            // exact 1.0 boundary are ambiguous and can otherwise invent empty
            // neighbouring tiles.
            let u = ((ua[0] + ub[0] + uc[0]) / 3.0) - offset.x;
            let v = ((ua[1] + ub[1] + uc[1]) / 3.0) - offset.y;
            if u.is_finite() && v.is_finite() {
                tiles.insert((tile_index(u), tile_index(v)));
            }
        }
    }

    let Some(&(min_tile_u, min_tile_v)) = tiles.iter().next() else {
        return UvAnalysis::default();
    };
    let Some(&(max_tile_u, max_tile_v)) = tiles.iter().next_back() else {
        return UvAnalysis::default();
    };
    let cols = (max_tile_u - min_tile_u + 1).max(0) as u32;
    let rows = (max_tile_v - min_tile_v + 1).max(0) as u32;
    let normalized_min = min - offset;
    let normalized_max = max - offset;
    let enabled = offset != Vec2::ZERO
        || tiles.len() > 1
        || normalized_min.x < -1e-4
        || normalized_min.y < -1e-4
        || normalized_max.x > 1.0 + 1e-4
        || normalized_max.y > 1.0 + 1e-4;

    let tile_numbers = if enabled {
        tiles
            .iter()
            .map(|(u, v)| (1001 + u + 10 * v).max(1) as u32)
            .collect()
    } else {
        Vec::new()
    };

    UvAnalysis {
        udim: UdimInfo {
            enabled,
            tiles: tile_numbers,
            cols: if enabled { cols } else { 1 },
            rows: if enabled { rows } else { 1 },
            min_tile_u,
            max_tile_v,
        },
        offset,
    }
}

fn normalize_offset_uvs(
    entries: &[(Entity, Handle<Mesh>, GlobalTransform)],
    meshes: &mut Assets<Mesh>,
    offset: Vec2,
) {
    let mut seen = HashSet::<AssetId<Mesh>>::new();
    for (_, handle, _) in entries {
        if !seen.insert(handle.id()) {
            continue;
        }
        let Some(mut mesh) = meshes.get_mut(handle) else {
            continue;
        };
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute_mut(Mesh::ATTRIBUTE_UV_0)
        else {
            continue;
        };
        for uv in uvs {
            uv[0] -= offset.x;
            uv[1] -= offset.y;
        }
    }
}

/// Textures for the UV overlay.
///
/// The 2D canvas gets the full triangle wireframe (like a DCC UV editor), while
/// the 3D viewport inset gets a small, borders-only layout so island outlines
/// stay readable at thumbnail size.
pub struct UvLayoutTextures {
    pub canvas: Image,
    pub inset: Image,
    pub triangles: usize,
}

/// Wireframe styling. A dark halo keeps lines readable over bright normal maps
/// and the light core keeps them readable over dark areas. Island borders are
/// wider and use the accent colour so shells stand out from the interior
/// triangulation.
const UV_HALO: [u8; 4] = [10, 12, 16, 220];
const UV_INTERIOR: [u8; 4] = [235, 243, 255, 205];
const UV_BORDER: [u8; 4] = [255, 188, 108, 255];
const UV_INSET_BG_A: [u8; 4] = [27, 29, 34, 255];
const UV_INSET_BG_B: [u8; 4] = [22, 24, 28, 255];

#[derive(Clone, Copy)]
struct UvEdge {
    a: [f32; 2],
    b: [f32; 2],
    border: bool,
}

type UvEdgeKey = ((i64, i64), (i64, i64));

struct UvLayoutMap {
    width: usize,
    height: usize,
    cell: f32,
    enabled: bool,
    bottom_left: bool,
    cols: u32,
    rows: u32,
    min_tile_u: i32,
    max_tile_v: i32,
}

impl UvLayoutMap {
    fn new(udim: &UdimInfo, bottom_left: bool, size: usize, max_cell: usize) -> Self {
        if udim.enabled {
            let cols = udim.cols.max(1);
            let rows = udim.rows.max(1);
            let cell = (size / cols.max(rows) as usize).clamp(32, max_cell) as f32;
            Self {
                width: (cols as f32 * cell).round() as usize,
                height: (rows as f32 * cell).round() as usize,
                cell,
                enabled: true,
                bottom_left,
                cols,
                rows,
                min_tile_u: udim.min_tile_u,
                max_tile_v: udim.max_tile_v,
            }
        } else {
            Self {
                width: size,
                height: size,
                cell: 0.0,
                enabled: false,
                bottom_left,
                cols: 1,
                rows: 1,
                min_tile_u: 0,
                max_tile_v: 0,
            }
        }
    }

    /// Return the tile key and pixel position for a UV. Returns `None` for UVs
    /// outside the occupied UDIM range.
    fn to_pixel(&self, uv: [f32; 2]) -> Option<((i32, i32), (i32, i32))> {
        let v = if self.bottom_left {
            1.0 - uv_fraction(uv[1])
        } else {
            uv_fraction(uv[1])
        };
        if !self.enabled {
            let x = (uv_fraction(uv[0]) * (self.width - 1) as f32).round() as i32;
            let y = (v * (self.height - 1) as f32).round() as i32;
            return Some(((0, 0), (x, y)));
        }

        if !uv[0].is_finite() || !uv[1].is_finite() {
            return None;
        }
        let tile_u = tile_index(uv[0]);
        let tile_v = tile_index(uv[1]);
        let column = tile_u - self.min_tile_u;
        let row = self.max_tile_v - tile_v;
        if column < 0 || row < 0 || column >= self.cols as i32 || row >= self.rows as i32 {
            return None;
        }
        let local_u = uv[0] - tile_u as f32;
        let local_v = uv[1] - tile_v as f32;
        let local_v = if self.bottom_left {
            1.0 - local_v
        } else {
            local_v
        };
        let x = (column as f32 * self.cell + local_u * self.cell).round() as i32;
        let y = (row as f32 * self.cell + local_v * self.cell).round() as i32;
        Some(((tile_u, tile_v), (x, y)))
    }
}

fn build_uv_layout(
    entries: &[(Entity, Handle<Mesh>, GlobalTransform)],
    meshes: &Assets<Mesh>,
    udim: &UdimInfo,
    bottom_left: bool,
) -> Option<UvLayoutTextures> {
    let (edges, triangles) = collect_uv_edges(entries, meshes);
    if triangles == 0 || edges.is_empty() {
        return None;
    }

    let canvas_size = if udim.enabled {
        UV_CANVAS_MAX
    } else {
        UV_CANVAS_SIZE
    };
    let canvas_map = UvLayoutMap::new(udim, bottom_left, canvas_size, UV_CANVAS_CELL_MAX);
    let mut canvas = vec![0u8; canvas_map.width * canvas_map.height * 4];
    // Halo first so every core line sits on top of it. Borders get a wider
    // halo and are painted last so they win over interior edges.
    for edge in &edges {
        let width = if edge.border { 7 } else { 4 };
        draw_uv_line(&mut canvas, &canvas_map, edge.a, edge.b, width, UV_HALO);
    }
    for edge in edges.iter().filter(|edge| !edge.border) {
        draw_uv_line(&mut canvas, &canvas_map, edge.a, edge.b, 2, UV_INTERIOR);
    }
    for edge in edges.iter().filter(|edge| edge.border) {
        draw_uv_line(&mut canvas, &canvas_map, edge.a, edge.b, 4, UV_BORDER);
    }

    let inset_map = UvLayoutMap::new(udim, bottom_left, UV_INSET_SIZE, UV_INSET_SIZE);
    let mut inset = uv_checkerboard(inset_map.width, inset_map.height);
    for edge in edges.iter().filter(|edge| edge.border) {
        draw_uv_line(&mut inset, &inset_map, edge.a, edge.b, 6, UV_HALO);
    }
    for edge in edges.iter().filter(|edge| edge.border) {
        draw_uv_line(&mut inset, &inset_map, edge.a, edge.b, 3, UV_BORDER);
    }

    Some(UvLayoutTextures {
        canvas: uv_image(canvas_map.width, canvas_map.height, canvas),
        inset: uv_image(inset_map.width, inset_map.height, inset),
        triangles,
    })
}

/// Collect the unique UV-space triangle edges of every imported mesh. Edges
/// shared by two triangles are interior; edges seen once are island borders.
/// Keying by quantized UV endpoints (rather than vertex indices) keeps seams
/// correct when an importer welds vertices but splits their UVs.
fn collect_uv_edges(
    entries: &[(Entity, Handle<Mesh>, GlobalTransform)],
    meshes: &Assets<Mesh>,
) -> (Vec<UvEdge>, usize) {
    let mut edges: HashMap<UvEdgeKey, (UvEdge, u16)> = HashMap::new();
    let mut seen = HashSet::<AssetId<Mesh>>::new();
    let mut triangles = 0usize;

    for (_, handle, _) in entries {
        if !seen.insert(handle.id()) {
            continue;
        }
        let Some(mesh) = meshes.get(handle) else {
            continue;
        };
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            continue;
        };
        for triangle in mesh_indices(mesh).chunks_exact(3) {
            let Some((ua, ub, uc)) = uv_indices(uvs, [triangle[0], triangle[1], triangle[2]])
            else {
                continue;
            };
            triangles += 1;
            for (a, b) in [(ua, ub), (ub, uc), (uc, ua)] {
                let key_a = uv_key(a);
                let key_b = uv_key(b);
                let key = if key_a <= key_b {
                    (key_a, key_b)
                } else {
                    (key_b, key_a)
                };
                edges
                    .entry(key)
                    .and_modify(|(_, count)| *count = count.saturating_add(1))
                    .or_insert((
                        UvEdge {
                            a,
                            b,
                            border: false,
                        },
                        1,
                    ));
            }
        }
    }

    let edges = edges
        .into_values()
        .map(|(mut edge, count)| {
            edge.border = count == 1;
            edge
        })
        .collect();
    (edges, triangles)
}

/// Quantize UV endpoints to roughly 1/65536 of a tile so shared edges compare
/// equal across triangles and meshes.
fn uv_key(uv: [f32; 2]) -> (i64, i64) {
    let quantize = |value: f32| {
        if value.is_finite() {
            (value as f64 * 65_536.0).round() as i64
        } else {
            0
        }
    };
    (quantize(uv[0]), quantize(uv[1]))
}

/// Keep 0 and 1 as opposite edges of the layout; wrap only values that are
/// actually outside the unit interval (rem_euclid(1.0) would collapse 1 to 0).
fn uv_fraction(value: f32) -> f32 {
    if !value.is_finite() {
        0.0
    } else if (0.0..=1.0).contains(&value) {
        value
    } else {
        value.rem_euclid(1.0)
    }
}

fn uv_indices(uvs: &[[f32; 2]], indices: [u32; 3]) -> Option<([f32; 2], [f32; 2], [f32; 2])> {
    let a = *uvs.get(indices[0] as usize)?;
    let b = *uvs.get(indices[1] as usize)?;
    let c = *uvs.get(indices[2] as usize)?;
    Some((a, b, c))
}

fn draw_uv_line(
    data: &mut [u8],
    mapper: &UvLayoutMap,
    a: [f32; 2],
    b: [f32; 2],
    width: usize,
    color: [u8; 4],
) -> bool {
    let (Some((tile_a, (mut x0, mut y0))), Some((tile_b, (x1, y1)))) =
        (mapper.to_pixel(a), mapper.to_pixel(b))
    else {
        return false;
    };
    if tile_a != tile_b {
        return false;
    }
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;
    let half = (width.saturating_sub(1) / 2) as i32;

    loop {
        for offset_y in -half..=half {
            for offset_x in -half..=half {
                let pixel_x = x0 + offset_x;
                let pixel_y = y0 + offset_y;
                if pixel_x >= 0
                    && pixel_y >= 0
                    && (pixel_x as usize) < mapper.width
                    && (pixel_y as usize) < mapper.height
                {
                    let index = (pixel_y as usize * mapper.width + pixel_x as usize) * 4;
                    blend_pixel(&mut data[index..index + 4], color);
                }
            }
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let doubled = 2 * error;
        if doubled >= dy {
            error += dy;
            x0 += sx;
        }
        if doubled <= dx {
            error += dx;
            y0 += sy;
        }
    }
    true
}

/// Composite `color` over the existing RGBA pixel. Direct writes would let the
/// bright core erase the dark halo beneath it instead of stacking on top.
fn blend_pixel(dst: &mut [u8], color: [u8; 4]) {
    let src_a = color[3] as f32 / 255.0;
    if src_a <= 0.0 {
        return;
    }
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return;
    }
    for (channel, value) in color.iter().take(3).enumerate() {
        let under = dst[channel] as f32;
        dst[channel] = ((*value as f32 * src_a + under * dst_a * (1.0 - src_a)) / out_a)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Opaque checkerboard used behind the 3D inset, so the UV layout reads as its
/// own canvas instead of floating over the viewport.
fn uv_checkerboard(width: usize, height: usize) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 4];
    let cell = (width / 24).max(8);
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) * 4;
            let color = if ((x / cell) + (y / cell)) % 2 == 0 {
                UV_INSET_BG_A
            } else {
                UV_INSET_BG_B
            };
            data[index..index + 4].copy_from_slice(&color);
        }
    }
    data
}

fn uv_image(width: usize, height: usize, data: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        mag_filter: bevy::image::ImageFilterMode::Linear,
        min_filter: bevy::image::ImageFilterMode::Linear,
        ..default()
    });
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::mesh::Indices;
    use bevy::render::render_resource::PrimitiveTopology;

    fn uv_quad() -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0.0f32, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        );
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
        mesh
    }

    fn udim_pair() -> Mesh {
        let mut mesh = uv_quad();
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0.0f32, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [2.0, 0.0, 0.0],
                [3.0, 0.0, 0.0],
                [3.0, 1.0, 0.0],
                [2.0, 1.0, 0.0],
            ],
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![
                [0.0f32, 0.0],
                [0.999, 0.0],
                [0.999, 1.0],
                [0.0, 1.0],
                [1.0, 0.0],
                [1.999, 0.0],
                [1.999, 1.0],
                [1.0, 1.0],
            ],
        );
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]));
        mesh
    }

    #[test]
    fn uv_layout_contains_edges() {
        let mesh = uv_quad();
        let mut meshes = Assets::<Mesh>::default();
        let handle = meshes.add(mesh);
        let entries = vec![(Entity::PLACEHOLDER, handle, GlobalTransform::default())];
        let layout =
            build_uv_layout(&entries, &meshes, &UdimInfo::default(), false).expect("quad has UVs");
        assert_eq!(layout.triangles, 2);
        assert_eq!(layout.canvas.width(), UV_CANVAS_SIZE as u32);
        assert_eq!(layout.inset.width(), UV_INSET_SIZE as u32);
        let pixels = layout.canvas.data.as_ref().unwrap();
        // The shared diagonal is interior; four sides remain as UV borders.
        let filled = pixels.chunks_exact(4).filter(|pixel| pixel[3] != 0).count();
        assert!(filled > 4, "the quad border should cover many pixels");
    }

    #[test]
    fn uv_layout_marks_interior_edges_and_draws_them() {
        let mesh = uv_quad();
        let mut meshes = Assets::<Mesh>::default();
        let handle = meshes.add(mesh);
        let entries = vec![(Entity::PLACEHOLDER, handle, GlobalTransform::default())];

        let (edges, triangles) = collect_uv_edges(&entries, &meshes);
        assert_eq!(triangles, 2);
        assert_eq!(edges.len(), 5, "four sides plus the shared diagonal");
        assert_eq!(edges.iter().filter(|edge| edge.border).count(), 4);
        assert_eq!(edges.iter().filter(|edge| !edge.border).count(), 1);

        let layout =
            build_uv_layout(&entries, &meshes, &UdimInfo::default(), false).expect("quad has UVs");
        let pixels = layout.canvas.data.as_ref().unwrap();
        // UV (0.5, 0.5) sits on the interior diagonal. The old borders-only
        // layout left this transparent, which made dense meshes look empty.
        let center = ((UV_CANVAS_SIZE / 2) * UV_CANVAS_SIZE + UV_CANVAS_SIZE / 2) * 4;
        assert!(
            pixels[center + 3] > 0,
            "interior triangle edges should be drawn"
        );
    }

    #[test]
    fn uv_edges_are_classified_in_uv_space() {
        // Two triangles with duplicated vertex indices but identical UVs along
        // the shared diagonal: the edge is interior even though its indices
        // differ, because UV-space classification is what the overlay shows.
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0.0f32, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![
                [0.0f32, 0.0],
                [1.0, 0.0],
                [1.0, 1.0],
                [0.0, 0.0],
                [1.0, 1.0],
                [0.0, 1.0],
            ],
        );
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 3, 4, 5]));
        let mut meshes = Assets::<Mesh>::default();
        let handle = meshes.add(mesh);
        let entries = vec![(Entity::PLACEHOLDER, handle, GlobalTransform::default())];

        let (edges, _) = collect_uv_edges(&entries, &meshes);
        assert_eq!(edges.len(), 5);
        assert_eq!(edges.iter().filter(|edge| edge.border).count(), 4);
        assert_eq!(edges.iter().filter(|edge| !edge.border).count(), 1);
    }

    #[test]
    fn udim_tiles_are_detected_and_laid_out() {
        let mesh = udim_pair();
        let mut meshes = Assets::<Mesh>::default();
        let handle = meshes.add(mesh);
        let entries = vec![(Entity::PLACEHOLDER, handle, GlobalTransform::default())];
        let analysis = analyze_uvs(&entries, &meshes);
        assert!(analysis.udim.enabled);
        assert_eq!(analysis.offset, Vec2::ZERO);
        assert_eq!(analysis.udim.tiles, vec![1001, 1002]);
        assert_eq!(analysis.udim.cols, 2);
        assert_eq!(analysis.udim.rows, 1);

        let layout = build_uv_layout(&entries, &meshes, &analysis.udim, false).expect("UVs");
        assert!(layout.canvas.width() > layout.canvas.height());
        assert!(layout.inset.width() > layout.inset.height());
        assert!(layout
            .canvas
            .data
            .as_ref()
            .unwrap()
            .chunks_exact(4)
            .any(|pixel| pixel[3] != 0));
    }

    #[test]
    fn offset_udim_uvs_are_normalized() {
        let mut mesh = uv_quad();
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![
                [1001.0f32, 0.0],
                [1001.999, 0.0],
                [1001.999, 1.0],
                [1001.0, 1.0],
            ],
        );
        let mut meshes = Assets::<Mesh>::default();
        let handle = meshes.add(mesh);
        let entries = vec![(Entity::PLACEHOLDER, handle, GlobalTransform::default())];

        let analysis = analyze_uvs(&entries, &meshes);
        assert!(analysis.udim.enabled);
        assert_eq!(analysis.offset, Vec2::new(1001.0, 0.0));
        assert_eq!(analysis.udim.tiles, vec![1001]);

        normalize_offset_uvs(&entries, &mut meshes, analysis.offset);
        let normalized = analyze_uvs(&entries, &meshes);
        assert_eq!(normalized.offset, Vec2::ZERO);
        // A single normalized tile is a regular 0–1 layout; the pre-normalized
        // metadata is what labels it as UDIM 1001 for the import.
        assert!(!normalized.udim.enabled);
        let layout = build_uv_layout(&entries, &meshes, &analysis.udim, false).expect("UVs");
        assert!(layout
            .canvas
            .data
            .as_ref()
            .unwrap()
            .chunks_exact(4)
            .any(|pixel| pixel[3] != 0));
    }

    #[test]
    fn udim_tile_numbers_map_to_layout_cells() {
        let udim = UdimInfo {
            enabled: true,
            tiles: vec![1001, 1002, 1011, 1012],
            cols: 2,
            rows: 2,
            min_tile_u: 0,
            max_tile_v: 1,
        };
        assert_eq!(udim.base_tile(), 1001);
        assert_eq!(udim.tile_cell(1001), Some((0, 1)));
        assert_eq!(udim.tile_cell(1002), Some((1, 1)));
        assert_eq!(udim.tile_cell(1011), Some((0, 0)));
        assert_eq!(udim.tile_cell(1012), Some((1, 0)));
        assert_eq!(udim.tile_cell(1003), None);
        assert_eq!(udim.tile_cell(999), None);

        let single = UdimInfo::default();
        assert_eq!(single.tile_cell(1001), Some((0, 0)));
        assert_eq!(single.tile_cell(1002), None);
    }

    #[test]
    fn sanitize_name_blocks_traversal() {
        assert_eq!(sanitize_name("../../evil"), ".._.._evil");
        assert_eq!(sanitize_name("panel mesh"), "panel_mesh");
        assert_eq!(sanitize_name("..."), "mesh");
    }

    #[test]
    fn headless_udim_tiles_are_read_from_a_mesh_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("udim.obj");
        std::fs::write(
            &path,
            "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\n\
             vt 0.1 0.1\nvt 0.9 0.1\nvt 0.1 0.9\n\
             vt 1.1 0.1\nvt 1.9 0.1\nvt 1.1 0.9\n\
             f 1/1 2/2 3/3\nf 2/4 4/5 3/6\n",
        )
        .unwrap();

        assert_eq!(udim_tiles_for_mesh(&path), vec![1001, 1002]);
        assert!(udim_tiles_for_mesh(&dir.path().join("missing.obj")).is_empty());
    }

    #[test]
    fn gltf_scene_types_are_auto_registered() {
        // glTF scenes are WorldAssets. Spawning one resolves every reflected
        // component through AppTypeRegistry, so an empty registry panics with
        // "unregistered type Transform" when the first GLB is imported.
        let app = bevy::app::App::new();
        let registry = app
            .world()
            .resource::<bevy::ecs::reflect::AppTypeRegistry>();
        let registry = registry.read();
        assert!(
            registry
                .get_with_type_path(std::any::type_name::<bevy::prelude::Transform>())
                .is_some(),
            "Transform must be registered for GLB scenes to spawn"
        );
    }

    #[test]
    fn glb_staging_lowercases_and_content_addresses() {
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("My Mesh.GLB");
        std::fs::write(&source, b"glTF first").unwrap();
        let assets = tempfile::tempdir().unwrap();

        let first = stage_gltf(&source, assets.path()).expect("staged");
        assert!(
            first.starts_with("imports/My_Mesh/My_Mesh-"),
            "staged path: {first}"
        );
        assert!(
            first.ends_with(".glb"),
            "loader extensions are lowercase: {first}"
        );
        assert!(assets.path().join(&first).is_file());

        let again = stage_gltf(&source, assets.path()).expect("staged again");
        assert_eq!(first, again, "unchanged content keeps its staged path");

        std::fs::write(&source, b"glTF second").unwrap();
        let changed = stage_gltf(&source, assets.path()).expect("staged change");
        assert_ne!(first, changed, "an edited file gets a fresh asset path");
        assert!(assets.path().join(&changed).is_file());
    }

    #[test]
    fn mesh_extension_is_checked() {
        assert!(is_mesh_path(Path::new("hat.GLB")));
        assert!(is_mesh_path(Path::new("hat.gltf")));
        assert!(is_mesh_path(Path::new("hat.obj")));
        assert!(is_mesh_path(Path::new("hat.fbx")));
        assert!(is_mesh_path(Path::new("hat.dae")));
        assert!(is_mesh_path(Path::new("hat.usd")));
        assert!(is_mesh_path(Path::new("hat.usda")));
        assert!(is_mesh_path(Path::new("hat.usdc")));
        assert!(is_mesh_path(Path::new("hat.usdz")));
        assert!(!is_mesh_path(Path::new("hat.png")));
    }
}
