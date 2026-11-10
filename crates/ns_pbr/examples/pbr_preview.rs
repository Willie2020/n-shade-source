//! Headless single-image PBR run used for profiling and visual checks.
//!
//! Usage: `cargo run -p ns_pbr --example pbr_preview -- <input> <output-dir>`

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use ns_pbr::export::{export_maps, ExportOptions};
use ns_pbr::{
    run_pipeline, stage_keys, MapStore, MapToggles, PbrSettings, Quality, Stage, StageOutput,
};

fn main() -> ns_pbr::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| "output/pbr_input.png".into());
    let output = args.next().unwrap_or_else(|| "output/pbr_maps".into());
    let source = ns_pbr::pixels::load(std::path::Path::new(&input))?;
    println!("input {}x{}", source.w, source.h);

    let settings = PbrSettings {
        quality: Quality::Standard,
        maps: MapToggles {
            normal: true,
            height: true,
            ao: true,
            cavity: true,
            roughness: true,
            delit: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let hash = ns_pbr::hash_image(&source);
    let (plan, keys) = stage_keys(&settings, hash);
    let started = Instant::now();
    let output_pipeline = run_pipeline(
        Arc::new(source),
        &settings,
        &plan,
        &keys,
        &HashMap::new(),
        &AtomicBool::new(false),
        &|stage, fraction| {
            if fraction > 0.0 {
                println!("  {} done", stage.label());
            }
        },
    )?;
    println!(
        "pipeline total {:.0} ms; stages: {:?}",
        started.elapsed().as_secs_f32() * 1000.0,
        output_pipeline.timings_ms
    );

    if let Some(StageOutput::Geometry(geometry)) = output_pipeline
        .stages
        .get(&Stage::Geometry)
        .map(AsRef::as_ref)
    {
        let height = &geometry.height;
        let (min, max) = height.min_max();
        let mean = height.data.iter().sum::<f32>() / height.data.len() as f32;
        println!("height min {min:.4} max {max:.4} mean {mean:.4}");
        let mut nx = (f32::INFINITY, f32::NEG_INFINITY);
        let mut ny = (f32::INFINITY, f32::NEG_INFINITY);
        let mut nz = (f32::INFINITY, f32::NEG_INFINITY);
        let mut slope_sum = 0.0;
        for normal in &geometry.normal.data {
            nx.0 = nx.0.min(normal.x);
            nx.1 = nx.1.max(normal.x);
            ny.0 = ny.0.min(normal.y);
            ny.1 = ny.1.max(normal.y);
            nz.0 = nz.0.min(normal.z);
            nz.1 = nz.1.max(normal.z);
            slope_sum += normal.x.abs() + normal.y.abs();
        }
        println!(
            "normal x {:?} y {:?} z {:?} mean |slope| {:.4}",
            nx,
            ny,
            nz,
            slope_sum / (geometry.normal.data.len() as f32 * 2.0)
        );
    }

    let mut store = MapStore::default();
    for (key, entry) in output_pipeline.new_entries {
        store.insert(key, entry);
    }
    store.set_current(&output_pipeline.keys);
    let options = ExportOptions {
        sixteen_bit: true,
        packed_gltf: true,
        packed_orm: true,
        packed_unity: true,
        resolution: 0,
        maps: None,
    };
    let report = export_maps(
        std::path::Path::new(&output),
        "preview",
        &settings,
        &store,
        &options,
    )?;
    println!("exported {:?}", report.files);
    for error in report.errors {
        println!("export warning: {error}");
    }
    Ok(())
}
