//! Optional Intel Open Image Denoise integration.
//!
//! OIDN is not a build dependency. The shared library is located at runtime
//! (`OpenImageDenoise.dll` on Windows, `libOpenImageDenoise.so*` on Linux,
//! `libOpenImageDenoise.dylib` on macOS) and the C API is loaded dynamically.
//! When the library is missing or a call fails, the caller falls back to the
//! built-in edge-aware denoiser, so lightmap baking stays a single optional
//! feature with no mandatory native dependency.

use std::ffi::{c_char, c_void, CStr};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libloading::Library;

// Intel Open Image Denoise device types from `OIDNDeviceType`. These values
// are part of the C ABI: DEFAULT=0, CPU=1, SYCL=2, CUDA=3. Passing the wrong
// number silently creates a CPU device for a SYCL request and a SYCL device
// for a CUDA request.
const DEVICE_CPU: i32 = 1;
const DEVICE_SYCL: i32 = 2;
const DEVICE_CUDA: i32 = 3;
const FORMAT_FLOAT3: i32 = 3;

type NewDevice = unsafe extern "C" fn(i32) -> *mut c_void;
type ReleaseDevice = unsafe extern "C" fn(*mut c_void);
type CommitDevice = unsafe extern "C" fn(*mut c_void);
type NewBuffer = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
type ReleaseBuffer = unsafe extern "C" fn(*mut c_void);
type WriteBuffer = unsafe extern "C" fn(*mut c_void, usize, usize, *const c_void);
type ReadBuffer = unsafe extern "C" fn(*mut c_void, usize, usize, *mut c_void);
type NewFilter = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type ReleaseFilter = unsafe extern "C" fn(*mut c_void);
type SetFilterImage = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *mut c_void,
    i32,
    usize,
    usize,
    usize,
    usize,
    usize,
);
type SetFilterBool = unsafe extern "C" fn(*mut c_void, *const c_char, bool);
type CommitFilter = unsafe extern "C" fn(*mut c_void);
type ExecuteFilter = unsafe extern "C" fn(*mut c_void);
type GetDeviceError = unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> i32;

struct OidnApi {
    _library: Library,
    new_device: NewDevice,
    release_device: ReleaseDevice,
    commit_device: CommitDevice,
    new_buffer: NewBuffer,
    release_buffer: ReleaseBuffer,
    write_buffer: WriteBuffer,
    read_buffer: ReadBuffer,
    new_filter: NewFilter,
    release_filter: ReleaseFilter,
    set_filter_image: SetFilterImage,
    set_filter_bool: SetFilterBool,
    commit_filter: CommitFilter,
    execute_filter: ExecuteFilter,
    get_device_error: GetDeviceError,
}

impl OidnApi {
    fn load() -> Option<Self> {
        // Try every candidate instead of stopping at the first library that
        // happens to load: a stale `OpenImageDenoise.dll` from PATH may load
        // but lack the 2.x entry points, and the portable build ships a known
        // good copy next to the executable.
        for library in load_libraries() {
            if let Some(api) = Self::from_library(library) {
                return Some(api);
            }
        }
        None
    }

