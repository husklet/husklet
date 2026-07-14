//! Hand-written EGL entry points — display/config/context/surface *query + lifecycle*, ported
//! faithfully from `gl_shim.c`. Values mirror the C shim so behavior is identical for the apps that
//! depend on them (glmark2, ANGLE-gl-egl).
//!
//! These are exported directly (`#[no_mangle] extern "C"`); `build.rs` skips their names (see
//! `IMPLEMENTED`) so there is no duplicate symbol. The PRESENT path is intentionally NOT here this
//! increment: `eglCreateWindowSurface` (surface bring-up: renderD128 alloc + wayland) and
//! `eglSwapBuffers` (IR emit + commit) remain generated stubs, owned by the present/draw work.

use core::ffi::{c_char, c_void};
use std::sync::{Mutex, OnceLock};

use crate::glconst::*;
use crate::state::{egl, gl, shim_es3, Surface};

/// Static, nul-terminated string → `*const c_char` (stable for the process lifetime).
macro_rules! cstr {
    ($s:literal) => {
        concat!($s, "\0").as_ptr() as *const c_char
    };
}

/// `eglGetError` — the calling thread's EGL error with first-error retention, cleared on read. The
/// first error since the last query is kept (a later error does not overwrite it); reading resets it to
/// EGL_SUCCESS. Per-thread, so one thread's error never leaks into another's.
#[no_mangle]
pub extern "C" fn eglGetError() -> i32 {
    crate::state::egl_take_error()
}

/// `eglQueryString` — vendor/version/APIs/extensions. Mirrors gl_shim.c, including the split client- vs
/// display-extension set keyed on `EGL_NO_DISPLAY` (ANGLE's client-extension probe).
#[no_mangle]
pub extern "C" fn eglQueryString(dpy: *mut c_void, name: i32) -> *const c_char {
    match name {
        0x3053 => cstr!("dd"),          // EGL_VENDOR
        0x3054 => cstr!("1.4 hl-shim"), // EGL_VERSION
        0x308D => cstr!("OpenGL_ES"),   // EGL_CLIENT_APIS
        0x3055 => {
            // EGL_EXTENSIONS
            if dpy.is_null() {
                cstr!("EGL_EXT_client_extensions EGL_KHR_platform_gbm EGL_KHR_platform_wayland EGL_EXT_platform_base")
            } else {
                cstr!("EGL_KHR_create_context EGL_KHR_surfaceless_context EGL_KHR_no_config_context")
            }
        }
        _ => cstr!(""),
    }
}

/// `eglGetDisplay` — one implicit display (non-null opaque sentinel, matches the C shim).
#[no_mangle]
pub extern "C" fn eglGetDisplay(_native_display: *mut c_void) -> *mut c_void {
    1 as *mut c_void
}

