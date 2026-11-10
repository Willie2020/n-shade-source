//! Optional vendor-neutral GPU path tracer for lightmaps.
//!
//! This backend uses wgpu directly on a worker thread. It works on DirectX 12,
//! Vulkan and Metal adapters (NVIDIA, AMD, Intel, and software fallbacks) and
//! is selected by the `gpu` feature of `ns_bake`. The CPU path tracer remains
//! the default fallback when no adapter is available or a GPU resource limit is
//! exceeded.
//!
//! The GPU shader is intentionally close to the CPU tracer: a white diffuse
//! surface, one directional sun with shadow rays, environment next-event
//! sampling with multiple importance sampling, cosine-sampled indirect bounces,
//! an equirectangular environment, and the automatic unshadowed fill that keeps
//! surfaces facing away from the HDRI's bright regions lit. Samples are
//! submitted in small batches so low-end GPUs do not hit the Windows TDR
//! watchdog.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use wgpu::util::DeviceExt;

use crate::bvh::Bvh;
use crate::lightmap::{
    build_cdf, denoise, denoise_v2, environment_fill_radiance, finish_lightmap, luminance,
    prepare_lightmap, prepare_scene_lights, rasterize, rasterize_v2, sample_environment,
    BackendUsed, LightmapOutput, LightmapPipeline, LightmapSettings, SampledEnvironment,
};
use crate::{BakeError, BakeMesh, EnvironmentMap, Result};

const WORKGROUP_SIZE: u32 = 8;

#[repr(C, align(16))]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    size: [u32; 2],
    samples: u32,
    bounces: u32,
    sample_offset: u32,
    seed: u32,
    sun_enabled: u32,
    batch: u32,
    sun_dir: [f32; 4],
    sun_color: [f32; 4],
    env_params: [f32; 4],
    fill: [f32; 4],
    eps: f32,
    shadow_dist: f32,
    sun_cos_max: f32,
    /// 1 for ambient occlusion, 0 for a radiance bake.
    mode: u32,
    /// `x` is the number of scene lights; the rest is padding.
    light_info: [u32; 4],
    /// [v2] `x` feature bits, `y` global first sample of this dispatch,
    /// `z` total samples, `w` unused.
    v2_flags: [u32; 4],
    /// [v2] `x` raw mesh extent, `y` AO max distance in mesh units, `zw` unused.
    v2_misc: [f32; 4],
}

/// One scene light in raw mesh space. Matches `GpuLight` in the shader.
#[repr(C, align(16))]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuLight {
    /// `xyz` center, `w` kind (0 point, 1 spot, 2 area).
    position: [f32; 4],
    /// `xyz` emission axis, `w` outer cone cosine.
    direction: [f32; 4],
    /// `rgb` color × intensity, `w` inner cone cosine.
    color: [f32; 4],
    /// Half width, half height, area, one-sided flag (1 = two-sided).
    size: [f32; 4],
    /// `x` cutoff range, `yzw` padding.
    range: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuTriangle {
    p0: [f32; 4],
    p1: [f32; 4],
    p2: [f32; 4],
    n0: [f32; 4],
    n1: [f32; 4],
    n2: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuNode {
    min: [f32; 4],
    max: [f32; 4],
    links: [u32; 4],
}

struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_name: String,
}

impl GpuContext {
    fn new() -> Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .map_err(|error| {
            BakeError::GpuUnavailable(format!("no DirectX 12/Vulkan/Metal adapter found: {error}"))
        })?;

        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return Err(BakeError::GpuUnavailable(
                "only a software graphics adapter is available".into(),
            ));
        }
        let adapter_info = adapter.get_info();
        let adapter_name = format!("{} · {:?}", adapter_info.name, adapter_info.backend);

        let adapter_limits = adapter.limits();
        let required_limits = wgpu::Limits {
            max_storage_buffer_binding_size: adapter_limits.max_storage_buffer_binding_size,
            max_buffer_size: adapter_limits.max_buffer_size,
            ..wgpu::Limits::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("N-shade lightmap GPU"),
            required_features: wgpu::Features::empty(),
            required_limits,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|error| {
            BakeError::GpuUnavailable(format!("could not create a GPU device: {error}"))
        })?;

        Ok(Self {
            device,
            queue,
            adapter_name,
        })
    }
}

