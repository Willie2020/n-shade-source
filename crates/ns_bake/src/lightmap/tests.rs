//! Lightmap tests.

use super::trace::{SceneLight, LIGHT_KIND_AREA};
use super::*;

use std::sync::atomic::AtomicU32;
fn plane() -> BakeMesh {
    BakeMesh {
        positions: vec![
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ],
        normals: vec![[0.0, 0.0, 1.0]; 4],
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    }
}

fn settings() -> LightmapSettings {
    LightmapSettings {
        resolution: 24,
        samples: 16,
        bounces: 0,
        padding: 0,
        denoiser: LightmapDenoiser::None,
        sun_enabled: true,
        sun_azimuth_deg: 90.0,
        sun_elevation_deg: 0.0,
        sun_size_deg: 0.0,
        sun_intensity: 1.0,
        ambient_intensity: 0.0,
        ..Default::default()
    }
}

fn run(
    mesh: &BakeMesh,
    env: Option<&EnvironmentMap>,
    settings: &LightmapSettings,
) -> LightmapOutput {
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    bake_lightmap(mesh, env, settings, &progress, &cancel).expect("bake succeeds")
}

fn center_pixel(output: &LightmapOutput) -> [u8; 4] {
    let index = ((output.height / 2) * output.width + output.width / 2) as usize * 4;
    [
        output.pixels[index],
        output.pixels[index + 1],
        output.pixels[index + 2],
        output.pixels[index + 3],
    ]
}

/// A plane plus a small ceiling above its centre. The blocker uses tile 1002 so
/// it casts shadows and AO but is not rasterized into the baked tile.
fn plane_with_blocker() -> BakeMesh {
    let mut mesh = plane();
    let base = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(&[
        [-0.75, -0.75, 0.8],
        [0.75, -0.75, 0.8],
        [0.75, 0.75, 0.8],
        [-0.75, 0.75, 0.8],
    ]);
    mesh.normals.extend_from_slice(&[[0.0, 0.0, 1.0]; 4]);
    mesh.uvs
        .extend_from_slice(&[[1.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0]]);
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    mesh
}

#[test]
fn point_light_lights_the_surface() {
    let mut settings = settings();
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_intensity = 0.0;
    settings.denoiser = LightmapDenoiser::None;
    settings.lights = vec![LightmapSceneLight {
        // Normalized mesh space: the plane spans two raw units, so this lands
        // two units above its centre.
        position: [0.0, 0.0, 1.0],
        intensity: 2.0,
        range: 0.0,
        ..LightmapSceneLight::new(LightmapLightKind::Point)
    }];

    let output = run(&plane(), None, &settings);
    let pixel = center_pixel(&output);
    // 2.0 / 2² = 0.5 linear radiance, comfortably above the sRGB midpoint.
    assert!(pixel[0] > 100, "{pixel:?}");
    assert_eq!(pixel[3], 255);
    assert!(output.stats.rays > 0);
}

#[test]
fn disabled_scene_light_does_not_contribute() {
    let mut settings = settings();
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_intensity = 0.0;
    settings.denoiser = LightmapDenoiser::None;
    settings.lights = vec![LightmapSceneLight {
        enabled: false,
        position: [0.0, 0.0, 1.0],
        intensity: 4.0,
        ..LightmapSceneLight::new(LightmapLightKind::Point)
    }];

    let pixel = center_pixel(&run(&plane(), None, &settings));
    assert_eq!(pixel, [0, 0, 0, 255], "{pixel:?}");
}

#[test]
fn spot_light_only_lights_inside_its_cone() {
    let mut settings = settings();
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_intensity = 0.0;
    settings.denoiser = LightmapDenoiser::None;
    settings.lights = vec![LightmapSceneLight {
        position: [0.0, 0.0, 1.0],
        target: [0.0, 0.0, 0.0],
        intensity: 4.0,
        spot_angle_deg: 20.0,
        spot_blend: 0.0,
        range: 0.0,
        ..LightmapSceneLight::new(LightmapLightKind::Spot)
    }];

    let aimed = center_pixel(&run(&plane(), None, &settings));
    assert!(aimed[0] > 200, "{aimed:?}");

    // Aiming the narrow cone at a far corner puts the centre outside it.
    settings.lights[0].target = [0.9, 0.9, 0.0];
    let away = center_pixel(&run(&plane(), None, &settings));
    assert_eq!(away[0], 0, "{away:?}");
}

