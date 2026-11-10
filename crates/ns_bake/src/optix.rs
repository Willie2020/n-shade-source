//! Optional NVIDIA OptiX HDR denoiser loaded at runtime.
//!
//! OptiX is not a build dependency: the module loads `nvoptix.dll` (Windows)
//! or `libnvoptix.so.1` (Linux) from the installed NVIDIA driver and queries
//! the ABI 60 function table. CUDA driver allocations and a stream come from
//! `nvcuda.dll` / `libcuda.so.1`. When any of this is unavailable the caller
//! keeps the built-in edge-aware denoiser.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::mem::{size_of_val, zeroed};
use std::path::PathBuf;
use std::ptr;
use std::sync::OnceLock;

use glam::Vec3;
use libloading::Library;

const OPTIX_ABI_VERSION: c_int = 60;
const OPTIX_DENOISER_MODEL_KIND_HDR: c_int = 0x2323;
// OptixPixelFormat from the OptiX headers. The enum is not sequential:
// FLOAT4 = 0x2204 (HALF2, for example, is 0x2207). Color and guide images
// use FLOAT4.
const OPTIX_PIXEL_FORMAT_FLOAT4: c_int = 0x2204;
const OPTIX_DENOISER_ALPHA_MODE_COPY: c_int = 0;
const FUNCTION_TABLE_ENTRIES: usize = 43;

// Function-table indices for OptiX 7.5 / ABI 60.
const TABLE_GET_ERROR_NAME: usize = 0;
const TABLE_DEVICE_CONTEXT_CREATE: usize = 2;
const TABLE_DEVICE_CONTEXT_DESTROY: usize = 3;
const TABLE_DENOISER_CREATE: usize = 35;
const TABLE_DENOISER_DESTROY: usize = 36;
const TABLE_DENOISER_MEMORY_RESOURCES: usize = 37;
const TABLE_DENOISER_SETUP: usize = 38;
const TABLE_DENOISER_INVOKE: usize = 39;
const TABLE_DENOISER_INTENSITY: usize = 40;

