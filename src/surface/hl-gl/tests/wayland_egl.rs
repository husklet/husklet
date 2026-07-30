//! Wayland-EGL platform ABI: drive the REAL staged `libwayland-egl.so.1` + `libEGL.so.1` objects through
//! a `dlopen`, exactly as a Wayland GUI app (`weston-simple-egl`) would after linking them.

#![cfg(target_os = "linux")]
//!
//! This proves the wayland window path end to end WITHOUT a compositor:
//!   1. `libEGL` advertises `EGL_EXT_platform_wayland` / `EGL_KHR_platform_wayland` in
//!      `eglQueryString(EGL_NO_DISPLAY, EGL_EXTENSIONS)`, so a real app takes the wayland path.
//!   2. `libwayland-egl` exports the `wl_egl_window_*` ABI (`create`/`resize`/`get_attached_size`/`destroy`)
//!      and its struct is layout-compatible with what libEGL reads back.
//!   3. `eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, wl_display)` opens a display, and
//!      `eglCreateWindowSurface(dpy, cfg, wl_egl_window)` sizes the surface from the `wl_egl_window`
//!      (verified via `eglQuerySurface(EGL_WIDTH/EGL_HEIGHT)`), including the resize + the EGL-1.5
//!      `eglCreatePlatformWindowSurface` spelling.
//!
//! `$HL_GL_NO_WAYLAND` is set so `eglCreateWindowSurface` does NOT attempt a live compositor socket (there
//! is none in the test harness) — it only exercises the ABI + sizing, never a fake present.

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;

// ---- EGL enum values the app passes (mirrors the shim's constants) ----
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_PLATFORM_WAYLAND_KHR: u32 = 0x31D8;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_TRUE: u32 = 1;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;

/// The staged shim directory for the host arch (`~/.hl/gl/<arch>/`), where `build.rs` installs the libs.
fn stage_dir() -> PathBuf {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the wayland-egl test: {other}"),
    };
    PathBuf::from(env!("HL_GL_STAGE_GL")).join(arch)
}

