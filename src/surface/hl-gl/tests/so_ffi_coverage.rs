//! `.so`-integration coverage for the ~65 GL/EGL entry points whose ONLY reachable code lives in the
//! shim's C-ABI marshalling (`shim/egl/src/driver.rs`) — the `glGet*` / `glIs*` / `egl*` families that
//! read/write raw guest pointers and hand back C-ABI-widened results. The `hl_gl` lib exhaustive pass
//! (commit 1b2a66f8) exercises the `hl_gl::service::*` semantics in-process, but it CANNOT reach the
//! `extern "C"` bodies: the `data.is_null()` guards, the `*data.add(i) = buf[i]` out-param stores, the
//! `as i64`/`as u8`/`as f32` width conversions, the `*mut c_void` token round-trips, and the `bool → u32`
//! / `bool → u8` `GLboolean` ABI returns. Chrome (ANGLE) hits these through `dlopen` on `libGLESv2.so.2`
//! / `libEGL.so.1` EXACTLY as this test does, so a marshalling bug here is a real Chrome-facing bug.
//!
//! LOADING: `libGLESv2.so.2` carries a `DT_NEEDED libEGL.so.1` (it imports the shared-state accessor
//! `hl_shim_state_ptr` from libEGL — the two objects share ONE process-global `State`). The staged dir has
//! no `RPATH`, and `LD_LIBRARY_PATH` is frozen at process start, so we `dlopen` `libEGL.so.1` FIRST with
//! `RTLD_GLOBAL` (its `DT_SONAME` is `libEGL.so.1`); `libGLESv2.so.2`'s `NEEDED` then binds to the
//! already-loaded object and `hl_shim_state_ptr` resolves from the global scope.
//!
//! SERIALIZATION: `gl*` (libGLESv2) and `egl*` (libEGL) share the process-global `State`, but cargo runs
//! the `#[test]`s in this ONE binary on parallel threads. So every test grabs [`SERIAL`] first — the GL
//! object model is a single shared context, and concurrent drivers would race it.

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
// GL / GLES enum values the caller passes (mirror the shim's constants; a C app passes these raw)
// ==================================================================================================
const GL_NO_ERROR: u32 = 0;
const GL_INVALID_ENUM: u32 = 0x0500;
const GL_INVALID_VALUE: u32 = 0x0501;
const GL_TRUE: i32 = 1;
const GL_FALSE: i32 = 0;

const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;
const GL_EXTENSIONS: u32 = 0x1F03;
const GL_SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;
const GL_NUM_EXTENSIONS: u32 = 0x821D;

const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;
const GL_MAX_VERTEX_ATTRIBS: u32 = 0x8869;
const GL_MAJOR_VERSION: u32 = 0x821B;
const GL_MINOR_VERSION: u32 = 0x821C;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_DEPTH_BITS: u32 = 0x0D56;
const GL_STENCIL_BITS: u32 = 0x0D57;

const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;
const GL_DEPTH_CLEAR_VALUE: u32 = 0x0B73;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_DEPTH_WRITEMASK: u32 = 0x0B72;

const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_UNIFORM_BUFFER: u32 = 0x8A11;
const GL_UNIFORM_BUFFER_BINDING: u32 = 0x8A28;
const GL_BUFFER_SIZE: u32 = 0x8764;
const GL_BUFFER_USAGE: u32 = 0x8765;
const GL_STATIC_DRAW: u32 = 0x88E4;

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_NEAREST: i32 = 0x2600;
const GL_LINEAR: i32 = 0x2601;
const GL_RGBA: u32 = 0x1908;
const GL_RGBA8: u32 = 0x8058;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_WIDTH: u32 = 0x1000;
const GL_TEXTURE_HEIGHT: u32 = 0x1001;
const GL_TEXTURE_INTERNAL_FORMAT: u32 = 0x1003;
const GL_COMPRESSED_RGBA8_ETC2_EAC: u32 = 0x9278;

const GL_RENDERBUFFER: u32 = 0x8D41;
const GL_RENDERBUFFER_WIDTH: u32 = 0x8D42;
const GL_RENDERBUFFER_HEIGHT: u32 = 0x8D43;
const GL_RENDERBUFFER_INTERNAL_FORMAT: u32 = 0x8D44;

const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE: u32 = 0x8CD0;
const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: u32 = 0x8CD1;
const GL_TEXTURE: i32 = 0x1702;

