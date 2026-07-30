//! `EGL_EXT_image_dma_buf_import_modifiers` query ABI, driven against the staged `libEGL.so.1` through a
//! real `dlopen` — the sequence Weston's `simple-dmabuf-egl` client (and Mesa-based compositors) use.
//!
//! REGRESSION GUARD (real failure): `weston-simple-dmabuf-egl` printed "Failed to query EGL modifiers for
//! format" and exited with zero frames. Its second `eglQueryDmaBufModifiersEXT` call passes
//! `max_modifiers > 0`, a real `modifiers` array and `external_only == NULL`; the shim rejected the null
//! `external_only` even though the extension spec makes that out-parameter optional
//! ("<external_only> ... may be NULL"). The same clause governs `num_modifiers` reporting only the number of
//! entries actually WRITTEN when the array is non-NULL, and the extension's consistency requirement that
//! every format returned by `eglQueryDmaBufFormatsEXT` be accepted by `eglQueryDmaBufModifiersEXT`.

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;

const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_FALSE: u32 = 0;
/// `DRM_FORMAT_MOD_INVALID` — the sentinel a caller must never receive as a supported modifier.
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;

fn stage_dir() -> PathBuf {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the egl dmabuf modifier test: {other}"),
    };
    PathBuf::from(env!("HL_GL_STAGE_GL")).join(arch)
}

