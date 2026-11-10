//! Optional real-time PBR preview of the lightmap lighting setup.
//!
//! The bake itself is path-traced. This module gives the 3D viewport a
//! rasterized preview instead: the imported low-poly mesh gets a neutral white
//! material, a directional light matches the sun settings, and an
//! equirectangular HDR is converted to cubemaps for the skybox and image-based
//! lighting. The preview is off by default and never changes the bake.

use std::path::PathBuf;

use bevy::asset::RenderAssetUsages;
use bevy::camera::Exposure;
use bevy::core_pipeline::Skybox;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::CascadeShadowConfigBuilder;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool, Task};
use glam::Vec3;
use half::f16;
use ns_bake::{EnvironmentMap, LightmapLightKind, LightmapSceneLight};

use crate::app::{EditorState, PreviewHandles, PreviewShape, ViewportTab};
use crate::import::MeshImport;
use crate::lightmap::LightmapRuntime;
use crate::preview::PreviewCamera;

/// Face size of the cubemap used for the skybox and specular environment.
const CUBE_FACE_SIZE: u32 = 512;
/// Face size of the irradiance cubemap used for diffuse environment lighting.
const DIFFUSE_FACE_SIZE: u32 = 16;
/// Cosine-weighted samples per diffuse cubemap texel.
const DIFFUSE_SAMPLES: u32 = 64;
/// Directional-light illuminance for a lightmap sun intensity of 1.0.
const SUN_LUX: f32 = 7000.0;
/// Ambient brightness for a lightmap ambient intensity of 1.0.
pub(crate) const AMBIENT_LUX: f32 = 1200.0;
/// Radiance multiplier that makes raw HDR values visible under the preview
/// camera's SDR output (the bake treats HDRI values as arbitrary linear
/// radiance).
const ENVIRONMENT_NITS: f32 = 1000.0;
/// Longest side of the HDR decoded for the preview. The skybox faces are
/// 512 px, so decoding a larger source only costs memory and startup time.
const PREVIEW_ENVIRONMENT_MAX_SIDE: u32 = 2048;
/// Luminous scale for one unit of lightmap light intensity in the preview.
/// Matches [`SUN_LUX`] at one normalized unit of distance so the rasterized
/// preview and the path-traced bake stay in the same ballpark.
const SCENE_LIGHT_LUMENS: f32 = 88_000.0;
/// Radius of the marker sphere drawn at each scene light.
const SCENE_LIGHT_MARKER_RADIUS: f32 = 0.02;

pub struct LightmapPreviewPlugin;

impl Plugin for LightmapPreviewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightmapPreviewEnvironment>()
            .init_resource::<LightmapPreviewSceneLights>()
            .add_systems(Startup, spawn_preview_sun)
            .add_systems(
                Update,
                (
                    sync_preview_mode,
                    sync_preview_materials,
                    sync_preview_light,
                    sync_preview_lighting,
                    sync_preview_scene_lights,
                ),
            );
    }
}

/// Marks the directional light that mirrors the lightmap sun in the preview.
#[derive(Component)]
struct LightmapPreviewSun;

/// Mirrors `LightmapSettings::lights` in the 3D viewport. Rebuilt when the
/// number or kind of lights changes; individual lights are updated in place so
/// dragging a position slider stays cheap.
#[derive(Resource, Default)]
struct LightmapPreviewSceneLights {
    entities: Vec<Entity>,
    kinds: Vec<LightmapLightKind>,
    /// Emissive marker material per light, recolored when the light color
    /// changes. Kept here so the marker can follow the color without rebuilding
    /// the entity.
    marker_materials: Vec<Handle<StandardMaterial>>,
    /// One sphere mesh shared by every marker.
    marker_mesh: Option<Handle<Mesh>>,
}

/// Cached cubemaps and the environment path they were generated from.
#[derive(Resource, Default)]
pub(crate) struct LightmapPreviewEnvironment {
    path: Option<PathBuf>,
    specular: Option<Handle<Image>>,
    diffuse: Option<Handle<Image>>,
    applied: bool,
    /// Solid-angle-weighted average radiance of the loaded HDR, used by
    /// [`crate::preview::apply_preview_style`] to derive the same automatic
    /// fill as the bake.
    pub(crate) average_radiance: f32,
    /// In-flight HDR decode and cubemap build. Keeping it here means the
    /// render thread never blocks on a multi-second file decode.
    task: Option<Task<Result<BuiltEnvironment, String>>>,
}

