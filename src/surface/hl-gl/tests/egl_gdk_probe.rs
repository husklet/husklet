//! GDK's GL-context probe, run against the staged shim through a real `dlopen`.
//!
//! GTK4's GSK renderer refuses anything below ES 3.0 and falls back to the software Cairo renderer over
//! `wl_shm` when it cannot get a context. This test records, for each version GDK asks for, whether
//! `eglCreateContext` succeeds and what `glGetString(GL_VERSION)` then reports — the table that says
//! whether GTK's failure is a version-report problem or a context-creation problem.
//!
//! Each version is probed twice: with the version attributes alone, and with `EGL_CONTEXT_FLAGS_KHR = 0`
//! alongside them. An unrecognized attribute is `EGL_BAD_ATTRIBUTE`, so an attribute this driver refuses is
//! a TOTAL failure of context creation at every version — indistinguishable, in a toolkit's log, from
//! "no supported version". `EGL_CONTEXT_FLAGS_KHR` belongs to `EGL_KHR_create_context`, which this driver
//! advertises in its display extension string, so it must be accepted.

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

const EGL_NONE: i32 = 0x3038;
const EGL_CONTEXT_MAJOR_VERSION: i32 = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: i32 = 0x30FB;
// `EGL_KHR_create_context`, which this driver advertises: the flags word (default 0) that carries the
// debug / forward-compatible / robust-access request bits.
const EGL_CONTEXT_FLAGS_KHR: i32 = 0x30FC;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_SUCCESS: i32 = 0x3000;
const GL_VERSION: u32 = 0x1F02;

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
        other => panic!("unsupported host arch for the gdk probe test: {other}"),
    };
    PathBuf::from(env!("HL_GL_STAGE_GL")).join(arch)
}

fn open(path: &Path) -> *mut c_void {
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
    assert!(!p.is_null(), "symbol {name} not found");
    p
}

/// One row of the probe table: what GDK asked for, whether it got a context, and what the context said.
#[derive(Debug)]
struct Row {
    asked: (i32, i32),
    /// Whether `EGL_CONTEXT_FLAGS_KHR = 0` was passed alongside the version.
    flags: bool,
    created: bool,
    egl_error: i32,
    reported: String,
}

type CreateContext =
    extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void;