/// `eglGetPlatformDisplay` (EGL 1.5 / EGL_EXT_platform_base) — the modern display-open entry point most
/// toolkits (GTK4, Qt, SDL) use instead of `eglGetDisplay`. Resolves to the same single display handle
/// (gl_shim.c parity), so the rest of the display/config/context lifecycle proceeds unchanged.
#[no_mangle]
pub extern "C" fn eglGetPlatformDisplay(_platform: u32, _native: *mut c_void, _attrib_list: *const isize) -> *mut c_void {
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

/// `eglTerminate` — nothing to tear down in our model.
#[no_mangle]
pub extern "C" fn eglTerminate(_dpy: *mut c_void) -> u32 {
    EGL_TRUE
}

/// `eglReleaseThread` — release the calling thread's EGL state: unbind its current context and draw/read
/// surfaces (so `eglGetCurrentContext`/`eglGetCurrentDisplay` report EGL_NO_CONTEXT/EGL_NO_DISPLAY).
#[no_mangle]
pub extern "C" fn eglReleaseThread() -> u32 {
    crate::state::egl_make_current(core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut());
    EGL_TRUE
}

extern "C" {
    // RTLD_DEFAULT lookup resolves against this .so's own exported symbols (and the process), so a
    // proc-address query for any GLES2/EGL entry point returns its real address — generated stubs
    // included. On glibc >= 2.34 `dlsym` lives in libc; the guest rootfs provides it.
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `eglGetProcAddress` — resolve a GL/EGL entry point by name to its exported address via
/// `dlsym(RTLD_DEFAULT, name)`, making the whole registry-generated surface reachable without a
/// hand-maintained dispatch table.
#[no_mangle]
pub extern "C" fn eglGetProcAddress(procname: *const c_char) -> *mut c_void {
    if procname.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { dlsym(core::ptr::null_mut(), procname) } // RTLD_DEFAULT == null handle on glibc
}

// ===================================================================================================
// config selection — one RGBA8 + depth24 window config (id 1), as gl_shim.c
// ===================================================================================================

/// The one advertised config's id.
const CONFIG_ID: i32 = 1;

/// How an attribute participates in `eglChooseConfig` matching (EGL 1.4 §3.4.1).
enum MatchRule {
    /// Config value must be >= the requested value (size / sample minimums).
    AtLeast,
    /// Requested bits must ALL be present in the config value (bitmask attributes).
    Mask,
    /// Config value must equal the requested value.
    Exact,
    /// Selection-only or informational — not a filter constraint.
    Ignore,
}

/// The real value config 1 reports for `attribute`, or `None` if the attribute is unknown. Single
/// source of truth for both `eglGetConfigAttrib` and `eglChooseConfig`.
fn config_attrib_value(attribute: i32) -> Option<i32> {
    let es3_bit = if shim_es3() { EGL_OPENGL_ES3_BIT_KHR } else { 0 };
    let v = match attribute {
        EGL_CONFIG_ID => CONFIG_ID,
        EGL_RED_SIZE | EGL_GREEN_SIZE | EGL_BLUE_SIZE | EGL_ALPHA_SIZE => 8,
        EGL_BUFFER_SIZE => 32,
        EGL_DEPTH_SIZE => 24,
        EGL_STENCIL_SIZE => 0,
        EGL_LUMINANCE_SIZE | EGL_ALPHA_MASK_SIZE => 0,
        EGL_SURFACE_TYPE => EGL_WINDOW_BIT | EGL_PBUFFER_BIT,
        EGL_RENDERABLE_TYPE | EGL_CONFORMANT => EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES_BIT | es3_bit,
        EGL_COLOR_BUFFER_TYPE => EGL_RGB_BUFFER,
        EGL_CONFIG_CAVEAT => EGL_NONE,
        EGL_NATIVE_RENDERABLE => EGL_TRUE as i32,
        EGL_NATIVE_VISUAL_ID => DRM_FMT_XRGB8888,
        EGL_NATIVE_VISUAL_TYPE => 0,
        EGL_MAX_PBUFFER_WIDTH | EGL_MAX_PBUFFER_HEIGHT => 4096,
        EGL_MAX_PBUFFER_PIXELS => 4096 * 4096,
        EGL_MIN_SWAP_INTERVAL => 0,
        EGL_MAX_SWAP_INTERVAL => 1,
        EGL_SAMPLES | EGL_SAMPLE_BUFFERS | EGL_LEVEL => 0,
        EGL_TRANSPARENT_TYPE => EGL_NONE,
        EGL_BIND_TO_TEXTURE_RGB | EGL_BIND_TO_TEXTURE_RGBA => EGL_FALSE as i32,
        _ => return None,
    };
    Some(v)
}

fn match_rule(attribute: i32) -> MatchRule {
    match attribute {
        EGL_BUFFER_SIZE | EGL_RED_SIZE | EGL_GREEN_SIZE | EGL_BLUE_SIZE | EGL_ALPHA_SIZE | EGL_DEPTH_SIZE
        | EGL_STENCIL_SIZE | EGL_LUMINANCE_SIZE | EGL_ALPHA_MASK_SIZE | EGL_SAMPLES | EGL_SAMPLE_BUFFERS => {
            MatchRule::AtLeast
        }
        EGL_SURFACE_TYPE | EGL_RENDERABLE_TYPE | EGL_CONFORMANT => MatchRule::Mask,
        EGL_CONFIG_ID | EGL_COLOR_BUFFER_TYPE | EGL_CONFIG_CAVEAT | EGL_LEVEL | EGL_TRANSPARENT_TYPE
        | EGL_MIN_SWAP_INTERVAL | EGL_MAX_SWAP_INTERVAL => MatchRule::Exact,
        _ => MatchRule::Ignore,
    }
}

/// Whether config 1 satisfies every constraint in a (key, value, … , EGL_NONE) attribute list.
unsafe fn config1_matches(attrib_list: *const i32) -> bool {
    if attrib_list.is_null() {
        return true;
    }
    let mut p = attrib_list;
    while *p != EGL_NONE {
        let key = *p;
        let want = *p.add(1);
        p = p.add(2);
        if want == EGL_DONT_CARE {
            continue;
        }
        let Some(have) = config_attrib_value(key) else { continue };
        let ok = match match_rule(key) {
            MatchRule::AtLeast => have >= want,
            MatchRule::Mask => (have & want) == want,
            MatchRule::Exact => have == want,
            MatchRule::Ignore => true,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// `eglChooseConfig` — a truthful matcher: it returns the single config only when it satisfies the
/// requested attributes; an impossible/over-constrained request is a valid query with ZERO matches
/// (the caller's config slot is left untouched), not a silent selection of config 1.
#[no_mangle]
pub extern "C" fn eglChooseConfig(
    _dpy: *mut c_void,
    attrib_list: *const i32,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    let matched = unsafe { config1_matches(attrib_list) };
    unsafe {
        if matched {
            if !configs.is_null() && config_size >= 1 {
                *configs = CONFIG_ID as usize as *mut c_void;
            }
            if !num_config.is_null() {
                *num_config = 1;
            }
        } else if !num_config.is_null() {
            *num_config = 0; // leave the caller's `configs` slot untouched on a zero-match query
        }
    }
    EGL_TRUE
}

/// `eglGetConfigs` — enumerate the single config (id 1).
#[no_mangle]
pub extern "C" fn eglGetConfigs(_dpy: *mut c_void, configs: *mut *mut c_void, config_size: i32, num_config: *mut i32) -> u32 {
    unsafe {
        if !configs.is_null() && config_size >= 1 {
            *configs = 1 as *mut c_void;
        }
        if !num_config.is_null() {
            *num_config = 1;
        }
    }
    EGL_TRUE
}

/// `eglGetConfigAttrib` — real, self-consistent attributes for config 1 (RGBA8, depth24, stencil0). A
/// forged config handle is EGL_BAD_CONFIG and an unknown attribute is EGL_BAD_ATTRIBUTE; both fail
/// WITHOUT writing the caller's output.
#[no_mangle]
pub extern "C" fn eglGetConfigAttrib(_dpy: *mut c_void, config: *mut c_void, attribute: i32, value: *mut i32) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    if config as usize != CONFIG_ID as usize {
        crate::state::egl_set_error(EGL_BAD_CONFIG);
        return EGL_FALSE;
    }
    match config_attrib_value(attribute) {
        Some(r) => {
            unsafe { *value = r };
            EGL_TRUE
        }
        None => {
            crate::state::egl_set_error(EGL_BAD_ATTRIBUTE);
            EGL_FALSE
        }
    }
}

// ===================================================================================================
// context lifecycle
// ===================================================================================================

/// `eglCreateContext` — validates the requested client version against the shim's max (ES3 iff
/// `HL_SHIM_ES3`) and returns a UNIQUE context handle. If `share` is a live context, the new context
/// JOINS that context's share group (shared GL object namespace); otherwise it gets an independent
/// group. On too-high a version request it sets EGL_BAD_MATCH and returns EGL_NO_CONTEXT (null).
#[no_mangle]
pub extern "C" fn eglCreateContext(
    _dpy: *mut c_void,
    _config: *mut c_void,
    share: *mut c_void,
    attrib_list: *const i32,
) -> *mut c_void {
    let (mut req_major, mut req_minor) = (1i32, 0i32);
    if !attrib_list.is_null() {
        // attribs are [key, value, ... , EGL_NONE]
        let mut p = attrib_list;
        unsafe {
            while *p != EGL_NONE {
                let k = *p;
                let v = *p.add(1);
                if k == EGL_CONTEXT_CLIENT_VERSION {
                    req_major = v;
                } else if k == EGL_CONTEXT_MINOR_VERSION_KHR {
                    req_minor = v;
                }
                p = p.add(2);
            }
        }
    }
    let max_major = if shim_es3() { 3 } else { 2 };
    let max_minor = 0;
    if req_major > max_major || (req_major == max_major && req_minor > max_minor) {
        crate::state::egl_set_error(EGL_BAD_MATCH);
        return core::ptr::null_mut(); // EGL_NO_CONTEXT
    }
    crate::state::egl_create_context(share, req_major, req_minor)
}

/// `eglDestroyContext` — free a live context handle (its share group is retained for sibling contexts).
/// An unknown handle sets EGL_BAD_CONTEXT.
#[no_mangle]
pub extern "C" fn eglDestroyContext(_dpy: *mut c_void, ctx: *mut c_void) -> u32 {
    if crate::state::egl_destroy_context(ctx) {
        EGL_TRUE
    } else {
        crate::state::egl_set_error(EGL_BAD_CONTEXT);
        EGL_FALSE
    }
}

/// `eglMakeCurrent` — bind `ctx` as the CALLING THREAD's current context (per-thread current, so
/// distinct threads may be current on distinct contexts concurrently). A null context unbinds. An
/// unknown context sets EGL_BAD_CONTEXT and fails.
#[no_mangle]
pub extern "C" fn eglMakeCurrent(_dpy: *mut c_void, draw: *mut c_void, read: *mut c_void, ctx: *mut c_void) -> u32 {
    if crate::state::egl_make_current(ctx, draw, read) {
        EGL_TRUE
    } else {
        crate::state::egl_set_error(EGL_BAD_CONTEXT);
        EGL_FALSE
    }
}

/// `eglQueryContext` — client type/version of the given context handle.
#[no_mangle]
pub extern "C" fn eglQueryContext(dpy: *mut c_void, ctx: *mut c_void, attribute: i32, value: *mut i32) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    if dpy as usize != 1 {
        crate::state::egl_set_error(EGL_BAD_DISPLAY);
        return EGL_FALSE;
    }
    if !crate::state::egl_ctx_is_live(ctx) {
        crate::state::egl_set_error(EGL_BAD_CONTEXT);
        return EGL_FALSE;
    }
    let v = match attribute {
        EGL_CONTEXT_CLIENT_TYPE => EGL_OPENGL_ES_API as i32,
        EGL_CONTEXT_CLIENT_VERSION => crate::state::egl_ctx_major(ctx),
        EGL_CONTEXT_MINOR_VERSION_KHR => crate::state::egl_ctx_minor(ctx),
        EGL_CONFIG_ID => CONFIG_ID,
        EGL_RENDER_BUFFER => EGL_BACK_BUFFER,
        _ => {
            crate::state::egl_set_error(EGL_BAD_ATTRIBUTE);
            return EGL_FALSE;
        }
    };
    unsafe { *value = v };
    EGL_TRUE
}

// ===================================================================================================
// surface query / destroy (bring-up stays a stub — see module docs)
// ===================================================================================================

/// `eglQuerySurface` — per-surface width/height from the typed arena. A stale/forged handle is
/// EGL_BAD_SURFACE and fails WITHOUT writing the caller's output.
#[no_mangle]
pub extern "C" fn eglQuerySurface(_dpy: *mut c_void, surface: *mut c_void, attribute: i32, value: *mut i32) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    let Some((_kind, w, h)) = crate::state::egl_surface_lookup(surface) else {
        crate::state::egl_set_error(EGL_BAD_SURFACE);
        return EGL_FALSE;
    };
    let v = match attribute {
        EGL_WIDTH => w,
        EGL_HEIGHT => h,
        _ => 0,
    };
    unsafe { *value = v };
    EGL_TRUE
}

/// `eglDestroySurface` — retire a live surface handle (its arena slot's generation is bumped so the
/// handle can never validate again). A stale/forged handle is EGL_BAD_SURFACE.
#[no_mangle]
pub extern "C" fn eglDestroySurface(_dpy: *mut c_void, surface: *mut c_void) -> u32 {
    if crate::state::egl_destroy_surface(surface) {
        EGL_TRUE
    } else {
        crate::state::egl_set_error(EGL_BAD_SURFACE);
        EGL_FALSE
    }
}

// ===================================================================================================
// EGL 1.4 mandatory tail — real bodies (truthful for the genuinely-unsupported native-pixmap /
// client-buffer / texture-binding paths; a benign, validated attribute set for eglSurfaceAttrib).
// ===================================================================================================

/// `eglSurfaceAttrib` — set a surface attribute (swap behavior / mipmap level). The shim tracks no
/// mutable per-surface attribute, so a known attribute on a live surface is a benign accepted no-op; a
/// stale/forged surface is EGL_BAD_SURFACE.
#[no_mangle]
pub extern "C" fn eglSurfaceAttrib(_dpy: *mut c_void, surface: *mut c_void, _attribute: i32, _value: i32) -> u32 {
    if crate::state::egl_surface_lookup(surface).is_none() {
        crate::state::egl_set_error(EGL_BAD_SURFACE);
        return EGL_FALSE;
    }
    EGL_TRUE
}

/// `eglBindTexImage` / `eglReleaseTexImage` — render-a-pbuffer-to-a-texture. The advertised config is
/// NOT bind-to-texture capable (`EGL_BIND_TO_TEXTURE_RGB/RGBA` == FALSE), so this is truthfully
/// unsupported: a bad surface is EGL_BAD_SURFACE, an otherwise-valid surface is EGL_BAD_MATCH.
#[no_mangle]
pub extern "C" fn eglBindTexImage(_dpy: *mut c_void, surface: *mut c_void, _buffer: i32) -> u32 {
    let err = if crate::state::egl_surface_lookup(surface).is_none() { EGL_BAD_SURFACE } else { EGL_BAD_MATCH };
    crate::state::egl_set_error(err);
    EGL_FALSE
}
#[no_mangle]
pub extern "C" fn eglReleaseTexImage(_dpy: *mut c_void, surface: *mut c_void, _buffer: i32) -> u32 {
    let err = if crate::state::egl_surface_lookup(surface).is_none() { EGL_BAD_SURFACE } else { EGL_BAD_MATCH };
    crate::state::egl_set_error(err);
    EGL_FALSE
}

/// `eglCopyBuffers` — copy the surface's color buffer to a native pixmap. No native-pixmap target
/// exists in this (Wayland-only) surface: EGL_BAD_SURFACE for a bad handle, else EGL_BAD_NATIVE_PIXMAP.
#[no_mangle]
pub extern "C" fn eglCopyBuffers(_dpy: *mut c_void, surface: *mut c_void, _target: *mut c_void) -> u32 {
    let err = if crate::state::egl_surface_lookup(surface).is_none() { EGL_BAD_SURFACE } else { EGL_BAD_NATIVE_PIXMAP };
    crate::state::egl_set_error(err);
    EGL_FALSE
}

/// `eglCreatePixmapSurface` — native pixmaps are not a surface type this shim backs (no X11 / GBM
/// pixmap). Truthfully unsupported: EGL_NO_SURFACE + EGL_BAD_NATIVE_PIXMAP.
#[no_mangle]
pub extern "C" fn eglCreatePixmapSurface(_dpy: *mut c_void, _config: *mut c_void, _pixmap: *mut c_void, _attrib_list: *const i32) -> *mut c_void {
    crate::state::egl_set_error(EGL_BAD_NATIVE_PIXMAP);
    core::ptr::null_mut()
}

/// `eglCreatePbufferFromClientBuffer` — client buffers (OpenVG images) are not a supported source.
/// Truthfully unsupported: EGL_NO_SURFACE + EGL_BAD_PARAMETER.
#[no_mangle]
pub extern "C" fn eglCreatePbufferFromClientBuffer(_dpy: *mut c_void, _buftype: u32, _buffer: *mut c_void, _config: *mut c_void, _attrib_list: *const i32) -> *mut c_void {
    crate::state::egl_set_error(EGL_BAD_PARAMETER);
    core::ptr::null_mut()
}

// ===================================================================================================
// API selection + misc no-op lifecycle
// ===================================================================================================

/// `eglBindAPI` — this shim exposes OpenGL ES only. Selecting any other API (desktop OpenGL / OpenVG)
/// is EGL_BAD_PARAMETER, so a later `eglCreateContext` cannot be steered onto an unsupported client API.
#[no_mangle]
pub extern "C" fn eglBindAPI(api: u32) -> u32 {
    if api == EGL_OPENGL_ES_API {
        EGL_TRUE
    } else {
        crate::state::egl_set_error(EGL_BAD_PARAMETER);
        EGL_FALSE
    }
}

#[no_mangle]
pub extern "C" fn eglQueryAPI() -> u32 {
    EGL_OPENGL_ES_API
}

#[no_mangle]
pub extern "C" fn eglSwapInterval(_dpy: *mut c_void, _interval: i32) -> u32 {
    EGL_TRUE
}

/// `eglGetCurrentSurface` — THIS thread's bound draw (EGL_DRAW=0x3059) or read (EGL_READ=0x305A)
/// surface, as set by the last `eglMakeCurrent` (EGL_NO_SURFACE when no context is current).
#[no_mangle]
pub extern "C" fn eglGetCurrentSurface(readdraw: i32) -> *mut c_void {
    match readdraw {
        0x3059 => crate::state::egl_current_draw_surface(),
        0x305A => crate::state::egl_current_read_surface(),
        _ => core::ptr::null_mut(),
    }
}

/// `eglGetCurrentDisplay` — the display bound to THIS thread's current context (EGL_NO_DISPLAY if none).
#[no_mangle]
pub extern "C" fn eglGetCurrentDisplay() -> *mut c_void {
    if crate::state::egl_current_context().is_null() {
        core::ptr::null_mut() // EGL_NO_DISPLAY
    } else {
        1 as *mut c_void
    }
}

/// `eglGetCurrentContext` — THIS thread's current context handle (EGL_NO_CONTEXT if none).
#[no_mangle]
pub extern "C" fn eglGetCurrentContext() -> *mut c_void {
    crate::state::egl_current_context()
}

#[no_mangle]
pub extern "C" fn eglWaitClient() -> u32 {
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglWaitGL() -> u32 {
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglWaitNative(_engine: i32) -> u32 {
    EGL_TRUE
}

// ===================================================================================================
// window surface bring-up + present (the frame boundary)
// ===================================================================================================

/// Our own `libwayland-egl` window handle (mirrors gl_shim.c `struct hl_wl_egl_window`). The first
/// field is Mesa-ABI-compatible (`intptr_t version`) so a stray Mesa struct is still parseable.
const HL_WL_EGL_MAGIC: isize = 0x0064_6477_6c65_676c; // "ddwlegl"
#[repr(C)]
pub struct HlWlEglWindow {
    version: isize,
    width: i32,
    height: i32,
    dx: i32,
    dy: i32,
    attached_width: i32,
    attached_height: i32,
    driver_private: *mut c_void,
    resize_cb: *mut c_void,
    destroy_cb: *mut c_void,
    surface: *mut c_void,
}

/// Parse the app's native window handle to a backing `(width, height)`. Handles our
/// `hl_wl_egl_window` magic struct (glmark2/Chrome via our libwayland-egl) plus gl_shim.c's stock-app
/// heuristics (es2*/ANGLE two-int + Mesa `wl_egl_window` word shapes).
unsafe fn parse_native_window(w: *const c_void) -> (u32, u32) {
    let (mut ww, mut hh) = (256i32, 256i32);
    if !w.is_null() {
        let version = *(w as *const isize);
        if version == HL_WL_EGL_MAGIC {
            let win = &*(w as *const HlWlEglWindow);
            ww = win.width;
            hh = win.height;
            if win.attached_width > 0 && win.attached_height > 0 && win.attached_width <= 8192 && win.attached_height <= 8192 {
                ww = ww.max(win.attached_width);
                hh = hh.max(win.attached_height);
            }
        } else {
            let p = w as *const i32;
            let g = |i: isize| *p.offset(i);
            if g(0) > 0 && g(0) <= 16 && g(2) > 16 && g(2) <= 8192 && g(3) > 16 && g(3) <= 8192 {
                ww = g(2);
                hh = g(3);
            } else if g(0) > 0 && g(0) <= 16 && g(1) > 16 && g(1) <= 8192 {
                ww = g(1);
                hh = g(2);
            } else {
                ww = g(0);
                hh = g(1);
            }
        }
    }
    let big = |v: i32| v > 0 && v <= 8192;
    (if big(ww) { ww as u32 } else { 256 }, if big(hh) { hh as u32 } else { 256 })
}

// ---- libwayland-egl surface (our own; glmark2/Chrome call these) ----
#[no_mangle]
pub extern "C" fn wl_egl_window_create(surface: *mut c_void, width: i32, height: i32) -> *mut HlWlEglWindow {
    if width <= 0 || height <= 0 {
        return core::ptr::null_mut();
    }
    let w = Box::into_raw(Box::new(HlWlEglWindow {
        version: HL_WL_EGL_MAGIC,
        width,
        height,
        dx: 0,
        dy: 0,
        attached_width: 0,
        attached_height: 0,
        driver_private: core::ptr::null_mut(),
        resize_cb: core::ptr::null_mut(),
        destroy_cb: core::ptr::null_mut(),
        surface,
    }));
    let mut s = gl();
    s.pending_logical_w = width;
    s.pending_logical_h = height;
    s.pending_attach_x = 0;
    s.pending_attach_y = 0;
    w
}

#[no_mangle]
pub extern "C" fn wl_egl_window_resize(w: *mut HlWlEglWindow, width: i32, height: i32, dx: i32, dy: i32) {
    if w.is_null() {
        return;
    }
    let win = unsafe { &mut *w };
    win.width = width;
    win.height = height;
    win.dx = dx;
    win.dy = dy;
    let mut s = gl();
    s.pending_logical_w = width;
    s.pending_logical_h = height;
    s.pending_attach_x = dx;
    s.pending_attach_y = dy;
}

#[no_mangle]
pub extern "C" fn wl_egl_window_get_attached_size(w: *mut HlWlEglWindow, width: *mut i32, height: *mut i32) {
    if w.is_null() {
        return;
    }
    let win = unsafe { &*w };
    unsafe {
        if !width.is_null() {
            *width = if win.attached_width != 0 { win.attached_width } else { win.width };
        }
        if !height.is_null() {
            *height = if win.attached_height != 0 { win.attached_height } else { win.height };
        }
    }
}

#[no_mangle]
pub extern "C" fn wl_egl_window_destroy(w: *mut HlWlEglWindow) {
    if !w.is_null() {
        unsafe { drop(Box::from_raw(w)) };
    }
}

/// The process-global wayland session (deployed present path). None in HL_IR_DUMP/host-tool mode.
fn wayland_session() -> &'static Mutex<Option<crate::wayland::Wayland>> {
    static W: OnceLock<Mutex<Option<crate::wayland::Wayland>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(None))
}

fn surface_geometry(s: &Surface) -> crate::wayland::Geometry {
    crate::wayland::Geometry {
        backing_w: s.width,
        backing_h: s.height,
        logical_w: s.logical_w,
        logical_h: s.logical_h,
        geom_x: s.geom_x,
        geom_y: s.geom_y,
        attach_x: s.attach_x,
        attach_y: s.attach_y,
    }
}

/// `eglCreateWindowSurface` — bring up the presented default framebuffer from the native window size.
/// In HL_IR_DUMP/host-tool mode this only records the surface (id 1); the renderD128 IOSurface
/// registration + wayland handshake are the deployed-path plumbing (see `present_frame`).
#[no_mangle]
pub extern "C" fn eglCreateWindowSurface(_dpy: *mut c_void, _config: *mut c_void, win: *mut c_void, _attribs: *const i32) -> *mut c_void {
    let (w, h) = unsafe { parse_native_window(win) };
    let geom = {
        let mut s = gl();
        // stock two-int windows set no pending logical size — default it to the backing size.
        if s.pending_logical_w <= 0 {
            s.pending_logical_w = w as i32;
            s.pending_logical_h = h as i32;
        }
        s.surface_up(w, h);
        egl().surface_logical_w = s.surf.logical_w;
        egl().surface_logical_h = s.surf.logical_h;
        surface_geometry(&s.surf)
    };
    // Deployed path (not host-tool HL_IR_DUMP): register the GPU buffer + bring up the wayland surface.
    if std::env::var_os("HL_IR_DUMP").is_none() {
        if let Ok(a) = hl_shim::transport::renderd::alloc(w, h, 0) {
            let mut s = gl();
            s.surf.id = a.id;
            s.surf.stride = a.stride;
            s.surf.fd = a.fd;
            s.surf.width = a.width;
            s.surf.height = a.height;
            // The engine returns the allocation generation in the alloc reply's `format` field (output).
            s.surf.generation = a.format & 0x7fff;
        }
        if let Some(wl) = crate::wayland::Wayland::connect_and_handshake(&geom) {
            *wayland_session().lock().unwrap_or_else(|e| e.into_inner()) = Some(wl);
        }
    }
    // A distinct, generation-checked window-surface handle (no longer the immortal singleton `1`).
    crate::state::egl_create_surface(crate::state::SurfaceKind::Window, w as i32, h as i32)
}

/// `eglCreatePbufferSurface` — an offscreen surface whose dimensions come from `EGL_WIDTH`/`EGL_HEIGHT`
/// in the attribute list (defaulting to 1x1). Returns a distinct typed handle; swap is a no-op on it
/// because `eglSwapBuffers` only presents a window surface.
#[no_mangle]
pub extern "C" fn eglCreatePbufferSurface(_dpy: *mut c_void, _config: *mut c_void, attribs: *const i32) -> *mut c_void {
    let (mut w, mut h) = (1i32, 1i32);
    if !attribs.is_null() {
        let mut p = attribs;
        unsafe {
            while *p != EGL_NONE {
                let (k, v) = (*p, *p.add(1));
                if k == EGL_WIDTH {
                    w = v;
                } else if k == EGL_HEIGHT {
                    h = v;
                }
                p = p.add(2);
            }
        }
    }
    crate::state::egl_create_surface(crate::state::SurfaceKind::Pbuffer, w, h)
}

fn exec_conn() -> &'static Mutex<hl_shim::transport::ExecConn> {
    static C: OnceLock<Mutex<hl_shim::transport::ExecConn>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(hl_shim::transport::ExecConn::from_env()))
}

