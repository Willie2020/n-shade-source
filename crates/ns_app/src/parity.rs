//! Headless CPU↔GPU parity harness (T0.4).
//!
//! Builds the same packed scene the editor uses, dispatches
//! `normal_studio.wgsl` through a bare `wgpu` device, and compares the decoded
//! normal/height textures against `ns_core`'s CPU oracle. This is deliberately
//! independent of the Bevy render world so it runs in `cargo test`.
//!
//! The live GPU textures are `rgba8unorm`, so the thresholds account for 8-bit
//! quantization (see the comments on each threshold). They are a regression
//! gate, not a claim that the preview is bit-identical to the CPU path.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use ns_core::{demo_document, stress_document, Document, Grid, Layer};
use wgpu::util::DeviceExt;

use crate::gpu::{build_scene_cached, GpuSceneCache, GpuSceneData};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Header {
    width: u32,
    height: u32,
    layer_count: u32,
    vertex_count: u32,
    height_scale: f32,
    _pad: [u32; 3],
}

struct GpuReadback {
    width: u32,
    height: u32,
    normals: Vec<Vec3>,
    heights: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
struct Thresholds {
    mean_deg: f64,
    p999_deg: f64,
    max_deg: f64,
    height_nrmse: f64,
    /// Pixels above 5° are allowed at 1-pixel silhouettes and cusps, but a
    /// missing layer or a wrong blend produces far more than this.
    outlier_fraction: f64,
}

impl Thresholds {
    /// Analytic shape stacks: 8-bit normal quantization alone contributes
    /// roughly 0.3° on flat pixels. Strict metrics are measured away from the
    /// two-to-three pixel band where the CPU EDT and the GPU analytic SDF
    /// choose different nearest edges.
    const ANALYTIC: Self = Self {
        mean_deg: 2.5,
        p999_deg: 8.0,
        max_deg: 15.0,
        height_nrmse: 0.02,
        outlier_fraction: 0.05,
    };

    /// CPU-precomputed layers add one RGBA8 round trip on top of the analytic
    /// path, so allow a little more height quantization.
    const TEXTURED: Self = Self {
        mean_deg: 2.5,
        p999_deg: 8.0,
        max_deg: 15.0,
        height_nrmse: 0.03,
        outlier_fraction: 0.05,
    };

    /// Many small inverted shapes with sharp star cusps: the largest legitimate
    /// EDT/SDF disagreement, used as a coverage scene rather than a tight
    /// shading gate.
    const STRESS: Self = Self {
        mean_deg: 4.0,
        p999_deg: 9.0,
        max_deg: 15.0,
        height_nrmse: 0.02,
        outlier_fraction: 0.12,
    };

