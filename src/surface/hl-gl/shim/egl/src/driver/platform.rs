use super::*;

mod fence;
mod image;
pub use fence::*;
pub use image::*;
// ==================================================================================================
// EGL: the remaining lifecycle / query / sync / image / surface-creation entry points
// ==================================================================================================

/// `eglGetPlatformDisplay(platform, native_display, attrib_list)` — the EGL 1.5 display getter; returns the
/// same single display token as `eglGetDisplay` (this driver has one display).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetPlatformDisplay(
    platform: u32,
    _native_display: *mut c_void,
    _attrib_list: *const isize,
) -> *mut c_void {
    crate::stub::trace("eglGetPlatformDisplay", &format!("platform=0x{platform:x}"));
    DISPLAY_TOKEN as *mut c_void
}

// ---- EGL_EXT_platform_base entry points ----------------------------------------------------------
//
// The client extension string ([`hl_gl::adapter::wayland::egl_client_extensions`]) advertises
// `EGL_EXT_platform_base` (+ `EGL_EXT_platform_wayland`). By that extension's contract a caller that
// sees the string resolves these `*EXT` entry points through `eglGetProcAddress` and CALLS them WITHOUT
// a null check (the extension being advertised is the promise they exist) — e.g. the real Khronos
// `eglinfo` calls `eglGetProcAddress("eglGetPlatformDisplayEXT")` for every named platform and invokes
// the returned pointer directly. Advertising the extension while resolving these to null is therefore a
// crash-the-caller bug: `eglinfo` SIGSEGVs jumping through the null. These are the EGLint-attrib-list
// `EXT` spellings of the EGL 1.5 core `eglGetPlatformDisplay` / `eglCreatePlatform*Surface` above and
// forward to the same single display / surface bring-up. They are resolved ONLY via `eglGetProcAddress`
// (extension functions are not part of the exported ABI surface — the 402-symbol golden), so they are
// deliberately not `#[no_mangle]`.

/// `eglGetPlatformDisplayEXT(platform, native_display, attrib_list)` — `EGL_EXT_platform_base` display
/// getter. Same single display token as `eglGetDisplay`; the surfaceless / wayland / gbm / x11 platform
/// enums all map to the one initializable display this driver serves.
pub(super) extern "C" fn eglGetPlatformDisplayEXT(
    _platform: u32,
    _native_display: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    DISPLAY_TOKEN as *mut c_void
}
/// `eglCreatePlatformWindowSurfaceEXT(dpy, config, native_window, attrib_list)` — `EGL_EXT_platform_base`
/// window-surface getter; `native_window` is the `wl_egl_window*`. Brings up the window surface exactly
/// like the core `eglCreatePlatformWindowSurface`.
pub(super) extern "C" fn eglCreatePlatformWindowSurfaceEXT(
    dpy: *mut c_void,
    config: *mut c_void,
    native_window: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return core::ptr::null_mut();
    }
    if config as usize != CONFIG_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
        return core::ptr::null_mut();
    }
    WindowSurface::create(native_window)
}
/// `eglCreatePlatformPixmapSurfaceEXT(...)` — `EGL_EXT_platform_base` pixmap-surface getter; a fresh
/// surface token (no native-pixmap target is modeled).
pub(super) extern "C" fn eglCreatePlatformPixmapSurfaceEXT(
    _dpy: *mut c_void,
    _config: *mut c_void,
    _native_pixmap: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    GlobalState::access(|s| s.mint_token())
}

// ---- EGL_EXT_device_base / device_query / device_enumeration entry points ------------------------
//
// Advertised in both the client and per-display extension strings, so a toolkit's GL loader (libepoxy
// for GTK/GDK) resolves these via `eglGetProcAddress` + the extension string and CALLS them without a
// null check. GDK's Wayland EGL bring-up specifically requires one of
// `EGL_EXT_device_base`/`device_query`/`EGL_KHR_display_reference`/`EGL_NV_stream_metadata` and then
// calls `eglQueryDisplayAttribEXT` to learn the display's backing device.
//
// The model is one truthful device ([`DEVICE_TOKEN`]) representing the hl-gl renderer backed by Husklet's
// projected `/dev/dri/renderD128`. It advertises only `EGL_EXT_device_drm_render_node`: there is no primary
// DRM node, physical vendor/device identity, or other native-device attribute to report. Every
// body is panic-free across the C-ABI seam (raw pointers null-checked, unknown handles/attributes raise
// the accurate `EGL_*` error and return `EGL_FALSE`/null — never a deref crash or fabricated value),
// mirroring the `eglGetConfigAttrib` / `eglGetConfigs` contract discipline. Resolved only via
// `eglGetProcAddress` (extension functions are not part of the exported 402-symbol ABI), so they are
// deliberately not `#[no_mangle]`.

