//! `libgbm.so.1` FFI battery for the `gbm_surface_*` swap-chain family, driven through a real `dlopen` of
//! the STAGED object — exactly how a guest toolkit reaches it.
//!
//! DEFECT THIS PINS: the shim shipped the device + buffer-object API but none of `gbm_surface_*`, so the
//! object loaded fine and then broke an unrelated client at symbol resolution:
//!   `gtk4-widget-factory: symbol lookup error: libgstgl-1.0.so.0: undefined symbol:
//!    gbm_surface_lock_front_buffer`
//! (exit 127, zero GPU submits). The fix is symbol PRESENCE plus honest failure, not a fake swap chain:
//! this driver has no way to render into a gbm_surface and the virtual render node reports
//! `DRM_CAP_PRIME = 0`, so `gbm_surface_create` must return NULL with `errno` set and no `gbm_surface`
//! object may ever come into existence.
//!
//! LOADING: `libgbm.so.1` carries `DT_NEEDED libEGL.so.1` (it imports `hl_shim_external_buffers_enabled`),
//! so libEGL is `dlopen`ed FIRST with `RTLD_GLOBAL`, as the guest loader does.

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_uint, c_void};
use std::path::{Path, PathBuf};

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn __errno_location() -> *mut c_int;
}
const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

const ENOTSUP: c_int = 95;
const EINVAL: c_int = 22;

const GBM_FORMAT_ARGB8888: u32 = 0x3432_5241;
const GBM_BO_USE_RENDERING: u32 = 1 << 2;
const GBM_BO_USE_LINEAR: u32 = 1 << 4;

/// Every `gbm_surface_*` entry point a complete `libgbm` exports.
const SURFACE_FAMILY: &[&str] = &[
    "gbm_surface_create",
    "gbm_surface_create_with_modifiers",
    "gbm_surface_create_with_modifiers2",
    "gbm_surface_destroy",
    "gbm_surface_has_free_buffers",
    "gbm_surface_lock_front_buffer",
    "gbm_surface_release_buffer",
];

fn stage_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the gbm ffi test: {other}"),
    };
    PathBuf::from(home).join(".hl/gl").join(arch)
}

fn dlopen_global(path: &Path) -> *mut c_void {
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
    let pointer = unsafe { dlsym(handle, c.as_ptr()) };
    assert!(
        !pointer.is_null(),
        "symbol {name} not resolvable in the staged libgbm.so.1 — this is the gtk4/libgstgl failure"
    );
    pointer
}

macro_rules! f {
    ($h:expr, $name:literal, $ty:ty) => {
        unsafe { core::mem::transmute::<*mut c_void, $ty>(sym($h, $name)) }
    };
}

fn errno() -> c_int {
    unsafe { *__errno_location() }
}

fn set_errno(value: c_int) {
    unsafe { *__errno_location() = value };
}

/// The staged `libgbm.so.1`, or `None` when the shim is not staged for this arch (see build.rs) so the
/// test skips rather than failing for an unrelated reason.
fn load() -> Option<*mut c_void> {
    let dir = stage_dir();
    let egl = dir.join("libEGL.so.1");
    let gbm = dir.join("libgbm.so.1");
    if !egl.exists() || !gbm.exists() {
        eprintln!(
            "staged shim missing under {} — skipping (guest std not installed)",
            dir.display()
        );
        return None;
    }
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    dlopen_global(&egl);
    Some(dlopen_global(&gbm))
}

/// A `gbm_device` over a plain fd — the shim only dups it, so any valid fd is a legal device here.
fn device(gbm: *mut c_void) -> *mut c_void {
    let create = f!(
        gbm,
        "gbm_create_device",
        extern "C" fn(c_int) -> *mut c_void
    );
    let fd = std::fs::File::open("/dev/null").expect("open /dev/null for the device fd");
    let device = create(std::os::unix::io::AsRawFd::as_raw_fd(&fd));
    assert!(!device.is_null(), "gbm_create_device rejected a valid fd");
    device
}

#[test]
fn surface_family_symbols_resolve() {
    let Some(gbm) = load() else { return };
    for name in SURFACE_FAMILY {
        let pointer = sym(gbm, name);
        assert!(!pointer.is_null(), "{name} resolved to null");
    }
}

#[test]
fn surface_create_fails_honestly_instead_of_returning_a_broken_surface() {
    let Some(gbm) = load() else { return };
    let device = device(gbm);

    let create = f!(
        gbm,
        "gbm_surface_create",
        extern "C" fn(*mut c_void, u32, u32, u32, u32) -> *mut c_void
    );
    // A supported format + usage: still NULL, because the capability is absent regardless of arguments.
    set_errno(0);
    let surface = create(device, 64, 64, GBM_FORMAT_ARGB8888, GBM_BO_USE_RENDERING);
    assert!(
        surface.is_null(),
        "gbm_surface_create must not hand back a surface this driver cannot render into"
    );
    assert_eq!(
        errno(),
        ENOTSUP,
        "gbm_surface_create must fail with ENOTSUP so the client's fallback path runs"
    );

    // A null device is equally refused, and never dereferenced.
    set_errno(0);
    assert!(create(
        core::ptr::null_mut(),
        64,
        64,
        GBM_FORMAT_ARGB8888,
        GBM_BO_USE_RENDERING
    )
    .is_null());
    assert_eq!(errno(), ENOTSUP);

    // Degenerate geometry / unknown format / unknown flags must not change the verdict or crash.
    for (width, height, format, flags) in [
        (0u32, 0u32, GBM_FORMAT_ARGB8888, GBM_BO_USE_RENDERING),
        (u32::MAX, u32::MAX, GBM_FORMAT_ARGB8888, 0),
        (16, 16, 0, u32::MAX),
    ] {
        set_errno(0);
        assert!(create(device, width, height, format, flags).is_null());
        assert_eq!(errno(), ENOTSUP);
    }
}