    /// Two overlapping bevels: the extra silhouette band loosens the EDT/SDF
    /// comparison slightly. A wrong blend branch still measures tens of degrees.
    const DETAIL_BLEND: Self = Self {
        mean_deg: 3.0,
        p999_deg: 12.0,
        max_deg: 20.0,
        height_nrmse: 0.02,
        outlier_fraction: 0.08,
    };
}

#[derive(Clone, Copy, Debug)]
struct ParityStats {
    mean_deg: f64,
    p999_deg: f64,
    max_deg: f64,
    height_nrmse: f64,
    outlier_fraction: f64,
    pixels: usize,
    stable_pixels: usize,
}

fn gpu_eval(doc: &Document) -> Option<GpuReadback> {
    let mut cache = GpuSceneCache::default();
    let scene = build_scene_cached(doc, Default::default(), &mut cache)?;
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .or_else(|_| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        }))
    })
    .ok()?;

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ns parity"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .ok()?;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ns parity evaluator"),
        source: wgpu::ShaderSource::Wgsl(include_str!("normal_studio.wgsl").into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ns parity bind group layout"),
        entries: &bind_group_layout_entries(),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ns parity pipeline layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("ns parity evaluator"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });

    let header = Header {
        width: scene.width,
        height: scene.height,
        layer_count: scene.layers.len() as u32,
        vertex_count: scene.vertices.len() as u32,
        height_scale: scene.height_scale,
        _pad: [0; 3],
    };
    let header_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("ns parity header"),
        contents: bytemuck::bytes_of(&header),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let layer_buffer = storage_buffer(&device, "ns parity layers", &scene.layers);
    let vertex_buffer = storage_buffer(&device, "ns parity vertices", &scene.vertices);
    let mask_buffer = storage_buffer(&device, "ns parity masks", &scene.masks);
    let contour_buffer = storage_buffer(&device, "ns parity contours", &scene.contours);
    let layer_pixels = flatten_pixels(&scene);
    let mask_pixels = flatten_mask_pixels(&scene);
    let layer_pixel_buffer = storage_buffer(&device, "ns parity layer pixels", &layer_pixels);
    let mask_pixel_buffer = storage_buffer(&device, "ns parity mask pixels", &mask_pixels);

    let (normal_texture, normal_view) =
        output_texture(&device, scene.width, scene.height, "normal");
    let (height_texture, height_view) =
        output_texture(&device, scene.width, scene.height, "height");

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ns parity bind group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: header_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: layer_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: vertex_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&normal_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&height_view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: mask_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: contour_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: layer_pixel_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: mask_pixel_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("ns parity dispatch"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("ns parity evaluator"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            scene.width.div_ceil(8).max(1),
            scene.height.div_ceil(8).max(1),
            1,
        );
    }
    queue.submit(Some(encoder.finish()));

    let padded_bytes_per_row = (scene.width * 4).div_ceil(256) * 256;
    let readback_size = u64::from(padded_bytes_per_row) * u64::from(scene.height);
    let normal_readback = readback_buffer(&device, readback_size, "ns parity normal readback");
    let height_readback = readback_buffer(&device, readback_size, "ns parity height readback");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("ns parity readback"),
    });
    copy_texture(
        &mut encoder,
        &normal_texture,
        &normal_readback,
        scene.width,
        scene.height,
        padded_bytes_per_row,
    );
    copy_texture(
        &mut encoder,
        &height_texture,
        &height_readback,
        scene.width,
        scene.height,
        padded_bytes_per_row,
    );
    queue.submit(Some(encoder.finish()));

    let normal_bytes = map_readback(&device, &normal_readback)?;
    let height_bytes = map_readback(&device, &height_readback)?;
    let mut normals = Vec::with_capacity((scene.width * scene.height) as usize);
    let mut heights = Vec::with_capacity((scene.width * scene.height) as usize);
    for y in 0..scene.height as usize {
        for x in 0..scene.width as usize {
            let index = y * padded_bytes_per_row as usize + x * 4;
            let n = Vec3::new(
                f32::from(normal_bytes[index]) / 127.5 - 1.0,
                f32::from(normal_bytes[index + 1]) / 127.5 - 1.0,
                f32::from(normal_bytes[index + 2]) / 127.5 - 1.0,
            );
            normals.push(if n.length_squared() > 1e-12 {
                n.normalize()
            } else {
                Vec3::Z
            });
            heights.push(f32::from(height_bytes[index]) / 255.0);
        }
    }

    Some(GpuReadback {
        width: scene.width,
        height: scene.height,
        normals,
        heights,
    })
}

fn bind_group_layout_entries() -> [wgpu::BindGroupLayoutEntry; 9] {
    let storage = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    [
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        storage(1),
        storage(2),
        storage_texture(3),
        storage_texture(4),
        storage(5),
        storage(6),
        storage(7),
        storage(8),
    ]
}