/// Present a frame's IR. `HL_IR_DUMP` (host-tool / parity-harness mode) writes the raw byte-stream to
/// the file so it can be diffed against gl_shim.c's dump. Otherwise it is submitted to the host
/// GPU-exec service over the shared transport. (The wayland/dma-buf commit that shows the rendered
/// IOSurface on screen is the remaining display-side plumbing, tracked separately from IR parity.)
/// Present a frame's IR, returning the delivery outcome so `eglSwapBuffers` can be TRANSACTIONAL: the
/// executor submission result is PROPAGATED (never discarded), so a failed submit surfaces as an EGL
/// error and the caller keeps its queued draw state instead of losing the frame. `HL_IR_DUMP` (host-
/// tool / parity-harness mode) writes the raw byte-stream so it can be diffed against gl_shim.c.
fn present_frame(surf: &Surface, ir: &[u8]) -> Result<(), String> {
    if let Some(path) = std::env::var_os("HL_IR_DUMP") {
        return std::fs::write(path, ir).map_err(|e| format!("HL_IR_DUMP write failed: {e}"));
    }
    // Submit the frame to the host GPU-exec service, propagating any transport/executor failure.
    let ts = hl_shim::transport::Surface {
        id: surf.id,
        width: surf.width,
        height: surf.height,
        stride: if surf.stride != 0 { surf.stride } else { surf.width * 4 },
        fd: surf.fd,
        ..Default::default()
    };
    exec_conn()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .submit(&ts, ir)
        .map_err(|e| format!("executor submit failed: {e}"))?;
    // ...then commit the rendered dma-buf/IOSurface to the compositor (hl-display). A commit failure
    // (short write / fd-pass / disconnect / protocol / frame timeout) propagates so the swap reports it
    // instead of pretending the frame was presented.
    if let Some(wl) = wayland_session().lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        wl.commit(surf, &surface_geometry(surf)).map_err(|e| format!("wayland commit failed: {e:?}"))?;
    }
    Ok(())
}