/// `eglQueryDisplayAttribEXT(dpy, attribute, value)` — a display attribute (`EGLAttrib`, i.e. `intptr_t`).
/// GDK asks `EGL_DEVICE_EXT` to learn the display's backing `EGLDeviceEXT`; we answer with our single
/// hl-gl device. A null `value` is `EGL_BAD_PARAMETER`; an attribute we do not model is
/// `EGL_BAD_ATTRIBUTE` — both `EGL_FALSE` WITHOUT writing / dereferencing `value`.
pub(super) extern "C" fn eglQueryDisplayAttribEXT(
    _dpy: *mut c_void,
    attribute: i32,
    value: *mut isize,
) -> u32 {
    if value.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    match attribute {
        EGL_DEVICE_EXT => {
            unsafe { *value = DEVICE_TOKEN as isize };
            EGL_TRUE
        }
        _ => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            EGL_FALSE
        }
    }
}

/// `eglQueryDeviceAttribEXT(device, attribute, value)` — an integer attribute of an `EGLDeviceEXT`. A
/// foreign device handle is `EGL_BAD_DEVICE_EXT`; a null `value` is `EGL_BAD_PARAMETER`. Our software
/// device exposes no vendor integer attributes (those are e.g. `EGL_CUDA_DEVICE_NV`), so any recognized-
/// device attribute is truthfully `EGL_BAD_ATTRIBUTE`. All paths return `EGL_FALSE` without a deref.
pub(super) extern "C" fn eglQueryDeviceAttribEXT(
    device: *mut c_void,
    _attribute: i32,
    value: *mut isize,
) -> u32 {
    if device as usize != DEVICE_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DEVICE_EXT));
        return EGL_FALSE;
    }
    if value.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    // No integer device attributes are modeled for the projected render-node device.
    GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
    EGL_FALSE
}

/// `eglQueryDeviceStringEXT(device, name)` — a string describing the `EGLDeviceEXT`. `EGL_EXTENSIONS`
/// reports `EGL_EXT_device_drm_render_node`; `EGL_DRM_RENDER_NODE_FILE_EXT` returns the projected render
/// node backing hl-gl. We deliberately do not advertise `EGL_EXT_device_drm` or return a primary-node path.
/// A foreign device handle is `EGL_BAD_DEVICE_EXT` + null; an unmodeled `name` is `EGL_BAD_PARAMETER` +
/// null (never a dangling pointer). Returned pointers are process-static (valid for the app's lifetime).
pub(super) extern "C" fn eglQueryDeviceStringEXT(device: *mut c_void, name: i32) -> *const c_char {
    if device as usize != DEVICE_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DEVICE_EXT));
        return core::ptr::null();
    }
    match name {
        // Device extensions are separate from the client/display extension strings.
        EGL_EXTENSIONS_Q => b"EGL_EXT_device_drm_render_node\0".as_ptr() as *const c_char,
        EGL_DRM_RENDER_NODE_FILE_EXT => b"/dev/dri/renderD128\0".as_ptr() as *const c_char,
        _ => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
            core::ptr::null()
        }
    }
}

