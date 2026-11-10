use std::collections::HashMap;
use std::error::Error;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use glam::{UVec2, Vec2};
use ns_core::derived;
use ns_core::io::{
    ensure_parent, load_height, load_mask, load_normal, load_project_ron, save_height, save_normal,
    save_project_ron, BitDepth,
};
use ns_core::{
    height_to_normal, normal_to_height, BevelParams, BlendMode, Document, EvalCache, FillRule,
    GeneratorKind, GreenChannel, Grid, Layer, LayerId, LayerKind, MaskGenerator, Node,
    NormalToHeightOptions, Path as VectorPath, Primitive, Profile, Result, ShapeSource, SubPath,
    VectorOp, Wrap,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> std::result::Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        return Ok(());
    }

    match args[1].as_str() {
        "height-to-normal" => height_to_normal_cmd(&args).map_err(Into::into),
        "normal-to-height" => normal_to_height_cmd(&args).map_err(Into::into),
        "bevel" => bevel_cmd(&args).map_err(Into::into),
        "derived" => derived_cmd(&args).map_err(Into::into),
        "stress" => stress_cmd(&args).map_err(Into::into),
        "bench" => bench_cmd(&args).map_err(Into::into),
        "project" => project_cmd(&args).map_err(Into::into),
        "demo" => demo_cmd(&args).map_err(Into::into),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("unknown command: {other}").into()),
    }
}

fn height_to_normal_cmd(args: &[String]) -> Result<()> {
    if args.len() < 4 {
        return Err(ns_core::CoreError::Message(
            "usage: ns_cli height-to-normal <height.png> <normal.png> [--strength=N] [--wrap] [--directx] [--8bit]"
                .into(),
        ));
    }

    let input = &args[2];
    let output = &args[3];
    let mut strength = 1.0f32;
    let mut wrap = Wrap::Clamp;
    let mut convention = GreenChannel::OpenGl;
    let mut bit_depth = BitDepth::Sixteen;

    for arg in &args[4..] {
        match arg.as_str() {
            "--wrap" => wrap = Wrap::Repeat,
            "--directx" => convention = GreenChannel::DirectX,
            "--8bit" => bit_depth = BitDepth::Eight,
            other => {
                if let Some(v) = other.strip_prefix("--strength=") {
                    strength = parse_f32(v, "strength")?;
                } else {
                    return Err(ns_core::CoreError::Message(format!(
                        "unknown option: {other}"
                    )));
                }
            }
        }
    }

    let h = load_height(input)?;
    // Internal normals are canonical OpenGL; the export convention is applied
    // only when saving.
    let n = height_to_normal(&h, strength, wrap, false);
    ensure_parent(output)?;
    save_normal(output, &n, convention, bit_depth)?;
    Ok(())
}

fn normal_to_height_cmd(args: &[String]) -> Result<()> {
    if args.len() < 4 {
        return Err(ns_core::CoreError::Message(
            "usage: ns_cli normal-to-height <normal.png> <height.png> [--directx-in] [--wrap] [--high-pass=N] [--8bit]"
                .into(),
        ));
    }

    let input = &args[2];
    let output = &args[3];
    let mut input_convention = GreenChannel::OpenGl;
    let mut options = NormalToHeightOptions::default();
    let mut bit_depth = BitDepth::Sixteen;

    for arg in &args[4..] {
        match arg.as_str() {
            "--directx-in" => input_convention = GreenChannel::DirectX,
            "--wrap" => options.wrap = Wrap::Repeat,
            "--8bit" => bit_depth = BitDepth::Eight,
            other => {
                if let Some(v) = other.strip_prefix("--high-pass=") {
                    options.high_pass = parse_f32(v, "high-pass")?;
                } else {
                    return Err(ns_core::CoreError::Message(format!(
                        "unknown option: {other}"
                    )));
                }
            }
        }
    }

    let normals = load_normal(input, input_convention)?;
    let height = normal_to_height(&normals, &options);
    ensure_parent(output)?;
    save_height(output, &height, bit_depth)?;
    Ok(())
}

fn bevel_cmd(args: &[String]) -> Result<()> {
    if args.len() < 4 {
        return Err(ns_core::CoreError::Message(
            "usage: ns_cli bevel <mask.png> <normal.png> [--radius=N] [--height=N] [--profile round|chamfer:45] [--softness=N] [--invert] [--wrap] [--directx] [--8bit]"
                .into(),
        ));
    }

    let input = &args[2];
    let output = &args[3];
    let mut params = BevelParams::default();
    let mut wrap = Wrap::Clamp;
    let mut convention = GreenChannel::OpenGl;
    let mut bit_depth = BitDepth::Sixteen;

    for arg in &args[4..] {
        match arg.as_str() {
            "--invert" => params.invert = true,
            "--wrap" => wrap = Wrap::Repeat,
            "--directx" => convention = GreenChannel::DirectX,
            "--8bit" => bit_depth = BitDepth::Eight,
            other => {
                if let Some(v) = other.strip_prefix("--radius=") {
                    params.radius_px = parse_f32(v, "radius")?;
                } else if let Some(v) = other.strip_prefix("--height=") {
                    params.height_px = parse_f32(v, "height")?;
                } else if let Some(v) = other.strip_prefix("--softness=") {
                    params.softness_px = parse_f32(v, "softness")?;
                } else if let Some(v) = other.strip_prefix("--profile=") {
                    params.profile = parse_profile(v)?;
                } else {
                    return Err(ns_core::CoreError::Message(format!(
                        "unknown option: {other}"
                    )));
                }
            }
        }
    }

    let mask = load_mask(input)?;
    let n = ns_core::sdf::bevel_mask_aa(&mask, &params, wrap);
    ensure_parent(output)?;
    save_normal(output, &n, convention, bit_depth)?;
    Ok(())
}