#[test]
fn area_light_lights_the_surface() {
    let mut settings = settings();
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_intensity = 0.0;
    settings.samples = 64;
    settings.denoiser = LightmapDenoiser::None;
    settings.lights = vec![LightmapSceneLight {
        position: [0.0, 0.0, 1.0],
        target: [0.0, 0.0, 0.0],
        area_size: [1.0, 1.0],
        intensity: 1.0,
        range: 0.0,
        ..LightmapSceneLight::new(LightmapLightKind::Area)
    }];

    let output = run(&plane(), None, &settings);
    let index = ((output.height / 2) * output.width + output.width / 2) as usize * 3;
    // A 2 x 2 raw-unit emitter two units above the plane gives a closed-form
    // irradiance of 0.752 for L = 1. Before the emitter cosine it read 0.805.
    let radiance = output.hdr[index];
    assert!(
        (radiance - 0.752).abs() < 0.03,
        "center radiance {radiance}, expected 0.752"
    );
    let pixel = center_pixel(&output);
    assert!(pixel[0] > 200, "{pixel:?}");
}

#[test]
fn area_light_weight_matches_closed_form_irradiance() {
    let (a, b, h) = (1.0f32, 1.0f32, 2.0f32);
    let light = SceneLight {
        kind: LIGHT_KIND_AREA,
        position: glam::Vec3::new(0.0, 0.0, h),
        direction: -glam::Vec3::Z,
        cos_outer: 0.0,
        cos_inner: 0.0,
        radiance: glam::Vec3::ONE,
        half_size: [a, b],
        area: 1.0, // the per-sample weight below is multiplied by dA explicitly
        two_sided: false,
        range: 0.0,
    };
    let n = 200;
    let d_area = (2.0 * a / n as f32) * (2.0 * b / n as f32);
    let mut sum = 0.0f32;
    for i in 0..n {
        for j in 0..n {
            let x = -a + (i as f32 + 0.5) * 2.0 * a / n as f32;
            let y = -b + (j as f32 + 0.5) * 2.0 * b / n as f32;
            if let Some((weight, _, _)) = light.direct_weight(
                glam::Vec3::ZERO,
                glam::Vec3::Z,
                glam::Vec3::new(x, y, h),
                1e-4,
                true,
            ) {
                sum += weight * d_area;
            }
        }
    }
    let (x, y) = (a / h, b / h);
    let closed = 2.0
        * ((x / (1.0 + x * x).sqrt()) * (y / (1.0 + x * x).sqrt()).atan()
            + (y / (1.0 + y * y).sqrt()) * (x / (1.0 + y * y).sqrt()).atan());
    assert!((sum - closed).abs() < 0.01, "sum {sum} vs closed {closed}");
}

#[test]
fn area_light_emitter_cosine_darkens_grazing_angles() {
    let light = SceneLight {
        kind: LIGHT_KIND_AREA,
        position: glam::Vec3::new(0.0, 0.0, 2.0),
        direction: -glam::Vec3::Z,
        cos_outer: 0.0,
        cos_inner: 0.0,
        radiance: glam::Vec3::ONE,
        half_size: [1.0, 1.0],
        area: 4.0,
        two_sided: false,
        range: 0.0,
    };
    let surface = glam::Vec3::new(3.0, 0.0, 0.0);
    let emitter = glam::Vec3::new(0.0, 0.0, 2.0);
    let with = light
        .direct_weight(surface, glam::Vec3::Z, emitter, 1e-4, true)
        .unwrap()
        .0;
    let without = light
        .direct_weight(surface, glam::Vec3::Z, emitter, 1e-4, false)
        .unwrap()
        .0;
    assert!(
        with < without * 0.6,
        "grazing emitter cosine must dim the light: {with} vs {without}"
    );
}

