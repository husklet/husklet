//! `eglQueryContext` and `eglQuerySurface` — the attribute sets EGL 1.4 tables 3.6 and 3.5 define, and
//! the errors it defines for the invalid cases. These are how a toolkit learns what handle it is holding,
//! so an attribute the specification requires that answers `EGL_BAD_ATTRIBUTE` reads to the caller as a
//! broken surface: Chromium queries `EGL_RENDER_BUFFER` on every surface it makes current.

use super::*;

/// Surface / context attributes EGL 1.4 tables 3.5 and 3.6 define beyond the ones the driver already had.
const EGL_CONFIG_ID: i32 = 0x3028;
const EGL_LARGEST_PBUFFER: i32 = 0x3058;
const EGL_NO_TEXTURE: i32 = 0x305C;
const EGL_TEXTURE_FORMAT: i32 = 0x3080;
const EGL_TEXTURE_TARGET: i32 = 0x3081;
const EGL_MIPMAP_TEXTURE: i32 = 0x3082;
const EGL_VG_COLORSPACE: i32 = 0x3087;
const EGL_VG_ALPHA_FORMAT: i32 = 0x3088;
const EGL_VG_COLORSPACE_SRGB: i32 = 0x3089;
const EGL_VG_ALPHA_FORMAT_NONPRE: i32 = 0x308B;
const EGL_HORIZONTAL_RESOLUTION: i32 = 0x3090;
const EGL_VERTICAL_RESOLUTION: i32 = 0x3091;
const EGL_PIXEL_ASPECT_RATIO: i32 = 0x3092;
/// EGL 1.4 §3.5.6: the value the resolution / aspect-ratio attributes take when the display geometry is
/// not known, which is always the case here — no native display reports physical dimensions.
const EGL_UNKNOWN: i32 = -1;

/// The single `EGLConfig` this driver exposes ([`hl_gl::service::config::NUM_CONFIGS`] is 1), so every
/// surface and every context was necessarily created on it and `EGL_CONFIG_ID` is exact rather than a guess.
fn only_config_id() -> i32 {
    hl_gl::service::config::CONFIG_ID
}

/// `eglQueryContext(dpy, ctx, attribute, value)` — report a context attribute. This driver serves exactly
/// one client API (OpenGL ES), so `EGL_CONTEXT_CLIENT_TYPE` MUST answer `EGL_OPENGL_ES_API` — libepoxy's
/// `epoxy_egl_get_current_gl_context_api()` queries this to classify the current context, and treats any
/// value other than `EGL_OPENGL_API`/`EGL_OPENGL_ES_API` as "no EGL context" (aborting
/// `epoxy_get_proc_address` with "Couldn't find current GLX or EGL context"). The other attributes report
/// the truthful fixed values of the single back-buffered ES3 context this driver models; an attribute that
/// is not a context attribute is `EGL_BAD_ATTRIBUTE`.
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
        // EGL 1.4 table 3.6: the id of the config the context was created with.
        EGL_CONFIG_ID => only_config_id(),
        _ => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            return EGL_FALSE;
        }
    };
    unsafe { *value = v };
    EGL_TRUE
}

/// `eglQuerySurface(dpy, surface, attribute, value)` — every attribute EGL 1.4 table 3.5 defines.
/// `EGL_WIDTH`/`EGL_HEIGHT` report the live surface size; the rest are the fixed, truthful properties of
/// the surfaces this driver creates: back-buffered, no render-to-texture, no known physical resolution.
/// An attribute that is not a surface attribute is `EGL_BAD_ATTRIBUTE`, an unknown handle `EGL_BAD_SURFACE`,
/// and neither writes `value`.
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
    if let Err(error) = surface_operand(dpy, surface) {
        GlobalState::access(|s| s.set_egl_error(error));
        return EGL_FALSE;
    }
    let extent = |pick: fn(&hl_gl::model::context::GlSurface) -> u32| {
        GlobalState::access(|s| s.surface(surface as usize).map(|s| pick(&s.render) as i32))
    };
    let v = match attribute {
        EGL_WIDTH => extent(|render| render.width),
        EGL_HEIGHT => extent(|render| render.height),
        EGL_CONFIG_ID => Some(only_config_id()),
        // The color buffer is the back buffer of a double-buffered surface; swapping discards it.
        EGL_RENDER_BUFFER => Some(EGL_BACK_BUFFER),
        EGL_SWAP_BEHAVIOR => Some(EGL_BUFFER_DESTROYED),
        // Nothing here resolves multisampling, and no config claims EGL_BIND_TO_TEXTURE_*, so a surface
        // never has a texture format, target, mipmaps or a mipmap level other than the defaults.
        EGL_MULTISAMPLE_RESOLVE => Some(EGL_MULTISAMPLE_RESOLVE_DEFAULT),
        EGL_TEXTURE_FORMAT | EGL_TEXTURE_TARGET => Some(EGL_NO_TEXTURE),
        EGL_MIPMAP_TEXTURE => Some(EGL_FALSE as i32),
        EGL_MIPMAP_LEVEL => Some(0),
        // A pbuffer is created at exactly the size asked for; EGL_LARGEST_PBUFFER is never honoured.
        EGL_LARGEST_PBUFFER => Some(EGL_FALSE as i32),
        EGL_HORIZONTAL_RESOLUTION | EGL_VERTICAL_RESOLUTION | EGL_PIXEL_ASPECT_RATIO => {
            Some(EGL_UNKNOWN)
        }
        // OpenVG is not served, but table 3.5 still specifies these two and gives them defaults.
        EGL_VG_COLORSPACE => Some(EGL_VG_COLORSPACE_SRGB),
        EGL_VG_ALPHA_FORMAT => Some(EGL_VG_ALPHA_FORMAT_NONPRE),
        _ => None,
    };
    let Some(v) = v else {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
        return EGL_FALSE;
    };
    unsafe { *value = v };
    EGL_TRUE
}
