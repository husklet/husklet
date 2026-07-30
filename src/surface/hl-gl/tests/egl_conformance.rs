//! EGL conformance breadth: the *advertised-versus-actual* surface of the staged `libEGL.so.1` /
//! `libGLESv2.so.2`, plus the surface/context/make-current matrix — driven through a real `dlopen`,
//! exactly as GTK, Qt, Chromium and `eglinfo` drive it.
//!
//! The four modules cover the classes where a defect is SILENT (a success return with a wrong or absent
//! value), rather than the obvious render paths:
//!
//! * [`extensions`] — every extension named in `eglQueryString(EGL_EXTENSIONS)` (client + display) and in
//!   `glGetString(GL_EXTENSIONS)` must have its entry points resolvable, and the indexed GL enumeration
//!   must agree with the space-separated string and with `GL_NUM_EXTENSIONS`.
//! * [`configs`] — every `eglGetConfigAttrib` on every config `eglGetConfigs` returns: internally
//!   consistent, at or above the EGL/GLES minima, agreeing with the GL `GL_*_BITS` a context on that
//!   config reports, and — for every `EGL_SURFACE_TYPE` bit claimed — actually able to create that surface.
//! * [`queries`] — `eglQueryString`, `eglQuerySurface`, `eglQueryContext`, `eglSurfaceAttrib`,
//!   `eglBindTexImage`, `eglCopyBuffers`: the specified attribute sets and the specified errors.
//! * [`matrix`] — window / pbuffer / surfaceless across the config set, NULL / empty / populated attribute
//!   lists, every context version served and refused, and the `eglMakeCurrent` transitions including
//!   release, rebind, and `EGL_BAD_ACCESS` for a context current on another thread.
//!
//! PREFLIGHT: a missing staged library PANICS naming what is required. A skip here would read as a pass.
//!
//! LOADING: `libGLESv2.so.2` has a `DT_NEEDED libEGL.so.1` and imports the shared-state accessor from it,
//! so libEGL is opened FIRST with `RTLD_GLOBAL` (see `tests/so_ffi_coverage.rs`).
//!
//! SERIALIZATION: both objects share ONE process-global EGL/GL state and cargo runs this binary's tests on
//! parallel threads, so every test takes [`SERIAL`] first.

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

// ---- EGL enums (EGL 1.5 core + the extensions this driver advertises) ----------------------------

pub const EGL_FALSE: u32 = 0;
pub const EGL_TRUE: u32 = 1;
pub const EGL_NONE: i32 = 0x3038;
pub const EGL_DONT_CARE: i32 = -1;

pub const EGL_SUCCESS: i32 = 0x3000;
pub const EGL_BAD_ACCESS: i32 = 0x3002;
pub const EGL_BAD_ATTRIBUTE: i32 = 0x3004;
pub const EGL_BAD_CONFIG: i32 = 0x3005;
pub const EGL_BAD_CONTEXT: i32 = 0x3006;
pub const EGL_BAD_MATCH: i32 = 0x3009;
pub const EGL_BAD_NATIVE_PIXMAP: i32 = 0x300A;
pub const EGL_BAD_PARAMETER: i32 = 0x300C;
pub const EGL_BAD_SURFACE: i32 = 0x300D;

pub const EGL_VENDOR: i32 = 0x3053;
pub const EGL_VERSION: i32 = 0x3054;
pub const EGL_EXTENSIONS: i32 = 0x3055;
pub const EGL_CLIENT_APIS: i32 = 0x308D;

