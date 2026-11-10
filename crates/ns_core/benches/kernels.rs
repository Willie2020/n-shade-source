//! Offline kernel microbenchmarks for `ns_core` (T0.3).
//!
//! Run with `cargo bench -p ns_core`. This is deliberately dependency-free so
//! the corpus runs without a network fetch; `criterion` can replace it later
//! without changing the fixtures below.

use std::hint::black_box;
use std::time::Instant;

use glam::{UVec2, Vec2};
use ns_core::derived;
use ns_core::filter::gaussian_blur;
use ns_core::sdf;
use ns_core::{
    blend, height_to_normal, normal_to_height, BlendMode, Document, Grid, Layer,
    NormalToHeightOptions, Primitive, ShapeSource, Wrap,
};

fn main() {
    println!("ns_core kernel benchmarks");
    println!("{:<30} {:>10} {:>12}", "kernel", "ms", "Mpix/s");

    benchmark_edt(256, Wrap::Clamp, false);
    benchmark_edt(256, Wrap::Repeat, false);
    benchmark_edt(1024, Wrap::Clamp, false);
    benchmark_edt(1024, Wrap::Repeat, false);
    benchmark_edt(1024, Wrap::Clamp, true);
    benchmark_edt(2048, Wrap::Clamp, false);
    benchmark_edt(2048, Wrap::Repeat, false);
    benchmark_blur(1024);
    benchmark_scharr(1024, Wrap::Clamp);
    benchmark_scharr(1024, Wrap::Repeat);
    benchmark_scharr(2048, Wrap::Clamp);
    benchmark_scharr(2048, Wrap::Repeat);
    benchmark_blend(1024);
    benchmark_shape_region(1024, 64.0);
    benchmark_shape_region(1024, 80.0);
    benchmark_shape_region(1024, 800.0);
    benchmark_fft(512, 2);
    benchmark_fft(1024, 1);
    benchmark_fft(2048, 1);
    benchmark_derived(512);
    benchmark_derived(1024);
}

fn benchmark_edt(size: u32, wrap: Wrap, inverted: bool) {
    let mut coverage = coverage(size);
    if inverted {
        coverage = coverage.map(|value| 1.0 - *value);
    }
    let pixels = u64::from(size) * u64::from(size);
    let suffix = match (wrap, inverted) {
        (Wrap::Clamp, false) => "clamp",
        (Wrap::Repeat, false) => "repeat",
        (Wrap::Clamp, true) => "clamp/outside",
        (Wrap::Repeat, true) => "repeat/outside",
    };
    let iterations = if size >= 2048 { 1 } else { 2 };
    measure(format!("edt/{size}/{suffix}"), pixels, iterations, || {
        black_box(sdf::signed_distance_aa(&coverage, wrap));
    });
}

fn benchmark_blur(size: u32) {
    let height = height(size);
    let pixels = u64::from(size) * u64::from(size);
    for radius in [2.0f32, 8.0, 32.0] {
        measure(format!("gaussian/{size}/r{radius}"), pixels, 2, || {
            black_box(gaussian_blur(&height, radius, Wrap::Clamp));
        });
    }
}

fn benchmark_scharr(size: u32, wrap: Wrap) {
    let height = height(size);
    let pixels = u64::from(size) * u64::from(size);
    let suffix = match wrap {
        Wrap::Clamp => "clamp",
        Wrap::Repeat => "repeat",
    };
    measure(format!("scharr/{size}/{suffix}"), pixels, 2, || {
        black_box(height_to_normal(&height, 1.0, wrap, false));
    });
}

fn benchmark_blend(size: u32) {
    let normals = height_to_normal(&height(size), 1.0, Wrap::Clamp, false);
    let detail = height_to_normal(&height(size), 0.35, Wrap::Clamp, false);
    let mask = coverage(size);
    let pixels = u64::from(size) * u64::from(size);
    for mode in [BlendMode::Derivative, BlendMode::Rnm, BlendMode::Replace] {
        measure(format!("blend/{size}/{mode:?}"), pixels, 2, || {
            black_box(Grid::from_fn(size, size, |x, y| {
                blend(*normals.get(x, y), *detail.get(x, y), mode, 0.5)
            }));
        });
    }
    measure(format!("blend/{size}/masked"), pixels, 2, || {
        black_box(Grid::from_fn(size, size, |x, y| {
            blend(
                *normals.get(x, y),
                *detail.get(x, y),
                BlendMode::Derivative,
                *mask.get(x, y),
            )
        }));
    });
}

fn benchmark_shape_region(size: u32, shape_size: f32) {
    let mut doc = Document::new(UVec2::splat(size));
    doc.layers.push(Layer::shape(
        "shape",
        ShapeSource::Primitive(Primitive::Rect {
            center: Vec2::splat(size as f32 * 0.5),
            size: Vec2::splat(shape_size),
            corner_radius: shape_size * 0.1,
        }),
    ));
    let pixels = u64::from(size) * u64::from(size);
    measure(
        format!("shape_region/{size}/{shape_size:.0}px"),
        pixels,
        2,
        || {
            black_box(doc.evaluate());
        },
    );
}

fn benchmark_fft(size: u32, iterations: usize) {
    let normals = height_to_normal(&height(size), 1.0, Wrap::Clamp, false);
    let options = NormalToHeightOptions::default();
    let pixels = u64::from(size) * u64::from(size);
    measure(format!("fft/{size}"), pixels, iterations, || {
        black_box(normal_to_height(&normals, &options));
    });
}

fn benchmark_derived(size: u32) {
    let height = height(size);
    let pixels = u64::from(size) * u64::from(size);
    measure(format!("ao/{size}"), pixels, 1, || {
        black_box(derived::ao(&height, 64.0, 32.0, 8, Wrap::Clamp));
    });
    measure(format!("cavity/{size}"), pixels, 1, || {
        black_box(derived::cavity(&height, 8.0, Wrap::Clamp));
    });
    measure(format!("curvature/{size}"), pixels, 1, || {
        black_box(derived::curvature(&height, Wrap::Clamp));
    });
}

fn measure(label: String, pixels: u64, iterations: usize, mut run: impl FnMut()) {
    run();
    let started = Instant::now();
    for _ in 0..iterations {
        run();
    }
    let seconds = started.elapsed().as_secs_f64() / iterations as f64;
    let mpix = pixels as f64 / seconds / 1_000_000.0;
    println!("{label:<30} {:>10.2} {:>12.1}", seconds * 1000.0, mpix);
}

fn height(size: u32) -> Grid<f32> {
    Grid::from_fn(size, size, |x, y| {
        let x = x as f32 / size as f32;
        let y = y as f32 / size as f32;
        let wave = (x * std::f32::consts::TAU).sin() * (y * std::f32::consts::TAU).cos();
        (wave * 0.25 + 0.5).clamp(0.0, 1.0)
    })
}

fn coverage(size: u32) -> Grid<f32> {
    ShapeSource::Primitive(Primitive::Ellipse {
        center: Vec2::splat(size as f32 * 0.5),
        radii: Vec2::splat(size as f32 * 0.35),
    })
    .rasterize(size, size)
}