pub fn bake_lightmap_gpu(
    mesh: &BakeMesh,
    environment: Option<&EnvironmentMap>,
    settings: &LightmapSettings,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<LightmapOutput> {
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }
    let start = Instant::now();
    let prepared = prepare_lightmap(mesh, settings)?;
    progress.store(80, Ordering::Relaxed);
    let resolution = settings.output_size();
    let features = settings.features();
    let v2 = settings.pipeline == LightmapPipeline::V2;
    let gbuffer = if v2 {
        rasterize_v2(&prepared, resolution, &features)
    } else {
        rasterize(&prepared, resolution)
    };
    progress.store(150, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }

    let texel_count = resolution as usize * resolution as usize;
    let covered_count = gbuffer.covered.iter().filter(|covered| **covered).count();
    if covered_count == 0 {
        return Err(BakeError::Message(
            "No UV texels were covered. Check the UV layout and UDIM tile.".into(),
        ));
    }

    let bvh = Bvh::from_mesh(mesh)?;
    let (bvh_triangles, bvh_nodes) = bvh.gpu_geometry();
    if bvh_triangles.is_empty() || bvh_nodes.is_empty() {
        return Err(BakeError::Message(
            "The mesh has no triangles for GPU tracing".into(),
        ));
    }

    let context = GpuContext::new()?;
    let adapter_limits = context.device.limits();
    let gbuffer_bytes = texel_count as u64 * 16;
    if gbuffer_bytes > adapter_limits.max_storage_buffer_binding_size as u64
        || gbuffer_bytes > adapter_limits.max_buffer_size
    {
        return Err(BakeError::GpuUnavailable(format!(
            "{} x {} lightmap G-buffers exceed this GPU's storage-buffer limit",
            resolution, resolution
        )));
    }

    let triangles: Vec<GpuTriangle> = bvh_triangles
        .iter()
        .map(|triangle| GpuTriangle {
            p0: vec4(triangle.p[0], 1.0),
            p1: vec4(triangle.p[1], 1.0),
            p2: vec4(triangle.p[2], 1.0),
            n0: vec4(triangle.n[0], 0.0),
            n1: vec4(triangle.n[1], 0.0),
            n2: vec4(triangle.n[2], 0.0),
        })
        .collect();
    let nodes: Vec<GpuNode> = bvh_nodes
        .iter()
        .map(|node| GpuNode {
            min: [node.min.x, node.min.y, node.min.z, 0.0],
            max: [node.max.x, node.max.y, node.max.z, 0.0],
            links: [node.left, node.right, node.start, node.count],
        })
        .collect();

    let mut positions = Vec::with_capacity(texel_count);
    let mut normals = Vec::with_capacity(texel_count);
    for index in 0..texel_count {
        let covered = gbuffer.covered[index];
        let position = gbuffer.positions[index];
        let normal = gbuffer.normals[index];
        positions.push([
            position.x,
            position.y,
            position.z,
            if covered { 1.0 } else { 0.0 },
        ]);
        normals.push([
            normal.x,
            normal.y,
            normal.z,
            if v2 {
                pack_geo_normal(gbuffer.geo_normals[index])
            } else {
                0.0
            },
        ]);
    }

    let gpu_environment = environment_image(environment, settings);
    let fill = environment
        .map(|map| environment_fill_radiance(settings, map.average_radiance()))
        .unwrap_or(Vec3::ZERO);
    let environment_row_bytes = gpu_environment.width as usize * 16;
    let environment_padded_row = environment_row_bytes.div_ceil(256) * 256;

    let device = &context.device;
    let queue = &context.queue;

    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("lightmap path tracer"),
        source: wgpu::ShaderSource::Wgsl(include_str!("lightmap_gpu.wgsl").into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("lightmap bind group layout"),
        entries: &bind_group_layout_entries(),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("lightmap pipeline layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("lightmap path tracer"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });

    let triangle_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lightmap triangles"),
        contents: bytemuck::cast_slice(&triangles),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let node_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lightmap BVH"),
        contents: bytemuck::cast_slice(&nodes),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let position_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lightmap gbuffer positions"),
        contents: bytemuck::cast_slice(&positions),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let normal_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lightmap gbuffer normals"),
        contents: bytemuck::cast_slice(&normals),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lightmap radiance"),
        size: gbuffer_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lightmap readback"),
        size: gbuffer_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let environment_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lightmap environment"),
        size: wgpu::Extent3d {
            width: gpu_environment.width,
            height: gpu_environment.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &environment_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &gpu_environment.bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(environment_padded_row as u32),
            rows_per_image: Some(gpu_environment.height),
        },
        wgpu::Extent3d {
            width: gpu_environment.width,
            height: gpu_environment.height,
            depth_or_array_layers: 1,
        },
    );
    let environment_view = environment_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let row_cdf_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lightmap environment row cdf"),
        contents: bytemuck::cast_slice(&gpu_environment.rows),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let col_cdf_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lightmap environment column cdf"),
        contents: bytemuck::cast_slice(&gpu_environment.cols),
        usage: wgpu::BufferUsages::STORAGE,
    });

    let mut params = Params {
        size: [resolution, resolution],
        samples: settings.effective_samples(),
        bounces: if settings.lighting.uses_bounces() {
            settings.effective_bounces()
        } else {
            0
        },
        sample_offset: 0,
        seed: settings.seed,
        sun_enabled: u32::from(
            settings.lighting.uses_sun() && settings.sun_enabled && settings.sun_intensity > 0.0,
        ),
        batch: 1,
        sun_dir: {
            let direction = settings.sun_direction();
            [direction.x, direction.y, direction.z, 0.0]
        },
        sun_color: {
            let color = Vec3::from_array(settings.sun_color) * settings.sun_intensity.max(0.0);
            [color.x, color.y, color.z, 0.0]
        },
        env_params: [
            gpu_environment.width as f32,
            gpu_environment.height as f32,
            if settings.lighting.uses_environment() {
                gpu_environment.total
            } else {
                0.0
            },
            gpu_environment.strength,
        ],
        fill: if settings.lighting.uses_environment() {
            [fill.x, fill.y, fill.z, 0.0]
        } else {
            [0.0; 4]
        },
        eps: (prepared.extent * 5.0e-4).max(1.0e-5),
        shadow_dist: (prepared.extent * 8.0).max(1.0),
        sun_cos_max: settings.sun_size_deg.clamp(0.0, 30.0).to_radians().cos(),
        mode: u32::from(settings.lighting.is_ambient_occlusion()),
        light_info: [0; 4],
        v2_flags: [
            u32::from(features.low_discrepancy)
                | (u32::from(features.geometric_offsets) << 1)
                | (u32::from(features.env_bilinear) << 2)
                | (u32::from(features.variance_denoise) << 3)
                | (u32::from(features.soft_ao) << 4)
                | (u32::from(features.area_emitter_cosine) << 5),
            0,
            settings.effective_samples(),
            0,
        ],
        v2_misc: [
            prepared.extent,
            settings.ao_max_distance.max(0.0) * prepared.extent,
            0.0,
            0.0,
        ],
    };

    // Scene lights use the same prepared form as the CPU tracer. The buffer
    // always holds at least one element because wgpu rejects zero-sized
    // storage bindings; the shader reads the count from `Params`.
    let scene_lights = prepare_scene_lights(settings, prepared.extent, prepared.center);
    let gpu_lights: Vec<GpuLight> = scene_lights
        .iter()
        .map(|light| GpuLight {
            position: [
                light.position.x,
                light.position.y,
                light.position.z,
                light.kind as f32,
            ],
            direction: [
                light.direction.x,
                light.direction.y,
                light.direction.z,
                light.cos_outer,
            ],
            color: [
                light.radiance.x,
                light.radiance.y,
                light.radiance.z,
                light.cos_inner,
            ],
            size: [
                light.half_size[0],
                light.half_size[1],
                light.area,
                if light.two_sided { 1.0 } else { 0.0 },
            ],
            range: [light.range, 0.0, 0.0, 0.0],
        })
        .collect();
    params.light_info[0] = gpu_lights.len() as u32;
    let mut light_buffer_data = gpu_lights.clone();
    if light_buffer_data.is_empty() {
        light_buffer_data.push(GpuLight::zeroed());
    }
    let light_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lightmap scene lights"),
        contents: bytemuck::cast_slice(&light_buffer_data),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lightmap params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lightmap bind group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: triangle_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: node_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: position_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: normal_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: output_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(&environment_view),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: row_cdf_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: col_cdf_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 9,
                resource: light_buffer.as_entire_binding(),
            },
        ],
    });

    let samples = settings.effective_samples();
    let batch = if resolution >= 2048 {
        1
    } else if resolution >= 1024 {
        2
    } else {
        4
    };
    let workgroups = resolution.div_ceil(WORKGROUP_SIZE);
    let mut offset = 0u32;
    while offset < samples {
        if cancel.load(Ordering::Relaxed) {
            return Err(BakeError::Cancelled);
        }
        params.sample_offset = offset;
        params.batch = batch.min(samples - offset);
        params.v2_flags[1] = offset;
        queue.write_buffer(&params_buffer, 0, bytemuck::bytes_of(&params));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lightmap trace"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("lightmap trace"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, workgroups, 1);
        }
        queue.submit(Some(encoder.finish()));
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        offset += params.batch;
        let fraction = offset as f32 / samples as f32;
        progress.store(150 + (fraction * 680.0) as u32, Ordering::Relaxed);
    }

    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(BakeError::GpuUnavailable(format!(
            "GPU validation error: {error}"
        )));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }

    let output_slice = {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lightmap readback"),
        });
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &readback_buffer, 0, gbuffer_bytes);
        queue.submit(Some(encoder.finish()));
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        readback_buffer.slice(..)
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    output_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver
        .recv()
        .map_err(|_| BakeError::GpuUnavailable("GPU readback channel closed".into()))?
        .map_err(|error| BakeError::GpuUnavailable(format!("GPU readback failed: {error:?}")))?;

    let mapped = output_slice.get_mapped_range();
    let values: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
    let mut radiance = vec![Vec3::ZERO; texel_count];
    let mut variance = if v2 {
        vec![0.0f32; texel_count]
    } else {
        Vec::new()
    };
    let sample_scale = 1.0 / samples as f32;
    let sample_count = samples as f32;
    for (index, value) in values.iter().enumerate().take(texel_count) {
        radiance[index] = Vec3::new(value[0], value[1], value[2]) * sample_scale;
        if v2 {
            let luminance_sum = luminance(Vec3::new(value[0], value[1], value[2]));
            let luminance_squared_sum = value[3];
            variance[index] = if sample_count < 2.0 {
                0.0
            } else {
                ((luminance_squared_sum - luminance_sum * luminance_sum / sample_count)
                    / (sample_count * (sample_count - 1.0)))
                    .max(0.0)
            };
        }
    }
    drop(mapped);
    readback_buffer.unmap();
    progress.store(880, Ordering::Relaxed);

    let denoiser = if v2 {
        denoise_v2(
            &mut radiance,
            &mut variance,
            &gbuffer,
            prepared.extent,
            resolution,
            settings,
        )
    } else {
        denoise(
            &mut radiance,
            &gbuffer,
            prepared.extent,
            resolution,
            settings,
        )
    };
    progress.store(930, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err(BakeError::Cancelled);
    }

    let mut output = finish_lightmap(
        &mut radiance,
        &gbuffer,
        resolution,
        settings,
        start.elapsed().as_secs_f32() * 1000.0,
        covered_count,
        prepared.triangle_count(),
        0,
        0,
        denoiser,
        BackendUsed::Gpu,
    );
    output.backend_name = Some(context.adapter_name.clone());
    progress.store(1000, Ordering::Relaxed);
    Ok(output)
}