// config attributes
pub const EGL_BUFFER_SIZE: i32 = 0x3020;
pub const EGL_ALPHA_SIZE: i32 = 0x3021;
pub const EGL_BLUE_SIZE: i32 = 0x3022;
pub const EGL_GREEN_SIZE: i32 = 0x3023;
pub const EGL_RED_SIZE: i32 = 0x3024;
pub const EGL_DEPTH_SIZE: i32 = 0x3025;
pub const EGL_STENCIL_SIZE: i32 = 0x3026;
pub const EGL_CONFIG_CAVEAT: i32 = 0x3027;
pub const EGL_CONFIG_ID: i32 = 0x3028;
pub const EGL_LEVEL: i32 = 0x3029;
pub const EGL_MAX_PBUFFER_HEIGHT: i32 = 0x302A;
pub const EGL_MAX_PBUFFER_PIXELS: i32 = 0x302B;
pub const EGL_MAX_PBUFFER_WIDTH: i32 = 0x302C;
pub const EGL_NATIVE_RENDERABLE: i32 = 0x302D;
pub const EGL_NATIVE_VISUAL_ID: i32 = 0x302E;
pub const EGL_NATIVE_VISUAL_TYPE: i32 = 0x302F;
pub const EGL_SAMPLES: i32 = 0x3031;
pub const EGL_SAMPLE_BUFFERS: i32 = 0x3032;
pub const EGL_SURFACE_TYPE: i32 = 0x3033;
pub const EGL_TRANSPARENT_TYPE: i32 = 0x3034;
pub const EGL_TRANSPARENT_BLUE_VALUE: i32 = 0x3035;
pub const EGL_TRANSPARENT_GREEN_VALUE: i32 = 0x3036;
pub const EGL_TRANSPARENT_RED_VALUE: i32 = 0x3037;
pub const EGL_BIND_TO_TEXTURE_RGB: i32 = 0x3039;
pub const EGL_BIND_TO_TEXTURE_RGBA: i32 = 0x303A;
pub const EGL_MIN_SWAP_INTERVAL: i32 = 0x303B;
pub const EGL_MAX_SWAP_INTERVAL: i32 = 0x303C;
pub const EGL_LUMINANCE_SIZE: i32 = 0x303D;
pub const EGL_ALPHA_MASK_SIZE: i32 = 0x303E;
pub const EGL_COLOR_BUFFER_TYPE: i32 = 0x303F;
pub const EGL_RENDERABLE_TYPE: i32 = 0x3040;
pub const EGL_CONFORMANT: i32 = 0x3042;
pub const EGL_RGB_BUFFER: i32 = 0x308E;

// surface-type bits / renderable bits
pub const EGL_PBUFFER_BIT: i32 = 0x0001;
pub const EGL_PIXMAP_BIT: i32 = 0x0002;
pub const EGL_WINDOW_BIT: i32 = 0x0004;
pub const EGL_OPENGL_ES_BIT: i32 = 0x0001;
pub const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
pub const EGL_OPENGL_ES3_BIT: i32 = 0x0040;

// surface attributes
pub const EGL_HEIGHT: i32 = 0x3056;
pub const EGL_WIDTH: i32 = 0x3057;
pub const EGL_LARGEST_PBUFFER: i32 = 0x3058;
pub const EGL_TEXTURE_FORMAT: i32 = 0x3080;
pub const EGL_TEXTURE_TARGET: i32 = 0x3081;
pub const EGL_MIPMAP_TEXTURE: i32 = 0x3082;
pub const EGL_MIPMAP_LEVEL: i32 = 0x3083;
pub const EGL_BACK_BUFFER: i32 = 0x3084;
pub const EGL_RENDER_BUFFER: i32 = 0x3086;
pub const EGL_VG_COLORSPACE: i32 = 0x3087;
pub const EGL_VG_ALPHA_FORMAT: i32 = 0x3088;
pub const EGL_HORIZONTAL_RESOLUTION: i32 = 0x3090;
pub const EGL_VERTICAL_RESOLUTION: i32 = 0x3091;
pub const EGL_PIXEL_ASPECT_RATIO: i32 = 0x3092;
pub const EGL_SWAP_BEHAVIOR: i32 = 0x3093;
pub const EGL_MULTISAMPLE_RESOLVE: i32 = 0x3099;
pub const EGL_BUFFER_PRESERVED: i32 = 0x3094;
pub const EGL_BUFFER_DESTROYED: i32 = 0x3095;