fn derived_cmd(args: &[String]) -> Result<()> {
    if args.len() < 4 {
        return Err(ns_core::CoreError::Message(
            "usage: ns_cli derived <height.png> <out_prefix> [--radius=N] [--height-scale=N] [--gain=N] [--wrap] [--8bit]"
                .into(),
        ));
    }

    let input = &args[2];
    let prefix = Path::new(&args[3]);
    let mut radius = 16.0f32;
    let mut height_scale = 64.0f32;
    let mut gain = 16.0f32;
    let mut wrap = Wrap::Clamp;
    let mut bit_depth = BitDepth::Eight;

    for arg in &args[4..] {
        match arg.as_str() {
            "--wrap" => wrap = Wrap::Repeat,
            "--8bit" => bit_depth = BitDepth::Eight,
            other => {
                if let Some(v) = other.strip_prefix("--radius=") {
                    radius = parse_f32(v, "radius")?;
                } else if let Some(v) = other.strip_prefix("--height-scale=") {
                    height_scale = parse_f32(v, "height-scale")?;
                } else if let Some(v) = other.strip_prefix("--gain=") {
                    gain = parse_f32(v, "gain")?;
                } else {
                    return Err(ns_core::CoreError::Message(format!(
                        "unknown option: {other}"
                    )));
                }
            }
        }
    }

    let height = load_height(input)?;
    ensure_parent(with_suffix(prefix, "_ao"))?;
    export_derived(prefix, &height, wrap, height_scale, radius, gain, bit_depth)?;
    println!(
        "wrote {}_ao.png, {}_cavity.png, {}_curvature.png, {}_convexity.png",
        prefix.display(),
        prefix.display(),
        prefix.display(),
        prefix.display()
    );
    Ok(())
}

fn project_cmd(args: &[String]) -> Result<()> {
    if args.len() < 4 {
        return Err(ns_core::CoreError::Message(
            "usage: ns_cli project <project.ron> <normal.png> [--height=FILE] [--ao=FILE] [--cavity=FILE] [--curvature=FILE] [--convexity=FILE] [--integrate] [--directx] [--8bit] [--gain=N] [--height-scale=N]"
                .into(),
        ));
    }

    let project = &args[2];
    let normal_path = &args[3];
    let mut height_path: Option<PathBuf> = None;
    let mut ao_path: Option<PathBuf> = None;
    let mut cavity_path: Option<PathBuf> = None;
    let mut curvature_path: Option<PathBuf> = None;
    let mut convexity_path: Option<PathBuf> = None;
    let mut integrate = false;
    let mut convention = None;
    let mut bit_depth = BitDepth::Sixteen;
    let mut gain = 16.0f32;
    let mut height_scale: Option<f32> = None;

    for arg in &args[4..] {
        match arg.as_str() {
            "--integrate" => integrate = true,
            "--directx" => convention = Some(GreenChannel::DirectX),
            "--opengl" => convention = Some(GreenChannel::OpenGl),
            "--8bit" => bit_depth = BitDepth::Eight,
            other => {
                if let Some(v) = other.strip_prefix("--height=") {
                    height_path = Some(PathBuf::from(v));
                } else if let Some(v) = other.strip_prefix("--ao=") {
                    ao_path = Some(PathBuf::from(v));
                } else if let Some(v) = other.strip_prefix("--cavity=") {
                    cavity_path = Some(PathBuf::from(v));
                } else if let Some(v) = other.strip_prefix("--curvature=") {
                    curvature_path = Some(PathBuf::from(v));
                } else if let Some(v) = other.strip_prefix("--convexity=") {
                    convexity_path = Some(PathBuf::from(v));
                } else if let Some(v) = other.strip_prefix("--gain=") {
                    gain = parse_f32(v, "gain")?;
                } else if let Some(v) = other.strip_prefix("--height-scale=") {
                    height_scale = Some(parse_f32(v, "height-scale")?);
                } else {
                    return Err(ns_core::CoreError::Message(format!(
                        "unknown option: {other}"
                    )));
                }
            }
        }
    }

    let doc = ns_core::archive::load_project(project)?.document;
    let normals = doc.evaluate();
    let convention = convention.unwrap_or(doc.green);

    let needs_height = height_path.is_some()
        || ao_path.is_some()
        || cavity_path.is_some()
        || curvature_path.is_some()
        || convexity_path.is_some();

    let height = if needs_height && (integrate || !doc.layers.iter().any(layer_has_height)) {
        // Normal-only documents have no analytic height; integrate the result.
        normal_to_height(
            &normals,
            &NormalToHeightOptions {
                wrap: doc.wrap,
                ..Default::default()
            },
        )
    } else if needs_height {
        doc.evaluate_height()
    } else {
        // Not used, but keeps the branches simple.
        ns_core::Grid::new(doc.size.x, doc.size.y, 0.0)
    };

    ensure_parent(normal_path)?;
    save_normal(normal_path, &normals, convention, bit_depth)?;

    let scale = height_scale.unwrap_or(doc.height_scale_px);
    if let Some(path) = &height_path {
        ensure_parent(path)?;
        save_height(path, &height, bit_depth)?;
    }
    if let Some(path) = &ao_path {
        ensure_parent(path)?;
        let ao = derived::ao(&height, scale, 48.0, 16, doc.wrap);
        save_height(path, &ao, BitDepth::Eight)?;
    }
    if let Some(path) = &cavity_path {
        ensure_parent(path)?;
        let cavity = derived::cavity(&height, 8.0, doc.wrap);
        save_height(path, &stretch01(&cavity), BitDepth::Eight)?;
    }
    if let Some(path) = &curvature_path {
        ensure_parent(path)?;
        let curvature = derived::curvature(&height, doc.wrap);
        let mapped = curvature.map(|v| (0.5 + *v * gain).clamp(0.0, 1.0));
        save_height(path, &mapped, BitDepth::Eight)?;
    }
    if let Some(path) = &convexity_path {
        ensure_parent(path)?;
        let convexity = derived::convexity(&height, 8.0, doc.wrap);
        save_height(path, &stretch01(&convexity), BitDepth::Eight)?;
    }

    Ok(())
}

