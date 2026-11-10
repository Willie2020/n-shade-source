use bevy::asset::RenderAssetUsages;
use bevy::camera::Viewport;
use bevy::core_pipeline::Skybox;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::pbr::ParallaxMappingMethod;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::window::PrimaryWindow;
use bevy_egui::PrimaryEguiContext;
use glam::{Vec2, Vec3};

use crate::app::{
    DocumentRes, EditorState, EnvironmentPreset, LightingPreset, MaterialPreset, PreviewHandles,
    PreviewMeshHandles, PreviewParallax, PreviewShape, PreviewTessellation,
};
use crate::gpu;
use crate::import::MeshImport;
use crate::lightmap::LightmapRuntime;
use crate::lightmap_preview::{LightmapPreviewEnvironment, AMBIENT_LUX};

#[derive(Component)]
pub struct PreviewCamera;

/// Full-window camera that owns the primary egui context. It renders no
/// geometry; its only job is to give egui the whole window as its viewport so
/// the UI is independent of the 3D preview's sub-viewport.
#[derive(Component)]
pub struct UiCamera;

#[derive(Component)]
pub struct RotatingPreview;

/// Marks the key/fill directional lights so preview presets can drive them.
#[derive(Component)]
pub struct PreviewLight {
    pub key: bool,
}

/// Builds the 3D preview scene and the GPU output textures used by the compute
/// evaluator, the material and the egui canvas.
/// Generate MikkTSpace tangents so the normal map shades correctly. On failure
/// log and continue: the mesh still renders, just without tangent-space normals.
fn with_tangents(mut mesh: Mesh, label: &str) -> Mesh {
    if let Err(error) = mesh.generate_tangents() {
        warn!("preview: could not generate tangents for the {label} mesh: {error}");
    }
    mesh
}

/// Build and upload the four built-in preview meshes at one tessellation level.
fn preview_mesh_set(level: PreviewTessellation, meshes: &mut Assets<Mesh>) -> PreviewMeshHandles {
    let cube = if level == PreviewTessellation::Off {
        Mesh::from(Cuboid::new(1.05, 1.05, 1.05))
    } else {
        subdivided_cube_mesh(0.525, level.edge_segments())
    };
    let plane = Plane3d::default()
        .mesh()
        .size(1.1, 1.1)
        .subdivisions(level.plane_subdivisions())
        .build();
    PreviewMeshHandles {
        cube: meshes.add(with_tangents(cube, "cube")),
        rounded_cube: meshes.add(with_tangents(
            rounded_cube_mesh(0.52, ROUNDED_CUBE_EXPONENT, level.rounded_segments()),
            "rounded cube",
        )),
        plane: meshes.add(with_tangents(plane, "plane")),
        cylinder: meshes.add(with_tangents(
            Cylinder::new(0.5, 1.0)
                .mesh()
                .resolution(level.cylinder_segments())
                .build(),
            "cylinder",
        )),
    }
}

