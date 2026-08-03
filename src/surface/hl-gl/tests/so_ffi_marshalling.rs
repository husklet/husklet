//! `.so`-integration **marshalling** coverage — the second half of the C-ABI boundary the in-process
//! `hl_gl` service tests cannot reach, complementing `tests/so_ffi_coverage.rs`. Where that file drives the
//! `glGet*` / `glIs*` getter families, this one pins the EGL config/query/API entry points and the GLES
//! array-in draw + buffer-upload path — the exact symbols libepoxy / ANGLE / GTK resolve out of the staged
//! `libEGL.so.1` + `libGLESv2.so.2` via `dlopen`, marshalled through their real C signatures:
//!
//!   * STRING RETURNS         — `eglQueryString` (vendor / version / client-APIs / client+display EXTENSIONS)
//!   * POINTER-OUT + errors   — `eglGetConfigAttrib` (real 8/8/8/8 + 24/8 sizes; BAD_CONFIG / BAD_ATTRIBUTE
//!     / BAD_PARAMETER without writing `value`)
//!   * IN-OUT arrays + count  — `eglChooseConfig` / `eglGetConfigs` (attrib-list in; `configs[]` + `num`
//!     out; null-array count-only; bounded copy; null-`num` → BAD_PARAMETER)
//!   * SCALAR in/out          — `eglBindAPI` / `eglQueryAPI` (per-thread API bind + read-back; a non-GLES
//!     API → EGL_FALSE + BAD_PARAMETER)
//!   * FUNCTION-PTR return    — `eglGetProcAddress` (a core name resolves to a *callable* pointer that
//!     returns the right value; an unknown name / null → null)
//!   * ARRAY-IN contents      — `glBufferData` + `glBufferSubData` ptr+size upload, read back BYTE-FOR-BYTE
//!     through `glMapBufferRange`'s host-storage pointer
//!   * ARRAY-IN draw path     — `glVertexAttribPointer` + `glDrawArrays` / `glDrawElements` over both
//!     VBO-backed and CLIENT-side vertex/index arrays (the shim reads guest memory
//!     through the marshalled pointer), plus their `GL_INVALID_*` error paths
//!   * POINTER-OUT (pixels)   — `glReadPixels` argument + error-path marshalling (7 args; bad enum / value)
//!   * POINTER-OUT (attrib)   — `glGetVertexAttrib{i,f}v` null-safe out-param stores
//!
//! Loading / serialization / arch-skip mirror `so_ffi_coverage.rs` exactly (see that file's header): libEGL
//! FIRST with RTLD_GLOBAL so `libGLESv2`'s `DT_NEEDED` binds it and `hl_shim_state_ptr` resolves; every test
//! grabs `SERIAL` because the gl*/egl* objects share ONE process-global `State`.

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::Mutex;

// ---- dynamic-loader FFI ---------------------------------------------------------------------------
extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

/// Serialize every test — they all drive the ONE process-global GL `State`.
static SERIAL: Mutex<()> = Mutex::new(());

// ==================================================================================================
// GL / EGL enum values a C caller passes raw (mirror the shim's constants)
// ==================================================================================================
const GL_NO_ERROR: u32 = 0;
const GL_INVALID_ENUM: u32 = 0x0500;
const GL_INVALID_VALUE: u32 = 0x0501;
const GL_INVALID_OPERATION: u32 = 0x0502;

const GL_VENDOR: u32 = 0x1F00;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const GL_STATIC_DRAW: u32 = 0x88E4;
const GL_MAP_READ_BIT: u32 = 0x0001;
const GL_FLOAT: u32 = 0x1406;
const GL_TRIANGLES: u32 = 0x0004;
const GL_UNSIGNED_SHORT: u32 = 0x1403;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_RGBA: u32 = 0x1908;
const GL_VERTEX_ATTRIB_ARRAY_ENABLED: u32 = 0x8622;
const GL_COLOR: u32 = 0x1800;
const GL_DEPTH: u32 = 0x1801;
const GL_STENCIL: u32 = 0x1802;
const GL_DEPTH_STENCIL: u32 = 0x84F9;

// EGL config attributes.
const EGL_BUFFER_SIZE: i32 = 0x3020;
const EGL_ALPHA_SIZE: i32 = 0x3021;
const EGL_BLUE_SIZE: i32 = 0x3022;
const EGL_GREEN_SIZE: i32 = 0x3023;
const EGL_RED_SIZE: i32 = 0x3024;
const EGL_DEPTH_SIZE: i32 = 0x3025;
const EGL_STENCIL_SIZE: i32 = 0x3026;
const EGL_CONFIG_ID: i32 = 0x3028;
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_NONE: i32 = 0x3038;
const EGL_COLOR_BUFFER_TYPE: i32 = 0x303F;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_RGB_BUFFER: i32 = 0x308E;
const EGL_WINDOW_BIT: i32 = 0x0004;
const EGL_PBUFFER_BIT: i32 = 0x0001;
const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
const EGL_OPENGL_ES3_BIT: i32 = 0x0040;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;

// EGL query-string names.
const EGL_VENDOR_Q: i32 = 0x3053;
const EGL_BAD_DISPLAY_Q: i32 = 0x3008;
const EGL_VERSION_Q: i32 = 0x3054;
const EGL_EXTENSIONS_Q: i32 = 0x3055;
const EGL_CLIENT_APIS_Q: i32 = 0x308D;