fn demo_cmd(args: &[String]) -> Result<()> {
    let dir = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("n-shade_demo"));
    std::fs::create_dir_all(&dir)?;

    let doc = ns_core::demo_document(1024);

    let project = dir.join("demo.nsproj");
    save_project_ron(&project, &doc)?;

    let normals = doc.evaluate();
    let height = doc.evaluate_height();

    save_normal(
        dir.join("demo_normal.png"),
        &normals,
        GreenChannel::OpenGl,
        BitDepth::Sixteen,
    )?;
    save_height(dir.join("demo_height.png"), &height, BitDepth::Sixteen)?;

    let ao = derived::ao(&height, doc.height_scale_px, 48.0, 16, doc.wrap);
    save_height(dir.join("demo_ao.png"), &ao, BitDepth::Eight)?;

    let cavity = derived::cavity(&height, 8.0, doc.wrap);
    save_height(
        dir.join("demo_cavity.png"),
        &stretch01(&cavity),
        BitDepth::Eight,
    )?;

    println!("wrote {}", project.display());
    println!("wrote {}", dir.join("demo_normal.png").display());
    println!("wrote {}", dir.join("demo_height.png").display());
    println!("wrote {}", dir.join("demo_ao.png").display());
    println!("wrote {}", dir.join("demo_cavity.png").display());
    Ok(())
}

fn stress_cmd(args: &[String]) -> Result<()> {
    let mut size = 1024u32;
    let mut layers = 100usize;
    let mut out: Option<PathBuf> = None;
    let mut bit_depth = BitDepth::Sixteen;

    for arg in &args[2..] {
        if let Some(v) = arg.strip_prefix("--size=") {
            size = v
                .parse()
                .map_err(|_| ns_core::CoreError::Message(format!("invalid size: {v}")))?;
        } else if let Some(v) = arg.strip_prefix("--layers=") {
            layers = v
                .parse()
                .map_err(|_| ns_core::CoreError::Message(format!("invalid layer count: {v}")))?;
        } else if let Some(v) = arg.strip_prefix("--out=") {
            out = Some(PathBuf::from(v));
        } else if arg == "--8bit" {
            bit_depth = BitDepth::Eight;
        } else {
            return Err(ns_core::CoreError::Message(format!(
                "unknown option: {arg}"
            )));
        }
    }

    if size == 0 || size > 8192 {
        return Err(ns_core::CoreError::Message(
            "stress size must be between 1 and 8192".into(),
        ));
    }
    if layers > 2000 {
        return Err(ns_core::CoreError::Message(
            "stress layer count is capped at 2000".into(),
        ));
    }

    let doc = ns_core::stress_document(size, layers);
    let visible = doc.visible_layer_count();
    let estimate_mib = doc.estimated_cache_bytes() / (1024 * 1024);

    let started = std::time::Instant::now();
    let normals = doc.evaluate();
    let normal_ms = started.elapsed().as_secs_f32() * 1000.0;

    let started = std::time::Instant::now();
    let height = doc.evaluate_height();
    let height_ms = started.elapsed().as_secs_f32() * 1000.0;

    println!(
        "stress: {size}x{size}, {visible} visible layers, streaming normal {normal_ms:.1} ms, height {height_ms:.1} ms, full-cache estimate {estimate_mib} MiB"
    );

    if let Some(prefix) = out {
        let normal_path = with_suffix(&prefix, "_normal");
        let height_path = with_suffix(&prefix, "_height");
        ensure_parent(&normal_path)?;
        save_normal(&normal_path, &normals, doc.green, bit_depth)?;
        save_height(&height_path, &height, bit_depth)?;
        println!(
            "wrote {} and {}",
            normal_path.display(),
            height_path.display()
        );
    }

    Ok(())
}