#[test]
fn gdk_descending_es_probe_reports_creation_and_version_per_request() {
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
    let create_context: CreateContext =
        unsafe { std::mem::transmute(sym(egl, "eglCreateContext")) };
    let make_current: extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglMakeCurrent")) };
    let destroy_context: extern "C" fn(*mut c_void, *mut c_void) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglDestroyContext")) };
    let get_error: extern "C" fn() -> i32 = unsafe { std::mem::transmute(sym(egl, "eglGetError")) };
    let get_string: extern "C" fn(u32) -> *const c_char =
        unsafe { std::mem::transmute(sym(gles, "glGetString")) };

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
    let mut configs = 0i32;
    assert_eq!(
        choose_config(display, [EGL_NONE].as_ptr(), &mut config, 1, &mut configs),
        EGL_TRUE
    );
    assert_eq!(configs, 1);

    // ASSUMED SEQUENCE: GDK probes candidate ES versions in DESCENDING order and keeps the best that
    // succeeds; GTK4's GSK renderer then refuses anything below 3.0. GTK's own source is not available
    // offline here, so the order is assumed — but the table below is order-independent: it records every
    // candidate independently.
    let candidates = [(3, 2), (3, 1), (3, 0), (2, 0)];
    let mut table = Vec::new();
    for (major, minor) in candidates {
        for flags in [false, true] {
            let attributes: Vec<i32> = if flags {
                vec![
                    EGL_CONTEXT_MAJOR_VERSION,
                    major,
                    EGL_CONTEXT_MINOR_VERSION,
                    minor,
                    EGL_CONTEXT_FLAGS_KHR,
                    0,
                    EGL_NONE,
                ]
            } else {
                vec![
                    EGL_CONTEXT_MAJOR_VERSION,
                    major,
                    EGL_CONTEXT_MINOR_VERSION,
                    minor,
                    EGL_NONE,
                ]
            };
            let _ = get_error();
            let context =
                create_context(display, config, core::ptr::null_mut(), attributes.as_ptr());
            let egl_error = get_error();
            let mut row = Row {
                asked: (major, minor),
                flags,
                created: !context.is_null(),
                egl_error,
                reported: String::new(),
            };
            if row.created {
                assert_eq!(
                    make_current(
                        display,
                        core::ptr::null_mut(),
                        core::ptr::null_mut(),
                        context
                    ),
                    EGL_TRUE,
                    "a created context must bind"
                );
                let version = get_string(GL_VERSION);
                assert!(!version.is_null(), "a bound context reports GL_VERSION");
                row.reported = unsafe { std::ffi::CStr::from_ptr(version) }
                    .to_string_lossy()
                    .into_owned();
                make_current(
                    display,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                );
                destroy_context(display, context);
            }
            println!(
                "GDK probe {major}.{minor} flags_khr={flags}: created={} egl_error=0x{:04x} reported={:?}",
                row.created, row.egl_error, row.reported
            );
            table.push(row);
        }
    }

    let row = |major: i32, minor: i32, flags: bool| -> &Row {
        table
            .iter()
            .find(|r| r.asked == (major, minor) && r.flags == flags)
            .expect("every candidate probed")
    };

    // THE ANSWER GTK DEPENDS ON. An explicit ES 3.0 request must succeed and report ES 3.0; if it does not,
    // GDK falls back to 2.0, GSK rejects 2.0, and GTK4 renders with software Cairo.
    let es30 = row(3, 0, false);
    assert!(
        es30.created,
        "an explicit ES 3.0 context must be creatable — GTK4's GSK renderer requires it: {es30:?}"
    );
    assert_eq!(es30.egl_error, EGL_SUCCESS);
    assert!(
        es30.reported.contains("OpenGL ES 3.0"),
        "ES 3.0 request reports ES 3.0: {:?}",
        es30.reported
    );

    // An explicit request is honoured exactly, so 2.0 stays 2.0 (Chrome/ANGLE depend on this).
    let es20 = row(2, 0, false);
    assert!(es20.created, "an explicit ES 2.0 context is creatable");
    assert!(
        es20.reported.contains("OpenGL ES 2.0"),
        "ES 2.0 request reports ES 2.0: {:?}",
        es20.reported
    );

    // ES 3.1 is served only to a client that asks for it (Dawn does).
    let es31 = row(3, 1, false);
    assert!(es31.created, "an explicit ES 3.1 context is creatable");
    assert!(
        es31.reported.contains("OpenGL ES 3.1"),
        "ES 3.1 request reports ES 3.1: {:?}",
        es31.reported
    );

    // ES 3.2 is not served: this driver has no geometry/tessellation stage. Refusing it is correct, and
    // GDK's descending probe simply moves on to 3.1 — so this must be a clean typed failure, not a crash.
    let es32 = row(3, 2, false);
    assert!(
        !es32.created,
        "ES 3.2 is not served and must be refused, not silently downgraded"
    );
    assert_ne!(
        es32.egl_error, EGL_SUCCESS,
        "a refused context sets an EGL error"
    );

    // `EGL_CONTEXT_FLAGS_KHR` is part of `EGL_KHR_create_context`, which this driver advertises, and its
    // default value is 0. Refusing it fails EVERY version at once, which is what makes a toolkit log
    // "unable to create a GL context" no matter which version it probed.
    for (major, minor) in [(3, 1), (3, 0), (2, 0)] {
        let bare = row(major, minor, false);
        let flagged = row(major, minor, true);
        assert_eq!(
            flagged.created, bare.created,
            "EGL_CONTEXT_FLAGS_KHR = 0 must not change whether ES {major}.{minor} is creatable: \
             bare={bare:?} flagged={flagged:?}"
        );
        assert_eq!(
            flagged.reported, bare.reported,
            "EGL_CONTEXT_FLAGS_KHR = 0 must not change the reported version"
        );
    }
}