    fn from_library(library: Library) -> Option<Self> {
        // SAFETY: every symbol is looked up with its documented OIDN C
        // signature. The library stays alive for the lifetime of the struct.
        unsafe {
            macro_rules! get {
                ($name:literal, $ty:ty) => {
                    *library.get::<$ty>(concat!($name, "\0").as_bytes()).ok()?
                };
            }
            let new_device = get!("oidnNewDevice", NewDevice);
            let release_device = get!("oidnReleaseDevice", ReleaseDevice);
            let commit_device = get!("oidnCommitDevice", CommitDevice);
            let new_buffer = get!("oidnNewBuffer", NewBuffer);
            let release_buffer = get!("oidnReleaseBuffer", ReleaseBuffer);
            let write_buffer = get!("oidnWriteBuffer", WriteBuffer);
            let read_buffer = get!("oidnReadBuffer", ReadBuffer);
            let new_filter = get!("oidnNewFilter", NewFilter);
            let release_filter = get!("oidnReleaseFilter", ReleaseFilter);
            let set_filter_image = get!("oidnSetFilterImage", SetFilterImage);
            // OIDN 2.x renamed the 1.x `oidnSetFilter1b` entry point to
            // `oidnSetFilterBool`; accept either (and the older shared name)
            // so a 1.4 or 2.x library both work.
            let set_filter_bool = [
                "oidnSetFilterBool",
                "oidnSetSharedFilterBool",
                "oidnSetFilter1b",
            ]
            .iter()
            .find_map(|name| {
                let symbol = format!("{name}\0");
                library
                    .get::<SetFilterBool>(symbol.as_bytes())
                    .ok()
                    .map(|symbol| *symbol)
            })?;
            let commit_filter = get!("oidnCommitFilter", CommitFilter);
            let execute_filter = get!("oidnExecuteFilter", ExecuteFilter);
            let get_device_error = get!("oidnGetDeviceError", GetDeviceError);
            Some(Self {
                _library: library,
                new_device,
                release_device,
                commit_device,
                new_buffer,
                release_buffer,
                write_buffer,
                read_buffer,
                new_filter,
                release_filter,
                set_filter_image,
                set_filter_bool,
                commit_filter,
                execute_filter,
                get_device_error,
            })
        }
    }
}

fn load_libraries() -> Vec<Library> {
    const NAMES: [&str; 9] = [
        "OpenImageDenoise.dll",
        "openimagedenoise.dll",
        "oidn.dll",
        "libOpenImageDenoise.so.2",
        "libOpenImageDenoise.so",
        "libOpenImageDenoise.2.dylib",
        "libOpenImageDenoise.dylib",
        "OpenImageDenoise",
        "oidn",
    ];

    let mut libraries = Vec::new();

    // Trusted absolute paths first: the bundled copy next to the executable,
    // then explicit overrides, then system installs. A stale copy from PATH
    // must never win over the known-good bundled one.
    for directory in trusted_directories() {
        for name in NAMES {
            let path = directory.join(name);
            if path.is_file() {
                if let Some(library) = open_trusted(&path) {
                    libraries.push(library);
                }
            }
        }
    }

    // Last resort: the OS search path. `from_library` still rejects anything
    // that lacks the 2.x entry points.
    for name in NAMES {
        // SAFETY: loading a shared library is inherently unsafe; we only use
        // well-known OIDN library names and validate every symbol we need.
        if let Ok(library) = unsafe { Library::new(name) } {
            libraries.push(library);
        }
    }
    libraries
}

/// Load a library by absolute path without consulting PATH or the working
/// directory. `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR` lets OIDN find its own device
/// modules (`OpenImageDenoise_device_*.dll`) next to it.
fn open_trusted(path: &Path) -> Option<Library> {
    #[cfg(windows)]
    {
        use libloading::os::windows::{
            Library as WinLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        };
        // SAFETY: loading a DLL from a trusted install directory, the same
        // trust level as the executable. Symbols are validated in `from_library`.
        unsafe {
            WinLibrary::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            )
        }
        .ok()
        .map(Library::from)
    }
    #[cfg(not(windows))]
    {
        // SAFETY: as above.
        unsafe { Library::new(path) }.ok()
    }
}

/// Directories in trust order: the executable's own folder first, then
/// explicit environment overrides, then system install roots.
fn trusted_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        directories.push(exe_dir.clone());
        directories.push(exe_dir.join("bin"));
        directories.push(exe_dir.join("lib"));
    }
    // The oneAPI/standalone OIDN installers do not always add their bin folder
    // to PATH, and Intel SYCL needs the DLLs copied next to the library.
    for variable in ["OIDN_DIR", "OIDN_ROOT"] {
        if let Some(root) = std::env::var_os(variable).map(PathBuf::from) {
            directories.push(root.clone());
            directories.push(root.join("bin"));
            directories.push(root.join("lib"));
        }
    }
    #[cfg(windows)]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
            let Some(root) = std::env::var_os(variable).map(PathBuf::from) else {
                continue;
            };
            directories.push(root.join("Intel").join("OpenImageDenoise").join("bin"));
            directories.push(
                root.join("Intel")
                    .join("oneAPI")
                    .join("oidn")
                    .join("latest")
                    .join("bin"),
            );
            let versions = root.join("Intel").join("oneAPI").join("oidn");
            if let Ok(entries) = std::fs::read_dir(versions) {
                for entry in entries.flatten() {
                    directories.push(entry.path().join("bin"));
                }
            }
        }
    }
    directories
}