fn layer_has_height(layer: &Layer) -> bool {
    match &layer.kind {
        LayerKind::Group { children } => children.iter().any(layer_has_height),
        LayerKind::PhotoHeight(_)
        | LayerKind::Shape(_)
        | LayerKind::HeightPaint { .. }
        | LayerKind::Fill { .. } => true,
        LayerKind::Flat | LayerKind::ImportedNormal { .. } | LayerKind::NormalPaint { .. } => false,
    }
}

fn export_derived(
    prefix: &Path,
    height: &ns_core::Grid<f32>,
    wrap: Wrap,
    height_scale: f32,
    radius: f32,
    gain: f32,
    bit_depth: BitDepth,
) -> Result<()> {
    let ao = derived::ao(height, height_scale, radius, 16, wrap);
    save_height(with_suffix(prefix, "_ao"), &ao, bit_depth)?;

    let cavity = derived::cavity(height, radius * 0.5, wrap);
    save_height(
        with_suffix(prefix, "_cavity"),
        &stretch01(&cavity),
        bit_depth,
    )?;

    let curvature = derived::curvature(height, wrap);
    let mapped = curvature.map(|v| (0.5 + *v * gain).clamp(0.0, 1.0));
    save_height(with_suffix(prefix, "_curvature"), &mapped, bit_depth)?;

    let convexity = derived::convexity(height, radius * 0.5, wrap);
    save_height(
        with_suffix(prefix, "_convexity"),
        &stretch01(&convexity),
        bit_depth,
    )?;
    Ok(())
}

fn with_suffix(prefix: &Path, suffix: &str) -> PathBuf {
    let stem = prefix.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let name = format!("{stem}{suffix}.png");
    prefix.with_file_name(name)
}