/// The GPU-ready result of one background environment build.
struct BuiltEnvironment {
    specular: Image,
    diffuse: Image,
    average_radiance: f32,
}

fn spawn_preview_sun(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: 0.0,
            shadow_maps_enabled: false,
            ..default()
        },
        CascadeShadowConfigBuilder {
            num_cascades: 1,
            maximum_distance: 12.0,
            ..default()
        }
        .build(),
        Transform::default(),
        LightmapPreviewSun,
        Name::new("Lightmap preview sun"),
    ));
}

/// While the preview is active, show the imported bake mesh and stop the
/// turntable so the preview matches the fixed bake orientation.
fn sync_preview_mode(
    runtime: Res<LightmapRuntime>,
    import: Res<MeshImport>,
    mut state: ResMut<EditorState>,
    mut previous: Local<Option<(PreviewShape, bool)>>,
) {
    let ready = runtime.preview_enabled && import.ready && import.uv_triangles > 0;
    if ready {
        if previous.is_none() {
            *previous = Some((state.preview_shape, state.auto_rotate));
            state.viewport_tab = ViewportTab::ThreeD;
            state.auto_rotate = false;
        }
        // The preview is specifically about the mesh that will be baked.
        if state.preview_shape != PreviewShape::Imported {
            state.preview_shape = PreviewShape::Imported;
        }
    } else if let Some((shape, auto_rotate)) = previous.take() {
        if state.preview_shape == PreviewShape::Imported {
            state.preview_shape = shape;
        }
        if !state.auto_rotate {
            state.auto_rotate = auto_rotate;
        }
    }
}

/// Swap the normal-map preview material for the neutral lightmap material.
fn sync_preview_materials(
    runtime: Res<LightmapRuntime>,
    handles: Res<PreviewHandles>,
    mut materials: Query<&mut MeshMaterial3d<StandardMaterial>>,
) {
    let wanted = if runtime.preview_enabled {
        &handles.lightmap_material
    } else {
        &handles.material
    };
    for mut material in &mut materials {
        let current = material.0.clone();
        if current == *wanted {
            continue;
        }
        if current == handles.material || current == handles.lightmap_material {
            material.0 = wanted.clone();
        }
    }
}

/// Keep the preview sun in sync with the lightmap sun settings.
fn sync_preview_light(
    runtime: Res<LightmapRuntime>,
    mut sun: Query<(&mut DirectionalLight, &mut Transform), With<LightmapPreviewSun>>,
) {
    let active = runtime.preview_enabled
        && runtime.settings.sun_enabled
        && runtime.settings.sun_intensity > 0.0;
    for (mut light, mut transform) in &mut sun {
        if light.shadow_maps_enabled != active {
            light.shadow_maps_enabled = active;
        }
        if !active {
            if light.illuminance != 0.0 {
                light.illuminance = 0.0;
            }
            continue;
        }
        let direction = runtime.settings.sun_direction();
        if direction.length_squared() <= 1e-8 {
            light.illuminance = 0.0;
            continue;
        }
        // The light travels along its local -Z axis. Rotating +Z to the
        // direction of the sun makes -Z point from the sun toward the scene.
        let rotation = Quat::from_rotation_arc(Vec3::Z, direction);
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
        let color = runtime.settings.sun_color;
        let color = Color::linear_rgb(color[0].max(0.0), color[1].max(0.0), color[2].max(0.0));
        if light.color != color {
            light.color = color;
        }
        let illuminance = runtime.settings.sun_intensity.max(0.0) * SUN_LUX;
        if light.illuminance != illuminance {
            light.illuminance = illuminance;
        }
    }
}