/// `eglSwapBuffers` — the frame boundary: lower the recorded draw-list to IR and present it, then reset
/// per-frame state (gl_shim.c tail). Returns EGL_FALSE if no surface is up. Frames that need the GLSL
/// translator (a real draw) produce no IR yet and are a no-op present until that path lands.
#[no_mangle]
pub extern "C" fn eglSwapBuffers(_dpy: *mut c_void, surface: *mut c_void) -> u32 {
    // A stale/forged surface handle is EGL_BAD_SURFACE (the frame is not touched).
    if crate::state::egl_surface_lookup(surface).is_none() {
        crate::state::egl_set_error(EGL_BAD_SURFACE);
        return EGL_FALSE;
    }
    // Build the frame's IR WITHOUT yet discarding the queued draw state.
    let (ir, surf, touched_default) = {
        let mut s = gl();
        if !s.surf.have {
            return EGL_FALSE;
        }
        if s.have_draw_snap {
            s.attr = s.attr_snap; // apps disable attribs before swap — use the draw-time snapshot
        }
        let ir = crate::frame::build_frame_ir(&s);
        let surf = s.surf;
        let touched_default = s.draws.iter().any(|d| d.target_tex == 0);
        (ir, surf, touched_default)
    };
    // TRANSACTIONAL submit: present BEFORE resetting per-frame state. On a delivery failure the queued
    // draws are RETAINED and the error is reported (EGL_CONTEXT_LOST) — the frame is not silently lost.
    if let Some(bytes) = &ir {
        if let Err(_e) = present_frame(&surf, bytes) {
            crate::state::egl_set_error(EGL_CONTEXT_LOST);
            return EGL_FALSE;
        }
        // The host accepted AND acknowledged the frame (the transport `submit` returns only on
        // ACK_OK; the IR-dump path is a synchronous successful write), so this is the real
        // cross-process completion boundary: advance the sync completion serial. A fence created
        // earlier in the frame is now signaled by an actual host ack, not a local glFinish.
        crate::gles::note_frame_presented();
    }
    // Delivery succeeded (or there was no IR to send): now reset per-frame draw state.
    {
        let mut s = gl();
        s.draws.clear();
        s.draw_mode = -1;
        s.have_draw_snap = false;
        s.draw_indexed = false;
        if touched_default {
            s.default_surface_valid = true;
        }
        s.default_full_clear_since_swap = false;
    }
    EGL_TRUE
}