fn storage_texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba8Unorm,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn storage_buffer<T: Pod + Zeroable>(
    device: &wgpu::Device,
    label: &str,
    data: &[T],
) -> wgpu::Buffer {
    let fallback = [T::zeroed()];
    let contents = if data.is_empty() { &fallback[..] } else { data };
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(contents),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

fn flatten_pixels(scene: &GpuSceneData) -> Vec<u32> {
    let mut out = Vec::with_capacity(scene.layer_pixel_total as usize);
    for pixels in &scene.layer_pixels {
        out.extend_from_slice(pixels);
    }
    out
}

fn flatten_mask_pixels(scene: &GpuSceneData) -> Vec<u32> {
    let mut out = Vec::with_capacity(scene.mask_pixel_total as usize);
    for pixels in &scene.mask_pixels {
        out.extend_from_slice(pixels);
    }
    out
}

fn output_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &str,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn readback_buffer(device: &wgpu::Device, size: u64, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

fn copy_texture(
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    buffer: &wgpu::Buffer,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
) {
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

fn map_readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Option<Vec<u8>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver.recv().ok()?.ok()?;
    let mapped = buffer.slice(..).get_mapped_range();
    let bytes = mapped.to_vec();
    drop(mapped);
    buffer.unmap();
    Some(bytes)
}

fn compare(
    cpu_normals: &Grid<Vec3>,
    cpu_height: &Grid<f32>,
    gpu: &GpuReadback,
) -> Option<ParityStats> {
    if cpu_normals.w != gpu.width || cpu_normals.h != gpu.height {
        return None;
    }
    let mut angles = Vec::with_capacity(gpu.normals.len());
    let mut stable = Vec::with_capacity(gpu.normals.len());
    let mut outliers = 0usize;
    for (index, (&cpu_n, &gpu_n)) in cpu_normals.data.iter().zip(gpu.normals.iter()).enumerate() {
        let cpu_n = if cpu_n.length_squared() > 1e-12 {
            cpu_n.normalize()
        } else {
            Vec3::Z
        };
        let angle = f64::from(cpu_n.dot(gpu_n).clamp(-1.0, 1.0).acos().to_degrees());
        if angle > 5.0 {
            outliers += 1;
        }
        angles.push(angle);
        let x = (index % cpu_height.w as usize) as u32;
        let y = (index / cpu_height.w as usize) as u32;
        stable.push(!is_unstable(cpu_normals, cpu_height, gpu, x, y));
    }
    let pixel_count = angles.len().max(1) as f64;
    let mean_deg = angles.iter().sum::<f64>() / pixel_count;
    let outlier_fraction = outliers as f64 / pixel_count;

    let mut stable_angles: Vec<f64> = angles
        .iter()
        .copied()
        .zip(stable.iter())
        .filter_map(|(angle, stable)| stable.then_some(angle))
        .collect();
    let stable_pixels = stable_angles.len();
    if stable_angles.is_empty() {
        return None;
    }
    stable_angles.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p999_index = ((stable_angles.len().saturating_sub(1)) as f64 * 0.999).round() as usize;
    let p999_deg = stable_angles.get(p999_index).copied().unwrap_or(0.0);
    let max_deg = stable_angles.last().copied().unwrap_or(0.0);

    let mut height_error = 0.0f64;
    let mut height_min = f64::INFINITY;
    let mut height_max = f64::NEG_INFINITY;
    for (y, x) in (0..gpu.height).flat_map(|y| (0..gpu.width).map(move |x| (y, x))) {
        let index = (y * gpu.width + x) as usize;
        if !stable[index] {
            continue;
        }
        let cpu_h = f64::from(*cpu_height.get(x, y));
        let gpu_h = f64::from(gpu.heights[index]);
        height_error += (gpu_h - cpu_h).powi(2);
        height_min = height_min.min(cpu_h);
        height_max = height_max.max(cpu_h);
    }
    let range = (height_max - height_min).max(1e-6);
    let height_nrmse = (height_error / stable_pixels as f64).sqrt() / range;
    Some(ParityStats {
        mean_deg,
        p999_deg,
        max_deg,
        height_nrmse,
        outlier_fraction,
        pixels: angles.len(),
        stable_pixels,
    })
}

/// A one-pixel band around a silhouette or cusp is excluded from the strict
/// metrics: the CPU uses an exact EDT and the GPU uses analytic SDFs, so their
/// nearest-edge choice is allowed to differ there.
fn is_unstable(
    cpu_normals: &Grid<Vec3>,
    cpu_height: &Grid<f32>,
    gpu: &GpuReadback,
    x: u32,
    y: u32,
) -> bool {
    const HEIGHT_STEP: f32 = 0.05;
    // Exclude the two-to-three pixel band around a cusp: sharp polygon
    // corners legitimately gradient differently under EDT vs analytic SDF.
    const NORMAL_STEP: f32 = 0.9848; // cos(10°)
    let h = *cpu_height.get(x, y);
    let index = (y * gpu.width + x) as usize;
    let gh = gpu.heights[index];
    let cn = cpu_normals.get(x, y).normalize_or_zero();
    let gn = gpu.normals[index];
    for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
        let nx = x as i32 + dx;
        let ny = y as i32 + dy;
        if nx < 0 || ny < 0 || nx >= gpu.width as i32 || ny >= gpu.height as i32 {
            continue;
        }
        let nx = nx as u32;
        let ny = ny as u32;
        let neighbor_index = (ny * gpu.width + nx) as usize;
        if (h - *cpu_height.get(nx, ny)).abs() > HEIGHT_STEP
            || (gh - gpu.heights[neighbor_index]).abs() > HEIGHT_STEP
        {
            return true;
        }
        let cpu_neighbor = cpu_normals.get(nx, ny).normalize_or_zero();
        if cn.dot(cpu_neighbor) < NORMAL_STEP || gn.dot(gpu.normals[neighbor_index]) < NORMAL_STEP {
            return true;
        }
    }
    false
}

fn report_worst(cpu_normals: &Grid<Vec3>, cpu_height: &Grid<f32>, gpu: &GpuReadback, count: usize) {
    let mut errors: Vec<(f64, u32, u32)> = Vec::with_capacity(gpu.normals.len());
    for y in 0..gpu.height {
        for x in 0..gpu.width {
            let index = (y * gpu.width + x) as usize;
            let cpu = cpu_normals.get(x, y).normalize_or_zero();
            let dot = f64::from(cpu.dot(gpu.normals[index]).clamp(-1.0, 1.0));
            errors.push((dot.acos().to_degrees(), x, y));
        }
    }
    errors.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (angle, x, y) in errors.into_iter().take(count) {
        let index = (y * gpu.width + x) as usize;
        eprintln!(
            "  worst {angle:.2}° at ({x},{y}) cpu={:?} gpu={:?} h_cpu={:.4} h_gpu={:.4}",
            cpu_normals.get(x, y),
            gpu.normals[index],
            cpu_height.get(x, y),
            gpu.heights[index]
        );
    }
}

fn report_worst_height(cpu_height: &Grid<f32>, gpu: &GpuReadback, count: usize) {
    let mut errors: Vec<(f64, u32, u32)> = Vec::with_capacity(gpu.heights.len());
    for y in 0..gpu.height {
        for x in 0..gpu.width {
            let index = (y * gpu.width + x) as usize;
            let cpu = f64::from(*cpu_height.get(x, y));
            let gpu = f64::from(gpu.heights[index]);
            errors.push(((gpu - cpu).abs(), x, y));
        }
    }
    errors.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (error, x, y) in errors.into_iter().take(count) {
        let index = (y * gpu.width + x) as usize;
        eprintln!(
            "  height worst {error:.5} at ({x},{y}) cpu={:.5} gpu={:.5}",
            cpu_height.get(x, y),
            gpu.heights[index]
        );
    }
}

/// CI sets `NS_REQUIRE_GPU=1` so "no adapter" fails instead of silently passing.
fn gpu_required() -> bool {
    std::env::var_os("NS_REQUIRE_GPU").is_some_and(|value| value != "0" && !value.is_empty())
}

fn assert_parity(name: &str, doc: &Document, thresholds: Thresholds) {
    let Some(gpu) = gpu_eval(doc) else {
        assert!(
            !gpu_required(),
            "parity {name}: no wgpu adapter (hardware or fallback) and NS_REQUIRE_GPU is set"
        );
        eprintln!("parity {name}: SKIPPED, no wgpu adapter");
        return;
    };
    let cpu_normals = doc.evaluate();
    let cpu_height = doc.evaluate_height();
    let Some(stats) = compare(&cpu_normals, &cpu_height, &gpu) else {
        panic!("parity {name}: CPU/GPU dimensions differ");
    };
    println!(
        "parity {name}: pixels={} stable={} mean={:.3}° p99.9={:.3}° max={:.3}° \
         height_nrmse={:.4} outliers={:.4}%",
        stats.pixels,
        stats.stable_pixels,
        stats.mean_deg,
        stats.p999_deg,
        stats.max_deg,
        stats.height_nrmse,
        stats.outlier_fraction * 100.0
    );
    if stats.max_deg > thresholds.max_deg {
        report_worst(&cpu_normals, &cpu_height, &gpu, 8);
    }
    if stats.height_nrmse > thresholds.height_nrmse {
        report_worst_height(&cpu_height, &gpu, 8);
    }
    assert!(
        stats.mean_deg <= thresholds.mean_deg,
        "parity {name}: mean angular error {:.3}° exceeds {:.3}°",
        stats.mean_deg,
        thresholds.mean_deg
    );
    assert!(
        stats.p999_deg <= thresholds.p999_deg,
        "parity {name}: p99.9 angular error {:.3}° exceeds {:.3}°",
        stats.p999_deg,
        thresholds.p999_deg
    );
    assert!(
        stats.max_deg <= thresholds.max_deg,
        "parity {name}: max angular error {:.3}° exceeds {:.3}°",
        stats.max_deg,
        thresholds.max_deg
    );
    assert!(
        stats.height_nrmse <= thresholds.height_nrmse,
        "parity {name}: height NRMSE {:.4} exceeds {:.4}",
        stats.height_nrmse,
        thresholds.height_nrmse
    );
    assert!(
        stats.outlier_fraction <= thresholds.outlier_fraction,
        "parity {name}: {:.3}% of pixels exceed 5°; the limit is {:.3}%",
        stats.outlier_fraction * 100.0,
        thresholds.outlier_fraction * 100.0
    );
}

#[test]
fn parity_demo_scene() {
    assert_parity("demo", &demo_document(256), Thresholds::ANALYTIC);
}

#[test]
fn parity_stress_scene() {
    assert_parity("stress-32", &stress_document(256, 32), Thresholds::STRESS);
}

#[test]
fn parity_textured_scene() {
    let mut doc = demo_document(256);
    let photo = Grid::from_fn(256, 256, |x, y| {
        let x = x as f32 / 255.0;
        let y = y as f32 / 255.0;
        (x * std::f32::consts::TAU).sin() * 0.25 + (y * std::f32::consts::TAU).cos() * 0.25 + 0.5
    });
    doc.layers.push(Layer::photo_height(
        "photo",
        photo,
        1.5,
        [2.0, 8.0, 24.0],
        [1.0, 0.7, 0.3],
    ));
    assert_parity("textured", &doc, Thresholds::TEXTURED);
}

/// Every selectable detail blend must match the CPU oracle, not just the
/// default derivative path. A wrong shader branch here is invisible in the
/// demo scene because all of its layers use Derivative.
#[test]
fn parity_detail_blend_modes() {
    use glam::{UVec2, Vec2};
    use ns_core::{BlendMode, LayerKind, Primitive, ShapeSource};

    for mode in [
        BlendMode::Derivative,
        BlendMode::Rnm,
        BlendMode::Replace,
        BlendMode::Whiteout,
        BlendMode::Udn,
    ] {
        let mut doc = Document::new(UVec2::splat(256));
        let mut base = Layer::shape(
            "base",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::splat(128.0),
                size: Vec2::splat(170.0),
                corner_radius: 24.0,
            }),
        );
        if let LayerKind::Shape(shape) = &mut base.kind {
            shape.radius_px = 16.0;
            shape.height_px = 40.0;
        }
        doc.layers.push(base);

        let mut detail = Layer::shape(
            "detail",
            ShapeSource::Primitive(Primitive::Rect {
                center: Vec2::new(140.0, 132.0),
                size: Vec2::new(120.0, 90.0),
                corner_radius: 10.0,
            }),
        );
        detail.blend = mode;
        if let LayerKind::Shape(shape) = &mut detail.kind {
            shape.radius_px = 9.0;
            shape.height_px = 14.0;
        }
        doc.layers.push(detail);

        assert_parity(&format!("blend-{mode:?}"), &doc, Thresholds::DETAIL_BLEND);
    }
}