/// Mirror the editable scene lights into the 3D preview.
///
/// The bake reads the lights straight from `LightmapSettings`, so the preview
/// is only a visualization: hiding a light here (`visible_in_view = false`)
/// removes its entity and marker from the viewport but leaves it in the bake.
fn sync_preview_scene_lights(
    mut commands: Commands,
    runtime: Res<LightmapRuntime>,
    mut preview: ResMut<LightmapPreviewSceneLights>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut lights: Query<(
        &mut Transform,
        &mut Visibility,
        Option<&mut PointLight>,
        Option<&mut SpotLight>,
        Option<&mut RectLight>,
    )>,
) {
    let scene_lights = &runtime.settings.lights;
    if !runtime.preview_enabled {
        if preview.entities.is_empty() {
            return;
        }
        for entity in preview.entities.drain(..) {
            commands.entity(entity).despawn();
        }
        preview.kinds.clear();
        for handle in preview.marker_materials.drain(..) {
            materials.remove(&handle);
        }
        return;
    }

    let rebuild = preview.entities.len() != scene_lights.len()
        || preview
            .kinds
            .iter()
            .zip(scene_lights)
            .any(|(kind, light)| *kind != light.kind);
    if rebuild {
        for entity in preview.entities.drain(..) {
            commands.entity(entity).despawn();
        }
        preview.kinds.clear();
        for handle in preview.marker_materials.drain(..) {
            materials.remove(&handle);
        }
        let marker_mesh = preview
            .marker_mesh
            .get_or_insert_with(|| meshes.add(Sphere::new(SCENE_LIGHT_MARKER_RADIUS)))
            .clone();
        for light in scene_lights {
            let (entity, marker) =
                spawn_preview_scene_light(&mut commands, &mut materials, &marker_mesh, light);
            preview.entities.push(entity);
            preview.kinds.push(light.kind);
            preview.marker_materials.push(marker);
        }
        return;
    }

    for ((entity, light), marker_material) in preview
        .entities
        .iter()
        .zip(scene_lights)
        .zip(&preview.marker_materials)
    {
        let Ok((mut transform, mut visibility, point, spot, area)) = lights.get_mut(*entity) else {
            continue;
        };
        let wanted_visibility = if light.visible_in_view {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted_visibility {
            *visibility = wanted_visibility;
        }

        let position = Vec3::from_array(light.position);
        if transform.translation != position {
            transform.translation = position;
        }
        // Spot and rect lights emit along their local -Z axis.
        let to_target = Vec3::from_array(light.target) - position;
        let rotation = if to_target.length_squared() > 1e-8 {
            Quat::from_rotation_arc(Vec3::Z, -to_target.normalize())
        } else {
            Quat::IDENTITY
        };
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }

        let color = Color::linear_rgb(
            light.color[0].max(0.0),
            light.color[1].max(0.0),
            light.color[2].max(0.0),
        );
        if let Some(mut material) = materials.get_mut(marker_material) {
            if material.base_color != color {
                material.base_color = color;
                material.emissive = color.into();
            }
        }

        let range = if light.range > 0.0 {
            light.range
        } else {
            100.0
        };
        let intensity = light.intensity.max(0.0) * SCENE_LIGHT_LUMENS;
        if let Some(mut point) = point {
            if point.color != color {
                point.color = color;
            }
            if point.intensity != intensity {
                point.intensity = intensity;
            }
            if point.range != range {
                point.range = range;
            }
        }
        if let Some(mut spot) = spot {
            if spot.color != color {
                spot.color = color;
            }
            if spot.intensity != intensity {
                spot.intensity = intensity;
            }
            if spot.range != range {
                spot.range = range;
            }
            let outer = (light.spot_angle_deg * 0.5)
                .to_radians()
                .clamp(0.01, std::f32::consts::FRAC_PI_2 - 0.01);
            let inner = outer * (1.0 - light.spot_blend.clamp(0.0, 1.0));
            if spot.outer_angle != outer {
                spot.outer_angle = outer;
            }
            if spot.inner_angle != inner {
                spot.inner_angle = inner;
            }
        }
        if let Some(mut area) = area {
            if area.color != color {
                area.color = color;
            }
            // Our area intensity is emitted radiance; Bevy wants total lumens.
            let area_intensity =
                intensity * (light.area_size[0].abs() * light.area_size[1].abs()).max(0.0);
            if area.intensity != area_intensity {
                area.intensity = area_intensity;
            }
            if area.range != range {
                area.range = range;
            }
            let width = light.area_size[0].abs().max(1e-4);
            let height = light.area_size[1].abs().max(1e-4);
            if area.width != width {
                area.width = width;
            }
            if area.height != height {
                area.height = height;
            }
        }
    }
}