fn stretch01(grid: &ns_core::Grid<f32>) -> ns_core::Grid<f32> {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for v in &grid.data {
        if v.is_finite() {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
    }
    if !lo.is_finite() || !hi.is_finite() || hi - lo <= 1e-6 {
        return ns_core::Grid::new(grid.w, grid.h, 0.5);
    }
    grid.map(|v| ((*v - lo) / (hi - lo)).clamp(0.0, 1.0))
}

fn parse_f32(value: &str, name: &str) -> Result<f32> {
    value
        .parse()
        .map_err(|_| ns_core::CoreError::Message(format!("invalid {name}: {value}")))
}

fn parse_profile(value: &str) -> Result<Profile> {
    if value == "round" {
        return Ok(Profile::Round);
    }
    if let Some(angle) = value.strip_prefix("chamfer:") {
        return Ok(Profile::Chamfer {
            degrees: parse_f32(angle, "chamfer angle")?,
        });
    }
    Err(ns_core::CoreError::Message(format!(
        "invalid profile '{value}' (use round or chamfer:45)"
    )))
}

// ---------------------------------------------------------------------------
// Corpus and release baseline (T0.1)
// ---------------------------------------------------------------------------

const SCENE_KEYS: [&str; 10] = [
    "s01", "s02", "s03", "s04", "s05", "s06", "s07", "s08", "s09", "s10",
];

/// The benchmark needs a real cache to show warm/edit costs, but the core
/// cache has no eviction policy; cap it so a 4K scene cannot exhaust the box.
const WARM_CACHE_LIMIT: u64 = 1024 * 1024 * 1024;

fn bench_cmd(args: &[String]) -> Result<()> {
    let mut out = PathBuf::from("bench/scenes");
    let mut only: Option<Vec<String>> = None;
    let mut repeats = 1usize;
    let mut s06_size = 2048u32;
    let mut save_raster = false;
    let mut no_run = false;
    let mut showcase = PathBuf::from("showcase/scifi_panel_detailed.nsproj");

    for arg in &args[2..] {
        if let Some(value) = arg.strip_prefix("--out=") {
            out = PathBuf::from(value);
        } else if let Some(value) = arg.strip_prefix("--scenes=") {
            only = Some(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .map(String::from)
                    .collect(),
            );
        } else if let Some(value) = arg.strip_prefix("--repeats=") {
            repeats = value
                .parse()
                .map_err(|_| ns_core::CoreError::Message(format!("invalid repeats: {value}")))?;
        } else if let Some(value) = arg.strip_prefix("--s06-size=") {
            s06_size = value
                .parse()
                .map_err(|_| ns_core::CoreError::Message(format!("invalid s06 size: {value}")))?;
        } else if let Some(value) = arg.strip_prefix("--showcase=") {
            showcase = PathBuf::from(value);
        } else if arg == "--save-raster" {
            save_raster = true;
        } else if arg == "--no-run" {
            no_run = true;
        } else {
            return Err(ns_core::CoreError::Message(format!(
                "unknown option: {arg}"
            )));
        }
    }

    if !(1..=32).contains(&repeats) {
        return Err(ns_core::CoreError::Message(
            "repeats must be between 1 and 32".into(),
        ));
    }
    if !(64..=8192).contains(&s06_size) {
        return Err(ns_core::CoreError::Message(
            "s06 size must be between 64 and 8192".into(),
        ));
    }
    std::fs::create_dir_all(&out)?;
    let wanted = |key: &str| {
        only.as_ref().is_none_or(|list| {
            list.iter()
                .any(|want| want == key || key.starts_with(want.as_str()) || want.starts_with(key))
        })
    };
    if !no_run {
        println!(
            "| scene | size | layers | file MiB | cold normal ms | cold height ms | \
             warm normal ms | warm height ms | edit normal ms | cache MiB | peak RSS MiB |"
        );
        println!("|---|---|---|---|---|---|---|---|---|---|---|");
    }

    for key in SCENE_KEYS {
        if !wanted(key) {
            continue;
        }
        if key == "s03" {
            let name = "s03_247_showcase";
            let path = out.join(format!("{name}.nsproj"));
            if !showcase.exists() {
                println!("// {name}: {} is missing; skipped", showcase.display());
                continue;
            }
            std::fs::copy(&showcase, &path)?;
            let mut doc = load_project_ron(&showcase)?;
            let bytes = std::fs::metadata(&path)?.len();
            if !no_run {
                bench_one(name, &mut doc, bytes, repeats);
            } else {
                println!(
                    "// wrote {} ({} layers, {:.1} MiB)",
                    path.display(),
                    doc.visible_layer_count(),
                    bytes as f64 / (1024.0 * 1024.0)
                );
            }
            continue;
        }

        let Some((name, mut doc, save)) = build_corpus_scene(key, s06_size) else {
            continue;
        };
        let save = save || (key == "s06" && save_raster);
        stabilize_ids(&mut doc);
        let path = out.join(format!("{name}.nsproj"));
        let bytes = if save {
            save_project_ron(&path, &doc)?;
            std::fs::metadata(&path)?.len()
        } else {
            println!(
                "// {name}: raster grids are not saved by default; pass --save-raster to commit a {:.0} MiB project",
                doc.estimated_cache_bytes() as f64 / (1024.0 * 1024.0)
            );
            0
        };
        if no_run {
            println!(
                "// wrote {} ({} layers, {:.1} MiB)",
                if save {
                    path.display().to_string()
                } else {
                    name.to_string()
                },
                doc.visible_layer_count(),
                bytes as f64 / (1024.0 * 1024.0)
            );
        } else {
            bench_one(name, &mut doc, bytes, repeats);
        }
    }
    Ok(())
}

fn build_corpus_scene(key: &str, s06_size: u32) -> Option<(&'static str, Document, bool)> {
    Some(match key {
        "s01" => ("s01_8shapes_4masks_1k", scene_shapes_and_masks(), true),
        "s02" => (
            "s02_101layers_1k",
            ns_core::stress_document(1024, 100),
            true,
        ),
        "s04" => ("s04_honeycomb_compound", scene_honeycomb(), true),
        "s05" => ("s05_paths_8x5000", scene_paths(), true),
        "s06" => ("s06_photo_paint_2k", scene_photo_paint(s06_size), false),
        "s07" => ("s07_nested_groups", scene_nested_groups(), true),
        "s08" => ("s08_smart_masks", scene_smart_masks(), true),
        "s09" => ("s09_tileable_repeat", scene_tileable_repeat(), true),
        "s10" => ("s10_4k_20layers", ns_core::stress_document(4096, 19), true),
        _ => return None,
    })
}

fn scene_shapes_and_masks() -> Document {
    let size = 1024u32;
    let s = size as f32;
    let mut doc = Document::new_with_base(UVec2::splat(size));
    doc.height_scale_px = 64.0;
    for index in 0..8 {
        let column = (index % 4) as f32;
        let row = (index / 4) as f32;
        let center = Vec2::new((column + 0.5) * s / 4.0, (row + 0.5) * s / 2.0);
        let source = match index % 3 {
            0 => ShapeSource::Primitive(Primitive::Rect {
                center,
                size: Vec2::new(170.0, 120.0),
                corner_radius: 22.0,
            }),
            1 => ShapeSource::Primitive(Primitive::Ellipse {
                center,
                radii: Vec2::new(95.0, 72.0),
            }),
            _ => ShapeSource::Primitive(Primitive::Star {
                center,
                inner_radius: 52.0,
                outer_radius: 110.0,
                points: 6,
                rotation: index as f32 * 0.37,
            }),
        };
        let mut layer = shape_layer(format!("plate {index}"), source, 34.0, 18.0);
        if index < 4 {
            let cx = 0.25 + (index % 2) as f32 * 0.5;
            let cy = 0.25 + (index / 2) as f32 * 0.5;
            layer.mask = Some(Grid::from_fn(128, 128, |x, y| {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / 128.0;
                let distance = (p - Vec2::new(cx, cy)).length();
                (1.0 - distance * 3.2).clamp(0.0, 1.0)
            }));
            layer.mask_settings.density = 0.85;
            layer.mask_settings.feather_px = 2.0;
            layer.touch_mask();
        }
        doc.layers.push(layer);
    }
    doc
}

fn scene_honeycomb() -> Document {
    let mut doc = Document::new_with_base(UVec2::splat(1024));
    doc.height_scale_px = 64.0;
    let cells = 15u32;
    let radius = 26.0f32;
    let dx = 3.0f32.sqrt() * radius * 1.02;
    let dy = 1.5 * radius * 1.02;
    let mut children = Vec::with_capacity((cells * cells) as usize);
    for row in 0..cells {
        for column in 0..cells {
            let center = Vec2::new(
                512.0
                    + (column as f32 - (cells - 1) as f32 * 0.5) * dx
                    + if !row.is_multiple_of(2) {
                        dx * 0.5
                    } else {
                        0.0
                    },
                512.0 + (row as f32 - (cells - 1) as f32 * 0.5) * dy,
            );
            let points = (0..6)
                .map(|corner| {
                    let angle =
                        std::f32::consts::FRAC_PI_6 + corner as f32 * std::f32::consts::FRAC_PI_3;
                    center + Vec2::new(angle.cos(), angle.sin()) * radius * 0.94
                })
                .collect();
            children.push(shape_layer(
                format!("cell {row:02}-{column:02}"),
                ShapeSource::Primitive(Primitive::Polygon { points }),
                radius * 0.16,
                radius * 0.08,
            ));
        }
    }
    let mut group = Layer::group("honeycomb", children);
    group.vector_op = Some(VectorOp::Add);
    doc.layers.push(group);
    doc
}

fn scene_paths() -> Document {
    let mut doc = Document::new_with_base(UVec2::splat(1024));
    doc.height_scale_px = 48.0;
    for index in 0..8u32 {
        let column = index % 4;
        let row = index / 4;
        let center = Vec2::new((column as f32 + 0.5) * 256.0, (row as f32 + 0.5) * 512.0);
        let mut path = VectorPath::new(FillRule::NonZero);
        let mut subpath = SubPath::new(true);
        for node_index in 0..5000u32 {
            let angle = node_index as f32 / 5000.0 * std::f32::consts::TAU;
            let radius =
                90.0 + 18.0 * (angle * 11.0 + index as f32).sin() + 9.0 * (angle * 23.0).cos();
            subpath.nodes.push(Node::corner(
                center + Vec2::new(angle.cos(), angle.sin()) * radius,
            ));
        }
        path.subpaths.push(subpath);
        doc.layers.push(shape_layer(
            format!("path {index}"),
            ShapeSource::Path(path),
            10.0,
            6.0,
        ));
    }
    doc
}

fn scene_photo_paint(size: u32) -> Document {
    let mut doc = Document::new_with_base(UVec2::splat(size));
    doc.height_scale_px = 96.0 * size as f32 / 1024.0;
    let height = Grid::from_fn(size, size, |x, y| {
        let u = x as f32 / size as f32;
        let v = y as f32 / size as f32;
        let ridges = ((u * 24.0).sin() * 0.5 + 0.5) * ((v * 18.0).cos() * 0.5 + 0.5);
        let bump = (-((u - 0.5).powi(2) + (v - 0.5).powi(2)) * 18.0).exp();
        (ridges * 0.6 + bump * 0.4).clamp(0.0, 1.0)
    });
    let normals = height_to_normal(&height, 2.0, Wrap::Clamp, false);
    doc.layers.push(Layer::photo_height(
        "photo height",
        height.clone(),
        18.0,
        [2.0, 8.0, 24.0],
        [0.55, 0.30, 0.15],
    ));
    doc.layers.push(Layer::height_paint(
        "height paint",
        Grid::from_fn(size, size, |x, y| {
            let u = x as f32 / size as f32;
            let v = y as f32 / size as f32;
            ((u * 7.0 + v * 3.0).sin() * 0.5 + 0.5) * 0.7
                + (-((u - 0.3).powi(2) + (v - 0.7).powi(2)) * 40.0).exp() * 0.3
        }),
        12.0,
    ));
    doc.layers
        .push(Layer::imported_normal("imported normal", normals));
    for index in 0..4u32 {
        let center = Vec2::new(size as f32 * (0.2 + 0.2 * index as f32), size as f32 * 0.5);
        doc.layers.push(shape_layer(
            format!("trim {index}"),
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size: Vec2::splat(size as f32 * 0.12),
                corner_radius: size as f32 * 0.02,
            }),
            size as f32 * 0.012,
            size as f32 * 0.006,
        ));
    }
    doc
}

