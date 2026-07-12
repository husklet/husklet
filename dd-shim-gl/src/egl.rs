//! Hand-written EGL bring-up entry points (display/config/query). Values mirror the retiring C shim
//! (`gl_shim.c`) so behavior is identical for the apps that depend on them (glmark2, ANGLE-gl-egl).
//!
//! These are exported directly (`#[no_mangle] extern "C"`); the generator in `build.rs` skips their
//! names (see `IMPLEMENTED`) so there is no duplicate symbol. Rendering entry points (surface
//! bring-up, `eglSwapBuffers`) are ported next; the transport they will drive already lives in
//! `dd_shim_common::transport`.

use core::ffi::{c_char, c_void};

// EGL enums (subset queried by the apps we serve).
const EGL_VENDOR: i32 = 0x3053;
const EGL_VERSION: i32 = 0x3054;
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_CLIENT_APIS: i32 = 0x308D;
const EGL_SUCCESS: u32 = 0x3000;
const EGL_TRUE: u32 = 1;

/// Static, nul-terminated string → `*const c_char` (stable for the process lifetime).
macro_rules! cstr {
    ($s:literal) => {
        concat!($s, "\0").as_ptr() as *const c_char
    };
}

/// `eglGetError` — last EGL error for the calling thread. We never fail the covered paths, so this is
/// `EGL_SUCCESS` until a real error path sets it.
#[no_mangle]
pub extern "C" fn eglGetError() -> i32 {
    EGL_SUCCESS as i32
}

/// `eglQueryString` — vendor/version/APIs/extensions. Mirrors gl_shim.c, including the split client- vs
/// display-extension set keyed on `EGL_NO_DISPLAY` (ANGLE's client-extension probe).
#[no_mangle]
pub extern "C" fn eglQueryString(dpy: *mut c_void, name: i32) -> *const c_char {
    match name {
        EGL_VENDOR => cstr!("dd"),
        EGL_VERSION => cstr!("1.4 dd-shim"),
        EGL_CLIENT_APIS => cstr!("OpenGL_ES"),
        EGL_EXTENSIONS => {
            if dpy.is_null() {
                cstr!("EGL_EXT_client_extensions EGL_KHR_platform_gbm EGL_KHR_platform_wayland EGL_EXT_platform_base")
            } else {
                cstr!("EGL_KHR_create_context EGL_KHR_surfaceless_context EGL_KHR_no_config_context")
            }
        }
        _ => cstr!(""),
    }
}

/// `eglGetDisplay` — one implicit display. A non-null opaque sentinel (matches the C shim's single
/// display handle); the argument is ignored.
#[no_mangle]
pub extern "C" fn eglGetDisplay(_native_display: *mut c_void) -> *mut c_void {
    1 as *mut c_void
}

/// `eglInitialize` — succeeds, reporting EGL 1.4.
#[no_mangle]
pub extern "C" fn eglInitialize(_dpy: *mut c_void, major: *mut i32, minor: *mut i32) -> u32 {
    unsafe {
        if !major.is_null() {
            *major = 1;
        }
        if !minor.is_null() {
            *minor = 4;
        }
    }
    EGL_TRUE
}

extern "C" {
    // RTLD_DEFAULT lookup resolves against this .so's own exported symbols (and the process), so a
    // proc-address query for any GLES2/EGL entry point returns its real address — generated stubs
    // included. On glibc >= 2.34 `dlsym` lives in libc; the guest rootfs provides it.
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `eglGetProcAddress` — resolve a GL/EGL entry point by name to its exported address. ANGLE and
/// glmark2 lean on this heavily; backing it with `dlsym(RTLD_DEFAULT, name)` makes the whole
/// registry-generated surface reachable without a hand-maintained dispatch table.
#[no_mangle]
pub extern "C" fn eglGetProcAddress(procname: *const c_char) -> *mut c_void {
    if procname.is_null() {
        return core::ptr::null_mut();
    }
    // RTLD_DEFAULT == null handle on glibc.
    unsafe { dlsym(core::ptr::null_mut(), procname) }
}