const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_VALIDATE_STATUS: u32 = 0x8B83;
const GL_SHADER_TYPE: u32 = 0x8B4F;
const GL_SHADER_SOURCE_LENGTH: u32 = 0x8B88;
const GL_ACTIVE_UNIFORMS: u32 = 0x8B86;
const GL_ACTIVE_UNIFORM_MAX_LENGTH: u32 = 0x8B87;
const GL_ACTIVE_ATTRIBUTES: u32 = 0x8B89;
const GL_ACTIVE_ATTRIBUTE_MAX_LENGTH: u32 = 0x8B8A;
const GL_ATTACHED_SHADERS: u32 = 0x8B85;
const GL_INFO_LOG_LENGTH: u32 = 0x8B84;

const GL_FLOAT: u32 = 0x1406;
const GL_FLOAT_VEC2: u32 = 0x8B50;
const GL_FLOAT_VEC3: u32 = 0x8B51;
const GL_FLOAT_VEC4: u32 = 0x8B52;
const GL_SAMPLER_2D: u32 = 0x8B5E;

const GL_SYNC_GPU_COMMANDS_COMPLETE: u32 = 0x9117;
const GL_WAIT_FAILED: u32 = 0x911D;

// ==================================================================================================
// EGL enum values
// ==================================================================================================
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_FALSE: u32 = 0;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_CONTEXT_CLIENT_TYPE: i32 = 0x3097;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_RENDER_BUFFER: i32 = 0x3086;
const EGL_BACK_BUFFER: i32 = 0x3084;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_SYNC_FENCE: u32 = 0x30F9;

// ==================================================================================================
// loader helpers
// ==================================================================================================
fn stage_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the so-ffi test: {other}"),
    };
    PathBuf::from(home).join(".hl/gl").join(arch)
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
        "symbol {name} not resolvable in the dlopened object"
    );
    p
}

/// Resolve a symbol from a handle and transmute it to the given `extern "C" fn` type.
macro_rules! f {
    ($h:expr, $name:literal, $ty:ty) => {
        unsafe { core::mem::transmute::<*mut c_void, $ty>(sym($h, $name)) }
    };
}

/// The loaded shim: (libGLESv2 handle for `gl*`, libEGL handle for `egl*`). `None` when the shim is not
/// staged (an x86_64 host without the guest std — see build.rs), so the test skips instead of failing.
struct Shim {
    gles: *mut c_void,
    egl: *mut c_void,
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
    // Never touch a live compositor from any surface path.
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    // libEGL FIRST + RTLD_GLOBAL so libGLESv2's `DT_NEEDED libEGL.so.1` binds it and `hl_shim_state_ptr`
    // resolves from the global scope; then the gl* object.
    let egl = dlopen_global(&egl_path);
    let gles = dlopen_global(&gles_path);
    // The GL/EGL error registers live on the ONE process-global `State`. Tests are serialized (SERIAL),
    // but their run ORDER is nondeterministic, so drain both registers to a clean slate before each test
    // asserts on error state (glGetError / eglGetError are read-and-clear).
    let gl_get_error = f!(gles, "glGetError", extern "C" fn() -> u32);
    let egl_get_error = f!(egl, "eglGetError", extern "C" fn() -> i32);
    gl_get_error();
    egl_get_error();
    Some(Shim { gles, egl })
}

fn cstr(p: *const u8) -> String {
    assert!(
        !p.is_null(),
        "GL string getter returned null (an app dereferences it unconditionally)"
    );
    unsafe { std::ffi::CStr::from_ptr(p as *const c_char) }
        .to_string_lossy()
        .into_owned()
}

// ==================================================================================================
// 1) glGetString / glGetStringi / glGetIntegerv / glGetFloatv / glGetBooleanv / glGetInteger64v
// ==================================================================================================

#[path = "so_ffi_coverage/egl.rs"]
mod egl;
#[path = "so_ffi_coverage/identity.rs"]
mod identity;
#[path = "so_ffi_coverage/image.rs"]
mod image;
#[path = "so_ffi_coverage/indexed.rs"]
mod indexed;
#[path = "so_ffi_coverage/lifetime.rs"]
mod lifetime;
#[path = "so_ffi_coverage/object.rs"]
mod object;
#[path = "so_ffi_coverage/reflection.rs"]
mod reflection;
#[path = "so_ffi_coverage/texture.rs"]
mod texture;