fn spawn_preview_scene_light(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    marker_mesh: &Handle<Mesh>,
    light: &LightmapSceneLight,
) -> (Entity, Handle<StandardMaterial>) {
    let color = Color::linear_rgb(
        light.color[0].max(0.0),
        light.color[1].max(0.0),
        light.color[2].max(0.0),
    );
    let material = materials.add(StandardMaterial {
        base_color: color,
        emissive: color.into(),
        unlit: true,
        ..default()
    });
    let marker = marker_mesh.clone();

    let mut entity = commands.spawn((
        Transform::from_translation(Vec3::from_array(light.position)),
        Visibility::Inherited,
        Name::new(format!("Bake light: {}", light.name)),
    ));
    let range = if light.range > 0.0 {
        light.range
    } else {
        100.0
    };
    let intensity = light.intensity.max(0.0) * SCENE_LIGHT_LUMENS;
    match light.kind {
        LightmapLightKind::Point => {
            entity.insert(PointLight {
                color,
                intensity,
                range,
                shadow_maps_enabled: true,
                ..default()
            });
        }
        LightmapLightKind::Spot => {
            let outer = (light.spot_angle_deg * 0.5)
                .to_radians()
                .clamp(0.01, std::f32::consts::FRAC_PI_2 - 0.01);
            entity.insert(SpotLight {
                color,
                intensity,
                range,
                outer_angle: outer,
                inner_angle: outer * (1.0 - light.spot_blend.clamp(0.0, 1.0)),
                shadow_maps_enabled: true,
                ..default()
            });
        }
        LightmapLightKind::Area => {
            entity.insert(RectLight {
                color,
                intensity: intensity
                    * (light.area_size[0].abs() * light.area_size[1].abs()).max(0.0),
                range,
                width: light.area_size[0].abs().max(1e-4),
                height: light.area_size[1].abs().max(1e-4),
            });
        }
    }
    entity.with_children(|parent| {
        parent.spawn((Mesh3d(marker), MeshMaterial3d(material.clone())));
    });
    (entity.id(), material)
}

