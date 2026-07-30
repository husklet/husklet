use super::*;

mod fence;
mod image;
mod query;
pub use fence::*;
pub use image::*;
pub use query::*;
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
    if super::egl::ConfigHandle::id(config).is_none() {
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
// The three attributes EGL 1.5 3.5.6 makes settable, and the tokens their values are drawn from.
const EGL_MIPMAP_LEVEL: i32 = 0x3083;
const EGL_SWAP_BEHAVIOR: i32 = 0x3093;
const EGL_BUFFER_PRESERVED: i32 = 0x3094;
const EGL_BUFFER_DESTROYED: i32 = 0x3095;
const EGL_MULTISAMPLE_RESOLVE: i32 = 0x3099;
const EGL_MULTISAMPLE_RESOLVE_DEFAULT: i32 = 0x309A;
const EGL_MULTISAMPLE_RESOLVE_BOX: i32 = 0x309B;

/// Validate the `(display, surface)` pair every surface operation takes, setting the accurate EGL error.
/// Reporting success for a handle the driver does not know is the failure mode most likely to precede a
/// crash later: the caller proceeds believing state changed.
fn surface_operand(dpy: *mut c_void, surface: *mut c_void) -> Result<(), i32> {
    if dpy as usize != DISPLAY_TOKEN {
        return Err(EGL_BAD_DISPLAY);
    }
    if !GlobalState::access(|state| state.has_surface(surface as usize)) {
        return Err(EGL_BAD_SURFACE);
    }
    Ok(())
}

fn surface_failure(error: i32) -> u32 {
    GlobalState::access(|state| state.set_egl_error(error));
    EGL_FALSE
}

/// `eglSurfaceAttrib(dpy, surface, attribute, value)` — set a surface attribute.
///
/// EGL 1.5 §3.5.6 defines exactly three settable attributes and makes any other `EGL_BAD_ATTRIBUTE`; an
/// unknown surface is `EGL_BAD_SURFACE`. This model tracks none of the three, so a recognized
/// attribute/value pair stays an accepted no-op — but a bad handle or an undefined token is no longer
/// reported as success.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglSurfaceAttrib(
    dpy: *mut c_void,
    surface: *mut c_void,
    attribute: i32,
    value: i32,
) -> u32 {
    if let Err(error) = surface_operand(dpy, surface) {
        return surface_failure(error);
    }
    let recognized = match attribute {
        EGL_MIPMAP_LEVEL => true,
        EGL_MULTISAMPLE_RESOLVE => {
            matches!(
                value,
                EGL_MULTISAMPLE_RESOLVE_DEFAULT | EGL_MULTISAMPLE_RESOLVE_BOX
            )
        }
        EGL_SWAP_BEHAVIOR => matches!(value, EGL_BUFFER_PRESERVED | EGL_BUFFER_DESTROYED),
        _ => false,
    };
    if !recognized {
        return surface_failure(EGL_BAD_ATTRIBUTE);
    }
    EGL_TRUE
}
/// `eglBindTexImage` / `eglReleaseTexImage` — bind/release a pbuffer as a texture image.
///
/// EGL 1.5 §3.6.1: `buffer` must be `EGL_BACK_BUFFER` (else `EGL_BAD_PARAMETER`), and a surface whose
/// `EGL_TEXTURE_FORMAT` is `EGL_NO_TEXTURE` is `EGL_BAD_SURFACE`. No render-to-texture pbuffer is modeled,
/// so every surface this driver creates has no texture format and the operation is refused — previously it
/// reported success for an operation nothing backs, and for handles that do not exist.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglBindTexImage(dpy: *mut c_void, surface: *mut c_void, buffer: i32) -> u32 {
    match surface_operand(dpy, surface) {
        Err(error) => surface_failure(error),
        Ok(()) if buffer != EGL_BACK_BUFFER => surface_failure(EGL_BAD_PARAMETER),
        Ok(()) => surface_failure(EGL_BAD_SURFACE),
    }
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglReleaseTexImage(dpy: *mut c_void, surface: *mut c_void, buffer: i32) -> u32 {
    eglBindTexImage(dpy, surface, buffer)
}
/// `eglCopyBuffers(dpy, surface, target)` — copy the surface color buffer to a native pixmap. EGL 1.5
/// §3.9.4: an invalid pixmap target is `EGL_BAD_NATIVE_PIXMAP`. No native pixmap target is modeled, so
/// there is no target this driver can accept; it no longer claims the copy happened.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCopyBuffers(
    dpy: *mut c_void,
    surface: *mut c_void,
    _target: *mut c_void,
) -> u32 {
    match surface_operand(dpy, surface) {
        Err(error) => surface_failure(error),
        Ok(()) => surface_failure(EGL_BAD_NATIVE_PIXMAP),
    }
}
/// The size an `eglCreatePbufferSurface` attribute list asks for.
///
/// EGL 1.5 §3.5.2: `EGL_WIDTH` / `EGL_HEIGHT` give the pbuffer's dimensions and a negative value is
/// `EGL_BAD_PARAMETER`. Unspecified dimensions default to this driver's offscreen size rather than the
/// spec's zero, because a zero-sized color target is not renderable and callers that omit the attributes
/// (ANGLE's / Chrome's offscreen bring-up) expect a usable surface. Every other attribute is accepted and
/// ignored: this driver models one RGBA8 color target with no texture-bound pbuffer path.
struct PbufferSize;

impl PbufferSize {
    /// Read the requested dimensions, or `None` when the list is malformed (negative dimension).
    fn parse(attrib_list: *const i32) -> Option<(u32, u32)> {
        let (mut width, mut height) = default_surface_wh();
        if attrib_list.is_null() {
            return Some((width, height));
        }
        for index in 0..64 {
            // SAFETY: an EGL attribute list is a caller-supplied `EGL_NONE`-terminated pair sequence; the
            // loop stops at that terminator and reads the value only after seeing a non-terminator key.
            let attribute = unsafe { *attrib_list.add(index * 2) };
            if attribute == EGL_NONE {
                break;
            }
            let value = unsafe { *attrib_list.add(index * 2 + 1) };
            match attribute {
                EGL_WIDTH if value < 0 => return None,
                EGL_HEIGHT if value < 0 => return None,
                EGL_WIDTH => width = value as u32,
                EGL_HEIGHT => height = value as u32,
                _ => {}
            }
        }
        Some((width.max(1), height.max(1)))
    }
}

/// `eglCreatePbufferSurface(dpy, config, attrib_list)` — an offscreen pbuffer surface, sized from the
/// attribute list per EGL 1.5 §3.5.2 and backed by the same single color target a window surface uses, so
/// GLES rendering into it (and `glReadPixels` out of it) works for `--off-screen` and headless callers.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePbufferSurface(
    dpy: *mut c_void,
    config: *mut c_void,
    attrib_list: *const i32,
) -> *mut c_void {
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return core::ptr::null_mut();
    }
    if super::egl::ConfigHandle::id(config).is_none() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
        return core::ptr::null_mut();
    }
    let Some((width, height)) = PbufferSize::parse(attrib_list) else {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return core::ptr::null_mut();
    };
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
    if super::egl::ConfigHandle::id(config).is_none() {
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
