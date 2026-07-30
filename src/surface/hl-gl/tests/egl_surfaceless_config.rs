//! Surfaceless display + config enumeration ABI — the path the real Khronos `eglinfo` (and every GL
//! toolkit's EGL bring-up: GTK, Qt, Chromium/Chrome ozone, Zed) drives at startup, run against the staged
//! `libEGL.so.1` through a `dlopen`, exactly as those programs do.

#![cfg(target_os = "linux")]
//!
//! REGRESSION GUARD (real crash): `eglinfo` SIGSEGV'd against our libEGL because the client extension
//! string advertises `EGL_EXT_platform_base`, so a caller resolves `eglGetPlatformDisplayEXT` via
//! `eglGetProcAddress` and — the extension being advertised is the promise it exists — CALLS the returned
//! pointer WITHOUT a null check. We resolved it to null, so the caller jumped through null and crashed.
//! This test asserts every entry point the advertised extensions promise is resolvable + callable, and
//! exercises the `eglGetConfigs` / `eglChooseConfig` / `eglGetConfigAttrib` contract (null-buffer count,
//! bounded copy, truthful attributes, `EGL_BAD_ATTRIBUTE` for an unknown attribute).

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;

// ---- EGL enum values the caller passes (mirror the shim's constants) ----
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_VENDOR: i32 = 0x3053;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_FALSE: u32 = 0;
const EGL_SUCCESS: i32 = 0x3000;
const EGL_BAD_ATTRIBUTE: i32 = 0x3004;
// config attributes
const EGL_RED_SIZE: i32 = 0x3024;
const EGL_GREEN_SIZE: i32 = 0x3023;
const EGL_BLUE_SIZE: i32 = 0x3022;
const EGL_ALPHA_SIZE: i32 = 0x3021;
const EGL_DEPTH_SIZE: i32 = 0x3025;
const EGL_STENCIL_SIZE: i32 = 0x3026;
const EGL_CONFIG_ID: i32 = 0x3028;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
const EGL_OPENGL_ES3_BIT: i32 = 0x0040;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;

fn stage_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the egl config test: {other}"),
    };
    PathBuf::from(home).join(".hl/gl").join(arch)
}

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

fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    let p = unsafe { dlsym(handle, c.as_ptr()) };
    assert!(
        !p.is_null(),
        "symbol {name} not found in the dlopened object"
    );
    p
}