fn open(path: &PathBuf) -> *mut c_void {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
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

fn cstr(p: *const c_char) -> String {
    assert!(!p.is_null(), "EGL string getter returned null");
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

type QueryFormats = extern "C" fn(*mut c_void, i32, *mut i32, *mut i32) -> u32;
type QueryModifiers = extern "C" fn(*mut c_void, i32, i32, *mut u64, *mut u32, *mut i32) -> u32;

/// The staged display plus the two extension entry points, or `None` when the guest shim is not staged.
struct Queries {
    display: *mut c_void,
    formats: QueryFormats,
    modifiers: QueryModifiers,
}

fn queries() -> Option<Queries> {
    let dir = stage_dir();
    let egl_path = dir.join("libEGL.so.1");
    if !egl_path.exists() {
        eprintln!(
            "staged libEGL missing at {} — skipping (guest std not installed)",
            dir.display()
        );
        return None;
    }
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    let egl = open(&egl_path);

    let query_string: extern "C" fn(*mut c_void, i32) -> *const c_char =
        unsafe { std::mem::transmute(sym(egl, "eglQueryString")) };
    let get_proc_address: extern "C" fn(*const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(sym(egl, "eglGetProcAddress")) };
    let initialize: extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglInitialize")) };
    let get_platform_display: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void = unsafe {
        let name = std::ffi::CString::new("eglGetPlatformDisplayEXT").unwrap();
        std::mem::transmute(get_proc_address(name.as_ptr()))
    };
    let display = get_platform_display(
        EGL_PLATFORM_SURFACELESS_MESA,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!display.is_null(), "surfaceless EGLDisplay is non-null");
    assert_eq!(initialize(display, &mut 0, &mut 0), EGL_TRUE);

    let extensions = cstr(query_string(display, EGL_EXTENSIONS));
    assert!(
        extensions.contains("EGL_EXT_image_dma_buf_import_modifiers"),
        "the display advertises the extension under test: {extensions:?}"
    );

    let resolve = |name: &str| {
        let c = std::ffi::CString::new(name).unwrap();
        let p = get_proc_address(c.as_ptr());
        assert!(!p.is_null(), "eglGetProcAddress({name}) must resolve");
        p
    };
    Some(Queries {
        display,
        formats: unsafe { std::mem::transmute(resolve("eglQueryDmaBufFormatsEXT")) },
        modifiers: unsafe { std::mem::transmute(resolve("eglQueryDmaBufModifiersEXT")) },
    })
}

/// Every advertised format, read with the spec's two-call pattern.
fn advertised_formats(q: &Queries) -> Vec<i32> {
    let mut count = -1i32;
    assert_eq!(
        (q.formats)(q.display, 0, core::ptr::null_mut(), &mut count),
        EGL_TRUE,
        "a null formats array reports the count"
    );
    assert!(count > 0, "at least one dma-buf format is advertised");
    let mut formats = vec![0i32; count as usize];
    let mut filled = -1i32;
    assert_eq!(
        (q.formats)(q.display, count, formats.as_mut_ptr(), &mut filled),
        EGL_TRUE
    );
    assert_eq!(
        filled, count,
        "the second call fills exactly what the first call promised"
    );
    formats
}

#[test]
fn dma_buf_format_query_honours_the_two_call_pattern() {
    let Some(q) = queries() else { return };
    let formats = advertised_formats(&q);
    assert!(
        formats.iter().all(|format| *format != 0),
        "every reported format is a real fourcc: {formats:?}"
    );

    // A bounded array writes at most `max_formats` entries and reports only what it wrote (spec: "the
    // number of entries written into <formats>"), so a caller never reads uninitialized memory.
    let mut bounded = vec![-1i32; formats.len() + 1];
    let mut written = -1i32;
    assert_eq!(
        (q.formats)(q.display, 1, bounded.as_mut_ptr(), &mut written),
        EGL_TRUE
    );
    assert_eq!(written, 1, "max_formats=1 writes and reports one entry");
    assert_eq!(bounded[1], -1, "no entry is written past max_formats");

    // Untrusted input: a null count, or a null array with a positive max, fails instead of writing.
    assert_eq!(
        (q.formats)(q.display, 0, core::ptr::null_mut(), core::ptr::null_mut()),
        EGL_FALSE,
        "a null num_formats is rejected"
    );
    let mut ignored = -1i32;
    assert_eq!(
        (q.formats)(q.display, 4, core::ptr::null_mut(), &mut ignored),
        EGL_FALSE,
        "max_formats>0 with a null array is rejected"
    );
    assert_eq!(
        (q.formats)(q.display, -1, bounded.as_mut_ptr(), &mut ignored),
        EGL_FALSE,
        "a negative max_formats is rejected"
    );
}

#[test]
fn dma_buf_modifier_query_answers_every_advertised_format_with_a_null_external_only() {
    let Some(q) = queries() else { return };

    for format in advertised_formats(&q) {
        // Weston's first call: count only.
        let mut count = -1i32;
        assert_eq!(
            (q.modifiers)(
                q.display,
                format,
                0,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                &mut count,
            ),
            EGL_TRUE,
            "format {format:#x} came from eglQueryDmaBufFormatsEXT, so its modifiers are queryable"
        );
        assert!(count > 0, "format {format:#x} has at least one modifier");

        // Weston's second call: a real array, `external_only == NULL` (the spec allows it).
        let mut modifiers = vec![DRM_FORMAT_MOD_INVALID; count as usize];
        let mut filled = -1i32;
        assert_eq!(
            (q.modifiers)(
                q.display,
                format,
                count,
                modifiers.as_mut_ptr(),
                core::ptr::null_mut(),
                &mut filled,
            ),
            EGL_TRUE,
            "a null external_only is legal for format {format:#x}"
        );
        assert_eq!(filled, count, "the fill matches the promised count");
        assert!(
            modifiers.iter().all(|m| *m != DRM_FORMAT_MOD_INVALID),
            "every slot is written with a real modifier: {modifiers:x?}"
        );

        // A bounded array reports only what it wrote, and never stores past `max_modifiers`.
        let mut bounded = vec![DRM_FORMAT_MOD_INVALID; count as usize + 1];
        let mut external = vec![u32::MAX; count as usize + 1];
        let mut written = -1i32;
        assert_eq!(
            (q.modifiers)(
                q.display,
                format,
                1,
                bounded.as_mut_ptr(),
                external.as_mut_ptr(),
                &mut written,
            ),
            EGL_TRUE
        );
        assert_eq!(written, 1, "max_modifiers=1 writes and reports one entry");
        assert_eq!(
            bounded[1], DRM_FORMAT_MOD_INVALID,
            "no modifier is written past max_modifiers"
        );
        assert_eq!(
            external[1],
            u32::MAX,
            "no external_only flag is written past max_modifiers"
        );
        assert!(
            external[0] == EGL_TRUE || external[0] == EGL_FALSE,
            "external_only is a boolean"
        );
    }
}

#[test]
fn dma_buf_modifier_query_rejects_bad_input_without_writing() {
    let Some(q) = queries() else { return };
    let format = advertised_formats(&q)[0];

    assert_eq!(
        (q.modifiers)(
            q.display,
            format,
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        ),
        EGL_FALSE,
        "a null num_modifiers is rejected"
    );
    let mut count = -1i32;
    assert_eq!(
        (q.modifiers)(
            q.display,
            format,
            2,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            &mut count,
        ),
        EGL_FALSE,
        "max_modifiers>0 with a null modifiers array is rejected"
    );
    let mut modifiers = [DRM_FORMAT_MOD_INVALID; 2];
    assert_eq!(
        (q.modifiers)(
            q.display,
            format,
            -1,
            modifiers.as_mut_ptr(),
            core::ptr::null_mut(),
            &mut count,
        ),
        EGL_FALSE,
        "a negative max_modifiers is rejected"
    );
    // An unsupported format is EGL_FALSE per the spec's EGL_BAD_PARAMETER clause.
    assert_eq!(
        (q.modifiers)(
            q.display,
            0x1234_5678,
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            &mut count,
        ),
        EGL_FALSE,
        "a format the driver never advertised is rejected"
    );
}