#[test]
fn indirect_mode_includes_scene_lights() {
    let mut settings = settings();
    settings.lighting = LightmapLightingMode::Indirect;
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_intensity = 0.0;
    settings.samples = 32;
    settings.lights = vec![LightmapSceneLight {
        position: [0.0, 0.0, 1.0],
        intensity: 4.0,
        ..LightmapSceneLight::new(LightmapLightKind::Point)
    }];

    let with_light = run(&plane(), None, &settings);
    settings.lights.clear();
    let without_light = run(&plane(), None, &settings);
    assert!(
        with_light.hdr.iter().sum::<f32>() > without_light.hdr.iter().sum::<f32>(),
        "Indirect bakes must still carry static fill lights"
    );
}

#[test]
fn ambient_occlusion_ignores_scene_lights() {
    let mut settings = settings();
    settings.lighting = LightmapLightingMode::AmbientOcclusion;
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_intensity = 0.0;
    settings.lights = vec![LightmapSceneLight {
        position: [0.0, 0.0, 1.0],
        intensity: 4.0,
        ..LightmapSceneLight::new(LightmapLightKind::Point)
    }];

    let with_light = run(&plane(), None, &settings);
    settings.lights.clear();
    let without_light = run(&plane(), None, &settings);
    assert_eq!(
        with_light.pixels, without_light.pixels,
        "AO is visibility only and must ignore scene lights"
    );
}

#[test]
fn scene_lights_are_scaled_from_normalized_space() {
    let mut settings = settings();
    settings.lights = vec![LightmapSceneLight {
        position: [0.0, 0.0, 1.0],
        target: [0.0, 0.0, 0.0],
        range: 2.0,
        area_size: [0.5, 0.25],
        ..LightmapSceneLight::new(LightmapLightKind::Area)
    }];
    let lights = prepare_scene_lights(&settings, 2.0, glam::Vec3::new(1.0, 2.0, 3.0));
    assert_eq!(lights.len(), 1);
    // The normalized centre maps through `p * extent + center`.
    assert!((lights[0].position - glam::Vec3::new(1.0, 2.0, 5.0)).length() < 1e-6);
    // Sizes and range scale with the extent too, so a rig is unit-independent.
    assert!((lights[0].half_size[0] - 0.5).abs() < 1e-6);
    assert!((lights[0].half_size[1] - 0.25).abs() < 1e-6);
    assert!((lights[0].range - 4.0).abs() < 1e-6);
}

#[test]
fn sun_lights_a_white_surface() {
    let output = run(&plane(), None, &settings());
    let pixel = center_pixel(&output);
    assert_eq!(pixel[3], 255);
    assert!(pixel[0] >= 250, "{pixel:?}");
    assert!(pixel[1] >= 250, "{pixel:?}");
    assert!(pixel[2] >= 250, "{pixel:?}");
    assert!(output.stats.rays > 0);
}

#[test]
fn occluder_casts_a_shadow() {
    let mesh = plane_with_blocker();

    let output = run(&mesh, None, &settings());
    let center = center_pixel(&output);
    assert!(center[0] < 24, "shadow should be dark: {center:?}");
    let corner = output.pixels[(2 * output.width as usize + 2) * 4];
    assert!(corner > 200, "unshadowed corner should be lit: {corner}");
    assert!(output.stats.hits > 0);
}

#[test]
fn direct_mode_ignores_the_environment() {
    let environment = EnvironmentMap::new(8, 4, vec![[4.0, 4.0, 4.0]; 32]).unwrap();
    let mut direct = settings();
    direct.lighting = LightmapLightingMode::Direct;
    direct.samples = 64;
    direct.ambient_intensity = 1.0;
    let without = run(&plane(), None, &direct);
    let with = run(&plane(), Some(&environment), &direct);
    assert_eq!(
        without.pixels, with.pixels,
        "direct mode must not sample the environment or ambient fill"
    );
}

