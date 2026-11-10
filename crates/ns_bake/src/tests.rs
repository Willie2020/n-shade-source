//! Focused tests for projection, bevel shading, UDIM selection and padding.

use std::sync::atomic::{AtomicBool, AtomicU32};

use crate::{
    bake, project_triplanar, BakeError, BakeMesh, BakeMode, BakeOutput, BakeSettings, BakeSpace,
    TriplanarSettings,
};

fn settings(mode: BakeMode) -> BakeSettings {
    BakeSettings {
        mode,
        resolution: 32,
        anti_alias: 1,
        padding: 0.0,
        ..Default::default()
    }
}

fn run(low: &BakeMesh, high: Option<BakeMesh>, settings: &BakeSettings) -> BakeOutput {
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    bake(low, high, settings, &progress, &cancel).expect("bake succeeds")
}

fn quad(z: f32, normal: [f32; 3]) -> BakeMesh {
    BakeMesh {
        positions: vec![
            [-1.0, -1.0, z],
            [1.0, -1.0, z],
            [1.0, 1.0, z],
            [-1.0, 1.0, z],
        ],
        normals: vec![normal; 4],
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    }
}

fn pixel(output: &BakeOutput, x: u32, y: u32) -> [u8; 4] {
    let index = ((y * output.width + x) * 4) as usize;
    [
        output.pixels[index],
        output.pixels[index + 1],
        output.pixels[index + 2],
        output.pixels[index + 3],
    ]
}

#[test]
fn triplanar_flat_source_keeps_the_surface_normal() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let settings = TriplanarSettings {
        resolution: 64,
        padding: 0,
        center: [0.0; 3],
        size: [1.0; 3],
        edge_feather: 0.0,
        ..Default::default()
    };
    let output = project_triplanar(&low, &settings, |_| [0.0, 0.0, 1.0]).expect("projection");
    let index = 32 * 64 + 32;
    let normal = output.normals[index];
    assert!(normal[0].abs() < 0.01, "{normal:?}");
    assert!(normal[1].abs() < 0.01, "{normal:?}");
    assert!(normal[2] > 0.99, "{normal:?}");
    assert!(output.coverage[index] > 0.99);
}

#[test]
fn triplanar_tilted_source_tilts_in_projection_space() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let settings = TriplanarSettings {
        resolution: 64,
        padding: 0,
        center: [0.0; 3],
        size: [1.0; 3],
        edge_feather: 0.0,
        ..Default::default()
    };
    let tilted = glam::Vec3::new(1.0, 0.0, 1.0).normalize();
    let output =
        project_triplanar(&low, &settings, move |_| tilted.to_array()).expect("projection");
    let normal = output.normals[32 * 64 + 32];
    assert!(normal[0] > 0.5, "{normal:?}");
    assert!(normal[1].abs() < 0.05, "{normal:?}");
    assert!(normal[2] > 0.5, "{normal:?}");
}

#[test]
fn triplanar_bounds_limit_the_projection_volume() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let settings = TriplanarSettings {
        resolution: 64,
        padding: 0,
        center: [0.0; 3],
        size: [0.2, 0.2, 0.2],
        edge_feather: 0.0,
        ..Default::default()
    };
    let output = project_triplanar(&low, &settings, |_| [0.0, 0.0, 1.0]).expect("projection");
    assert!(
        output.coverage[32 * 64 + 32] > 0.99,
        "cube centre is covered"
    );
    assert!(output.coverage[2 * 64 + 2] < 0.01, "UV corner is outside");
}

#[test]
fn high_to_low_transfers_flat_normals() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let high = quad(0.5, [0.0, 0.0, 1.0]);
    let mut settings = settings(BakeMode::HighToLow);
    settings.ray_distance_pct = 50.0;
    settings.cage_offset_pct = 0.0;
    let output = run(&low, Some(high), &settings);

    let center = pixel(&output, 16, 16);
    assert_eq!(center[3], 255);
    assert!((center[0] as i32 - 128).abs() <= 2, "{center:?}");
    assert!((center[1] as i32 - 128).abs() <= 2, "{center:?}");
    assert!(center[2] >= 250, "{center:?}");
    assert!(output.stats.hits > 0);
}

#[test]
fn tangent_space_encodes_tilted_normals() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let high = quad(0.5, [0.6, 0.0, 0.8]);
    let mut settings = settings(BakeMode::HighToLow);
    settings.ray_distance_pct = 50.0;
    settings.cage_offset_pct = 0.0;
    let opengl = run(&low, Some(high.clone()), &settings);
    let center = pixel(&opengl, 16, 16);
    assert!(center[0] > 180, "{center:?}");
    assert!(center[2] > 180, "{center:?}");

    settings.space = BakeSpace::TangentDirectX;
    let high_y = quad(0.5, [0.0, 0.6, 0.8]);
    let direct_x = run(&low, Some(high_y.clone()), &settings);
    let center = pixel(&direct_x, 16, 16);
    assert!(center[1] < 76, "{center:?}");

    settings.space = BakeSpace::TangentOpenGl;
    let opengl_y = run(&low, Some(high_y), &settings);
    let center = pixel(&opengl_y, 16, 16);
    assert!(center[1] > 180, "{center:?}");
}