// EGL API enums + errors.
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_OPENVG_API: u32 = 0x30A1;
const EGL_OPENGL_API: u32 = 0x30A2;
const EGL_SUCCESS: i32 = 0x3000;
const EGL_BAD_ATTRIBUTE: i32 = 0x3004;
const EGL_BAD_CONFIG: i32 = 0x3005;
const EGL_BAD_PARAMETER: i32 = 0x300C;
const EGL_TRUE: u32 = 1;
const EGL_FALSE: u32 = 0;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;

// ==================================================================================================
// loader helpers (identical contract to so_ffi_coverage.rs)
// ==================================================================================================
fn stage_dir() -> PathBuf {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the so-ffi test: {other}"),
    };
    PathBuf::from(env!("HL_GL_STAGE_GL")).join(arch)
}

fn dlopen_global(path: &PathBuf) -> *mut c_void {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    if h.is_null() {
        let e = unsafe { dlerror() };
        let msg = if e.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(e) }
                .to_string_lossy()
                .into_owned()
        };
        panic!("dlopen {} failed: {msg}", path.display());
    }
    h
}

fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    let p = unsafe { dlsym(handle, c.as_ptr()) };
    assert!(
        !p.is_null(),
        "symbol {name} not resolvable in the dlopened object (a real ABI gap)"
    );
    p
}

macro_rules! f {
    ($h:expr, $name:literal, $ty:ty) => {
        unsafe { core::mem::transmute::<*mut c_void, $ty>(sym($h, $name)) }
    };
}

struct Shim {
    gles: *mut c_void,
    egl: *mut c_void,
}

impl Shim {
    /// Make a fresh GLES2 context current on the CALLING thread.
    ///
    /// The current binding is per-thread TLS owned by libEGL (`hl_shim_current_ptr`), and every `gl*`
    /// dispatch resolves its share group from it — with no context current, `GlobalState::context`
    /// resolves no group and the call is a silent no-op that sets no GL error. The test harness runs each
    /// `#[test]` on its own thread, so a `gl*` test must establish its own binding, exactly as a real
    /// loader does before issuing GL.
    fn activate(&self) {
        let create_context = f!(
            self.egl,
            "eglCreateContext",
            extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void
        );
        let make_current = f!(
            self.egl,
            "eglMakeCurrent",
            extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32
        );
        let dpy = surfaceless_display(self);
        let attributes = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
        let ctx = create_context(
            dpy,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            attributes.as_ptr(),
        );
        assert!(!ctx.is_null(), "eglCreateContext returns a context token");
        assert_eq!(
            make_current(dpy, core::ptr::null_mut(), core::ptr::null_mut(), ctx),
            EGL_TRUE,
            "eglMakeCurrent binds the context on this thread"
        );
    }
}

fn load() -> Option<Shim> {
    let dir = stage_dir();
    let egl_path = dir.join("libEGL.so.1");
    let gles_path = dir.join("libGLESv2.so.2");
    if !egl_path.exists() || !gles_path.exists() {
        eprintln!(
            "staged shim missing under {} — skipping (guest std not installed)",
            dir.display()
        );
        return None;
    }
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    let egl = dlopen_global(&egl_path);
    let gles = dlopen_global(&gles_path);
    let shim = Shim { gles, egl };
    // Every `gl*` dispatch needs this thread's current binding; establish it before draining errors so
    // any setup noise is drained too.
    shim.activate();
    // Drain both read-and-clear error registers to a clean slate (run order is nondeterministic).
    f!(gles, "glGetError", extern "C" fn() -> u32)();
    f!(egl, "eglGetError", extern "C" fn() -> i32)();
    Some(shim)
}

fn cstr(p: *const c_char) -> String {
    assert!(
        !p.is_null(),
        "an EGL/GL string getter returned null (an app dereferences it unconditionally)"
    );
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

/// Bring up the surfaceless display (the path eglinfo / egl_surfaceless_config.rs drives) + initialize it.
fn surfaceless_display(sh: &Shim) -> *mut c_void {
    let egl_get_proc = f!(
        sh.egl,
        "eglGetProcAddress",
        extern "C" fn(*const c_char) -> *mut c_void
    );
    let egl_initialize = f!(
        sh.egl,
        "eglInitialize",
        extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32
    );
    let get_platform_display: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void = unsafe {
        let c = std::ffi::CString::new("eglGetPlatformDisplayEXT").unwrap();
        let p = egl_get_proc(c.as_ptr());
        assert!(
            !p.is_null(),
            "eglGetProcAddress(eglGetPlatformDisplayEXT) resolves"
        );
        core::mem::transmute(p)
    };
    let dpy = get_platform_display(
        EGL_PLATFORM_SURFACELESS_MESA,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!dpy.is_null(), "surfaceless EGLDisplay is non-null");
    assert_eq!(
        egl_initialize(dpy, &mut 0, &mut 0),
        EGL_TRUE,
        "eglInitialize succeeds"
    );
    dpy
}

// ==================================================================================================
// 1) eglQueryString — string returns (vendor / version / client-APIs / client & display EXTENSIONS)
// ==================================================================================================

#[path = "so_ffi_marshalling/api.rs"]
mod api;
#[path = "so_ffi_marshalling/buffer.rs"]
mod buffer;
#[path = "so_ffi_marshalling/clear.rs"]
mod clear;
#[path = "so_ffi_marshalling/config.rs"]
mod config;
#[path = "so_ffi_marshalling/debug.rs"]
mod debug;
#[path = "so_ffi_marshalling/draw.rs"]
mod draw;
#[path = "so_ffi_marshalling/query.rs"]
mod query;
#[path = "so_ffi_marshalling/read.rs"]
mod read;