#[test]
fn indirect_mode_ignores_the_sun() {
    let mut indirect = settings();
    indirect.lighting = LightmapLightingMode::Indirect;
    indirect.samples = 64;
    // Strong sun + weak ambient: leaking either sun or fill would change the
    // result visibly without clipping to white.
    indirect.sun_enabled = true;
    indirect.sun_intensity = 8.0;
    indirect.ambient_color = [1.0, 1.0, 1.0];
    indirect.ambient_intensity = 0.3;
    let with_sun = run(&plane(), None, &indirect);
    indirect.sun_enabled = false;
    let without_sun = run(&plane(), None, &indirect);
    assert_eq!(
        with_sun.pixels, without_sun.pixels,
        "indirect mode must not add direct sunlight"
    );
    let pixel = center_pixel(&with_sun);
    assert!(pixel[0] > 60 && pixel[0] < 200, "{pixel:?}");
}

#[test]
fn ambient_occlusion_darkens_under_an_occluder() {
    let mut settings = settings();
    settings.lighting = LightmapLightingMode::AmbientOcclusion;
    settings.samples = 256;
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_intensity = 0.0;
    settings.bounces = 0;

    let output = run(&plane_with_blocker(), None, &settings);
    let center = center_pixel(&output);
    let corner = output.pixels[(2 * output.width as usize + 2) * 4];

    assert_eq!(center[0], center[1]);
    assert_eq!(center[1], center[2]);
    assert!(
        center[0] < 235,
        "the blocker should darken the centre: {center:?}"
    );
    assert!(corner > 235, "open sky should stay bright: {corner}");
    assert!(
        corner > center[0] + 5,
        "the open corner must be lighter than the occluded centre: {corner} vs {}",
        center[0]
    );
    assert!(output.stats.hits > 0);
    assert_eq!(
        output.stats.lighting,
        LightmapLightingMode::AmbientOcclusion
    );
}

#[test]
fn uniform_hdr_environment_has_unit_energy() {
    let environment = EnvironmentMap::new(8, 4, vec![[1.0, 1.0, 1.0]; 32]).unwrap();
    let mut settings = settings();
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.samples = 256;
    settings.bounces = 1;
    let output = run(&plane(), Some(&environment), &settings);

    let pixel = center_pixel(&output);
    assert_eq!(pixel[3], 255);
    for channel in &pixel[..3] {
        assert!(
            (235..=255).contains(channel),
            "environment should converge near 1.0: {pixel:?}"
        );
    }
    assert!(output.stats.rays > 0);
}

#[test]
fn environment_average_radiance_is_solid_angle_weighted() {
    let uniform = EnvironmentMap::new(4, 2, vec![[2.0, 2.0, 2.0]; 8]).unwrap();
    assert!((uniform.average_radiance() - 2.0).abs() < 1e-3);

    // The upper hemisphere is black and the lower hemisphere is twice as
    // bright, so the average is half of the bright value.
    let mut pixels = vec![[0.0f32; 3]; 32];
    for row in 2..4 {
        for column in 0..8 {
            pixels[row * 8 + column] = [2.0, 2.0, 2.0];
        }
    }
    let split = EnvironmentMap::new(8, 4, pixels).unwrap();
    assert!((split.average_radiance() - 1.0).abs() < 1e-3);
}

#[test]
fn environment_fill_lifts_surfaces_facing_away_from_the_bright_side() {
    // Only longitudes with z < 0 are bright, so a +Z-facing plane (whose
    // cosine hemisphere is entirely z >= 0) receives no environment radiance.
    let mut pixels = vec![[0.0f32; 3]; 32];
    for row in 0..4 {
        for column in 6..8 {
            pixels[row * 8 + column] = [2.0, 2.0, 2.0];
        }
    }
    let environment = EnvironmentMap::new(8, 4, pixels).unwrap();
    let mut settings = settings();
    // This test pins the v1 fill semantics; v2's bilinear lookup is covered
    // separately so the strict dark-back check stays meaningful.
    settings.pipeline = LightmapPipeline::Legacy;
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_color = [1.0, 1.0, 1.0];
    settings.ambient_intensity = 0.5;
    settings.samples = 64;
    settings.environment_fill = 0.0;

    let dark = center_pixel(&run(&plane(), Some(&environment), &settings));
    assert!(
        dark[..3].iter().all(|channel| *channel <= 8),
        "the HDR alone leaves the back dark: {dark:?}"
    );

    settings.environment_fill = 1.0;
    let lit = center_pixel(&run(&plane(), Some(&environment), &settings));
    assert!(
        lit[..3].iter().all(|channel| *channel > 100),
        "the automatic fill should keep the back lit: {lit:?}"
    );
}