/// `eglQueryDevicesEXT(max_devices, devices, num_devices)` — enumerate the `EGLDeviceEXT`s. This driver
/// reports its single projected render-node device. Same enumeration contract as `eglGetConfigs`: a null `devices`
/// array reports the count in `num_devices`; a real array copies up to `max_devices` handles (bounded, no
/// OOB store) and `num_devices` reports how many were written. `num_devices` is required (`EGL_BAD_PARAMETER`
/// if null); a non-null `devices` with `max_devices <= 0` is `EGL_BAD_PARAMETER` (spec).
pub(super) extern "C" fn eglQueryDevicesEXT(
    max_devices: i32,
    devices: *mut *mut c_void,
    num_devices: *mut i32,
) -> u32 {
    if num_devices.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    const AVAILABLE: i32 = 1;
    if devices.is_null() {
        // Count-only query.
        unsafe { *num_devices = AVAILABLE };
        return EGL_TRUE;
    }
    if max_devices <= 0 {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let n = AVAILABLE.min(max_devices);
    unsafe {
        for i in 0..n as isize {
            *devices.offset(i) = DEVICE_TOKEN as *mut c_void;
        }
        *num_devices = n;
    }
    EGL_TRUE
}
/// `eglQueryAPI()` — the API bound by `eglBindAPI` on the CALLING THREAD (defaults to
/// `EGL_OPENGL_ES_API`, the only API this driver serves). libepoxy reads this to confirm GLES dispatch.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglQueryAPI() -> u32 {
    current::query_api()
}
/// `eglReleaseThread()` — release the calling thread's EGL state: the current context/surface/display
/// binding is dropped (the bound API resets to the `EGL_OPENGL_ES_API` default via the released cells).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglReleaseThread() -> u32 {
    let context = current::context();
    if context != 0 {
        let previous = (context, current::draw_surface(), current::read_surface());
        let _ = GlobalState::bind_current(previous, (0, 0, 0));
    }
    current::release();
    EGL_TRUE
}
/// `eglWaitClient` / `eglWaitGL` / `eglWaitNative` — flush + wait for the client/native pipeline. The
/// deferred model completes synchronously at swap, so these succeed immediately.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglWaitClient() -> u32 {
    EGL_TRUE
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglWaitGL() -> u32 {
    EGL_TRUE
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglWaitNative(_engine: i32) -> u32 {
    EGL_TRUE
}
/// `eglSurfaceAttrib(dpy, surface, attribute, value)` — set a surface attribute (swap behavior, mipmap
/// hint, …). This model tracks none of the settable attributes; the call is accepted. Success.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglSurfaceAttrib(
    _dpy: *mut c_void,
    _surface: *mut c_void,
    _attribute: i32,
    _value: i32,
) -> u32 {
    EGL_TRUE
}
/// `eglBindTexImage` / `eglReleaseTexImage` — bind/release a pbuffer as a texture image. No render-to-
/// texture pbuffer path is modeled; the call is accepted as a no-op. Success.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglBindTexImage(_dpy: *mut c_void, _surface: *mut c_void, _buffer: i32) -> u32 {
    EGL_TRUE
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglReleaseTexImage(
    _dpy: *mut c_void,
    _surface: *mut c_void,
    _buffer: i32,
) -> u32 {
    EGL_TRUE
}
/// `eglCopyBuffers(dpy, surface, target)` — copy the surface color buffer to a native pixmap. No native
/// pixmap target is modeled; accepted as a no-op. Success.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCopyBuffers(
    _dpy: *mut c_void,
    _surface: *mut c_void,
    _target: *mut c_void,
) -> u32 {
    EGL_TRUE
}
/// `eglQueryContext(dpy, ctx, attribute, value)` — report a context attribute. This driver serves exactly
/// one client API (OpenGL ES), so `EGL_CONTEXT_CLIENT_TYPE` MUST answer `EGL_OPENGL_ES_API` — libepoxy's
/// `epoxy_egl_get_current_gl_context_api()` queries this to classify the current context, and treats any
/// value other than `EGL_OPENGL_API`/`EGL_OPENGL_ES_API` as "no EGL context" (aborting
/// `epoxy_get_proc_address` with "Couldn't find current GLX or EGL context"). Other attributes report the
/// truthful fixed values of the single back-buffered ES3 context this driver models; unknown attributes
/// read `0` and still succeed.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglQueryContext(
    dpy: *mut c_void,
    ctx: *mut c_void,
    attribute: i32,
    value: *mut i32,
) -> u32 {
    if value.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return EGL_FALSE;
    }
    let Some(attributes) = GlobalState::access(|s| s.context_attributes(ctx as usize)) else {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONTEXT));
        return EGL_FALSE;
    };
    let v = match attribute {
        EGL_CONTEXT_CLIENT_TYPE => EGL_OPENGL_ES_API as i32,
        EGL_CONTEXT_CLIENT_VERSION => attributes.client_version,
        EGL_CONTEXT_MINOR_VERSION_KHR => attributes.minor_version,
        EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT => attributes.robust_access as i32,
        EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT => attributes.reset_strategy,
        EGL_CONTEXT_OPENGL_NO_ERROR_KHR => attributes.no_error as i32,
        EGL_RENDER_BUFFER => EGL_BACK_BUFFER,
        _ => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            return EGL_FALSE;
        }
    };
    unsafe { *value = v };
    EGL_TRUE
}
/// `eglQuerySurface(dpy, surface, attribute, value)` — the surface geometry. `EGL_WIDTH`/`EGL_HEIGHT`
/// report the live window-surface size (real); other attributes read `0`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglQuerySurface(
    dpy: *mut c_void,
    surface: *mut c_void,
    attribute: i32,
    value: *mut i32,
) -> u32 {
    if value.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return EGL_FALSE;
    }
    let Some(v) = GlobalState::access(|s| {
        let surface = s.surface(surface as usize)?;
        match attribute {
            EGL_WIDTH => Some(surface.render.width as i32),
            EGL_HEIGHT => Some(surface.render.height as i32),
            _ => None,
        }
    }) else {
        let error = if GlobalState::access(|s| s.has_surface(surface as usize)) {
            EGL_BAD_ATTRIBUTE
        } else {
            EGL_BAD_SURFACE
        };
        GlobalState::access(|s| s.set_egl_error(error));
        return EGL_FALSE;
    };
    unsafe { *value = v };
    EGL_TRUE
}
/// `eglCreatePbufferSurface(dpy, config, attrib_list)` — an offscreen pbuffer surface. Modeled as a window
/// surface sized from the environment (the driver renders to one color target); returns a fresh token.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePbufferSurface(
    dpy: *mut c_void,
    config: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return core::ptr::null_mut();
    }
    if config as usize != CONFIG_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
        return core::ptr::null_mut();
    }
    let (width, height) = default_surface_wh();
    GlobalState::access(|s| {
        s.create_surface(
            hl_gl::model::context::SurfaceKind::Offscreen,
            width,
            height,
            0,
            0,
        )
    })
}
/// `eglCreatePixmapSurface(dpy, config, pixmap, attrib_list)` — a native-pixmap-backed surface; modeled as
/// a fresh surface token (no separate pixmap storage).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePixmapSurface(
    _dpy: *mut c_void,
    _config: *mut c_void,
    _pixmap: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    GlobalState::access(|s| s.mint_token())
}
/// `eglCreatePbufferFromClientBuffer(...)` — a pbuffer wrapping a client buffer (e.g. an OpenVG image);
/// modeled as a fresh surface token.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePbufferFromClientBuffer(
    _dpy: *mut c_void,
    _buftype: u32,
    _buffer: *mut c_void,
    _config: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    GlobalState::access(|s| s.mint_token())
}
/// `eglCreatePlatformWindowSurface(dpy, config, native_window, attrib_list)` — the EGL 1.5 window-surface
/// getter modern toolkits use. `native_window` is the same `wl_egl_window*`; brings up the window surface
/// (size + Wayland present session) exactly like `eglCreateWindowSurface`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePlatformWindowSurface(
    dpy: *mut c_void,
    config: *mut c_void,
    native_window: *mut c_void,
    _attrib_list: *const isize,
) -> *mut c_void {
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return core::ptr::null_mut();
    }
    if config as usize != CONFIG_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
        return core::ptr::null_mut();
    }
    WindowSurface::create(native_window)
}
/// `eglCreatePlatformPixmapSurface(...)` — the EGL 1.5 pixmap-surface getter; a fresh surface token.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePlatformPixmapSurface(
    _dpy: *mut c_void,
    _config: *mut c_void,
    _native_pixmap: *mut c_void,
    _attrib_list: *const isize,
) -> *mut c_void {
    GlobalState::access(|s| s.mint_token())
}