fn scene_nested_groups() -> Document {
    let mut doc = Document::new_with_base(UVec2::splat(1024));
    doc.height_scale_px = 64.0;
    for index in 0..3u32 {
        let center = Vec2::new(170.0 + index as f32 * 342.0, 512.0);
        doc.layers.push(nested_group(3, index, 300.0, center));
    }
    doc
}

fn nested_group(depth: u32, index: u32, size: f32, center: Vec2) -> Layer {
    if depth == 0 {
        let primitive = match index % 3 {
            0 => Primitive::Rect {
                center,
                size: Vec2::splat(size * 0.55),
                corner_radius: size * 0.08,
            },
            1 => Primitive::Ellipse {
                center,
                radii: Vec2::splat(size * 0.30),
            },
            _ => Primitive::Star {
                center,
                inner_radius: size * 0.16,
                outer_radius: size * 0.32,
                points: 5,
                rotation: index as f32 * 0.4,
            },
        };
        return shape_layer(
            format!("leaf {index}"),
            ShapeSource::Primitive(primitive),
            size * 0.08,
            size * 0.04,
        );
    }
    let children = (0..3u32)
        .map(|child| {
            let offset = Vec2::new(
                (child as f32 - 1.0) * size * 0.22,
                if child.is_multiple_of(2) {
                    -size * 0.14
                } else {
                    size * 0.14
                },
            );
            nested_group(depth - 1, index * 3 + child, size * 0.62, center + offset)
        })
        .collect();
    let mut group = Layer::group(format!("group {depth}-{index}"), children);
    group.opacity = 0.92 - depth as f32 * 0.02;
    group.blend = match depth % 3 {
        0 => BlendMode::Derivative,
        1 => BlendMode::Rnm,
        _ => BlendMode::Replace,
    };
    if depth.is_multiple_of(2) {
        group.mask = Some(Grid::from_fn(64, 64, |x, y| {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / 64.0;
            let distance = (p - Vec2::splat(0.5)).length();
            (1.0 - distance * 2.4).clamp(0.0, 1.0)
        }));
        group.mask_settings.density = 0.85;
        group.touch_mask();
    }
    group
}