type CuContext = *mut c_void;
type CuStream = *mut c_void;
type CuDeviceptr = u64;
type OptixDeviceContext = *mut c_void;
type OptixDenoiser = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OptixImage2D {
    data: CuDeviceptr,
    width: u32,
    height: u32,
    row_stride_in_bytes: u32,
    pixel_stride_in_bytes: u32,
    format: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OptixDenoiserOptions {
    guide_albedo: c_uint,
    guide_normal: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OptixDenoiserSizes {
    state_size_in_bytes: usize,
    with_overlap_scratch_size_in_bytes: usize,
    without_overlap_scratch_size_in_bytes: usize,
    overlap_window_size_in_pixels: c_uint,
    compute_average_color_size_in_bytes: usize,
    compute_intensity_size_in_bytes: usize,
    internal_guide_layer_pixel_size_in_bytes: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OptixDenoiserGuideLayer {
    albedo: OptixImage2D,
    normal: OptixImage2D,
    flow: OptixImage2D,
    previous_output_internal_guide_layer: OptixImage2D,
    output_internal_guide_layer: OptixImage2D,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OptixDenoiserLayer {
    input: OptixImage2D,
    previous_output: OptixImage2D,
    output: OptixImage2D,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OptixDenoiserParams {
    denoise_alpha: c_int,
    hdr_intensity: CuDeviceptr,
    blend_factor: f32,
    hdr_average_color: CuDeviceptr,
    temporal_mode_use_previous_layers: c_uint,
}

#[repr(C)]
struct OptixDeviceContextOptions {
    log_callback_function:
        Option<unsafe extern "C" fn(c_uint, *const c_char, *const c_char, *mut c_void)>,
    log_callback_data: *mut c_void,
    log_callback_level: c_int,
    validation_mode: c_int,
}

impl Default for OptixDeviceContextOptions {
    fn default() -> Self {
        unsafe { zeroed() }
    }
}

type QueryFunctionTable = unsafe extern "C" fn(
    c_int,
    c_uint,
    *mut c_int,
    *const *const c_void,
    *mut c_void,
    usize,
) -> c_int;
type DeviceContextCreate = unsafe extern "C" fn(
    CuContext,
    *const OptixDeviceContextOptions,
    *mut OptixDeviceContext,
) -> c_int;
type DeviceContextDestroy = unsafe extern "C" fn(OptixDeviceContext) -> c_int;
type DenoiserCreate = unsafe extern "C" fn(
    OptixDeviceContext,
    c_int,
    *const OptixDenoiserOptions,
    *mut OptixDenoiser,
) -> c_int;
type DenoiserDestroy = unsafe extern "C" fn(OptixDenoiser) -> c_int;
type DenoiserMemoryResources =
    unsafe extern "C" fn(OptixDenoiser, c_uint, c_uint, *mut OptixDenoiserSizes) -> c_int;
type DenoiserSetup = unsafe extern "C" fn(
    OptixDenoiser,
    CuStream,
    c_uint,
    c_uint,
    CuDeviceptr,
    usize,
    CuDeviceptr,
    usize,
) -> c_int;
type DenoiserInvoke = unsafe extern "C" fn(
    OptixDenoiser,
    CuStream,
    *const OptixDenoiserParams,
    CuDeviceptr,
    usize,
    *const OptixDenoiserGuideLayer,
    *const OptixDenoiserLayer,
    c_uint,
    c_uint,
    c_uint,
    CuDeviceptr,
    usize,
) -> c_int;
type DenoiserIntensity = unsafe extern "C" fn(
    OptixDenoiser,
    CuStream,
    *const OptixImage2D,
    CuDeviceptr,
    CuDeviceptr,
    usize,
) -> c_int;
type ErrorName = unsafe extern "C" fn(c_int) -> *const c_char;

#[derive(Clone, Copy)]
struct OptixApi {
    device_context_create: DeviceContextCreate,
    device_context_destroy: DeviceContextDestroy,
    denoiser_create: DenoiserCreate,
    denoiser_destroy: DenoiserDestroy,
    denoiser_memory_resources: DenoiserMemoryResources,
    denoiser_setup: DenoiserSetup,
    denoiser_invoke: DenoiserInvoke,
    denoiser_intensity: DenoiserIntensity,
    error_name: Option<ErrorName>,
}

impl OptixApi {
    fn load(library: &Library) -> Result<Self, String> {
        let query: libloading::Symbol<QueryFunctionTable> =
            unsafe { library.get(b"optixQueryFunctionTable\0") }
                .map_err(|error| format!("optixQueryFunctionTable missing: {error}"))?;
        let mut table = [0usize; FUNCTION_TABLE_ENTRIES];
        let result = unsafe {
            query(
                OPTIX_ABI_VERSION,
                0,
                ptr::null_mut(),
                ptr::null(),
                table.as_mut_ptr() as *mut c_void,
                size_of_val(&table),
            )
        };
        if result != 0 {
            return Err(format!(
                "optixQueryFunctionTable(ABI {OPTIX_ABI_VERSION}) failed: {result}"
            ));
        }

        let function = |index: usize, name: &str| -> Result<usize, String> {
            let address = table.get(index).copied().unwrap_or(0);
            if address == 0 {
                Err(format!("OptiX function table has no {name}"))
            } else {
                Ok(address)
            }
        };
        Ok(Self {
            device_context_create: unsafe {
                std::mem::transmute::<usize, DeviceContextCreate>(function(
                    TABLE_DEVICE_CONTEXT_CREATE,
                    "optixDeviceContextCreate",
                )?)
            },
            device_context_destroy: unsafe {
                std::mem::transmute::<usize, DeviceContextDestroy>(function(
                    TABLE_DEVICE_CONTEXT_DESTROY,
                    "optixDeviceContextDestroy",
                )?)
            },
            denoiser_create: unsafe {
                std::mem::transmute::<usize, DenoiserCreate>(function(
                    TABLE_DENOISER_CREATE,
                    "optixDenoiserCreate",
                )?)
            },
            denoiser_destroy: unsafe {
                std::mem::transmute::<usize, DenoiserDestroy>(function(
                    TABLE_DENOISER_DESTROY,
                    "optixDenoiserDestroy",
                )?)
            },
            denoiser_memory_resources: unsafe {
                std::mem::transmute::<usize, DenoiserMemoryResources>(function(
                    TABLE_DENOISER_MEMORY_RESOURCES,
                    "optixDenoiserComputeMemoryResources",
                )?)
            },
            denoiser_setup: unsafe {
                std::mem::transmute::<usize, DenoiserSetup>(function(
                    TABLE_DENOISER_SETUP,
                    "optixDenoiserSetup",
                )?)
            },
            denoiser_invoke: unsafe {
                std::mem::transmute::<usize, DenoiserInvoke>(function(
                    TABLE_DENOISER_INVOKE,
                    "optixDenoiserInvoke",
                )?)
            },
            denoiser_intensity: unsafe {
                std::mem::transmute::<usize, DenoiserIntensity>(function(
                    TABLE_DENOISER_INTENSITY,
                    "optixDenoiserComputeIntensity",
                )?)
            },
            error_name: {
                let address = table.get(TABLE_GET_ERROR_NAME).copied().unwrap_or(0);
                if address == 0 {
                    None
                } else {
                    Some(unsafe { std::mem::transmute::<usize, ErrorName>(address) })
                }
            },
        })
    }

    fn error_name(self, code: c_int) -> String {
        let Some(name) = self.error_name else {
            return String::new();
        };
        let pointer = unsafe { name(code) };
        if pointer.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned()
        }
    }
}

#[derive(Clone, Copy)]
struct CudaApi {
    init: unsafe extern "C" fn(c_uint) -> c_int,
    device_get: unsafe extern "C" fn(*mut c_int, c_int) -> c_int,
    ctx_create: unsafe extern "C" fn(*mut CuContext, c_uint, c_int) -> c_int,
    ctx_set_current: unsafe extern "C" fn(CuContext) -> c_int,
    ctx_destroy: unsafe extern "C" fn(CuContext) -> c_int,
    stream_create: unsafe extern "C" fn(*mut CuStream, c_uint) -> c_int,
    stream_synchronize: unsafe extern "C" fn(CuStream) -> c_int,
    stream_destroy: unsafe extern "C" fn(CuStream) -> c_int,
    mem_alloc: unsafe extern "C" fn(*mut CuDeviceptr, usize) -> c_int,
    mem_free: unsafe extern "C" fn(CuDeviceptr) -> c_int,
    memcpy_htod: unsafe extern "C" fn(CuDeviceptr, *const c_void, usize) -> c_int,
    memcpy_dtoh: unsafe extern "C" fn(*mut c_void, CuDeviceptr, usize) -> c_int,
}

fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
    let symbol: libloading::Symbol<T> =
        unsafe { library.get(name) }.map_err(|error| error.to_string())?;
    Ok(*symbol)
}

macro_rules! cuda_fn {
    ($library:expr, $name:literal, $ty:ty) => {
        load_symbol::<$ty>($library, concat!($name, "\0").as_bytes())
    };
}

impl CudaApi {
    fn load(library: &Library) -> Result<Self, String> {
        Ok(Self {
            init: cuda_fn!(library, "cuInit", unsafe extern "C" fn(c_uint) -> c_int)?,
            device_get: cuda_fn!(
                library,
                "cuDeviceGet",
                unsafe extern "C" fn(*mut c_int, c_int) -> c_int
            )?,
            ctx_create: cuda_fn!(
                library,
                "cuCtxCreate_v2",
                unsafe extern "C" fn(*mut CuContext, c_uint, c_int) -> c_int
            )?,
            ctx_set_current: cuda_fn!(
                library,
                "cuCtxSetCurrent",
                unsafe extern "C" fn(CuContext) -> c_int
            )?,
            ctx_destroy: cuda_fn!(
                library,
                "cuCtxDestroy_v2",
                unsafe extern "C" fn(CuContext) -> c_int
            )?,
            stream_create: cuda_fn!(
                library,
                "cuStreamCreate",
                unsafe extern "C" fn(*mut CuStream, c_uint) -> c_int
            )?,
            stream_synchronize: cuda_fn!(
                library,
                "cuStreamSynchronize",
                unsafe extern "C" fn(CuStream) -> c_int
            )?,
            stream_destroy: cuda_fn!(
                library,
                "cuStreamDestroy_v2",
                unsafe extern "C" fn(CuStream) -> c_int
            )?,
            mem_alloc: cuda_fn!(
                library,
                "cuMemAlloc_v2",
                unsafe extern "C" fn(*mut CuDeviceptr, usize) -> c_int
            )?,
            mem_free: cuda_fn!(
                library,
                "cuMemFree_v2",
                unsafe extern "C" fn(CuDeviceptr) -> c_int
            )?,
            memcpy_htod: cuda_fn!(
                library,
                "cuMemcpyHtoD_v2",
                unsafe extern "C" fn(CuDeviceptr, *const c_void, usize) -> c_int
            )?,
            memcpy_dtoh: cuda_fn!(
                library,
                "cuMemcpyDtoH_v2",
                unsafe extern "C" fn(*mut c_void, CuDeviceptr, usize) -> c_int
            )?,
        })
    }
}

struct Session {
    _cuda_library: Library,
    cuda: CudaApi,
    _optix_library: Option<Library>,
    optix: Option<OptixApi>,
    context: CuContext,
    stream: CuStream,
    device_context: OptixDeviceContext,
    denoiser: OptixDenoiser,
    state: CuDeviceptr,
    scratch: CuDeviceptr,
    color_in: CuDeviceptr,
    normal_in: CuDeviceptr,
    albedo_in: CuDeviceptr,
    color_out: CuDeviceptr,
    intensity: CuDeviceptr,
}

impl Session {
    fn new() -> Result<Self, String> {
        let cuda_library = load_cuda_library()?;
        let cuda = CudaApi::load(&cuda_library)?;
        if unsafe { (cuda.init)(0) } != 0 {
            return Err("cuInit failed".into());
        }
        let mut device = 0;
        if unsafe { (cuda.device_get)(&mut device, 0) } != 0 {
            return Err("No CUDA device available".into());
        }
        let mut context = ptr::null_mut();
        let result = unsafe { (cuda.ctx_create)(&mut context, 0, device) };
        if result != 0 || context.is_null() {
            return Err(format!("cuCtxCreate failed: {result}"));
        }

        let mut session = Self {
            _cuda_library: cuda_library,
            cuda,
            _optix_library: None,
            optix: None,
            context,
            stream: ptr::null_mut(),
            device_context: ptr::null_mut(),
            denoiser: ptr::null_mut(),
            state: 0,
            scratch: 0,
            color_in: 0,
            normal_in: 0,
            albedo_in: 0,
            color_out: 0,
            intensity: 0,
        };
        check_cuda(
            unsafe { (session.cuda.ctx_set_current)(session.context) },
            "cuCtxSetCurrent",
        )?;
        let result = unsafe { (session.cuda.stream_create)(&mut session.stream, 0) };
        check_cuda(result, "cuStreamCreate")?;

        let optix_library = load_optix_library()?;
        let optix = OptixApi::load(&optix_library)?;
        session._optix_library = Some(optix_library);
        session.optix = Some(optix);

        let options = OptixDeviceContextOptions::default();
        let mut device_context = ptr::null_mut();
        let result = unsafe {
            (optix.device_context_create)(session.context, &options, &mut device_context)
        };
        session.check_optix(result, "optixDeviceContextCreate")?;
        session.device_context = device_context;

        let denoiser_options = OptixDenoiserOptions {
            guide_albedo: 1,
            guide_normal: 1,
        };
        let mut denoiser = ptr::null_mut();
        let result = unsafe {
            (optix.denoiser_create)(
                device_context,
                OPTIX_DENOISER_MODEL_KIND_HDR,
                &denoiser_options,
                &mut denoiser,
            )
        };
        session.check_optix(result, "optixDenoiserCreate")?;
        session.denoiser = denoiser;
        Ok(session)
    }

    fn check_optix(&self, result: c_int, what: &str) -> Result<(), String> {
        if result == 0 {
            Ok(())
        } else {
            let name = self
                .optix
                .map(|optix| optix.error_name(result))
                .unwrap_or_default();
            if name.is_empty() {
                Err(format!("{what} failed: OptiX error {result}"))
            } else {
                Err(format!("{what} failed: OptiX error {result} ({name})"))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        radiance: &mut [Vec3],
        normals: &[Vec3],
        covered: &[bool],
        resolution: u32,
    ) -> Result<(), String> {
        let optix = self
            .optix
            .ok_or_else(|| "OptiX is not initialized".to_string())?;
        let cuda = self.cuda;
        let width = resolution;
        let height = resolution;
        let texels = width as usize * height as usize;
        let color_bytes = texels
            .checked_mul(16)
            .ok_or_else(|| "OptiX image is too large".to_string())?;
        let guide_bytes = color_bytes;

        let mut sizes: OptixDenoiserSizes = unsafe { zeroed() };
        let result =
            unsafe { (optix.denoiser_memory_resources)(self.denoiser, width, height, &mut sizes) };
        self.check_optix(result, "optixDenoiserComputeMemoryResources")?;
        let state_size = sizes.state_size_in_bytes;
        let scratch_size = sizes.without_overlap_scratch_size_in_bytes;
        if state_size == 0 || scratch_size == 0 {
            return Err("OptiX returned zero-sized denoiser buffers".into());
        }

        allocate(&cuda, &mut self.state, state_size, "OptiX state allocation")?;
        allocate(
            &cuda,
            &mut self.scratch,
            scratch_size,
            "OptiX scratch allocation",
        )?;
        allocate(&cuda, &mut self.color_in, color_bytes, "OptiX color input")?;
        allocate(
            &cuda,
            &mut self.normal_in,
            guide_bytes,
            "OptiX normal input",
        )?;
        allocate(
            &cuda,
            &mut self.albedo_in,
            guide_bytes,
            "OptiX albedo input",
        )?;
        allocate(
            &cuda,
            &mut self.color_out,
            color_bytes,
            "OptiX color output",
        )?;
        allocate(&cuda, &mut self.intensity, 4, "OptiX intensity")?;

        let mut color = Vec::with_capacity(texels * 4);
        let mut normal = Vec::with_capacity(texels * 4);
        let mut albedo = Vec::with_capacity(texels * 4);
        let mut covered_sum = Vec3::ZERO;
        let mut covered_count = 0usize;
        for index in 0..texels {
            if covered.get(index).copied().unwrap_or(false) {
                let value = radiance
                    .get(index)
                    .copied()
                    .unwrap_or(Vec3::ZERO)
                    .max(Vec3::ZERO);
                covered_sum += value;
                covered_count += 1;
                color.extend_from_slice(&[value.x, value.y, value.z, 1.0]);
                let value = normals.get(index).copied().unwrap_or(Vec3::Z);
                normal.extend_from_slice(&[value.x, value.y, value.z, 0.0]);
                // The lightmap is baked without albedo, so the surface guide is
                // a neutral white inside the UV islands and black outside.
                albedo.extend_from_slice(&[1.0, 1.0, 1.0, 1.0]);
            } else {
                color.extend_from_slice(&[0.0, 0.0, 0.0, 1.0]);
                normal.extend_from_slice(&[0.0, 0.0, 1.0, 0.0]);
                albedo.extend_from_slice(&[0.0, 0.0, 0.0, 1.0]);
            }
        }

        // `optixDenoiserComputeIntensity` averages the whole image, including
        // the empty space outside the atlas. A mostly-black background drags
        // the HDR intensity down and makes the denoiser under-smooth or smear.
        // Fill the uncovered texels with the mean covered radiance; the albedo
        // guide is still black there, so the denoiser treats them as holes.
        let background = if covered_count > 0 {
            covered_sum / covered_count as f32
        } else {
            Vec3::ZERO
        };
        for index in 0..texels {
            if covered.get(index).copied().unwrap_or(false) {
                continue;
            }
            let target = index * 4;
            color[target] = background.x;
            color[target + 1] = background.y;
            color[target + 2] = background.z;
        }

        let result = unsafe {
            (cuda.memcpy_htod)(self.color_in, color.as_ptr() as *const c_void, color_bytes)
        };
        check_cuda(result, "OptiX color upload")?;
        let result = unsafe {
            (cuda.memcpy_htod)(
                self.normal_in,
                normal.as_ptr() as *const c_void,
                guide_bytes,
            )
        };
        check_cuda(result, "OptiX normal upload")?;
        let result = unsafe {
            (cuda.memcpy_htod)(
                self.albedo_in,
                albedo.as_ptr() as *const c_void,
                guide_bytes,
            )
        };
        check_cuda(result, "OptiX albedo upload")?;

        let input_image = image(self.color_in, width, height);
        let output_image = image(self.color_out, width, height);
        let normal_image = image(self.normal_in, width, height);
        let albedo_image = image(self.albedo_in, width, height);

        let result = unsafe {
            (optix.denoiser_setup)(
                self.denoiser,
                self.stream,
                width,
                height,
                self.state,
                state_size,
                self.scratch,
                scratch_size,
            )
        };
        self.check_optix(result, "optixDenoiserSetup")?;

        let intensity_result = unsafe {
            (optix.denoiser_intensity)(
                self.denoiser,
                self.stream,
                &input_image,
                self.intensity,
                self.scratch,
                scratch_size,
            )
        };
        // The HDR model is documented to produce suboptimal results without a
        // valid intensity, so fall back instead of passing a null intensity.
        self.check_optix(intensity_result, "optixDenoiserComputeIntensity")?;
        let params = OptixDenoiserParams {
            denoise_alpha: OPTIX_DENOISER_ALPHA_MODE_COPY,
            hdr_intensity: self.intensity,
            blend_factor: 0.0,
            hdr_average_color: 0,
            temporal_mode_use_previous_layers: 0,
        };

        let mut guide: OptixDenoiserGuideLayer = unsafe { zeroed() };
        guide.albedo = albedo_image;
        guide.normal = normal_image;
        let mut layer: OptixDenoiserLayer = unsafe { zeroed() };
        layer.input = input_image;
        layer.output = output_image;

        let result = unsafe {
            (optix.denoiser_invoke)(
                self.denoiser,
                self.stream,
                &params,
                self.state,
                state_size,
                &guide,
                &layer,
                1,
                0,
                0,
                self.scratch,
                scratch_size,
            )
        };
        self.check_optix(result, "optixDenoiserInvoke")?;
        let result = unsafe { (cuda.stream_synchronize)(self.stream) };
        check_cuda(result, "cuStreamSynchronize")?;

        let mut output = vec![0.0f32; texels * 4];
        let result = unsafe {
            (cuda.memcpy_dtoh)(
                output.as_mut_ptr() as *mut c_void,
                self.color_out,
                color_bytes,
            )
        };
        check_cuda(result, "OptiX result download")?;

        for (index, target_radiance) in radiance.iter_mut().enumerate().take(texels) {
            if !covered[index] {
                continue;
            }
            let target = index * 4;
            let value = Vec3::new(output[target], output[target + 1], output[target + 2]);
            *target_radiance = if value.is_finite() {
                value.max(Vec3::ZERO)
            } else {
                Vec3::ZERO
            };
        }

        // Avoid keeping the denoiser buffers alive after a successful run.
        if self.state != 0 {
            let _ = unsafe { (cuda.mem_free)(self.state) };
            self.state = 0;
        }
        if self.scratch != 0 {
            let _ = unsafe { (cuda.mem_free)(self.scratch) };
            self.scratch = 0;
        }
        for slot in [
            &mut self.color_in,
            &mut self.normal_in,
            &mut self.albedo_in,
            &mut self.color_out,
            &mut self.intensity,
        ] {
            if *slot != 0 {
                let _ = unsafe { (cuda.mem_free)(*slot) };
                *slot = 0;
            }
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            if let Some(optix) = self.optix {
                if !self.denoiser.is_null() {
                    let _ = (optix.denoiser_destroy)(self.denoiser);
                }
                if !self.device_context.is_null() {
                    let _ = (optix.device_context_destroy)(self.device_context);
                }
            }
            let cuda = self.cuda;
            for pointer in [
                self.state,
                self.scratch,
                self.color_in,
                self.normal_in,
                self.albedo_in,
                self.color_out,
                self.intensity,
            ] {
                if pointer != 0 {
                    let _ = (cuda.mem_free)(pointer);
                }
            }
            if !self.stream.is_null() {
                let _ = (cuda.stream_destroy)(self.stream);
            }
            if !self.context.is_null() {
                let _ = (cuda.ctx_destroy)(self.context);
            }
        }
    }
}

fn image(data: CuDeviceptr, width: u32, height: u32) -> OptixImage2D {
    OptixImage2D {
        data,
        width,
        height,
        row_stride_in_bytes: width.saturating_mul(16),
        pixel_stride_in_bytes: 16,
        format: OPTIX_PIXEL_FORMAT_FLOAT4,
    }
}

fn check_cuda(result: c_int, what: &str) -> Result<(), String> {
    if result == 0 {
        Ok(())
    } else {
        Err(format!("{what} failed: CUDA error {result}"))
    }
}

fn allocate(
    cuda: &CudaApi,
    slot: &mut CuDeviceptr,
    bytes: usize,
    what: &str,
) -> Result<(), String> {
    let result = unsafe { (cuda.mem_alloc)(slot, bytes) };
    check_cuda(result, what)
}

fn load_cuda_library() -> Result<Library, String> {
    #[cfg(windows)]
    {
        open_system32("nvcuda.dll")
    }
    #[cfg(unix)]
    {
        unsafe { Library::new("libcuda.so.1") }
            .map_err(|error| format!("Could not load libcuda.so.1: {error}"))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err("CUDA is not supported on this platform".into())
    }
}

/// Load a driver component from System32 only, so PATH and the working
/// directory are never consulted.
#[cfg(windows)]
fn open_system32(name: &str) -> Result<Library, String> {
    use libloading::os::windows::{Library as WinLibrary, LOAD_LIBRARY_SEARCH_SYSTEM32};
    // SAFETY: nvcuda.dll and nvoptix.dll are NVIDIA driver components installed
    // in System32; loading them from there is the documented location.
    unsafe { WinLibrary::load_with_flags(name, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map(Library::from)
        .map_err(|error| format!("Could not load {name}: {error}"))
}

fn load_optix_library() -> Result<Library, String> {
    #[cfg(windows)]
    {
        if let Ok(library) = open_system32("nvoptix.dll") {
            return Ok(library);
        }
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let system32 = system_root.join("System32");
        let direct = system32.join("nvoptix.dll");
        if direct.is_file() {
            if let Ok(library) = unsafe { Library::new(&direct) } {
                return Ok(library);
            }
        }
        let repository = system32.join("DriverStore").join("FileRepository");
        if let Ok(entries) = std::fs::read_dir(&repository) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if !name.starts_with("nv") {
                    continue;
                }
                let candidate = entry.path().join("nvoptix.dll");
                if candidate.is_file() {
                    if let Ok(library) = unsafe { Library::new(&candidate) } {
                        return Ok(library);
                    }
                }
            }
        }
        Err("nvoptix.dll was not found (install the NVIDIA driver/OptiX runtime)".into())
    }
    #[cfg(unix)]
    {
        for name in ["libnvoptix.so.1", "libnvoptix.so"] {
            if let Ok(library) = unsafe { Library::new(name) } {
                return Ok(library);
            }
        }
        Err("libnvoptix.so was not found".into())
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err("OptiX is not supported on this platform".into())
    }
}

/// True when the NVIDIA OptiX runtime and CUDA driver can be loaded.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let Ok(cuda_library) = load_cuda_library() else {
            return false;
        };
        let Ok(cuda) = CudaApi::load(&cuda_library) else {
            return false;
        };
        if unsafe { (cuda.init)(0) } != 0 {
            return false;
        }
        let mut device = 0;
        if unsafe { (cuda.device_get)(&mut device, 0) } != 0 {
            return false;
        }
        let Ok(optix_library) = load_optix_library() else {
            return false;
        };
        OptixApi::load(&optix_library).is_ok()
    })
}

/// Denoise HDR radiance with the OptiX HDR model, using normal and albedo
/// guides derived from the UV coverage mask.
pub(crate) fn denoise(
    radiance: &mut [Vec3],
    normals: &[Vec3],
    covered: &[bool],
    resolution: u32,
) -> Result<(), String> {
    let texels = resolution as usize * resolution as usize;
    if radiance.len() != texels || normals.len() != texels || covered.len() != texels {
        return Err("OptiX denoiser received mismatched buffers".into());
    }
    let mut session = Session::new()?;
    session.run(radiance, normals, covered, resolution)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn optix_struct_layouts_match_the_sdk() {
        assert_eq!(size_of::<OptixImage2D>(), 32);
        assert_eq!(size_of::<OptixDenoiserSizes>(), 56);
        assert_eq!(size_of::<OptixDenoiserParams>(), 40);
        assert_eq!(size_of::<OptixDenoiserLayer>(), 96);
        assert_eq!(size_of::<OptixDenoiserGuideLayer>(), 160);
    }

    #[test]
    fn denoises_a_checkerboard_when_available() {
        if !available() {
            return;
        }
        let size = 32u32;
        let texels = size as usize * size as usize;
        let mut radiance = vec![Vec3::splat(0.5); texels];
        for y in 0..size as usize {
            for x in 0..size as usize {
                let value = if (x + y) % 2 == 0 { 0.4 } else { 0.6 };
                radiance[y * size as usize + x] = Vec3::splat(value);
            }
        }
        let normals = vec![Vec3::Z; texels];
        let covered = vec![true; texels];
        denoise(&mut radiance, &normals, &covered, size).expect("OptiX denoise");
        assert!(radiance.iter().all(|value| value.is_finite()));
        let center = radiance[(size as usize / 2) * size as usize + size as usize / 2];
        assert!((center.x - 0.5).abs() < 0.2, "{center:?}");
    }
}
