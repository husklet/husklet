//! `eglCreatePbufferSurface` behaviour, driven against the staged shim through a real `dlopen`.
//!
//! The single advertised `EGLConfig` claims `EGL_PBUFFER_BIT`, which is the promise that an offscreen
//! pbuffer surface is renderable. EGL 1.5 §3.5.2 makes the pbuffer's size come from the `EGL_WIDTH` /
//! `EGL_HEIGHT` attributes of `attrib_list` (defaulting to 0), and `eglQuerySurface` must report those
//! values back. `--off-screen` benchmark modes and Chrome's GPU-process offscreen surfaces depend on it.

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;

const EGL_HEIGHT: i32 = 0x3056;
const EGL_WIDTH: i32 = 0x3057;
const EGL_NONE: i32 = 0x3038;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_PBUFFER_BIT: i32 = 0x0001;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_NO_ERROR: u32 = 0;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

fn stage_dir() -> PathBuf {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the egl pbuffer test: {other}"),
    };
    PathBuf::from(env!("HL_GL_STAGE_GL")).join(arch)
}

fn open(path: &PathBuf) -> *mut c_void {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    if handle.is_null() {
        let error = unsafe { dlerror() };
        let message = if error.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        };
        panic!("dlopen {} failed: {message}", path.display());
    }
    handle
}

fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    let p = unsafe { dlsym(handle, c.as_ptr()) };
    assert!(
        !p.is_null(),
        "symbol {name} not found in the dlopened object"
    );
    p
}

/// The staged pair of guest libraries, or `None` when the guest shim is not staged.
struct Shim {
    egl: *mut c_void,
    gles: *mut c_void,
}

fn load() -> Option<Shim> {
    let dir = stage_dir();
    let (egl_path, gles_path) = (dir.join("libEGL.so.1"), dir.join("libGLESv2.so.2"));
    if !egl_path.exists() || !gles_path.exists() {
        eprintln!(
            "staged shim missing under {} — skipping (guest std not installed)",
            dir.display()
        );
        return None;
    }
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    let egl = open(&egl_path);
    let gles = open(&gles_path);
    Some(Shim { egl, gles })
}

#[test]
fn pbuffer_surface_takes_its_size_from_the_attributes() {
    let Some(shim) = load() else { return };

    let get_proc_address: extern "C" fn(*const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(sym(shim.egl, "eglGetProcAddress")) };
    let initialize: extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(shim.egl, "eglInitialize")) };
    let choose_config: extern "C" fn(
        *mut c_void,
        *const i32,
        *mut *mut c_void,
        i32,
        *mut i32,
    ) -> u32 = unsafe { std::mem::transmute(sym(shim.egl, "eglChooseConfig")) };
    let get_config_attrib: extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(shim.egl, "eglGetConfigAttrib")) };
    let create_pbuffer: extern "C" fn(*mut c_void, *mut c_void, *const i32) -> *mut c_void =
        unsafe { std::mem::transmute(sym(shim.egl, "eglCreatePbufferSurface")) };
    let query_surface: extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(shim.egl, "eglQuerySurface")) };
    let create_context: extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const i32,
    ) -> *mut c_void = unsafe { std::mem::transmute(sym(shim.egl, "eglCreateContext")) };
    let make_current: extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32 =
        unsafe { std::mem::transmute(sym(shim.egl, "eglMakeCurrent")) };
    let destroy_surface: extern "C" fn(*mut c_void, *mut c_void) -> u32 =
        unsafe { std::mem::transmute(sym(shim.egl, "eglDestroySurface")) };
    let clear_color: extern "C" fn(f32, f32, f32, f32) =
        unsafe { std::mem::transmute(sym(shim.gles, "glClearColor")) };
    let clear: extern "C" fn(u32) = unsafe { std::mem::transmute(sym(shim.gles, "glClear")) };
    let get_error: extern "C" fn() -> u32 =
        unsafe { std::mem::transmute(sym(shim.gles, "glGetError")) };

    let get_platform_display: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void = unsafe {
        let name = std::ffi::CString::new("eglGetPlatformDisplayEXT").unwrap();
        std::mem::transmute(get_proc_address(name.as_ptr()))
    };
    let display = get_platform_display(
        EGL_PLATFORM_SURFACELESS_MESA,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert_eq!(initialize(display, &mut 0, &mut 0), EGL_TRUE);

    let mut config: *mut c_void = core::ptr::null_mut();
    let mut configs = -1i32;
    assert_eq!(
        choose_config(display, core::ptr::null(), &mut config, 1, &mut configs),
        EGL_TRUE
    );
    assert_eq!(configs, 1);
    // The bit under test: the config claims pbuffer support, so the rest of this test must hold.
    let mut surface_type = 0i32;
    assert_eq!(
        get_config_attrib(display, config, EGL_SURFACE_TYPE, &mut surface_type),
        EGL_TRUE
    );
    assert_ne!(
        surface_type & EGL_PBUFFER_BIT,
        0,
        "the config advertises EGL_PBUFFER_BIT"
    );

    // EGL 1.5 §3.5.2: the pbuffer's size is EGL_WIDTH / EGL_HEIGHT from attrib_list.
    let (width, height) = (64i32, 48i32);
    let attributes = [EGL_WIDTH, width, EGL_HEIGHT, height, EGL_NONE];
    let surface = create_pbuffer(display, config, attributes.as_ptr());
    assert!(!surface.is_null(), "eglCreatePbufferSurface succeeds");

    let mut reported = -1i32;
    assert_eq!(
        query_surface(display, surface, EGL_WIDTH, &mut reported),
        EGL_TRUE
    );
    assert_eq!(reported, width, "EGL_WIDTH reads back the requested width");
    assert_eq!(
        query_surface(display, surface, EGL_HEIGHT, &mut reported),
        EGL_TRUE
    );
    assert_eq!(
        reported, height,
        "EGL_HEIGHT reads back the requested height"
    );

    let context_attributes = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
    let context = create_context(
        display,
        config,
        core::ptr::null_mut(),
        context_attributes.as_ptr(),
    );
    assert!(!context.is_null(), "eglCreateContext succeeds");
    assert_eq!(
        make_current(display, surface, surface, context),
        EGL_TRUE,
        "a pbuffer surface can be made current"
    );

    // Rendering into a pbuffer is accepted: the clear records against framebuffer 0 without a GL error.
    // The PIXELS are deliberately not asserted here — this process has no host GPU service, so a readback
    // has nothing to return (a window surface behaves the same way). Pixel truth for the offscreen path
    // belongs to the host e2e ladder.
    clear_color(1.0, 0.0, 0.0, 1.0);
    clear(GL_COLOR_BUFFER_BIT);
    assert_eq!(
        get_error(),
        GL_NO_ERROR,
        "clearing a pbuffer's default framebuffer raises no GL error"
    );

    assert_eq!(destroy_surface(display, surface), EGL_TRUE);
}