#[test]
fn normal_intensity_scales_the_baked_slope() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let high = quad(0.5, [0.6, 0.0, 0.8]);
    let mut settings = settings(BakeMode::HighToLow);
    settings.ray_distance_pct = 50.0;
    settings.cage_offset_pct = 0.0;

    let base = pixel(&run(&low, Some(high.clone()), &settings), 16, 16);
    assert!(base[0] > 180, "{base:?}");

    settings.normal_intensity = 2.0;
    let strong = pixel(&run(&low, Some(high.clone()), &settings), 16, 16);
    assert!(strong[0] > base[0] + 15, "{strong:?} vs {base:?}");

    settings.normal_intensity = 0.0;
    let flat = pixel(&run(&low, Some(high.clone()), &settings), 16, 16);
    assert!((flat[0] as i32 - 128).abs() <= 2, "{flat:?}");
    assert!(flat[2] >= 250, "{flat:?}");

    // Object-space bakes keep their raw vectors; intensity is tangent-only.
    settings.space = BakeSpace::Object;
    let object = pixel(&run(&low, Some(high), &settings), 16, 16);
    assert!(object[0] > 180, "{object:?}");
}

#[test]
fn supersampling_produces_valid_pixels() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let high = quad(0.5, [0.0, 0.0, 1.0]);
    let mut settings = settings(BakeMode::HighToLow);
    settings.anti_alias = 2;
    settings.ray_distance_pct = 50.0;
    settings.cage_offset_pct = 0.0;
    let output = run(&low, Some(high), &settings);

    assert_eq!(output.stats.effective_aa, 2);
    let center = pixel(&output, 16, 16);
    assert_eq!(center[3], 255);
    assert!((center[0] as i32 - 128).abs() <= 2, "{center:?}");
    assert!(center[2] >= 250, "{center:?}");
}

#[test]
fn misses_fall_back_to_the_low_surface_normal() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let high = quad(10.0, [0.0, 0.0, 1.0]);
    let mut settings = settings(BakeMode::HighToLow);
    settings.ray_distance_pct = 1.0;
    let output = run(&low, Some(high), &settings);

    assert!(output.stats.misses > 0);
    let center = pixel(&output, 16, 16);
    assert!((center[0] as i32 - 128).abs() <= 2, "{center:?}");
    assert!(center[2] >= 250, "{center:?}");
}

#[test]
fn high_to_low_respects_bottom_left_uv_origin() {
    // The bakers use the mesh's native bottom-left UV convention: v = 0 is the
    // bottom edge of the texture. The high plane has a tilted normal only in
    // its top half, so a correctly oriented bake must show the tilted result
    // in the upper image rows.
    let low = BakeMesh {
        positions: vec![
            [-1.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, -1.0, 0.0],
            [-1.0, -1.0, 0.0],
        ],
        normals: vec![[0.0, 0.0, 1.0]; 4],
        uvs: vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    };
    let high = BakeMesh {
        positions: vec![
            [-1.0, 0.0, 0.5],
            [1.0, 0.0, 0.5],
            [1.0, 1.0, 0.5],
            [-1.0, 1.0, 0.5],
            [-1.0, -1.0, 0.5],
            [1.0, -1.0, 0.5],
            [1.0, 0.0, 0.5],
            [-1.0, 0.0, 0.5],
        ],
        normals: vec![
            [0.6, 0.0, 0.8],
            [0.6, 0.0, 0.8],
            [0.6, 0.0, 0.8],
            [0.6, 0.0, 0.8],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ],
        uvs: Vec::new(),
        indices: vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7],
        ..Default::default()
    };
    let mut settings = settings(BakeMode::HighToLow);
    settings.resolution = 32;
    settings.uv_bottom_left = true;
    settings.ray_distance_pct = 50.0;
    settings.cage_offset_pct = 0.0;
    let output = run(&low, Some(high), &settings);

    let top = pixel(&output, 16, 16);
    let bottom = pixel(&output, 16, 48);
    assert!(
        top[0] > 180,
        "top half should carry the tilted normal: {top:?}"
    );
    assert!(
        (bottom[0] as i32 - 128).abs() <= 3,
        "bottom half should stay flat: {bottom:?}"
    );
    assert!(bottom[2] >= 250, "{bottom:?}");
}