// context attributes / queries
pub const EGL_CONTEXT_CLIENT_TYPE: i32 = 0x3097;
pub const EGL_CONTEXT_MAJOR_VERSION: i32 = 0x3098;
pub const EGL_CONTEXT_MINOR_VERSION: i32 = 0x30FB;
pub const EGL_CONTEXT_FLAGS_KHR: i32 = 0x30FC;
pub const EGL_CONTEXT_OPENGL_NO_ERROR_KHR: i32 = 0x31B3;
pub const EGL_OPENGL_ES_API: u32 = 0x30A0;
pub const EGL_OPENGL_API: u32 = 0x30A2;

pub const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;

// ---- GL enums used for the cross-checks ----------------------------------------------------------

pub const GL_NO_ERROR: u32 = 0;
pub const GL_EXTENSIONS: u32 = 0x1F03;
pub const GL_NUM_EXTENSIONS: u32 = 0x821D;
pub const GL_VERSION: u32 = 0x1F02;
pub const GL_RED_BITS: u32 = 0x0D52;
pub const GL_GREEN_BITS: u32 = 0x0D53;
pub const GL_BLUE_BITS: u32 = 0x0D54;
pub const GL_ALPHA_BITS: u32 = 0x0D55;
pub const GL_DEPTH_BITS: u32 = 0x0D56;
pub const GL_STENCIL_BITS: u32 = 0x0D57;
pub const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;
pub const GL_SAMPLES: u32 = 0x80A9;
pub const GL_SAMPLE_BUFFERS: u32 = 0x80A8;

// ---- dynamic-loader FFI --------------------------------------------------------------------------

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

/// Every test drives the ONE process-global EGL/GL state, so they run one at a time.
pub static SERIAL: Mutex<()> = Mutex::new(());

pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn last_dl_error() -> String {
    let error = unsafe { dlerror() };
    if error.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

/// The two staged guest objects, opened once per process.
pub struct Shim {
    pub egl: *mut c_void,
    pub gles: *mut c_void,
}

// SAFETY: the handles are process-global `dlopen` tokens; every use of the driver behind them is
// serialized through `SERIAL`.
unsafe impl Send for Shim {}
unsafe impl Sync for Shim {}

impl Shim {
    /// Open the staged driver, or PANIC naming what is required. A skip would read as a pass.
    fn open() -> Self {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            "x86_64" => "x86_64",
            other => panic!("unsupported host arch for the EGL conformance battery: {other}"),
        };
        // The staging directory the build script publishes; never a hardcoded path.
        let dir = PathBuf::from(env!("HL_GL_STAGE_GL")).join(arch);
        let (egl_path, gles_path) = (dir.join("libEGL.so.1"), dir.join("libGLESv2.so.2"));
        for path in [&egl_path, &gles_path] {
            assert!(
                path.exists(),
                "REQUIRED: the staged guest driver {} is missing. Build with \
                 HL_DRIVER_ARCHES={arch} and the {arch}-unknown-linux-gnu rust std installed; this \
                 battery must not be skipped.",
                path.display()
            );
        }
        // No compositor in this process: the window-surface path must not try to reach one.
        std::env::set_var("HL_GL_NO_WAYLAND", "1");
        let egl = Self::dlopen_global(&egl_path);
        let gles = Self::dlopen_global(&gles_path);
        Self { egl, gles }
    }

    fn dlopen_global(path: &PathBuf) -> *mut c_void {
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        assert!(
            !handle.is_null(),
            "dlopen {} failed: {}",
            path.display(),
            last_dl_error()
        );
        handle
    }

    pub fn get() -> &'static Shim {
        static SHIM: OnceLock<Shim> = OnceLock::new();
        SHIM.get_or_init(Shim::open)
    }

    /// An exported symbol's address; panics when the object does not export it.
    fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
        let c = std::ffi::CString::new(name).unwrap();
        let address = unsafe { dlsym(handle, c.as_ptr()) };
        assert!(!address.is_null(), "symbol {name} not exported");
        address
    }
}