fn bind_group_layout_entries() -> [wgpu::BindGroupLayoutEntry; 10] {
    let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
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
        storage(1, true),
        storage(2, true),
        storage(3, true),
        storage(4, true),
        storage(5, false),
        storage(7, true),
        storage(8, true),
        storage(9, true),
        wgpu::BindGroupLayoutEntry {
            binding: 6,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
    ]
}

fn vec4(value: Vec3, w: f32) -> [f32; 4] {
    [value.x, value.y, value.z, w]
}

fn sign_nz(value: f32) -> f32 {
    if value >= 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// Pack a geometric normal into the free `w` channel of the G-buffer normals.
///
/// A 24-bit octahedral code is exactly representable in an `f32`, so no bitcast
/// is involved and there is no NaN risk. The shader mirrors this in
/// `unpack_geo_normal`.
fn pack_geo_normal(normal: Vec3) -> f32 {
    let l1 = normal.x.abs() + normal.y.abs() + normal.z.abs();
    if l1 < 1e-12 {
        return 0.0;
    }
    let (mut x, mut y) = (normal.x / l1, normal.y / l1);
    if normal.z < 0.0 {
        let (ox, oy) = ((1.0 - y.abs()) * sign_nz(x), (1.0 - x.abs()) * sign_nz(y));
        x = ox;
        y = oy;
    }
    let qx = ((x * 0.5 + 0.5) * 4095.0).round() as u32;
    let qy = ((y * 0.5 + 0.5) * 4095.0).round() as u32;
    (qx * 4096 + qy) as f32
}

#[cfg(test)]
fn unpack_geo_normal(value: f32) -> Vec3 {
    let packed = value as u32;
    let x = (packed / 4096) as f32 / 4095.0 * 2.0 - 1.0;
    let y = (packed % 4096) as f32 / 4095.0 * 2.0 - 1.0;
    let z = 1.0 - x.abs() - y.abs();
    let normal = if z < 0.0 {
        Vec3::new(
            (1.0 - y.abs()) * sign_nz(x),
            (1.0 - x.abs()) * sign_nz(y),
            z,
        )
    } else {
        Vec3::new(x, y, z)
    };
    normal.normalize_or_zero()
}

struct GpuEnvironment {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    total: f32,
    strength: f32,
    rows: Vec<f32>,
    cols: Vec<f32>,
}

fn environment_image(
    environment: Option<&EnvironmentMap>,
    settings: &LightmapSettings,
) -> GpuEnvironment {
    let sampled = if let Some(environment) = environment {
        sample_environment(environment, 2048)
    } else {
        let color = Vec3::from_array(settings.ambient_color).max(Vec3::ZERO)
            * settings.ambient_intensity.max(0.0);
        SampledEnvironment {
            width: 1,
            height: 1,
            radiance: vec![color],
        }
    };
    let cdf = build_cdf(&sampled.radiance, sampled.width, sampled.height);
    let strength = if environment.is_some() {
        settings.environment_strength.max(0.0)
    } else {
        1.0
    };

    let row_bytes = sampled.width as usize * 16;
    let padded_row = row_bytes.div_ceil(256) * 256;
    let mut bytes = vec![0u8; padded_row * sampled.height as usize];
    for y in 0..sampled.height as usize {
        for x in 0..sampled.width as usize {
            let value = sampled.radiance[y * sampled.width as usize + x];
            let target = y * padded_row + x * 16;
            bytes[target..target + 16].copy_from_slice(bytemuck::cast_slice(&[vec4(value, 1.0)]));
        }
    }

    GpuEnvironment {
        bytes,
        width: sampled.width,
        height: sampled.height,
        total: cdf.total,
        strength,
        rows: cdf.rows,
        cols: cdf.cols,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu_unavailable(test: &str, reason: &dyn std::fmt::Display) {
        let required =
            std::env::var_os("NS_REQUIRE_GPU").is_some_and(|v| v != "0" && !v.is_empty());
        assert!(
            !required,
            "{test}: GPU unavailable ({reason}) and NS_REQUIRE_GPU is set"
        );
        eprintln!("{test}: SKIPPED, GPU unavailable ({reason})");
    }

    #[test]
    fn params_layout_is_append_only_and_16_byte_aligned() {
        // The shader mirrors this layout; a mismatch reads the wrong values
        // instead of failing validation.
        assert_eq!(std::mem::size_of::<Params>(), 160);
        assert_eq!(std::mem::align_of::<Params>(), 16);
        assert_eq!(std::mem::offset_of!(Params, light_info), 112);
        assert_eq!(std::mem::offset_of!(Params, v2_flags), 128);
        assert_eq!(std::mem::offset_of!(Params, v2_misc), 144);
    }

    #[test]
    fn geo_normal_pack_roundtrip() {
        let mut seed = 7u32;
        for _ in 0..10_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = (seed >> 8) as f32 / 16_777_216.0 * 2.0 - 1.0;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let y = (seed >> 8) as f32 / 16_777_216.0 * 2.0 - 1.0;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let z = (seed >> 8) as f32 / 16_777_216.0 * 2.0 - 1.0;
            let normal = Vec3::new(x, y, z);
            if normal.length_squared() < 1e-6 {
                continue;
            }
            let normal = normal.normalize();
            let unpacked = unpack_geo_normal(pack_geo_normal(normal));
            assert!(
                normal.dot(unpacked) > 0.999_99,
                "{normal:?} -> {unpacked:?}"
            );
        }
    }
    use crate::lightmap::prepare_lightmap;
    use crate::BakeMesh;
    use std::sync::atomic::{AtomicBool, AtomicU32};

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
    fn gpu_ambient_occlusion_matches_the_cpu_when_available() {
        let mesh = plane_with_blocker();
        let settings = LightmapSettings {
            resolution: 16,
            backend: crate::LightmapBackend::Cpu,
            lighting: crate::LightmapLightingMode::AmbientOcclusion,
            samples: 256,
            bounces: 0,
            padding: 0,
            denoiser: crate::LightmapDenoiser::None,
            sun_enabled: false,
            sun_intensity: 0.0,
            ambient_intensity: 0.0,
            ..Default::default()
        };
        let progress = AtomicU32::new(0);
        let cancel = AtomicBool::new(false);
        let cpu =
            crate::bake_lightmap(&mesh, None, &settings, &progress, &cancel).expect("cpu bake");
        let mut gpu_settings = settings.clone();
        gpu_settings.backend = crate::LightmapBackend::Gpu;
        match bake_lightmap_gpu(&mesh, None, &gpu_settings, &progress, &cancel) {
            Ok(gpu) => {
                let center = ((gpu.height / 2) * gpu.width + gpu.width / 2) as usize * 4;
                let cpu_center = &cpu.pixels[center..center + 4];
                let gpu_center = &gpu.pixels[center..center + 4];
                assert!(
                    (i16::from(cpu_center[0]) - i16::from(gpu_center[0])).abs() <= 16,
                    "cpu {cpu_center:?} gpu {gpu_center:?}"
                );
                assert_eq!(gpu_center[0], gpu_center[1]);
                assert_eq!(gpu_center[1], gpu_center[2]);
                let corner = ((2 * gpu.width as usize) + 2) * 4;
                assert!(gpu.pixels[corner] > 235, "open sky stays bright");
            }
            Err(BakeError::GpuUnavailable(reason)) => gpu_unavailable(
                "gpu_ambient_occlusion_matches_the_cpu_when_available",
                &reason,
            ),
            Err(error) => panic!("GPU AO bake failed: {error}"),
        }
    }

    #[test]
    fn gpu_environment_matches_the_cpu_when_available() {
        let mesh = plane();
        let environment = EnvironmentMap::new(8, 4, vec![[1.0, 1.0, 1.0]; 32]).unwrap();
        let settings = LightmapSettings {
            resolution: 16,
            backend: crate::LightmapBackend::Cpu,
            samples: 64,
            bounces: 0,
            padding: 0,
            denoiser: crate::LightmapDenoiser::None,
            sun_enabled: false,
            sun_intensity: 0.0,
            ambient_intensity: 0.0,
            ..Default::default()
        };
        let progress = AtomicU32::new(0);
        let cancel = AtomicBool::new(false);
        let cpu = crate::bake_lightmap(&mesh, Some(&environment), &settings, &progress, &cancel)
            .expect("cpu bake");
        let mut gpu_settings = settings.clone();
        gpu_settings.backend = crate::LightmapBackend::Gpu;
        match bake_lightmap_gpu(&mesh, Some(&environment), &gpu_settings, &progress, &cancel) {
            Ok(gpu) => {
                let index = ((gpu.height / 2) * gpu.width + gpu.width / 2) as usize * 4;
                let cpu_pixel = &cpu.pixels[index..index + 4];
                let gpu_pixel = &gpu.pixels[index..index + 4];
                assert!(
                    (i16::from(cpu_pixel[0]) - i16::from(gpu_pixel[0])).abs() <= 16,
                    "cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
            }
            Err(BakeError::GpuUnavailable(reason)) => {
                gpu_unavailable("gpu_environment_matches_the_cpu_when_available", &reason)
            }
            Err(error) => panic!("GPU environment bake failed: {error}"),
        }
    }

    #[test]
    fn gpu_environment_fill_matches_the_cpu_when_available() {
        let mesh = plane();
        // Bright only where z < 0, so the +Z-facing plane sees no environment
        // radiance and the result is the automatic fill alone.
        let mut pixels = vec![[0.0f32; 3]; 32];
        for row in 0..4 {
            for column in 6..8 {
                pixels[row * 8 + column] = [2.0, 2.0, 2.0];
            }
        }
        let environment = EnvironmentMap::new(8, 4, pixels).unwrap();
        let settings = LightmapSettings {
            resolution: 16,
            backend: crate::LightmapBackend::Cpu,
            samples: 16,
            bounces: 0,
            padding: 0,
            denoiser: crate::LightmapDenoiser::None,
            sun_enabled: false,
            sun_intensity: 0.0,
            ambient_color: [1.0, 1.0, 1.0],
            ambient_intensity: 0.4,
            environment_fill: 1.0,
            ..Default::default()
        };
        let progress = AtomicU32::new(0);
        let cancel = AtomicBool::new(false);
        let cpu = crate::bake_lightmap(&mesh, Some(&environment), &settings, &progress, &cancel)
            .expect("cpu bake");
        let mut gpu_settings = settings.clone();
        gpu_settings.backend = crate::LightmapBackend::Gpu;
        match bake_lightmap_gpu(&mesh, Some(&environment), &gpu_settings, &progress, &cancel) {
            Ok(gpu) => {
                let index = ((gpu.height / 2) * gpu.width + gpu.width / 2) as usize * 4;
                let cpu_pixel = &cpu.pixels[index..index + 4];
                let gpu_pixel = &gpu.pixels[index..index + 4];
                assert!(
                    (i16::from(cpu_pixel[0]) - i16::from(gpu_pixel[0])).abs() <= 2,
                    "cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
                assert!(
                    cpu_pixel[0] > 100,
                    "fill should light the back: {cpu_pixel:?}"
                );
            }
            Err(BakeError::GpuUnavailable(reason)) => gpu_unavailable(
                "gpu_environment_fill_matches_the_cpu_when_available",
                &reason,
            ),
            Err(error) => panic!("GPU environment fill bake failed: {error}"),
        }
    }

    #[test]
    fn gpu_backend_matches_the_cpu_sun_when_available() {
        let mesh = plane();
        let settings = LightmapSettings {
            resolution: 16,
            backend: crate::LightmapBackend::Gpu,
            samples: 4,
            bounces: 0,
            padding: 0,
            denoiser: crate::LightmapDenoiser::None,
            sun_enabled: true,
            sun_azimuth_deg: 90.0,
            sun_elevation_deg: 0.0,
            sun_size_deg: 0.0,
            sun_intensity: 1.0,
            ambient_intensity: 0.0,
            ..Default::default()
        };
        let progress = AtomicU32::new(0);
        let cancel = AtomicBool::new(false);
        match bake_lightmap_gpu(&mesh, None, &settings, &progress, &cancel) {
            Ok(output) => {
                let index = ((output.height / 2) * output.width + output.width / 2) as usize * 4;
                assert_eq!(output.pixels[index + 3], 255);
                assert!(
                    output.pixels[index] >= 245,
                    "{:?}",
                    &output.pixels[index..index + 4]
                );
                assert_eq!(output.stats.backend, BackendUsed::Gpu);
                let _ = prepare_lightmap(&mesh, &settings).expect("cpu preparation still works");
            }
            Err(BakeError::GpuUnavailable(reason)) => {
                gpu_unavailable("gpu_backend_matches_the_cpu_sun_when_available", &reason)
            }
            Err(error) => panic!("GPU bake failed: {error}"),
        }
    }

    #[test]
    fn gpu_area_light_uses_the_emitter_cosine_when_available() {
        let mesh = plane();
        let settings = LightmapSettings {
            resolution: 16,
            backend: crate::LightmapBackend::Cpu,
            samples: 64,
            bounces: 0,
            padding: 0,
            denoiser: crate::LightmapDenoiser::None,
            sun_enabled: false,
            sun_intensity: 0.0,
            ambient_intensity: 0.0,
            lights: vec![crate::LightmapSceneLight {
                position: [0.0, 0.0, 1.0],
                target: [0.0, 0.0, 0.0],
                area_size: [1.0, 1.0],
                intensity: 2.0,
                range: 0.0,
                ..crate::LightmapSceneLight::new(crate::LightmapLightKind::Area)
            }],
            ..Default::default()
        };
        let progress = AtomicU32::new(0);
        let cancel = AtomicBool::new(false);
        let cpu =
            crate::bake_lightmap(&mesh, None, &settings, &progress, &cancel).expect("cpu bake");
        let mut gpu_settings = settings.clone();
        gpu_settings.backend = crate::LightmapBackend::Gpu;
        match bake_lightmap_gpu(&mesh, None, &gpu_settings, &progress, &cancel) {
            Ok(gpu) => {
                let pixel = ((gpu.height / 2) * gpu.width + gpu.width / 2) as usize;
                let cpu_pixel = &cpu.pixels[pixel * 4..pixel * 4 + 4];
                let gpu_pixel = &gpu.pixels[pixel * 4..pixel * 4 + 4];
                assert!(
                    (i16::from(cpu_pixel[0]) - i16::from(gpu_pixel[0])).abs() <= 8,
                    "cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
                // Both backends must include the Lambertian emitter cosine:
                // 0.752 closed form for a 1x intensity area light, scaled by 2.
                let radiance = cpu.hdr[pixel * 3];
                assert!(
                    (radiance - 1.504).abs() < 0.08,
                    "center radiance {radiance}, expected 1.504"
                );
            }
            Err(BakeError::GpuUnavailable(reason)) => gpu_unavailable(
                "gpu_area_light_uses_the_emitter_cosine_when_available",
                &reason,
            ),
            Err(error) => panic!("GPU area-light bake failed: {error}"),
        }
    }

    #[test]
    fn gpu_parameter_layout_matches_the_shader() {
        // The WGSL structs use vec4-aligned members; these sizes are the
        // contract between `Params`/`GpuLight` and `lightmap_gpu.wgsl`.
        assert_eq!(std::mem::size_of::<Params>(), 160);
        assert_eq!(std::mem::size_of::<GpuLight>(), 80);
        assert_eq!(std::mem::align_of::<Params>(), 16);
    }

    #[test]
    fn gpu_scene_lights_match_the_cpu_when_available() {
        let mesh = plane();
        let settings = LightmapSettings {
            resolution: 16,
            backend: crate::LightmapBackend::Cpu,
            samples: 64,
            bounces: 0,
            padding: 0,
            denoiser: crate::LightmapDenoiser::None,
            sun_enabled: false,
            sun_intensity: 0.0,
            ambient_intensity: 0.0,
            lights: vec![crate::LightmapSceneLight {
                position: [0.0, 0.0, 1.0],
                intensity: 2.0,
                range: 0.0,
                ..crate::LightmapSceneLight::new(crate::LightmapLightKind::Point)
            }],
            ..Default::default()
        };
        let progress = AtomicU32::new(0);
        let cancel = AtomicBool::new(false);
        let cpu =
            crate::bake_lightmap(&mesh, None, &settings, &progress, &cancel).expect("cpu bake");
        let mut gpu_settings = settings.clone();
        gpu_settings.backend = crate::LightmapBackend::Gpu;
        match bake_lightmap_gpu(&mesh, None, &gpu_settings, &progress, &cancel) {
            Ok(gpu) => {
                let index = ((gpu.height / 2) * gpu.width + gpu.width / 2) as usize * 4;
                let cpu_pixel = &cpu.pixels[index..index + 4];
                let gpu_pixel = &gpu.pixels[index..index + 4];
                assert!(
                    (i16::from(cpu_pixel[0]) - i16::from(gpu_pixel[0])).abs() <= 4,
                    "cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
                assert!(gpu_pixel[0] > 100, "{gpu_pixel:?}");
            }
            Err(BakeError::GpuUnavailable(reason)) => {
                gpu_unavailable("gpu_scene_lights_match_the_cpu_when_available", &reason)
            }
            Err(error) => panic!("GPU scene-light bake failed: {error}"),
        }
    }
}