fn scene_smart_masks() -> Document {
    let mut doc = ns_core::demo_document(1024);
    for index in 0..6u32 {
        let center = Vec2::new(170.0 + index as f32 * 137.0, 512.0);
        let mut layer = shape_layer(
            format!("smart {index}"),
            ShapeSource::Primitive(Primitive::Rect {
                center,
                size: Vec2::new(150.0, 210.0),
                corner_radius: 28.0,
            }),
            30.0,
            14.0,
        );
        let generator = match index % 3 {
            0 => MaskGenerator::new(GeneratorKind::Slope {
                min_deg: 4.0,
                max_deg: 28.0,
                softness_deg: 7.0,
            }),
            1 => MaskGenerator::new(GeneratorKind::Curvature {
                mode: ns_core::CurvatureMode::Both,
                spread_px: 3.0,
                contrast: 1.8,
            }),
            _ => MaskGenerator::new(GeneratorKind::Noise {
                scale_px: 72.0,
                octaves: 4,
                seed: 7 + index,
                threshold: 0.38,
                softness: 0.24,
            }),
        };
        layer.mask_settings.generators.push(generator);
        if index % 2 == 0 {
            layer.mask = Some(Grid::from_fn(128, 128, |x, _| {
                (x as f32 / 128.0).clamp(0.0, 1.0)
            }));
            layer.touch_mask();
        }
        doc.layers.push(layer);
    }
    doc
}

fn scene_tileable_repeat() -> Document {
    let mut doc = ns_core::demo_document(1024);
    doc.wrap = Wrap::Repeat;
    for index in 0..4u32 {
        let center = match index {
            0 => Vec2::new(0.0, 512.0),
            1 => Vec2::new(1024.0, 300.0),
            2 => Vec2::new(420.0, 0.0),
            _ => Vec2::new(760.0, 1024.0),
        };
        doc.layers.push(shape_layer(
            format!("tile edge {index}"),
            ShapeSource::Primitive(Primitive::Ellipse {
                center,
                radii: Vec2::new(120.0, 90.0),
            }),
            28.0,
            14.0,
        ));
    }
    doc
}

fn shape_layer(name: impl Into<String>, source: ShapeSource, radius: f32, height: f32) -> Layer {
    let mut layer = Layer::shape(name, source);
    if let LayerKind::Shape(shape) = &mut layer.kind {
        shape.radius_px = radius;
        shape.height_px = height;
        shape.profile = Profile::Round;
    }
    layer
}

/// Replace the random layer ids with a stable sequence so regenerating the
/// corpus does not rewrite every line of a committed project.
fn stabilize_ids(doc: &mut Document) {
    fn assign(layers: &mut [Layer], next: &mut u128, map: &mut HashMap<LayerId, LayerId>) {
        for layer in layers {
            let old = layer.id;
            let new = uuid::Uuid::from_u128(0x4e53_0000_0000_0000_0000_0000_0000_0000 + *next);
            *next += 1;
            layer.id = new;
            map.insert(old, new);
            if let Some(children) = layer.children_mut() {
                assign(children, next, map);
            }
        }
    }
    fn remap(layers: &mut [Layer], map: &HashMap<LayerId, LayerId>) {
        for layer in layers {
            if let Some(id) = layer.mask_layer {
                layer.mask_layer = map.get(&id).copied();
            }
            for operand in &mut layer.boolean_operands {
                if let Some(id) = map.get(&operand.layer) {
                    operand.layer = *id;
                }
            }
            if let Some(children) = layer.children_mut() {
                remap(children, map);
            }
        }
    }
    let mut map = HashMap::new();
    let mut next = 0;
    assign(&mut doc.layers, &mut next, &mut map);
    remap(&mut doc.layers, &map);
}