#[test]
fn surface_create_with_modifiers_fails_honestly_for_every_modifier_list() {
    let Some(gbm) = load() else { return };
    let device = device(gbm);

    let create1 = f!(
        gbm,
        "gbm_surface_create_with_modifiers",
        extern "C" fn(*mut c_void, u32, u32, u32, *const u64, c_uint) -> *mut c_void
    );
    let create2 = f!(
        gbm,
        "gbm_surface_create_with_modifiers2",
        extern "C" fn(*mut c_void, u32, u32, u32, *const u64, c_uint, u32) -> *mut c_void
    );

    // LINEAR, the Husklet-private modifier's numeric value, and an unknown one: all refused, and the
    // modifier array is never read (a null array with a nonzero count must not fault).
    let modifiers: [u64; 3] = [0, 0x00ff_ffff_ffff_ffff, 0x0100_0000_0000_0002];
    for count in 0..=modifiers.len() {
        set_errno(0);
        assert!(create1(
            device,
            32,
            32,
            GBM_FORMAT_ARGB8888,
            modifiers.as_ptr(),
            count as c_uint
        )
        .is_null());
        assert_eq!(errno(), ENOTSUP);

        set_errno(0);
        assert!(create2(
            device,
            32,
            32,
            GBM_FORMAT_ARGB8888,
            modifiers.as_ptr(),
            count as c_uint,
            GBM_BO_USE_RENDERING
        )
        .is_null());
        assert_eq!(errno(), ENOTSUP);
    }

    set_errno(0);
    assert!(create1(device, 32, 32, GBM_FORMAT_ARGB8888, core::ptr::null(), 4).is_null());
    assert_eq!(errno(), ENOTSUP);
    set_errno(0);
    assert!(create2(device, 32, 32, GBM_FORMAT_ARGB8888, core::ptr::null(), 4, 0).is_null());
    assert_eq!(errno(), ENOTSUP);
}

#[test]
fn locking_a_front_buffer_reports_no_buffer_and_never_dereferences_the_surface() {
    let Some(gbm) = load() else { return };

    let lock = f!(
        gbm,
        "gbm_surface_lock_front_buffer",
        extern "C" fn(*mut c_void) -> *mut c_void
    );
    let release = f!(
        gbm,
        "gbm_surface_release_buffer",
        extern "C" fn(*mut c_void, *mut c_void)
    );
    let has_free = f!(
        gbm,
        "gbm_surface_has_free_buffers",
        extern "C" fn(*mut c_void) -> c_int
    );
    let destroy = f!(gbm, "gbm_surface_destroy", extern "C" fn(*mut c_void));

    // No surface can exist, so the only pointers these can receive are foreign. Null AND a deliberately
    // invalid non-null pointer must both be handled without a dereference.
    let bogus = 0xdead_0000_usize as *mut c_void;
    for surface in [core::ptr::null_mut(), bogus] {
        set_errno(0);
        assert!(
            lock(surface).is_null(),
            "lock_front_buffer must report no front buffer, never a plausible-looking bo"
        );
        assert_eq!(errno(), EINVAL);
        assert_eq!(
            has_free(surface),
            0,
            "has_free_buffers must report that no further render can start"
        );
        release(surface, bogus);
        destroy(surface);
    }
}

/// The buffer-object path Chrome depends on must be untouched by the surface family.
#[test]
fn buffer_object_path_still_allocates_and_maps() {
    let Some(gbm) = load() else { return };
    let device = device(gbm);

    let bo_create = f!(
        gbm,
        "gbm_bo_create",
        extern "C" fn(*mut c_void, u32, u32, u32, u32) -> *mut c_void
    );
    let bo_stride = f!(gbm, "gbm_bo_get_stride", extern "C" fn(*mut c_void) -> u32);
    let bo_width = f!(gbm, "gbm_bo_get_width", extern "C" fn(*mut c_void) -> u32);
    let bo_modifier = f!(
        gbm,
        "gbm_bo_get_modifier",
        extern "C" fn(*mut c_void) -> u64
    );
    let bo_destroy = f!(gbm, "gbm_bo_destroy", extern "C" fn(*mut c_void));

    let bo = bo_create(
        device,
        64,
        16,
        GBM_FORMAT_ARGB8888,
        GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR,
    );
    assert!(!bo.is_null(), "gbm_bo_create regressed");
    assert_eq!(bo_width(bo), 64);
    assert_eq!(bo_stride(bo), 256);
    assert_eq!(
        bo_modifier(bo),
        0,
        "linear bo must keep the LINEAR modifier"
    );
    bo_destroy(bo);
}