/// Resolve an exported symbol and transmute it to the given `extern "C" fn` type.
macro_rules! f {
    ($handle:expr, $name:literal, $ty:ty) => {
        unsafe { core::mem::transmute::<*mut core::ffi::c_void, $ty>(Shim::sym($handle, $name)) }
    };
}

fn cstr(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

/// The EGL entry points every module needs: an initialized display and the config/context/surface
/// lifecycle. Constructed per test (the underlying driver state is process-global and idempotent).
pub struct Egl {
    pub display: *mut c_void,
    pub get_error: extern "C" fn() -> i32,
    pub query_string: extern "C" fn(*mut c_void, i32) -> *const c_char,
    pub get_proc_address: extern "C" fn(*const c_char) -> *mut c_void,
    pub get_configs: extern "C" fn(*mut c_void, *mut *mut c_void, i32, *mut i32) -> u32,
    pub choose_config:
        extern "C" fn(*mut c_void, *const i32, *mut *mut c_void, i32, *mut i32) -> u32,
    pub get_config_attrib: extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32,
    pub create_context:
        extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void,
    pub destroy_context: extern "C" fn(*mut c_void, *mut c_void) -> u32,
    pub make_current: extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32,
    pub create_window_surface:
        extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void,
    pub create_pbuffer_surface: extern "C" fn(*mut c_void, *mut c_void, *const i32) -> *mut c_void,
    pub destroy_surface: extern "C" fn(*mut c_void, *mut c_void) -> u32,
    pub query_surface: extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32,
    pub query_context: extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32,
    pub surface_attrib: extern "C" fn(*mut c_void, *mut c_void, i32, i32) -> u32,
}

impl Egl {
    /// Bring up the surfaceless display the toolkits use when there is no compositor.
    pub fn bring_up() -> Self {
        let shim = Shim::get();
        let get_platform_display = f!(
            shim.egl,
            "eglGetPlatformDisplay",
            extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void
        );
        let initialize = f!(
            shim.egl,
            "eglInitialize",
            extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32
        );
        let display = get_platform_display(
            EGL_PLATFORM_SURFACELESS_MESA,
            core::ptr::null_mut(),
            core::ptr::null(),
        );
        assert!(
            !display.is_null(),
            "eglGetPlatformDisplay(SURFACELESS) returned EGL_NO_DISPLAY"
        );
        let (mut major, mut minor) = (0, 0);
        assert_eq!(
            initialize(display, &mut major, &mut minor),
            EGL_TRUE,
            "eglInitialize failed"
        );
        // The driver deliberately advertises EGL 1.4 (see `eglInitialize`: the 1.5 core sync contract is
        // not complete), and its platform/sync entry points come from KHR/EXT extensions instead.
        assert_eq!(
            (major, minor),
            (1, 4),
            "eglInitialize must report the version the driver documents"
        );
        Self {
            display,
            get_error: f!(shim.egl, "eglGetError", extern "C" fn() -> i32),
            query_string: f!(
                shim.egl,
                "eglQueryString",
                extern "C" fn(*mut c_void, i32) -> *const c_char
            ),
            get_proc_address: f!(
                shim.egl,
                "eglGetProcAddress",
                extern "C" fn(*const c_char) -> *mut c_void
            ),
            get_configs: f!(
                shim.egl,
                "eglGetConfigs",
                extern "C" fn(*mut c_void, *mut *mut c_void, i32, *mut i32) -> u32
            ),
            choose_config: f!(
                shim.egl,
                "eglChooseConfig",
                extern "C" fn(*mut c_void, *const i32, *mut *mut c_void, i32, *mut i32) -> u32
            ),
            get_config_attrib: f!(
                shim.egl,
                "eglGetConfigAttrib",
                extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32
            ),
            create_context: f!(
                shim.egl,
                "eglCreateContext",
                extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void
            ),
            destroy_context: f!(
                shim.egl,
                "eglDestroyContext",
                extern "C" fn(*mut c_void, *mut c_void) -> u32
            ),
            make_current: f!(
                shim.egl,
                "eglMakeCurrent",
                extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32
            ),
            create_window_surface: f!(
                shim.egl,
                "eglCreateWindowSurface",
                extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void
            ),
            create_pbuffer_surface: Self::extension(
                "eglCreatePbufferSurface",
                f!(
                    shim.egl,
                    "eglGetProcAddress",
                    extern "C" fn(*const c_char) -> *mut c_void
                ),
            ),
            destroy_surface: f!(
                shim.egl,
                "eglDestroySurface",
                extern "C" fn(*mut c_void, *mut c_void) -> u32
            ),
            query_surface: Self::extension(
                "eglQuerySurface",
                f!(
                    shim.egl,
                    "eglGetProcAddress",
                    extern "C" fn(*const c_char) -> *mut c_void
                ),
            ),
            query_context: Self::extension(
                "eglQueryContext",
                f!(
                    shim.egl,
                    "eglGetProcAddress",
                    extern "C" fn(*const c_char) -> *mut c_void
                ),
            ),
            surface_attrib: Self::extension(
                "eglSurfaceAttrib",
                f!(
                    shim.egl,
                    "eglGetProcAddress",
                    extern "C" fn(*const c_char) -> *mut c_void
                ),
            ),
        }
    }

    /// Resolve a core entry point that is reachable through `eglGetProcAddress` (EGL 1.5 §3.10 allows a
    /// library not to export every core symbol, but the getter must resolve it).
    fn extension<T>(
        name: &str,
        get_proc_address: extern "C" fn(*const c_char) -> *mut c_void,
    ) -> T {
        let c = std::ffi::CString::new(name).unwrap();
        let address = get_proc_address(c.as_ptr());
        assert!(
            !address.is_null(),
            "eglGetProcAddress({name}) resolved to null"
        );
        assert_eq!(
            core::mem::size_of::<T>(),
            core::mem::size_of::<*mut c_void>()
        );
        // SAFETY: `T` is the `extern "C" fn` type of the named entry point and is pointer-sized.
        unsafe { core::mem::transmute_copy(&address) }
    }

    /// Drain the error register so a following assertion reads only its own error.
    pub fn clear_error(&self) {
        (self.get_error)();
    }

    /// Every config `eglGetConfigs` enumerates.
    pub fn configs(&self) -> Vec<*mut c_void> {
        let mut count = -1;
        assert_eq!(
            (self.get_configs)(self.display, core::ptr::null_mut(), 0, &mut count),
            EGL_TRUE
        );
        assert!(
            count > 0,
            "eglGetConfigs must advertise at least one config"
        );
        let mut handles = vec![core::ptr::null_mut(); count as usize];
        let mut written = -1;
        assert_eq!(
            (self.get_configs)(self.display, handles.as_mut_ptr(), count, &mut written),
            EGL_TRUE
        );
        assert_eq!(
            written, count,
            "the count-only query and the filled array must agree"
        );
        handles
    }

    /// `eglGetConfigAttrib`, asserting success.
    pub fn attrib(&self, config: *mut c_void, attribute: i32) -> i32 {
        let mut value = i32::MIN;
        assert_eq!(
            (self.get_config_attrib)(self.display, config, attribute, &mut value),
            EGL_TRUE,
            "eglGetConfigAttrib(0x{attribute:04x}) failed with 0x{:04x}",
            (self.get_error)()
        );
        value
    }
}

#[path = "egl_conformance/configs.rs"]
mod configs;
#[path = "egl_conformance/extensions.rs"]
mod extensions;
#[path = "egl_conformance/matrix.rs"]
mod matrix;
#[path = "egl_conformance/queries.rs"]
mod queries;
