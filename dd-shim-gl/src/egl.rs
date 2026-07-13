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

/// `eglGetError` — last EGL error for the calling thread, cleared on read (gl_shim.c semantics).
#[no_mangle]
pub extern "C" fn eglGetError() -> i32 {
    let mut e = egl();
    let err = e.error;
    e.error = EGL_SUCCESS;
    err
}

/// `eglQueryString` — vendor/version/APIs/extensions. Mirrors gl_shim.c, including the split client- vs
/// display-extension set keyed on `EGL_NO_DISPLAY` (ANGLE's client-extension probe).
#[no_mangle]
pub extern "C" fn eglQueryString(dpy: *mut c_void, name: i32) -> *const c_char {
    match name {
        0x3053 => cstr!("dd"),          // EGL_VENDOR
        0x3054 => cstr!("1.4 dd-shim"), // EGL_VERSION
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

#[no_mangle]
pub extern "C" fn eglReleaseThread() -> u32 {
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

/// `eglChooseConfig` — always returns the single config (id 1).
#[no_mangle]
pub extern "C" fn eglChooseConfig(
    _dpy: *mut c_void,
    _attrib_list: *const i32,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
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

/// `eglGetConfigAttrib` — real, self-consistent attributes for config 1 (RGBA8, depth24, stencil0),
/// matching gl_shim.c exactly (incl. stencil=0 so glmark2's config scorer accepts it).
#[no_mangle]
pub extern "C" fn eglGetConfigAttrib(_dpy: *mut c_void, _config: *mut c_void, attribute: i32, value: *mut i32) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    let es3_bit = if shim_es3() { EGL_OPENGL_ES3_BIT_KHR } else { 0 };
    let r = match attribute {
        EGL_CONFIG_ID => 1,
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
        _ => 0,
    };
    unsafe { *value = r };
    EGL_TRUE
}

// ===================================================================================================
// context lifecycle
// ===================================================================================================

/// `eglCreateContext` — validates the requested client version against the shim's max (ES3 iff
/// `DD_SHIM_ES3`), records it, and returns a context handle. On too-high a request it sets
/// EGL_BAD_MATCH and returns EGL_NO_CONTEXT (null), exactly as gl_shim.c.
#[no_mangle]
pub extern "C" fn eglCreateContext(
    _dpy: *mut c_void,
    _config: *mut c_void,
    _share: *mut c_void,
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
        egl().error = EGL_BAD_MATCH;
        return core::ptr::null_mut(); // EGL_NO_CONTEXT
    }
    {
        let mut e = egl();
        e.ctx_major = req_major;
        e.ctx_minor = req_minor;
    }
    1 as *mut c_void
}

#[no_mangle]
pub extern "C" fn eglDestroyContext(_dpy: *mut c_void, _ctx: *mut c_void) -> u32 {
    EGL_TRUE
}

/// `eglMakeCurrent` — single implicit context; always succeeds (no per-surface bring-up here).
#[no_mangle]
pub extern "C" fn eglMakeCurrent(_dpy: *mut c_void, _draw: *mut c_void, _read: *mut c_void, _ctx: *mut c_void) -> u32 {
    EGL_TRUE
}

/// `eglQueryContext` — client type/version of the (single) context.
#[no_mangle]
pub extern "C" fn eglQueryContext(_dpy: *mut c_void, _ctx: *mut c_void, attribute: i32, value: *mut i32) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    let e = egl();
    let v = match attribute {
        EGL_CONTEXT_CLIENT_TYPE => EGL_OPENGL_ES_API as i32,
        EGL_CONTEXT_CLIENT_VERSION => e.ctx_major,
        EGL_CONTEXT_MINOR_VERSION_KHR => e.ctx_minor,
        _ => 0,
    };
    unsafe { *value = v };
    EGL_TRUE
}

// ===================================================================================================
// surface query / destroy (bring-up stays a stub — see module docs)
// ===================================================================================================

/// `eglQuerySurface` — width/height of the current window surface. The size is populated by surface
/// bring-up (`eglCreateWindowSurface`), which lands with the present-path work; until then it reports
/// the tracked logical size (0 by default).
#[no_mangle]
pub extern "C" fn eglQuerySurface(_dpy: *mut c_void, _surface: *mut c_void, attribute: i32, value: *mut i32) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    let e = egl();
    let v = match attribute {
        EGL_WIDTH => e.surface_logical_w,
        EGL_HEIGHT => e.surface_logical_h,
        _ => 0,
    };
    unsafe { *value = v };
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglDestroySurface(_dpy: *mut c_void, _surface: *mut c_void) -> u32 {
    EGL_TRUE
}

// ===================================================================================================
// API selection + misc no-op lifecycle
// ===================================================================================================

#[no_mangle]
pub extern "C" fn eglBindAPI(_api: u32) -> u32 {
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglQueryAPI() -> u32 {
    EGL_OPENGL_ES_API
}

#[no_mangle]
pub extern "C" fn eglSwapInterval(_dpy: *mut c_void, _interval: i32) -> u32 {
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglGetCurrentSurface(_readdraw: i32) -> *mut c_void {
    1 as *mut c_void
}

#[no_mangle]
pub extern "C" fn eglGetCurrentDisplay() -> *mut c_void {
    1 as *mut c_void
}

#[no_mangle]
pub extern "C" fn eglGetCurrentContext() -> *mut c_void {
    1 as *mut c_void
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

/// Our own `libwayland-egl` window handle (mirrors gl_shim.c `struct dd_wl_egl_window`). The first
/// field is Mesa-ABI-compatible (`intptr_t version`) so a stray Mesa struct is still parseable.
const DD_WL_EGL_MAGIC: isize = 0x0064_6477_6c65_676c; // "ddwlegl"
#[repr(C)]
pub struct DdWlEglWindow {
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
/// `dd_wl_egl_window` magic struct (glmark2/Chrome via our libwayland-egl) plus gl_shim.c's stock-app
/// heuristics (es2*/ANGLE two-int + Mesa `wl_egl_window` word shapes).
unsafe fn parse_native_window(w: *const c_void) -> (u32, u32) {
    let (mut ww, mut hh) = (256i32, 256i32);
    if !w.is_null() {
        let version = *(w as *const isize);
        if version == DD_WL_EGL_MAGIC {
            let win = &*(w as *const DdWlEglWindow);
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
pub extern "C" fn wl_egl_window_create(surface: *mut c_void, width: i32, height: i32) -> *mut DdWlEglWindow {
    if width <= 0 || height <= 0 {
        return core::ptr::null_mut();
    }
    let w = Box::into_raw(Box::new(DdWlEglWindow {
        version: DD_WL_EGL_MAGIC,
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
pub extern "C" fn wl_egl_window_resize(w: *mut DdWlEglWindow, width: i32, height: i32, dx: i32, dy: i32) {
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
pub extern "C" fn wl_egl_window_get_attached_size(w: *mut DdWlEglWindow, width: *mut i32, height: *mut i32) {
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
pub extern "C" fn wl_egl_window_destroy(w: *mut DdWlEglWindow) {
    if !w.is_null() {
        unsafe { drop(Box::from_raw(w)) };
    }
}

/// The process-global wayland session (deployed present path). None in DD_IR_DUMP/host-tool mode.
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
/// In DD_IR_DUMP/host-tool mode this only records the surface (id 1); the renderD128 IOSurface
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
    // Deployed path (not host-tool DD_IR_DUMP): register the GPU buffer + bring up the wayland surface.
    if std::env::var_os("DD_IR_DUMP").is_none() {
        if let Ok(a) = dd_shim_common::transport::renderd::alloc(w, h, 0) {
            let mut s = gl();
            s.surf.id = a.id;
            s.surf.stride = a.stride;
            s.surf.fd = a.fd;
            s.surf.width = a.width;
            s.surf.height = a.height;
        }
        if let Some(wl) = crate::wayland::Wayland::connect_and_handshake(&geom) {
            *wayland_session().lock().unwrap_or_else(|e| e.into_inner()) = Some(wl);
        }
    }
    1 as *mut c_void
}

/// `eglCreatePbufferSurface` — an inert 1x1 offscreen surface (ANGLE's bootstrap). Swap is a no-op on
/// it because `eglSwapBuffers` only acts once a window surface is up.
#[no_mangle]
pub extern "C" fn eglCreatePbufferSurface(_dpy: *mut c_void, _config: *mut c_void, _attribs: *const i32) -> *mut c_void {
    1 as *mut c_void
}

fn exec_conn() -> &'static Mutex<dd_shim_common::transport::ExecConn> {
    static C: OnceLock<Mutex<dd_shim_common::transport::ExecConn>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(dd_shim_common::transport::ExecConn::from_env()))
}

/// Present a frame's IR. `DD_IR_DUMP` (host-tool / parity-harness mode) writes the raw byte-stream to
/// the file so it can be diffed against gl_shim.c's dump. Otherwise it is submitted to the host
/// GPU-exec service over the shared transport. (The wayland/dma-buf commit that shows the rendered
/// IOSurface on screen is the remaining display-side plumbing, tracked separately from IR parity.)
fn present_frame(surf: &Surface, ir: &[u8]) {
    if let Some(path) = std::env::var_os("DD_IR_DUMP") {
        let _ = std::fs::write(path, ir);
        return;
    }
    // Submit the frame to the host GPU-exec service...
    let ts = dd_shim_common::transport::Surface {
        id: surf.id,
        width: surf.width,
        height: surf.height,
        stride: if surf.stride != 0 { surf.stride } else { surf.width * 4 },
        fd: surf.fd,
    };
    let _ = exec_conn().lock().unwrap_or_else(|e| e.into_inner()).submit(&ts, ir);
    // ...then commit the rendered dma-buf/IOSurface to the compositor (dd-display).
    if let Some(wl) = wayland_session().lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        wl.commit(surf, &surface_geometry(surf));
    }
}

/// `eglSwapBuffers` — the frame boundary: lower the recorded draw-list to IR and present it, then reset
/// per-frame state (gl_shim.c tail). Returns EGL_FALSE if no surface is up. Frames that need the GLSL
/// translator (a real draw) produce no IR yet and are a no-op present until that path lands.
#[no_mangle]
pub extern "C" fn eglSwapBuffers(_dpy: *mut c_void, _surface: *mut c_void) -> u32 {
    let (ir, surf) = {
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
        // reset per-frame draw state
        s.draws.clear();
        s.draw_mode = -1;
        s.have_draw_snap = false;
        s.draw_indexed = false;
        if touched_default {
            s.default_surface_valid = true;
        }
        s.default_full_clear_since_swap = false;
        (ir, surf)
    };
    if let Some(bytes) = ir {
        present_frame(&surf, &bytes);
    }
    EGL_TRUE
}