#[test]
fn solid_ambient_is_an_unshadowed_fill() {
    let mut settings = settings();
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_color = [1.0, 1.0, 1.0];
    settings.ambient_intensity = 1.0;
    let output = run(&plane(), None, &settings);
    let pixel = center_pixel(&output);
    assert!(pixel[0] > 250, "{pixel:?}");
}

#[test]
fn denoise_and_padding_keep_covered_pixels_valid() {
    let mut settings = settings();
    settings.samples = 8;
    settings.denoiser = LightmapDenoiser::Builtin;
    settings.denoise_passes = 2;
    settings.padding = 3;
    let output = run(&plane(), None, &settings);
    assert_eq!(output.stats.denoiser, DenoiserUsed::Builtin);
    let pixel = center_pixel(&output);
    assert_eq!(pixel[3], 255);
    assert!(output.hdr.iter().all(|value| value.is_finite()));
}

#[test]
fn oidn_device_ids_match_the_c_api() {
    // OIDNDeviceType: CPU = 1, SYCL = 2, CUDA = 3. These were once shifted by
    // one, which made an Intel SYCL request run on the CPU silently.
    assert_eq!(OidnDevice::Cpu.preference(), 1);
    assert_eq!(OidnDevice::Sycl.preference(), 2);
    assert_eq!(OidnDevice::Cuda.preference(), 3);
    assert_eq!(DenoiserUsed::from_oidn_device(1), DenoiserUsed::OidnCpu);
    assert_eq!(DenoiserUsed::from_oidn_device(2), DenoiserUsed::OidnSycl);
    assert_eq!(DenoiserUsed::from_oidn_device(3), DenoiserUsed::OidnCuda);
}

#[test]
fn bright_hdr_radiance_is_not_clamped() {
    let environment = EnvironmentMap::new(1, 1, vec![[100.0, 100.0, 100.0]]).unwrap();
    let mut settings = settings();
    settings.sun_enabled = false;
    settings.sun_intensity = 0.0;
    settings.ambient_intensity = 0.0;
    settings.environment_fill = 0.0;
    settings.samples = 8;
    settings.bounces = 0;
    settings.denoiser = LightmapDenoiser::None;
    let output = run(&plane(), Some(&environment), &settings);
    let index = ((output.height / 2) * output.width + output.width / 2) as usize * 3;
    assert!(
        output.hdr[index] > 64.0,
        "HDR path radiance was clamped: {}",
        output.hdr[index]
    );
}

#[test]
fn lightmap_udim_tiles_are_selected() {
    let mut mesh = plane();
    for uv in &mut mesh.uvs {
        uv[0] += 1.0;
    }
    let mut settings = settings();
    settings.tile = 1001;
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    let error =
        bake_lightmap(&mesh, None, &settings, &progress, &cancel).expect_err("tile 1001 is empty");
    assert!(matches!(error, BakeError::Message(_)));

    settings.tile = 1002;
    let output = run(&mesh, None, &settings);
    assert_eq!(center_pixel(&output)[3], 255);
}

#[test]
fn overbright_texels_are_reported_as_clipped() {
    let mut settings = settings();
    settings.samples = 32;
    settings.sun_intensity = 4.0;
    let output = run(&plane(), None, &settings);
    assert!(output.stats.clipped > 0);
    assert!(output.stats.clipped <= output.stats.texels);

    settings.sun_intensity = 0.1;
    let output = run(&plane(), None, &settings);
    assert_eq!(output.stats.clipped, 0);
}

#[test]
fn cancellation_is_reported() {
    let mesh = plane();
    let settings = settings();
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(true);
    let result = bake_lightmap(&mesh, None, &settings, &progress, &cancel);
    assert!(matches!(result, Err(BakeError::Cancelled)));
}