fn cstr(p: *const c_char) -> String {
    assert!(!p.is_null(), "EGL string getter returned null");
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn surfaceless_display_and_config_enumeration_end_to_end() {
    let dir = stage_dir();
    let egl_path = dir.join("libEGL.so.1");
    if !egl_path.exists() {
        // The x86_64 guest shim is skipped on a host without its rust std (see build.rs); nothing to drive.
        eprintln!(
            "staged libEGL missing at {} — skipping (guest std not installed)",
            dir.display()
        );
        return;
    }
    // Do not touch a live compositor from any surface path.
    std::env::set_var("HL_GL_NO_WAYLAND", "1");

    let egl = open(&egl_path);

    let egl_query_string: extern "C" fn(*mut c_void, i32) -> *const c_char =
        unsafe { std::mem::transmute(sym(egl, "eglQueryString")) };
    let egl_get_proc_address: extern "C" fn(*const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(sym(egl, "eglGetProcAddress")) };
    let egl_initialize: extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglInitialize")) };
    let egl_get_configs: extern "C" fn(*mut c_void, *mut *mut c_void, i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglGetConfigs")) };
    let egl_choose_config: extern "C" fn(
        *mut c_void,
        *const i32,
        *mut *mut c_void,
        i32,
        *mut i32,
    ) -> u32 = unsafe { std::mem::transmute(sym(egl, "eglChooseConfig")) };
    let egl_get_config_attrib: extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglGetConfigAttrib")) };
    let egl_get_error: extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(sym(egl, "eglGetError")) };

    // 1) The client extension string advertises EGL_EXT_platform_base — the promise below.
    let ext = cstr(egl_query_string(core::ptr::null_mut(), EGL_EXTENSIONS));
    assert!(
        ext.contains("EGL_EXT_platform_base"),
        "client extensions advertise EGL_EXT_platform_base: {ext:?}"
    );

    // 2) REGRESSION: every entry point EGL_EXT_platform_base promises MUST resolve non-null — a caller
    //    calls the returned pointer without a null check, so a null here is a jump-through-null crash.
    for name in [
        "eglGetPlatformDisplayEXT",
        "eglCreatePlatformWindowSurfaceEXT",
        "eglCreatePlatformPixmapSurfaceEXT",
    ] {
        let c = std::ffi::CString::new(name).unwrap();
        let p = egl_get_proc_address(c.as_ptr());
        assert!(
            !p.is_null(),
            "eglGetProcAddress({name}) must resolve (advertised by EGL_EXT_platform_base)"
        );
    }
    // An unadvertised / unknown extension name still resolves to null (spec-legal "not found").
    let bogus = std::ffi::CString::new("eglNoSuchEntryPointEXT").unwrap();
    assert!(
        egl_get_proc_address(bogus.as_ptr()).is_null(),
        "unknown proc resolves to null"
    );

    // 3) Surfaceless display bring-up through eglGetPlatformDisplayEXT (the eglinfo surfaceless path):
    //    resolve → call → the returned display initializes.
    let gpd = std::ffi::CString::new("eglGetPlatformDisplayEXT").unwrap();
    let get_platform_display_ext: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void =
        unsafe { std::mem::transmute(egl_get_proc_address(gpd.as_ptr())) };
    let dpy = get_platform_display_ext(
        EGL_PLATFORM_SURFACELESS_MESA,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(
        !dpy.is_null(),
        "eglGetPlatformDisplayEXT(SURFACELESS) returns a display"
    );
    let (mut major, mut minor) = (0i32, 0i32);
    assert_eq!(
        egl_initialize(dpy, &mut major, &mut minor),
        EGL_TRUE,
        "surfaceless display initializes"
    );
    assert_eq!(
        (major, minor),
        (1, 4),
        "advertises the complete EGL core version it implements"
    );
    assert_eq!(
        cstr(egl_query_string(dpy, EGL_VENDOR)),
        "hl-gl",
        "our vendor over the initialized display"
    );

    // 4) eglGetConfigs contract — null buffer reports the count, then a bounded copy fills the array.
    let mut count = -1i32;
    assert_eq!(
        egl_get_configs(dpy, core::ptr::null_mut(), 0, &mut count),
        EGL_TRUE
    );
    assert!(
        count >= 1,
        "at least one config is advertised (count={count})"
    );
    let mut buf: [*mut c_void; 8] = [core::ptr::null_mut(); 8];
    let mut got = -1i32;
    assert_eq!(
        egl_get_configs(dpy, buf.as_mut_ptr(), buf.len() as i32, &mut got),
        EGL_TRUE
    );
    assert_eq!(
        got, count,
        "the bounded copy returns every advertised config"
    );
    assert!(!buf[0].is_null(), "the first config handle is written");
    // A zero-capacity array writes nothing and reports zero (no OOB store).
    let mut zero = -1i32;
    assert_eq!(
        egl_get_configs(dpy, buf.as_mut_ptr(), 0, &mut zero),
        EGL_TRUE
    );
    assert_eq!(zero, 0, "config_size 0 copies no handles");

    // 5) eglChooseConfig with a NULL attrib list (match-all) returns a usable config.
    let mut chosen: *mut c_void = core::ptr::null_mut();
    let mut nchosen = -1i32;
    assert_eq!(
        egl_choose_config(dpy, core::ptr::null(), &mut chosen, 1, &mut nchosen),
        EGL_TRUE,
        "eglChooseConfig accepts a null attrib list"
    );
    assert_eq!(nchosen, 1);
    assert!(!chosen.is_null(), "eglChooseConfig returns a config handle");

    // 6) eglGetConfigAttrib returns TRUTHFUL attributes for our config (RGBA8 / D24 / S8, ES2+ES3).
    let attr = |a: i32| -> (u32, i32) {
        let mut v = -12345i32;
        let r = egl_get_config_attrib(dpy, chosen, a, &mut v);
        (r, v)
    };
    assert_eq!(attr(EGL_RED_SIZE), (EGL_TRUE, 8));
    assert_eq!(attr(EGL_GREEN_SIZE), (EGL_TRUE, 8));
    assert_eq!(attr(EGL_BLUE_SIZE), (EGL_TRUE, 8));
    assert_eq!(attr(EGL_ALPHA_SIZE), (EGL_TRUE, 8));
    assert_eq!(attr(EGL_DEPTH_SIZE), (EGL_TRUE, 24));
    assert_eq!(attr(EGL_STENCIL_SIZE), (EGL_TRUE, 8));
    assert_eq!(attr(EGL_CONFIG_ID).0, EGL_TRUE);
    assert!(
        attr(EGL_CONFIG_ID).1 > 0,
        "config id is a valid (nonzero) id"
    );
    assert_eq!(
        attr(EGL_RENDERABLE_TYPE),
        (EGL_TRUE, EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES3_BIT)
    );

    // 7) An UNKNOWN attribute is EGL_BAD_ATTRIBUTE + EGL_FALSE (never a fabricated value / a deref).
    assert_eq!(
        egl_get_error(),
        EGL_SUCCESS,
        "no pending error before the bad query"
    );
    let mut v = -12345i32;
    assert_eq!(
        egl_get_config_attrib(dpy, chosen, 0x0BAD_BEEFu32 as i32, &mut v),
        EGL_FALSE,
        "unknown attribute fails cleanly"
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_ATTRIBUTE,
        "and raises EGL_BAD_ATTRIBUTE"
    );
}