/// `dlopen(path, RTLD_NOW)` or panic with the `dlerror()` reason.
fn open(path: &PathBuf) -> *mut c_void {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
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

/// Resolve `name` in `handle` to a raw pointer (panics if unresolved).
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
fn wayland_egl_abi_and_window_sizing_end_to_end() {
    let dir = stage_dir();
    let egl_path = dir.join("libEGL.so.1");
    let wl_path = dir.join("libwayland-egl.so.1");
    if !egl_path.exists() || !wl_path.exists() {
        // The x86_64 guest shim is skipped on a host without its rust std (see build.rs); nothing to drive.
        eprintln!(
            "staged shims missing at {} — skipping (guest std not installed)",
            dir.display()
        );
        return;
    }

    // No live compositor in the harness: exercise the ABI + sizing only, never a socket connect.
    std::env::set_var("HL_GL_NO_WAYLAND", "1");

    let egl = open(&egl_path);
    let wl = open(&wl_path);

    // ---- resolve the entry points we drive ----
    let egl_query_string: extern "C" fn(*mut c_void, i32) -> *const c_char =
        unsafe { std::mem::transmute(sym(egl, "eglQueryString")) };
    let egl_get_platform_display: extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void =
        unsafe { std::mem::transmute(sym(egl, "eglGetPlatformDisplay")) };
    let egl_initialize: extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglInitialize")) };
    let egl_create_window_surface: extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const i32,
    ) -> *mut c_void = unsafe { std::mem::transmute(sym(egl, "eglCreateWindowSurface")) };
    let egl_create_platform_window_surface: extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const isize,
    ) -> *mut c_void = unsafe { std::mem::transmute(sym(egl, "eglCreatePlatformWindowSurface")) };
    let egl_query_surface: extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglQuerySurface")) };

    let wl_egl_window_create: extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
        unsafe { std::mem::transmute(sym(wl, "wl_egl_window_create")) };
    let wl_egl_window_get_attached_size: extern "C" fn(*mut c_void, *mut i32, *mut i32) =
        unsafe { std::mem::transmute(sym(wl, "wl_egl_window_get_attached_size")) };
    let wl_egl_window_resize: extern "C" fn(*mut c_void, i32, i32, i32, i32) =
        unsafe { std::mem::transmute(sym(wl, "wl_egl_window_resize")) };
    let wl_egl_window_destroy: extern "C" fn(*mut c_void) =
        unsafe { std::mem::transmute(sym(wl, "wl_egl_window_destroy")) };

    // 1) The CLIENT extension string (EGL_NO_DISPLAY) advertises the wayland platform.
    let ext = egl_query_string(core::ptr::null_mut(), EGL_EXTENSIONS);
    assert!(!ext.is_null());
    let ext = unsafe { std::ffi::CStr::from_ptr(ext) }
        .to_string_lossy()
        .into_owned();
    assert!(
        ext.contains("EGL_KHR_platform_wayland"),
        "client extensions advertise KHR platform_wayland: {ext:?}"
    );
    assert!(
        ext.contains("EGL_EXT_platform_wayland"),
        "client extensions advertise EXT platform_wayland: {ext:?}"
    );

    // 2) The wayland-egl ABI: wrap a (fake) wl_surface in a wl_egl_window at 640x480.
    let fake_wl_surface = 0x1234_5678usize as *mut c_void;
    let win = wl_egl_window_create(fake_wl_surface, 640, 480);
    assert!(!win.is_null(), "wl_egl_window_create returns a handle");
    let (mut aw, mut ah) = (0i32, 0i32);
    wl_egl_window_get_attached_size(win, &mut aw, &mut ah);
    assert_eq!(
        (aw, ah),
        (640, 480),
        "attached size defaults to the created size"
    );

    // 3) Open a wayland platform display + a window surface FROM the wl_egl_window; the surface takes the
    //    window's size (proving libEGL parsed the wl_egl_window that libwayland-egl produced).
    let fake_wl_display = 0x9999usize as *mut c_void;
    let dpy =
        egl_get_platform_display(EGL_PLATFORM_WAYLAND_KHR, fake_wl_display, core::ptr::null());
    assert!(
        !dpy.is_null(),
        "eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR) opens a display"
    );
    assert_eq!(
        egl_initialize(dpy, core::ptr::null_mut(), core::ptr::null_mut()),
        EGL_TRUE
    );

    let config = 1usize as *mut c_void;
    let surface = egl_create_window_surface(dpy, config, win, core::ptr::null());
    assert!(
        !surface.is_null(),
        "eglCreateWindowSurface(wl_egl_window) returns a surface"
    );
    let mut w = 0i32;
    let mut h = 0i32;
    assert_eq!(egl_query_surface(dpy, surface, EGL_WIDTH, &mut w), EGL_TRUE);
    assert_eq!(
        egl_query_surface(dpy, surface, EGL_HEIGHT, &mut h),
        EGL_TRUE
    );
    assert_eq!(
        (w, h),
        (640, 480),
        "the window surface is sized from the wl_egl_window"
    );

    // 4) Resize the wl_egl_window, then the EGL-1.5 eglCreatePlatformWindowSurface spelling picks up the
    //    new size.
    wl_egl_window_resize(win, 800, 600, 0, 0);
    let surface2 = egl_create_platform_window_surface(dpy, config, win, core::ptr::null());
    assert!(!surface2.is_null());
    assert_eq!(
        egl_query_surface(dpy, surface2, EGL_WIDTH, &mut w),
        EGL_TRUE
    );
    assert_eq!(
        egl_query_surface(dpy, surface2, EGL_HEIGHT, &mut h),
        EGL_TRUE
    );
    assert_eq!(
        (w, h),
        (800, 600),
        "eglCreatePlatformWindowSurface re-reads the resized wl_egl_window"
    );

    wl_egl_window_destroy(win);
}
