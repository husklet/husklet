//! `eglCreateContext` attribute-list defaults, driven against the staged shim through a real `dlopen`.
//!
//! REGRESSION GUARD (real blocker): a NULL `attrib_list` was rejected with `EGL_BAD_MATCH`, which failed
//! every caller that takes EGL 1.5 §3.7.1 at its word — a NULL or immediately-`EGL_NONE` list means "use
//! all defaults" and is legal. The default version this driver picks is ES 2.0 (the lowest version its only
//! config claims in `EGL_RENDERABLE_TYPE`), so the default context reports the ES 2.0 identity string.

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;

const EGL_NONE: i32 = 0x3038;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_SUCCESS: i32 = 0x3000;
const GL_VERSION: u32 = 0x1F02;
const GL_MAJOR_VERSION: u32 = 0x821B;
const GL_MINOR_VERSION: u32 = 0x821C;

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
        other => panic!("unsupported host arch for the egl context default test: {other}"),
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

#[test]
fn create_context_accepts_a_null_and_an_empty_attribute_list() {
    let dir = stage_dir();
    let (egl_path, gles_path) = (dir.join("libEGL.so.1"), dir.join("libGLESv2.so.2"));
    if !egl_path.exists() || !gles_path.exists() {
        eprintln!(
            "staged shim missing under {} — skipping (guest std not installed)",
            dir.display()
        );
        return;
    }
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    let egl = open(&egl_path);
    let gles = open(&gles_path);

    let get_proc_address: extern "C" fn(*const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(sym(egl, "eglGetProcAddress")) };
    let initialize: extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglInitialize")) };
    let choose_config: extern "C" fn(
        *mut c_void,
        *const i32,
        *mut *mut c_void,
        i32,
        *mut i32,
    ) -> u32 = unsafe { std::mem::transmute(sym(egl, "eglChooseConfig")) };
    let create_context: extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const i32,
    ) -> *mut c_void = unsafe { std::mem::transmute(sym(egl, "eglCreateContext")) };
    let make_current: extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglMakeCurrent")) };
    let destroy_context: extern "C" fn(*mut c_void, *mut c_void) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglDestroyContext")) };
    let get_error: extern "C" fn() -> i32 = unsafe { std::mem::transmute(sym(egl, "eglGetError")) };
    let get_string: extern "C" fn(u32) -> *const c_char =
        unsafe { std::mem::transmute(sym(gles, "glGetString")) };
    let get_integerv: extern "C" fn(u32, *mut i32) =
        unsafe { std::mem::transmute(sym(gles, "glGetIntegerv")) };

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

    // 1) NULL attrib_list — "use all defaults".
    let default_context = create_context(display, config, core::ptr::null_mut(), core::ptr::null());
    assert!(
        !default_context.is_null(),
        "a NULL attribute list is legal (eglGetError = {:#x})",
        get_error()
    );
    assert_eq!(get_error(), EGL_SUCCESS, "and raises no error");

    // 2) An empty list terminated immediately by EGL_NONE — the same defaults.
    let empty = [EGL_NONE];
    let empty_context = create_context(display, config, core::ptr::null_mut(), empty.as_ptr());
    assert!(
        !empty_context.is_null(),
        "an immediately-terminated attribute list is legal (eglGetError = {:#x})",
        get_error()
    );

    // 3) The documented default version: ES 3.0 — the highest COMPLETE profile the driver serves, which is
    //    what a client stating no version should get. This is the rung-0 probe a toolkit performs before any
    //    rendering: a bare eglCreateContext plus glGetString(GL_VERSION). It must not report ES 2.0, or GTK4
    //    concludes the driver cannot run GSK's GL renderer and silently falls back to Cairo over `wl_shm`.
    //    It must not report 3.1 either — GL_MAX_IMAGE_UNITS is 0, so the ES 3.1 compute/image requirements
    //    are unmet and 3.1 is served only on explicit request.
    assert_eq!(
        make_current(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            default_context
        ),
        EGL_TRUE
    );
    let version = unsafe { std::ffi::CStr::from_ptr(get_string(GL_VERSION)) }
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        version, "OpenGL ES 3.0 hl-gl",
        "a bare eglCreateContext reports the driver's highest complete profile"
    );
    // The integer queries must agree with the string — a client may read either.
    let mut major = -1;
    let mut minor = -1;
    get_integerv(GL_MAJOR_VERSION, &mut major);
    get_integerv(GL_MINOR_VERSION, &mut minor);
    assert_eq!(
        (major, minor),
        (3, 0),
        "GL_MAJOR/MINOR_VERSION agree with GL_VERSION for the default context"
    );

    // 4) An explicit ES 3 request still wins over the default.
    let es3 = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
    let es3_context = create_context(display, config, core::ptr::null_mut(), es3.as_ptr());
    assert!(!es3_context.is_null());
    assert_eq!(
        make_current(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            es3_context
        ),
        EGL_TRUE
    );
    let version = unsafe { std::ffi::CStr::from_ptr(get_string(GL_VERSION)) }
        .to_string_lossy()
        .into_owned();
    assert!(
        version.starts_with("OpenGL ES 3."),
        "an explicit ES 3 request is honoured, got {version:?}"
    );

    assert_eq!(destroy_context(display, default_context), EGL_TRUE);
    assert_eq!(destroy_context(display, empty_context), EGL_TRUE);
    assert_eq!(destroy_context(display, es3_context), EGL_TRUE);
}