/// True when an OIDN shared library can be loaded.
pub(crate) fn available() -> bool {
    api().is_some()
}

/// The process-wide OIDN handle. Loading is expensive (dozens of path probes
/// and a `LoadLibrary`), so `available()` and every `denoise()` share it.
fn api() -> Option<&'static OidnApi> {
    static API: OnceLock<Option<OidnApi>> = OnceLock::new();
    API.get_or_init(OidnApi::load).as_ref()
}

/// Denoise linear RGB radiance with OIDN's lightmap filter (or RT on older
/// libraries).
///
/// `color` and `normal` are tightly packed `width * height * 3` float arrays.
/// `preference` is `-1` for Auto, or an `OIDNDeviceType` value (`1` CPU,
/// `2` SYCL, `3` CUDA). Returns the output and the device type that actually
/// ran. A requested GPU device that is not available falls back to the CPU
/// device.
pub(crate) fn denoise(
    color: &[f32],
    normal: &[f32],
    albedo: &[f32],
    width: u32,
    height: u32,
    preference: i32,
) -> Option<(Vec<f32>, i32)> {
    let pixel_count = width as usize * height as usize;
    let component_count = pixel_count.checked_mul(3)?;
    if width == 0
        || height == 0
        || color.len() != component_count
        || normal.len() != component_count
        || albedo.len() != component_count
    {
        return None;
    }
    let api = api()?;
    let candidates: &[i32] = match preference {
        -1 => &[DEVICE_CUDA, DEVICE_SYCL, DEVICE_CPU],
        DEVICE_CUDA => &[DEVICE_CUDA, DEVICE_CPU],
        DEVICE_SYCL => &[DEVICE_SYCL, DEVICE_CPU],
        _ => &[DEVICE_CPU],
    };
    for &device_type in candidates {
        if let Some(output) = try_denoise(api, device_type, color, normal, albedo, width, height) {
            return Some((output, device_type));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn try_denoise(
    api: &OidnApi,
    device_type: i32,
    color: &[f32],
    normal: &[f32],
    albedo: &[f32],
    width: u32,
    height: u32,
) -> Option<Vec<f32>> {
    let pixel_count = width as usize * height as usize;
    let component_count = pixel_count.checked_mul(3)?;
    let byte_size = component_count.checked_mul(std::mem::size_of::<f32>())?;

    // SAFETY: pointers returned by OIDN are checked for null, buffers are sized
    // from the same component count, and every object is released below.
    unsafe {
        let device = (api.new_device)(device_type);
        if device.is_null() {
            return None;
        }
        (api.commit_device)(device);
        if (api.get_device_error)(device, std::ptr::null_mut()) != 0 {
            (api.release_device)(device);
            return None;
        }

        let color_buffer = (api.new_buffer)(device, byte_size);
        let normal_buffer = (api.new_buffer)(device, byte_size);
        let albedo_buffer = (api.new_buffer)(device, byte_size);
        let output_buffer = (api.new_buffer)(device, byte_size);
        if color_buffer.is_null()
            || normal_buffer.is_null()
            || albedo_buffer.is_null()
            || output_buffer.is_null()
        {
            if !color_buffer.is_null() {
                (api.release_buffer)(color_buffer);
            }
            if !normal_buffer.is_null() {
                (api.release_buffer)(normal_buffer);
            }
            if !albedo_buffer.is_null() {
                (api.release_buffer)(albedo_buffer);
            }
            if !output_buffer.is_null() {
                (api.release_buffer)(output_buffer);
            }
            (api.release_device)(device);
            return None;
        }

        (api.write_buffer)(color_buffer, 0, byte_size, color.as_ptr() as *const c_void);
        (api.write_buffer)(
            normal_buffer,
            0,
            byte_size,
            normal.as_ptr() as *const c_void,
        );
        (api.write_buffer)(
            albedo_buffer,
            0,
            byte_size,
            albedo.as_ptr() as *const c_void,
        );

        // Lightmap-specific filter first (OIDN 2.x), then the general RT
        // filter with normal/albedo guides for older libraries.
        let mut denoised = false;
        for (name, use_guides) in [(c"RTLightmap", false), (c"RT", true)] {
            if run_filter(
                api,
                device,
                name,
                use_guides,
                color_buffer,
                normal_buffer,
                albedo_buffer,
                output_buffer,
                width,
                height,
            ) {
                denoised = true;
                break;
            }
        }
        if !denoised {
            (api.release_buffer)(color_buffer);
            (api.release_buffer)(normal_buffer);
            (api.release_buffer)(albedo_buffer);
            (api.release_buffer)(output_buffer);
            (api.release_device)(device);
            return None;
        }

        let mut output = vec![0.0f32; component_count];
        (api.read_buffer)(
            output_buffer,
            0,
            byte_size,
            output.as_mut_ptr() as *mut c_void,
        );

        (api.release_buffer)(color_buffer);
        (api.release_buffer)(normal_buffer);
        (api.release_buffer)(albedo_buffer);
        (api.release_buffer)(output_buffer);
        (api.release_device)(device);

        Some(output)
    }
}

/// Run one OIDN filter attempt. Returns false when the filter is unavailable
/// or execution reported a device error; the caller then tries the next filter.
#[allow(clippy::too_many_arguments)]
unsafe fn run_filter(
    api: &OidnApi,
    device: *mut c_void,
    name: &CStr,
    use_guides: bool,
    color_buffer: *mut c_void,
    normal_buffer: *mut c_void,
    albedo_buffer: *mut c_void,
    output_buffer: *mut c_void,
    width: u32,
    height: u32,
) -> bool {
    let filter = (api.new_filter)(device, name.as_ptr());
    if filter.is_null() {
        return false;
    }
    let set_image = |name: &CStr, buffer: *mut c_void| {
        (api.set_filter_image)(
            filter,
            name.as_ptr(),
            buffer,
            FORMAT_FLOAT3,
            width as usize,
            height as usize,
            0,
            0,
            0,
        );
    };
    set_image(c"color", color_buffer);
    if use_guides {
        set_image(c"normal", normal_buffer);
        set_image(c"albedo", albedo_buffer);
    }
    set_image(c"output", output_buffer);
    (api.set_filter_bool)(filter, c"hdr".as_ptr(), true);
    if use_guides {
        // The guides come from the G-buffer, not the noisy render.
        (api.set_filter_bool)(filter, c"cleanAux".as_ptr(), true);
    }
    (api.commit_filter)(filter);
    (api.execute_filter)(filter);
    let ok = (api.get_device_error)(device, std::ptr::null_mut()) == 0;
    (api.release_filter)(filter);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_directories_put_the_executable_first() {
        let directory = trusted_directories()
            .into_iter()
            .next()
            .expect("the executable directory is always present");
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(directory, exe_dir);
    }

    #[test]
    fn the_api_is_loaded_once() {
        let first = api().map(|api| api as *const OidnApi);
        let second = api().map(|api| api as *const OidnApi);
        assert_eq!(first, second);
        assert_eq!(available(), first.is_some());
    }

    #[test]
    fn denoises_a_small_image_when_available() {
        if !available() {
            return;
        }
        let size = 16u32;
        let pixels = size as usize * size as usize;
        let mut color = vec![0.0f32; pixels * 3];
        let mut normal = vec![0.0f32; pixels * 3];
        let mut albedo = vec![0.0f32; pixels * 3];
        for index in 0..pixels {
            let value = if index % 3 == 0 { 0.8 } else { 0.2 };
            color[index * 3] = value;
            color[index * 3 + 1] = value;
            color[index * 3 + 2] = value;
            normal[index * 3 + 2] = 1.0;
            albedo[index * 3] = 1.0;
            albedo[index * 3 + 1] = 1.0;
            albedo[index * 3 + 2] = 1.0;
        }
        let (output, device) =
            denoise(&color, &normal, &albedo, size, size, DEVICE_CPU).expect("OIDN CPU denoise");
        assert_eq!(device, DEVICE_CPU);
        assert_eq!(output.len(), pixels * 3);
        assert!(output.iter().all(|value| value.is_finite()));
        assert!(output.iter().any(|value| *value > 0.01));
    }
}