fn bench_one(name: &str, doc: &mut Document, bytes: u64, repeats: usize) {
    let cold_normal = best_of(repeats, || {
        black_box(doc.evaluate());
    });
    let cold_height = best_of(repeats, || {
        black_box(doc.evaluate_height());
    });

    let cache_fits = doc.estimated_cache_bytes() <= WARM_CACHE_LIMIT;
    let mut cache = EvalCache::new();
    let warm_normal = if cache_fits {
        black_box(doc.evaluate_with(&mut cache));
        best_of(repeats, || {
            black_box(doc.evaluate_with(&mut cache));
        })
    } else {
        best_of(repeats, || {
            black_box(doc.evaluate());
        })
    };
    let warm_height = if cache_fits {
        black_box(doc.evaluate_height_with(&mut cache));
        best_of(repeats, || {
            black_box(doc.evaluate_height_with(&mut cache));
        })
    } else {
        best_of(repeats, || {
            black_box(doc.evaluate_height());
        })
    };

    let edited = edit_first_shape(doc);
    let edit_normal = if edited {
        // With a warm cache only the first call re-evaluates the edited layer;
        // later calls are warm again, so best-of would hide the edit cost.
        if cache_fits {
            timed(&mut || {
                black_box(doc.evaluate_with(&mut cache));
            })
        } else {
            best_of(repeats, || {
                black_box(doc.evaluate());
            })
        }
    } else {
        f64::NAN
    };

    let marked = if cache_fits { "" } else { "*" };
    let rss = peak_rss_mib()
        .map(|mib| format!("{mib:.0}"))
        .unwrap_or_else(|| "-".into());
    println!(
        "| {name} | {}² | {} | {:.1} | {cold_normal:.1} | {cold_height:.1} | \
         {warm_normal:.1}{marked} | {warm_height:.1}{marked} | {edit_normal:.1}{marked} | \
         {:.0} | {rss} |",
        doc.size.x,
        doc.visible_layer_count(),
        bytes as f64 / (1024.0 * 1024.0),
        doc.estimated_cache_bytes() as f64 / (1024.0 * 1024.0),
    );
    if !cache_fits {
        println!(
            "// {name}: estimated cache is above 1 GiB; warm/edit rows use uncached \
             re-evaluation and are marked *"
        );
    }
}

/// Best-of-N timing. Background apps and thermal drift shift a shared laptop's
/// wall clock by more than 5 % between runs; the fastest clean run is the most
/// stable estimate of the kernel itself.
fn best_of(mut runs: usize, mut run: impl FnMut()) -> f64 {
    runs = runs.max(1);
    let mut best = f64::INFINITY;
    for _ in 0..runs {
        best = best.min(timed(&mut run));
    }
    best
}

fn timed(run: &mut impl FnMut()) -> f64 {
    let started = Instant::now();
    run();
    started.elapsed().as_secs_f64() * 1000.0
}

fn edit_first_shape(doc: &mut Document) -> bool {
    fn first(layers: &mut [Layer]) -> Option<&mut Layer> {
        for layer in layers {
            if matches!(layer.kind, LayerKind::Shape(_)) {
                return Some(layer);
            }
            if let LayerKind::Group { children } = &mut layer.kind {
                if let Some(found) = first(children) {
                    return Some(found);
                }
            }
        }
        None
    }
    let Some(layer) = first(&mut doc.layers) else {
        return false;
    };
    if let LayerKind::Shape(shape) = &mut layer.kind {
        shape.height_px = (shape.height_px + 0.5).max(0.5);
        shape.radius_px = (shape.radius_px * 0.99).max(0.5);
    }
    layer.touch();
    true
}

#[cfg(windows)]
#[repr(C)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

#[cfg(windows)]
fn peak_rss_mib() -> Option<f64> {
    #[link(name = "kernel32")]
    extern "system" {
        fn K32GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }
    let mut counters: ProcessMemoryCounters = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    // (HANDLE)-1 is the pseudo-handle for the current process.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            -1isize as *mut core::ffi::c_void,
            &mut counters,
            counters.cb,
        )
    };
    (ok != 0).then(|| counters.peak_working_set_size as f64 / (1024.0 * 1024.0))
}

#[cfg(not(windows))]
fn peak_rss_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024.0)
}

fn print_usage() {
    println!(
        "N-shade CLI\n\
         \n\
         Commands:\n\
         \x20 height-to-normal <height.png> <normal.png> [--strength=N] [--wrap] [--directx] [--8bit]\n\
         \x20 normal-to-height <normal.png> <height.png> [--directx-in] [--wrap] [--high-pass=N] [--8bit]\n\
         \x20 bevel <mask.png> <normal.png> [--radius=N] [--height=N] [--profile=round|chamfer:45] [--softness=N] [--invert] [--wrap] [--directx] [--8bit]\n\
         \x20 derived <height.png> <out_prefix> [--radius=N] [--height-scale=N] [--gain=N] [--wrap] [--8bit]\n\
         \x20 stress [--size=N] [--layers=N] [--out=PREFIX] [--8bit]\n\
         \x20 bench [--out=DIR] [--scenes=s01,s04] [--repeats=N] [--s06-size=N] [--save-raster] [--no-run] [--showcase=FILE]\n\
         \x20 project <project.ron> <normal.png> [--height=FILE] [--ao=FILE] [--cavity=FILE] [--curvature=FILE] [--convexity=FILE] [--integrate] [--directx] [--8bit]\n\
         \x20 demo [output_dir]\n\
         \n\
         The default export is 16-bit PNG in OpenGL (Y+) convention."
    );
}