/// Flat-shaded cube made of six face grids. The face order, UV layout and
/// winding match [`rounded_cube_mesh`], so both shapes show the same part of
/// the map and share the preview's V-flip rule.
fn subdivided_cube_mesh(half: f32, resolution: u32) -> Mesh {
    let resolution = resolution.max(1);
    let stride = resolution + 1;
    let mut positions = Vec::with_capacity((6 * stride * stride) as usize);
    let mut normals = Vec::with_capacity(positions.capacity());
    let mut uvs = Vec::with_capacity(positions.capacity());
    let mut indices = Vec::with_capacity((6 * resolution * resolution * 6) as usize);

    for face in 0..6 {
        let base = positions.len() as u32;
        let normal = match face {
            0 => Vec3::Z,
            1 => -Vec3::Z,
            2 => Vec3::X,
            3 => -Vec3::X,
            4 => Vec3::Y,
            _ => -Vec3::Y,
        };
        for row in 0..=resolution {
            let v = -1.0 + 2.0 * row as f32 / resolution as f32;
            for column in 0..=resolution {
                let u = -1.0 + 2.0 * column as f32 / resolution as f32;
                let direction = match face {
                    0 => Vec3::new(u, v, 1.0),
                    1 => Vec3::new(-u, v, -1.0),
                    2 => Vec3::new(1.0, v, -u),
                    3 => Vec3::new(-1.0, v, u),
                    4 => Vec3::new(u, 1.0, -v),
                    _ => Vec3::new(u, -1.0, v),
                };
                positions.push((direction * half).to_array());
                normals.push(normal.to_array());
                uvs.push([(u + 1.0) * 0.5, (v + 1.0) * 0.5]);
            }
        }

        for row in 0..resolution {
            for column in 0..resolution {
                let a = base + row * stride + column;
                let b = a + 1;
                let c = a + stride;
                let d = c + 1;
                indices.extend_from_slice(&[a, b, c, b, d, c]);
            }
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    doc: Res<DocumentRes>,
    mut gpu_scene: ResMut<gpu::GpuScene>,
    mut gpu_cache: ResMut<gpu::GpuSceneCache>,
    mut state: ResMut<EditorState>,
) {
    let width = doc.0.size.x;
    let height = doc.0.size.y;
    state.tex_dims = [width as usize, height as usize];
    if state.selected.is_none() {
        state.selected = doc.0.layers.len().checked_sub(1).map(|index| vec![index]);
    }

    commands.spawn((
        UiCamera,
        Camera2d,
        Camera {
            order: 10,
            clear_color: bevy::camera::ClearColorConfig::Default,
            ..default()
        },
        PrimaryEguiContext,
    ));

    let normal_texture = images.add(gpu_normal_image(width, height));
    let height_texture = images.add(gpu_height_image(width, height));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.40, 0.43, 0.48),
        metallic: 0.1,
        perceptual_roughness: 0.55,
        normal_map_texture: Some(normal_texture.clone()),
        ..default()
    });
    let lightmap_material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        metallic: 0.0,
        perceptual_roughness: 0.82,
        ..default()
    });

    let preview_meshes = preview_mesh_set(state.preview_tessellation, &mut meshes);
    commands.insert_resource(PreviewHandles {
        material: material.clone(),
        lightmap_material,
        normal_texture: normal_texture.clone(),
        height_texture: height_texture.clone(),
        meshes: preview_meshes.clone(),
    });
    commands.insert_resource(gpu::GpuOutputs {
        normal: normal_texture,
        height: height_texture,
    });

    let data = if state.integrate_height {
        gpu::build_scene_cached(
            &doc.0,
            gpu::GpuSceneOptions {
                integrate_height: true,
            },
            &mut gpu_cache,
        )
    } else {
        gpu::build_scene_cached(&doc.0, gpu::GpuSceneOptions::default(), &mut gpu_cache)
    };
    state.using_gpu = data.is_some();
    // The GPU path is dispatched from the extracted scene generation. The CPU
    // fallback needs an explicit first evaluation, otherwise textures stay at
    // their flat initial contents until the first edit.
    state.dirty = data.is_none();
    gpu_scene.data = data;
    gpu_scene.generation = gpu_scene.generation.wrapping_add(1);
    info!(
        "preview scene: {width}x{height}, {} layers, GPU path: {}",
        doc.0.layer_count(),
        state.using_gpu
    );

    commands.spawn((
        Mesh3d(preview_meshes.cube.clone()),
        MeshMaterial3d(material.clone()),
        Transform::from_rotation(Quat::from_rotation_y(0.25)),
        PreviewShape::Cube,
        RotatingPreview,
    ));
    commands.spawn((
        Mesh3d(preview_meshes.rounded_cube.clone()),
        MeshMaterial3d(material.clone()),
        Transform::from_rotation(Quat::from_rotation_y(0.25)),
        PreviewShape::RoundedCube,
        Visibility::Hidden,
        RotatingPreview,
    ));
    commands.spawn((
        Mesh3d(preview_meshes.plane.clone()),
        MeshMaterial3d(material.clone()),
        Transform::from_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        PreviewShape::Plane,
        Visibility::Hidden,
    ));
    commands.spawn((
        Mesh3d(preview_meshes.cylinder.clone()),
        MeshMaterial3d(material.clone()),
        Transform::from_rotation(Quat::from_rotation_y(0.25)),
        PreviewShape::Cylinder,
        Visibility::Hidden,
        RotatingPreview,
    ));

    commands.spawn((
        DirectionalLight {
            illuminance: 7000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.7, 0.6, 0.0)),
        PreviewLight { key: true },
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 2500.0,
            shadow_maps_enabled: false,
            color: Color::srgb(0.7, 0.8, 1.0),
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, 0.5, -0.9, 0.0)),
        PreviewLight { key: false },
    ));

    commands.spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(Color::srgb(0.031, 0.059, 0.071)),
            ..default()
        },
        Projection::Perspective(PerspectiveProjection {
            fov: 38f32.to_radians(),
            ..default()
        }),
        Transform::from_xyz(0.0, 0.45, 3.2).looking_at(Vec3::ZERO, Vec3::Y),
        // Keep the component alive for the camera's lifetime so the skybox
        // pipeline stays prepared; the image and brightness are driven by the
        // lightmap preview.
        Skybox::default(),
        PreviewCamera,
    ));
}