/// Apply the HDRI as a skybox and as image-based lighting, and mirror the
/// lightmap preview exposure on the camera.
#[allow(clippy::type_complexity)]
fn sync_preview_lighting(
    mut commands: Commands,
    mut runtime: ResMut<LightmapRuntime>,
    mut preview: ResMut<LightmapPreviewEnvironment>,
    mut images: ResMut<Assets<Image>>,
    mut camera: Query<
        (
            Entity,
            &mut Exposure,
            Option<&mut Skybox>,
            Option<&mut EnvironmentMapLight>,
        ),
        With<PreviewCamera>,
    >,
) {
    let Ok((entity, mut exposure, mut skybox, mut environment)) = camera.single_mut() else {
        return;
    };

    if !runtime.preview_enabled {
        runtime.preview_loading = false;
        if preview.applied {
            if let Some(skybox) = skybox.as_mut() {
                skybox.image = None;
                skybox.brightness = 0.0;
            }
            commands.entity(entity).remove::<EnvironmentMapLight>();
            preview.applied = false;
        }
        if runtime.preview_error.is_some() {
            runtime.preview_error = None;
        }
        let ev100 = Exposure::default().ev100;
        if exposure.ev100 != ev100 {
            exposure.ev100 = ev100;
        }
        return;
    }

    // The preview camera is an SDR camera (runtime HDR markers render black in
    // Bevy 0.19), so lift the exposure value to keep the sun and HDRI from
    // clipping to white. The Exposure slider still adds artist stops.
    let ev100 = Exposure::default().ev100 + 2.5 - runtime.settings.exposure;
    if exposure.ev100 != ev100 {
        exposure.ev100 = ev100;
    }

    let path = runtime.environment_path.clone();
    if preview.path != path {
        // A different file invalidates the previous build and its images.
        preview.task = None;
        if preview.applied {
            if let Some(skybox) = skybox.as_mut() {
                skybox.image = None;
                skybox.brightness = 0.0;
            }
            commands.entity(entity).remove::<EnvironmentMapLight>();
            preview.applied = false;
        }
        if let Some(handle) = preview.specular.take() {
            images.remove(&handle);
        }
        if let Some(handle) = preview.diffuse.take() {
            images.remove(&handle);
        }
        runtime.preview_error = None;
        runtime.preview_loading = false;
        preview.path = path.clone();
        preview.average_radiance = 0.0;
    }

    // Decode and convert off the render thread. Starting the task here means
    // the first frame after choosing an HDR paints immediately, and the
    // spinner in the panel reports the wait.
    if let Some(path) = &path {
        if preview.task.is_none() && preview.specular.is_none() && runtime.preview_error.is_none() {
            preview.task = Some(start_environment_build(path.clone()));
            runtime.preview_loading = true;
        }
    }

    if let Some(mut task) = preview.task.take() {
        match block_on(poll_once(&mut task)) {
            Some(Ok(built)) => {
                preview.specular = Some(images.add(built.specular));
                preview.diffuse = Some(images.add(built.diffuse));
                preview.average_radiance = built.average_radiance;
                runtime.preview_error = None;
                runtime.preview_loading = false;
            }
            Some(Err(error)) => {
                runtime.preview_error = Some(error);
                runtime.preview_loading = false;
            }
            None => {
                preview.task = Some(task);
                runtime.preview_loading = true;
            }
        }
    }

    if let (Some(specular), Some(diffuse)) = (preview.specular.clone(), preview.diffuse.clone()) {
        let strength = runtime.settings.environment_strength.max(0.0) * ENVIRONMENT_NITS;
        if let Some(skybox) = skybox.as_mut() {
            if skybox.image.as_ref() != Some(&specular) {
                skybox.image = Some(specular.clone());
            }
            if skybox.brightness != strength {
                skybox.brightness = strength;
            }
        } else {
            commands.entity(entity).insert(Skybox {
                image: Some(specular.clone()),
                brightness: strength,
                ..default()
            });
        }
        if let Some(environment) = environment.as_mut() {
            if environment.diffuse_map != diffuse {
                environment.diffuse_map = diffuse;
            }
            if environment.specular_map != specular {
                environment.specular_map = specular;
            }
            if environment.intensity != strength {
                environment.intensity = strength;
            }
        } else {
            commands.entity(entity).insert(EnvironmentMapLight {
                diffuse_map: diffuse,
                specular_map: specular,
                intensity: strength,
                ..default()
            });
        }
        preview.applied = true;
    } else if preview.applied {
        if let Some(skybox) = skybox.as_mut() {
            skybox.image = None;
            skybox.brightness = 0.0;
        }
        commands.entity(entity).remove::<EnvironmentMapLight>();
        preview.applied = false;
    }
}

fn build_environment_images(environment: &EnvironmentMap) -> (Image, Image) {
    (
        cube_image(environment, CUBE_FACE_SIZE, false),
        cube_image(environment, DIFFUSE_FACE_SIZE, true),
    )
}

/// Decode the HDR and build both cubemaps on the compute pool.
///
/// This is the expensive part of enabling the preview: a 4K/8K equirectangular
/// map is hundreds of MiB once expanded to linear RGB. Running it on the
/// render thread was what made the editor appear to freeze.
fn start_environment_build(path: PathBuf) -> Task<Result<BuiltEnvironment, String>> {
    AsyncComputeTaskPool::get().spawn(async move {
        let environment =
            crate::lightmap::load_environment_limited(&path, Some(PREVIEW_ENVIRONMENT_MAX_SIDE))?;
        let average_radiance = environment.average_radiance();
        let (specular, diffuse) = build_environment_images(&environment);
        Ok(BuiltEnvironment {
            specular,
            diffuse,
            average_radiance,
        })
    })
}