#[test]
fn bevel_shades_shared_edges_without_shading_triangulation_diagonals() {
    // Horizontal face (+Y) and vertical face (+Z) meeting along the X axis.
    // Each face is a quad split into two triangles; the diagonal must not show
    // up as a bevel.
    let low = BakeMesh {
        positions: vec![
            [-1.0, 0.0, 0.0],
            [-1.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
        ],
        normals: vec![
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ],
        uvs: vec![
            [0.0, 0.0],
            [0.0, 1.0],
            [0.5, 1.0],
            [0.5, 0.0],
            [0.5, 0.0],
            [0.5, 1.0],
            [1.0, 1.0],
            [1.0, 0.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7],
        ..Default::default()
    };
    let mut settings = settings(BakeMode::Bevel);
    settings.resolution = 64;
    settings.uv_bottom_left = true;
    settings.bevel_radius_pct = 2.0;
    settings.bevel_falloff = 1.0;
    let output = run(&low, None, &settings);

    let near_edge_a = pixel(&output, 16, 63);
    assert!(near_edge_a[1] > 170, "{near_edge_a:?}");
    assert!(near_edge_a[2] < 250, "{near_edge_a:?}");

    let near_edge_b = pixel(&output, 48, 63);
    assert!(near_edge_b[1] > 170, "{near_edge_b:?}");

    let far_from_edge = pixel(&output, 16, 50);
    assert!(
        (far_from_edge[1] as i32 - 128).abs() <= 4,
        "{far_from_edge:?}"
    );
    assert!(far_from_edge[2] >= 250, "{far_from_edge:?}");

    // The diagonal runs from (u,v) = (0,0) to (0.5,1). A point just beside it
    // must stay flat rather than receiving a fake bevel.
    let diagonal = pixel(&output, 8, 17);
    assert!((diagonal[1] as i32 - 128).abs() <= 4, "{diagonal:?}");
}

#[test]
fn bevel_profile_is_smooth_at_both_ends() {
    use crate::raster::bevel_weight;

    assert!(bevel_weight(0.0, 1.0) == 0.0);
    assert!((bevel_weight(1.0, 1.0) - 1.0).abs() < 1e-6);

    // Smoothstep has zero slope at the start and at the geometric edge, so the
    // bevel cannot leave a visible ring or crease.
    let start_slope = (bevel_weight(0.01, 1.0) - bevel_weight(0.0, 1.0)) / 0.01;
    let end_slope = (bevel_weight(1.0, 1.0) - bevel_weight(0.99, 1.0)) / 0.01;
    assert!(start_slope < 0.05, "{start_slope}");
    assert!(end_slope < 0.05, "{end_slope}");

    let mut previous = -1.0;
    for step in 0..=100 {
        let weight = bevel_weight(step as f32 / 100.0, 1.0);
        assert!(weight >= previous, "{weight} < {previous}");
        previous = weight;
    }
}

#[test]
fn udim_tiles_are_selected() {
    let mut low = quad(0.0, [0.0, 0.0, 1.0]);
    for uv in &mut low.uvs {
        uv[0] += 1.0;
    }
    let high = quad(0.5, [0.0, 0.0, 1.0]);
    let mut settings = settings(BakeMode::HighToLow);
    settings.ray_distance_pct = 50.0;
    settings.cage_offset_pct = 0.0;

    settings.tile = 1001;
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    let error = bake(&low, Some(high.clone()), &settings, &progress, &cancel)
        .expect_err("tile 1001 is empty");
    assert!(matches!(error, BakeError::Message(_)));

    settings.tile = 1002;
    let output = run(&low, Some(high), &settings);
    assert_eq!(pixel(&output, 16, 16)[3], 255);
    assert_eq!(output.stats.tile, 1002);
}

#[test]
fn padding_fills_texels_beyond_the_island() {
    let mut low = quad(0.0, [0.0, 0.0, 1.0]);
    for uv in &mut low.uvs {
        uv[0] *= 0.5;
    }
    let high = quad(0.5, [0.0, 0.0, 1.0]);
    let mut settings = settings(BakeMode::HighToLow);
    settings.resolution = 64;
    settings.ray_distance_pct = 50.0;
    settings.cage_offset_pct = 0.0;
    settings.padding = 4.0;
    let output = run(&low, Some(high), &settings);

    let inside = pixel(&output, 15, 32);
    let outside = pixel(&output, 34, 32);
    assert_eq!(inside[3], 255);
    assert_eq!(outside[3], 255);
    assert!((outside[0] as i32 - inside[0] as i32).abs() <= 2);
    assert!((outside[2] as i32 - inside[2] as i32).abs() <= 2);
}

#[test]
fn supersampling_respects_the_memory_budget() {
    let settings = BakeSettings {
        resolution: 2048,
        anti_alias: 4,
        ..Default::default()
    };
    assert_eq!(settings.effective_aa(), 2);

    let settings = BakeSettings {
        resolution: 4096,
        anti_alias: 4,
        ..Default::default()
    };
    assert_eq!(settings.effective_aa(), 1);

    let settings = BakeSettings {
        resolution: 512,
        anti_alias: 4,
        ..Default::default()
    };
    assert_eq!(settings.effective_aa(), 4);
}

#[test]
fn cancellation_is_reported() {
    let low = quad(0.0, [0.0, 0.0, 1.0]);
    let high = quad(0.5, [0.0, 0.0, 1.0]);
    let settings = settings(BakeMode::HighToLow);
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(true);
    let result = bake(&low, Some(high), &settings, &progress, &cancel);
    assert!(matches!(result, Err(BakeError::Cancelled)));
}