pub fn gpu_normal_image(width: u32, height: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[128, 128, 255, 255],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage |= TextureUsages::STORAGE_BINDING;
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

pub fn gpu_height_image(width: u32, height: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage |= TextureUsages::STORAGE_BINDING;
    image
}

#[allow(clippy::too_many_arguments)]
pub fn apply_preview_style(
    state: Res<EditorState>,
    runtime: Res<LightmapRuntime>,
    preview_environment: Res<LightmapPreviewEnvironment>,
    handles: Res<PreviewHandles>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut lights: Query<(&PreviewLight, &mut DirectionalLight)>,
    mut applied: Local<Option<(MaterialPreset, LightingPreset, EnvironmentPreset, bool)>>,
) {
    let preview = runtime.preview_enabled;
    let current = (
        state.material_preset,
        state.lighting_preset,
        state.environment_preset,
        preview,
    );
    if !preview && *applied == Some(current) {
        return;
    }
    *applied = Some(current);

    if !preview {
        if let Some(mut material) = materials.get_mut(&handles.material) {
            let (color, metallic, roughness) = match state.material_preset {
                MaterialPreset::MatteGray => (Color::srgb(0.40, 0.43, 0.48), 0.1, 0.55),
                MaterialPreset::Clay => (Color::srgb(0.62, 0.38, 0.29), 0.0, 0.82),
                MaterialPreset::Steel => (Color::srgb(0.55, 0.59, 0.64), 0.88, 0.27),
            };
            material.base_color = color;
            material.metallic = metallic;
            material.perceptual_roughness = roughness;
        }
    }

    for (light, mut directional) in &mut lights {
        if preview {
            // The lightmap preview owns the lighting while it is active.
            directional.illuminance = 0.0;
            continue;
        }
        let (illuminance, color) = match (state.lighting_preset, light.key) {
            (LightingPreset::Studio, true) => (7000.0, Color::WHITE),
            (LightingPreset::Studio, false) => (2500.0, Color::srgb(0.70, 0.80, 1.0)),
            (LightingPreset::Soft, true) => (4200.0, Color::srgb(1.0, 0.96, 0.90)),
            (LightingPreset::Soft, false) => (2600.0, Color::srgb(0.82, 0.88, 1.0)),
            (LightingPreset::Dramatic, true) => (11000.0, Color::srgb(1.0, 0.88, 0.72)),
            (LightingPreset::Dramatic, false) => (450.0, Color::srgb(0.55, 0.70, 1.0)),
        };
        directional.illuminance = illuminance;
        directional.color = color;
    }

    if preview {
        let color = runtime.settings.ambient_color;
        ambient.color = Color::linear_rgb(color[0].max(0.0), color[1].max(0.0), color[2].max(0.0));
        // Mirror the bake: the solid ambient is the fill floor, and a loaded
        // HDRI scales it with its average radiance. Without this the only
        // lighting on surfaces facing away from the bright side is the HDR's
        // dark region, which reads as black.
        let fill_scale = if runtime.environment_path.is_some() {
            let average = preview_environment.average_radiance.max(0.0)
                * runtime.settings.environment_strength.max(0.0);
            average.max(1.0) * runtime.settings.environment_fill.max(0.0)
        } else {
            1.0
        };
        ambient.brightness = runtime.settings.ambient_intensity.max(0.0) * fill_scale * AMBIENT_LUX;
    } else {
        let (brightness, color) = match state.environment_preset {
            EnvironmentPreset::Neutral => (220.0, Color::srgb(0.75, 0.82, 1.0)),
            EnvironmentPreset::Cool => (270.0, Color::srgb(0.58, 0.76, 1.0)),
            EnvironmentPreset::Warm => (240.0, Color::srgb(1.0, 0.82, 0.64)),
        };
        ambient.brightness = brightness;
        ambient.color = color;
    }
}

pub fn auto_rotate(
    time: Res<Time>,
    state: Res<EditorState>,
    mut preview: Query<&mut Transform, With<RotatingPreview>>,
) {
    if !state.auto_rotate || state.preview_shape == PreviewShape::Plane {
        return;
    }
    for mut transform in &mut preview {
        transform.rotate_y(time.delta_secs() * 0.22);
    }
}

/// Superellipsoid exponent for the rounded cube. `0.5` (implicit power four)
/// keeps broad, nearly flat faces but gives the bevel a clearly visible radius;
/// `1.0` would be a sphere.
const ROUNDED_CUBE_EXPONENT: f32 = 0.5;
/// Generates a smooth superellipsoid that reads as a beveled/rounded cube.
///
/// The implicit surface is `|x|^n + |y|^n + |z|^n = 1` with `n = 2/e`; a small
/// exponent rounds the corners while keeping broad, nearly flat faces. Normals
/// come from the implicit gradient.
///
/// UVs are a six-face cube unwrap rather than a spherical projection: each face
/// gets the full 0-1 normal map and its own planar basis, matching the plain
/// cube preview. Adjacent face grids duplicate the vertices along the rounded
/// edges, so UVs can jump at the seam while positions and normals stay smooth.
fn rounded_cube_mesh(half: f32, exponent: f32, resolution: u32) -> Mesh {
    let resolution = resolution.max(2);
    let exponent = exponent.clamp(0.05, 1.0);
    let power = 2.0 / exponent;
    let stride = resolution + 1;
    let mut positions = Vec::with_capacity((6 * stride * stride) as usize);
    let mut normals = Vec::with_capacity(positions.capacity());
    let mut uvs = Vec::with_capacity(positions.capacity());
    let mut indices = Vec::with_capacity((6 * resolution * resolution * 6) as usize);

    for face in 0..6 {
        let base = positions.len() as u32;
        for row in 0..=resolution {
            let v = -1.0 + 2.0 * row as f32 / resolution as f32;
            for column in 0..=resolution {
                let u = -1.0 + 2.0 * column as f32 / resolution as f32;
                // Each face's tangent axes are chosen so `du x dv` points out
                // of the cube, which keeps one triangle winding for all faces.
                let direction = match face {
                    0 => Vec3::new(u, v, 1.0),   // +Z (front)
                    1 => Vec3::new(-u, v, -1.0), // -Z (back)
                    2 => Vec3::new(1.0, v, -u),  // +X (right)
                    3 => Vec3::new(-1.0, v, u),  // -X (left)
                    4 => Vec3::new(u, 1.0, -v),  // +Y (top)
                    _ => Vec3::new(u, -1.0, v),  // -Y (bottom)
                };
                let length = (direction.x.abs().powf(power)
                    + direction.y.abs().powf(power)
                    + direction.z.abs().powf(power))
                .powf(1.0 / power);
                let point = direction / length * half;
                let normal = superellipsoid_normal(point, half, power);
                positions.push(point.to_array());
                normals.push(normal.to_array());
                uvs.push([(u + 1.0) * 0.5, (v + 1.0) * 0.5]);
            }
        }

        for row in 0..resolution {
            for column in 0..resolution {
                let a = base + row * stride + column;
                let b = a + 1;
                let c = a + stride;
                let d = c + 1;
                indices.extend_from_slice(&[a, b, c, b, d, c]);
            }
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Implicit gradient of `|x|^n + |y|^n + |z|^n = 1` at a point on the surface.
fn superellipsoid_normal(point: Vec3, half: f32, power: f32) -> Vec3 {
    let inverse = 1.0 / half.max(1e-6);
    let gradient = Vec3::new(
        power * (point.x * inverse).abs().powf(power - 1.0) * point.x.signum(),
        power * (point.y * inverse).abs().powf(power - 1.0) * point.y.signum(),
        power * (point.z * inverse).abs().powf(power - 1.0) * point.z.signum(),
    );
    gradient.normalize_or_zero()
}

/// Built-in preview shapes do not share one UV origin. Bevy's `Cuboid` and
/// `Cylinder`, and the custom rounded cube, place V=0 at the bottom like
/// OBJ/FBX/DAE/USD, while `Plane3d` places it at the top. The document textures
/// use a top-left origin, so every bottom-left shape needs the same preview V
/// flip as an imported bottom-left mesh; the plane must not be flipped.
fn preview_flips_v(shape: PreviewShape, imported_uv_bottom_left: bool) -> bool {
    match shape {
        PreviewShape::Plane => false,
        PreviewShape::Imported => imported_uv_bottom_left,
        PreviewShape::Cube | PreviewShape::RoundedCube | PreviewShape::Cylinder => true,
    }
}

/// Imported meshes keep their original UVs (bottom-left for OBJ/FBX/DAE/USD),
/// while Bevy samples textures with a top-left origin. Flip only the preview
/// texture V so the 3D view matches the source; the bake and document maps
/// keep the mesh's native convention for other software.
///
/// Flipping V mirrors the normal map's tangent space as well: the mesh
/// tangents come from the original UVs, so the green channel has to be
/// negated whenever the texture is mirrored. Without that the bevels shade
/// as if the surface were carved instead of raised. A DirectX bake preview
/// adds a second green flip on top of the V mirror.
pub fn sync_preview_uv_transform(
    state: Res<EditorState>,
    mesh_import: Res<MeshImport>,
    bake: Option<Res<crate::bake::BakeRuntime>>,
    handles: Res<PreviewHandles>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut applied: Local<Option<(Affine2, bool)>>,
) {
    let tiling = state.tiling.max(0.05);
    let mirrored = preview_flips_v(state.preview_shape, mesh_import.uv_bottom_left);
    let directx_preview = bake.as_deref().is_some_and(|runtime| {
        runtime.preview_on_mesh
            && runtime
                .result
                .as_ref()
                .is_some_and(|result| result.space == ns_bake::BakeSpace::TangentDirectX)
    });
    let flip = mirrored ^ directx_preview;
    let transform = if mirrored {
        Affine2::from_translation(Vec2::new(0.0, tiling))
            * Affine2::from_scale(Vec2::new(tiling, -tiling))
    } else {
        Affine2::from_scale(Vec2::splat(tiling))
    };
    if applied.as_ref() == Some(&(transform, flip)) {
        return;
    }
    if let Some(mut material) = materials.get_mut(&handles.material) {
        material.uv_transform = transform;
        material.flip_normal_map_y = flip;
        *applied = Some((transform, flip));
    }
}

pub fn sync_preview_visibility(
    state: Res<EditorState>,
    mut preview: Query<(&PreviewShape, &mut Visibility)>,
) {
    for (shape, mut visibility) in &mut preview {
        let wanted = if *shape == state.preview_shape {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// Built-in preview surface edge in world units. Document heights are stored
/// normalized, so this converts the document's `height_scale_px` into a
/// parallax depth of the same fraction of the surface.
const PREVIEW_SURFACE_UNITS: f32 = 1.05;

/// Parallax depth in world units for a document of `side` pixels whose height
/// scale is `height_scale_px`. `scale` is the user multiplier.
fn preview_parallax_depth(height_scale_px: f32, side: u32, scale: f32) -> f32 {
    let side = side.max(1) as f32;
    (height_scale_px.max(0.0) / side) * PREVIEW_SURFACE_UNITS * scale.clamp(0.0, 4.0)
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PreviewHeightKey {
    mode: PreviewParallax,
    scale_bits: u32,
    side: u32,
    document_scale_bits: u32,
    tiling_bits: u32,
}

/// Feed the evaluated height texture to the preview material as a parallax
/// depth map. The PBR preview sets its own depth map, so this system steps
/// aside while it is active and restores the editor's setting afterwards.
pub fn sync_preview_height(
    state: Res<EditorState>,
    doc: Res<DocumentRes>,
    handles: Res<PreviewHandles>,
    pbr: Option<Res<crate::pbr_ui::PbrRuntime>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut applied: Local<Option<PreviewHeightKey>>,
) {
    if pbr.as_ref().is_some_and(|runtime| runtime.preview_active) {
        *applied = None;
        return;
    }

    let mode = state.preview_parallax;
    let scale = state.preview_height_scale.clamp(0.0, 4.0);
    let side = doc.0.size.x.max(1);
    let tiling = state.tiling.max(0.05);
    let key = PreviewHeightKey {
        mode,
        scale_bits: scale.to_bits(),
        side,
        document_scale_bits: doc.0.height_scale_px.to_bits(),
        tiling_bits: tiling.to_bits(),
    };
    if applied.as_ref() == Some(&key) {
        return;
    }
    *applied = Some(key);

    let Some(mut material) = materials.get_mut(&handles.material) else {
        return;
    };
    // A tiled map repeats the whole height field `tiling` times across the
    // surface, so the physical depth per copy shrinks by the same factor.
    let depth = preview_parallax_depth(doc.0.height_scale_px, side, scale) / tiling;
    if mode == PreviewParallax::Off || depth <= 1e-6 {
        material.depth_map = None;
        material.parallax_depth_scale = 0.0;
        return;
    }

    material.depth_map = Some(handles.height_texture.clone());
    material.parallax_depth_scale = depth;
    match mode {
        PreviewParallax::Off => {}
        PreviewParallax::Occlusion => {
            material.parallax_mapping_method = ParallaxMappingMethod::Occlusion;
            material.max_parallax_layer_count = 16.0;
        }
        PreviewParallax::Relief => {
            material.parallax_mapping_method = ParallaxMappingMethod::Relief { max_steps: 16 };
            material.max_parallax_layer_count = 32.0;
        }
    }
}

/// Rebuild the built-in preview meshes when the tessellation level changes.
/// Imported meshes keep their own topology.
pub fn sync_preview_tessellation(
    state: Res<EditorState>,
    mut handles: ResMut<PreviewHandles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut preview: Query<(&PreviewShape, &mut Mesh3d)>,
    mut applied: Local<Option<PreviewTessellation>>,
) {
    if applied.as_ref() == Some(&state.preview_tessellation) {
        return;
    }
    *applied = Some(state.preview_tessellation);
    handles.meshes = preview_mesh_set(state.preview_tessellation, &mut meshes);
    for (shape, mut mesh) in &mut preview {
        let wanted = match shape {
            PreviewShape::Cube => &handles.meshes.cube,
            PreviewShape::RoundedCube => &handles.meshes.rounded_cube,
            PreviewShape::Plane => &handles.meshes.plane,
            PreviewShape::Cylinder => &handles.meshes.cylinder,
            PreviewShape::Imported => continue,
        };
        if mesh.0 != *wanted {
            mesh.0 = wanted.clone();
        }
    }
}

/// Clip the 3D camera to the rectangle occupied by the central viewport and
/// disable it entirely while the 2D canvas is active.
pub fn apply_camera_viewport(
    state: Res<EditorState>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut camera: Query<(&mut Camera, &mut Projection, &mut Transform), With<PreviewCamera>>,
    mut ui_camera: Query<&mut Camera, (With<UiCamera>, Without<PreviewCamera>)>,
) {
    // The UI camera spans the whole window. When the 3D pane is visible, the 3D
    // camera has already cleared the window, so the UI camera must not clear it
    // again (that would erase the preview). In 2D-only mode the 2D canvas is
    // painted by egui, but the window still needs a clear, so this camera owns
    // it.
    if let Ok(mut ui) = ui_camera.single_mut() {
        if !ui.is_active {
            ui.is_active = true;
        }
        let wanted = if state.preview_rect.is_some() {
            bevy::camera::ClearColorConfig::None
        } else {
            bevy::camera::ClearColorConfig::Default
        };
        // `ClearColorConfig` has no `PartialEq`, and writing every frame would
        // mark the camera as changed and re-extract it for no reason.
        if std::mem::discriminant(&ui.clear_color) != std::mem::discriminant(&wanted) {
            ui.clear_color = wanted;
        }
    }
    let Ok((mut camera, mut projection, mut transform)) = camera.single_mut() else {
        return;
    };
    let Some(rect) = state.preview_rect else {
        if camera.is_active {
            camera.is_active = false;
        }
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let scale = window.resolution.scale_factor();
    let physical = UVec2::new(
        window.resolution.physical_width(),
        window.resolution.physical_height(),
    );
    let x = (rect.min.x * scale).round().max(0.0) as u32;
    let y = (rect.min.y * scale).round().max(0.0) as u32;
    let width = ((rect.width() * scale).round().max(1.0) as u32).min(physical.x.saturating_sub(x));
    let height =
        ((rect.height() * scale).round().max(1.0) as u32).min(physical.y.saturating_sub(y));
    if width == 0 || height == 0 {
        if camera.is_active {
            camera.is_active = false;
        }
        return;
    }
    if !camera.is_active {
        camera.is_active = true;
    }
    let viewport = Viewport {
        physical_position: UVec2::new(x, y),
        physical_size: UVec2::new(width, height),
        depth: 0.0..1.0,
    };
    // `Viewport` does not implement `PartialEq`; only assign when the pane
    // actually moved or resized so the camera is not re-extracted every frame.
    let viewport_changed = match &camera.viewport {
        Some(current) => {
            current.physical_position != viewport.physical_position
                || current.physical_size != viewport.physical_size
                || current.depth != viewport.depth
        }
        None => true,
    };
    if viewport_changed {
        camera.viewport = Some(viewport);
    }
    if let Projection::Perspective(perspective) = &mut *projection {
        let aspect = width as f32 / height as f32;
        if (perspective.aspect_ratio - aspect).abs() > f32::EPSILON {
            perspective.aspect_ratio = aspect;
        }
    }

    let direction = Vec3::new(
        state.orbit_distance * state.orbit_pitch.cos() * state.orbit_yaw.sin(),
        state.orbit_distance * state.orbit_pitch.sin(),
        state.orbit_distance * state.orbit_pitch.cos() * state.orbit_yaw.cos(),
    );
    let desired = Transform::from_translation(direction).looking_at(Vec3::ZERO, Vec3::Y);
    if transform.translation != desired.translation
        || transform.rotation != desired.rotation
        || transform.scale != desired.scale
    {
        *transform = desired;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::mesh::VertexAttributeValues;

    fn positions(mesh: &Mesh) -> Vec<Vec3> {
        let Some(VertexAttributeValues::Float32x3(values)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("rounded cube has positions");
        };
        values
            .iter()
            .map(|value| Vec3::from_array(*value))
            .collect()
    }

    fn uvs(mesh: &Mesh) -> Vec<Vec2> {
        let Some(VertexAttributeValues::Float32x2(values)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            panic!("rounded cube has UVs");
        };
        values
            .iter()
            .map(|value| Vec2::from_array(*value))
            .collect()
    }

    fn normals(mesh: &Mesh) -> Vec<Vec3> {
        let Some(VertexAttributeValues::Float32x3(values)) = mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        else {
            panic!("rounded cube has normals");
        };
        values
            .iter()
            .map(|value| Vec3::from_array(*value))
            .collect()
    }

    #[test]
    fn preview_v_flip_matches_the_shape_uv_origin() {
        assert!(preview_flips_v(PreviewShape::Cube, false));
        assert!(preview_flips_v(PreviewShape::RoundedCube, false));
        assert!(preview_flips_v(PreviewShape::Cylinder, false));
        // `Plane3d` is already top-left, so the imported flag must not reach it.
        assert!(!preview_flips_v(PreviewShape::Plane, true));
        assert!(preview_flips_v(PreviewShape::Imported, true));
        assert!(!preview_flips_v(PreviewShape::Imported, false));
    }

    #[test]
    fn builtin_uv_origins_are_what_the_flip_assumes() {
        // These are the Bevy/primitive conventions `preview_flips_v` encodes:
        // the cuboid puts V=0 at the bottom, the plane puts it at the top. If a
        // Bevy update changes either, this test fails before the preview does.
        let cube = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
        let cube_positions = positions(&cube);
        let cube_uvs = uvs(&cube);
        let bottom_left = cube_positions
            .iter()
            .position(|position| (*position - Vec3::new(-0.5, -0.5, 0.5)).length() < 1e-5)
            .expect("cuboid front bottom-left vertex");
        assert_eq!(
            cube_uvs[bottom_left],
            Vec2::ZERO,
            "Cuboid V=0 is at the bottom"
        );

        let plane = Plane3d::default().mesh().size(1.0, 1.0).build();
        let plane_positions = positions(&plane);
        let plane_uvs = uvs(&plane);
        let top = plane_positions
            .iter()
            .position(|position| (*position - Vec3::new(-0.5, 0.0, -0.5)).length() < 1e-5)
            .expect("plane corner");
        assert_eq!(plane_uvs[top], Vec2::ZERO, "Plane3d V=0 is at the top");
    }

    #[test]
    fn rounded_cube_has_face_uvs_and_a_visible_bevel() {
        let half = 0.5;
        let mut mesh = rounded_cube_mesh(half, ROUNDED_CUBE_EXPONENT, 8);
        let positions = positions(&mesh);
        let uvs = uvs(&mesh);
        let normals = normals(&mesh);
        assert_eq!(positions.len(), 6 * 9 * 9);
        assert_eq!(uvs.len(), positions.len());
        assert_eq!(normals.len(), positions.len());

        // Each face samples the full normal map rather than a pinched polar
        // projection: the six axis centres all map to the middle of the UV
        // square.
        for axis in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z] {
            let target = axis * half;
            let index = positions
                .iter()
                .position(|position| (*position - target).length() < 1.0e-4)
                .unwrap_or_else(|| panic!("face centre {axis} exists"));
            assert!(
                (uvs[index] - Vec2::splat(0.5)).length() < 1.0e-4,
                "face centre {axis} UV: {:?}",
                uvs[index]
            );
        }
        assert!(uvs
            .iter()
            .all(|uv| (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y)));

        // The rounded corner sits well inside the ideal cube corner, so the
        // bevel is visible instead of a hairline.
        let corner = half * 3.0_f32.powf(-ROUNDED_CUBE_EXPONENT * 0.5);
        assert!(half - corner > half * 0.2);
        assert!(positions
            .iter()
            .any(|position| (*position - Vec3::splat(corner)).length() < 1.0e-4));

        // All face grids wind outward, so the mesh can be lit and tangent
        // generated without back-facing triangles.
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("rounded cube has index data");
        };
        for triangle in indices.chunks_exact(3) {
            let [a, b, c] = [
                triangle[0] as usize,
                triangle[1] as usize,
                triangle[2] as usize,
            ];
            let face = (positions[b] - positions[a]).cross(positions[c] - positions[a]);
            let normal = normals[a] + normals[b] + normals[c];
            assert!(face.dot(normal) > 0.0, "triangle winding faces outward");
        }
        assert!(mesh.generate_tangents().is_ok());
    }

    #[test]
    fn every_preview_mesh_gets_tangents() {
        let meshes = [
            with_tangents(Mesh::from(Cuboid::new(1.05, 1.05, 1.05)), "cube"),
            with_tangents(
                rounded_cube_mesh(
                    0.52,
                    ROUNDED_CUBE_EXPONENT,
                    PreviewTessellation::Off.rounded_segments(),
                ),
                "rounded cube",
            ),
            with_tangents(Plane3d::default().mesh().size(1.1, 1.1).build(), "plane"),
            with_tangents(
                Cylinder::new(0.5, 1.0).mesh().resolution(64).build(),
                "cylinder",
            ),
        ];
        for mesh in meshes {
            assert!(
                mesh.attribute(Mesh::ATTRIBUTE_TANGENT).is_some(),
                "preview meshes must carry tangents"
            );
        }
    }

    #[test]
    fn subdivided_cube_matches_the_other_preview_faces() {
        let half = 0.5;
        let mut mesh = subdivided_cube_mesh(half, 4);
        let positions = positions(&mesh);
        let uvs = uvs(&mesh);
        let normals = normals(&mesh);
        assert_eq!(positions.len(), 6 * 5 * 5);
        assert_eq!(uvs.len(), positions.len());
        assert_eq!(normals.len(), positions.len());

        // Every face grid spans the whole map and its centre sits at UV 0.5.
        for axis in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z] {
            let target = axis * half;
            let index = positions
                .iter()
                .position(|position| (*position - target).length() < 1.0e-4)
                .unwrap_or_else(|| panic!("face centre {axis} exists"));
            assert!((uvs[index] - Vec2::splat(0.5)).length() < 1.0e-4);
            assert!((normals[index] - axis).length() < 1.0e-5);
        }
        assert!(uvs
            .iter()
            .all(|uv| (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y)));

        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("subdivided cube has index data");
        };
        for triangle in indices.as_chunks::<3>().0 {
            let [a, b, c] = *triangle;
            let [a, b, c] = [a as usize, b as usize, c as usize];
            let face = (positions[b] - positions[a]).cross(positions[c] - positions[a]);
            assert!(
                face.dot(normals[a]) > 0.0,
                "subdivided cube winding faces outward"
            );
        }
        assert!(mesh.generate_tangents().is_ok());
    }

    #[test]
    fn preview_parallax_depth_follows_the_document_height_scale() {
        // 64 px of height over a 1024 px document is 1/16 of the 1.05-unit
        // preview surface.
        let depth = preview_parallax_depth(64.0, 1024, 1.0);
        assert!((depth - 0.065_625).abs() < 1e-5, "depth {depth}");
        assert_eq!(preview_parallax_depth(64.0, 1024, 0.0), 0.0);
        assert!((preview_parallax_depth(64.0, 1024, 2.0) - depth * 2.0).abs() < 1e-5);
        // The multiplier is clamped to the slider range.
        assert_eq!(
            preview_parallax_depth(64.0, 1024, 99.0),
            preview_parallax_depth(64.0, 1024, 4.0)
        );
    }

    #[test]
    fn tessellation_levels_get_denser() {
        let levels = PreviewTessellation::ALL;
        for pair in levels.windows(2) {
            assert!(pair[0].edge_segments() <= pair[1].edge_segments());
            assert!(pair[0].rounded_segments() <= pair[1].rounded_segments());
            assert!(pair[0].cylinder_segments() <= pair[1].cylinder_segments());
            assert!(pair[0].plane_subdivisions() <= pair[1].plane_subdivisions());
        }
    }
}