fn cube_image(environment: &EnvironmentMap, face_size: u32, diffuse: bool) -> Image {
    let face_size = face_size.max(1);
    let mut data = Vec::with_capacity((face_size * face_size * 6 * 8) as usize);
    for face in 0..6 {
        for y in 0..face_size {
            for x in 0..face_size {
                let s = (x as f32 + 0.5) / face_size as f32;
                let t = (y as f32 + 0.5) / face_size as f32;
                let direction = face_direction(face, s, t);
                let radiance = if diffuse {
                    diffuse_radiance(environment, direction)
                } else {
                    environment.radiance(direction)
                };
                push_rgba16(&mut data, radiance);
            }
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: face_size,
            height: face_size,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba16Float,
        RenderAssetUsages::default(),
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        array_layer_count: Some(6),
        ..default()
    });
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        address_mode_w: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

/// Direction in the equirectangular environment for a cubemap face texel.
///
/// The face order and the `-z` conversion match wgpu's left-handed cubemaps,
/// which Bevy's skybox and environment-map shaders also use.
fn face_direction(face: usize, s: f32, t: f32) -> Vec3 {
    let sc = 2.0 * s - 1.0;
    let tc = 2.0 * t - 1.0;
    let cube = match face {
        0 => Vec3::new(1.0, -tc, -sc),
        1 => Vec3::new(-1.0, -tc, sc),
        2 => Vec3::new(sc, 1.0, tc),
        3 => Vec3::new(sc, -1.0, -tc),
        4 => Vec3::new(sc, -tc, 1.0),
        5 => Vec3::new(-sc, -tc, -1.0),
        _ => Vec3::Z,
    };
    Vec3::new(cube.x, cube.y, -cube.z).normalize_or_zero()
}

/// Cosine-weighted average radiance around `normal`, i.e. irradiance / PI.
fn diffuse_radiance(environment: &EnvironmentMap, normal: Vec3) -> Vec3 {
    let normal = normal.normalize_or_zero();
    if normal.length_squared() <= 1e-8 {
        return Vec3::ZERO;
    }
    let tangent = if normal.y.abs() < 0.999 {
        Vec3::Y.cross(normal).normalize_or_zero()
    } else {
        Vec3::X
    };
    let bitangent = normal.cross(tangent);
    let mut sum = Vec3::ZERO;
    for index in 0..DIFFUSE_SAMPLES {
        let u1 = (index as f32 + 0.5) / DIFFUSE_SAMPLES as f32;
        let u2 = (index as f32 * 0.618_034 + 0.37).fract();
        let radius = u1.sqrt();
        let phi = u2 * std::f32::consts::TAU;
        let local = Vec3::new(
            radius * phi.cos(),
            radius * phi.sin(),
            (1.0 - u1).max(0.0).sqrt(),
        );
        let direction = tangent * local.x + bitangent * local.y + normal * local.z;
        sum += environment.radiance(direction);
    }
    sum / DIFFUSE_SAMPLES as f32
}

fn push_rgba16(data: &mut Vec<u8>, value: Vec3) {
    for channel in [value.x, value.y, value.z, 1.0] {
        let encoded = f16::from_f32(channel.clamp(0.0, 65_000.0));
        data.extend_from_slice(&encoded.to_bits().to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_face_centers_match_the_equirectangular_directions() {
        let center = |face| face_direction(face, 0.5, 0.5);
        assert!((center(0) - Vec3::X).length() < 1e-5);
        assert!((center(1) + Vec3::X).length() < 1e-5);
        assert!((center(2) - Vec3::Y).length() < 1e-5);
        assert!((center(3) + Vec3::Y).length() < 1e-5);
        // Bevy negates z when sampling left-handed cubemaps.
        assert!((center(4) + Vec3::Z).length() < 1e-5);
        assert!((center(5) - Vec3::Z).length() < 1e-5);
    }

    #[test]
    fn cube_texels_are_unit_directions() {
        for face in 0..6 {
            for (s, t) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.2)] {
                let direction = face_direction(face, s, t);
                assert!((direction.length() - 1.0).abs() < 1e-5);
            }
        }
    }
}
