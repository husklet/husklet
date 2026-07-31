//! The hand-written `egl*` + `gl*` entry points: marshal the GLES2/EGL C ABI into the `hl_gl` lowering
//! services and (only at swap) submit through the process-global [`crate::state`] sink.
//!
//! Two groups: the **EGL lifecycle** (display / config / context / surface bring-up + present) that
//! returns real, sane values so a dlopen + probe accepts the driver, and the **GLES core render set**
//! (buffer / texture / shader / program creation + the bound draw state + `glDraw*`) that RECORDS into
//! the per-context model exactly as the shared `hl_gl::service::record` ops do — the SAME deferred
//! lowering the in-process test exercises — with the whole frame's IR emitted at `eglSwapBuffers`
//! ([`hl_gl::service::swap`]).
//!
//! Every body is panic-free across the C-ABI seam: raw pointers are null-checked, and a lowering
//! [`hl_gpu::GpuError`] at swap is mapped to the accurate `EGL_*` error via [`hl_gl::result`] (never a
//! false success). The crate builds with `panic = "abort"` as a belt-and-braces second guarantee.

mod barriers;
mod chromium;
mod commands;
mod drawing;
mod egl;
mod objects;
mod platform;
mod present;
mod program_uniforms;
mod programs;
mod queries;
mod raster;
mod reflection;
mod resolver;
mod robust;
mod storage;
mod synchronization;
mod textures;
mod uniforms;

#[cfg(test)]
mod tests;

pub use barriers::*;
pub use chromium::*;
pub use commands::*;
pub use drawing::*;
pub use egl::*;
pub use objects::*;
pub use platform::*;
pub use program_uniforms::*;
pub use programs::*;
pub use queries::*;
pub use raster::*;
pub use reflection::*;
pub use resolver::*;
pub use robust::*;
pub use storage::*;
pub use synchronization::*;
pub use textures::*;
pub use uniforms::*;

use egl::WindowSurface;
use platform::{
    eglClientWaitSyncKHR, eglCreatePlatformPixmapSurfaceEXT, eglCreatePlatformWindowSurfaceEXT,
    eglCreateSyncKHR, eglDestroySyncKHR, eglGetPlatformDisplayEXT, eglGetSyncAttribKHR,
    eglQueryDeviceAttribEXT, eglQueryDeviceStringEXT, eglQueryDevicesEXT, eglQueryDisplayAttribEXT,
    eglWaitSyncKHR,
};
use programs::{slice_f32, slice_i32, write_empty_info_log, LittleEndian, Uniform};
use queries::write_c_name;
use uniforms::{
    mat_bytes_cr, DEVICE_TOKEN, EGL_BACK_BUFFER, EGL_BAD_DEVICE_EXT, EGL_CONTEXT_CLIENT_TYPE,
    EGL_CONTEXT_CLIENT_VERSION, EGL_DEVICE_EXT, EGL_HEIGHT, EGL_OPENGL_ES_API, EGL_RENDER_BUFFER,
    EGL_WIDTH,
};

use core::ffi::{c_char, c_void};

use hl_gl::model::context::GlContext;
use hl_gl::model::glconst::*;
use hl_gl::result::{
    egl_error_from_gpu_error, EGL_BAD_ACCESS, EGL_BAD_ATTRIBUTE, EGL_BAD_CONFIG, EGL_BAD_CONTEXT,
    EGL_BAD_DISPLAY, EGL_BAD_MATCH, EGL_BAD_NATIVE_PIXMAP, EGL_BAD_PARAMETER, EGL_BAD_SURFACE,
    EGL_FALSE, EGL_NOT_INITIALIZED, EGL_TRUE, GL_INVALID_ENUM, GL_INVALID_VALUE, GL_OUT_OF_MEMORY,
};
use hl_gl::service::{
    compute, config, es3, intro, map, query, readpixels, record, swap, sync, upload::Upload,
};
use hl_gpu::BufferId;

use crate::state::{
    current, AppPresentOutcome, ContextAttributes, GlobalState, MakeCurrentError, Observation,
    CONFIG_TOKEN, DISPLAY_TOKEN,
};

fn gpu_read_pixels(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
) -> hl_gpu::Result<Vec<u8>> {
    let completed = GlobalState::gpu_io(std::time::Duration::from_secs(31), move |group, sink| {
        readpixels::prepare_pixels(&mut group.gl, sink, x, y, width, height, format)
    })?;
    let raw = completed
        .observations
        .into_iter()
        .find_map(|observation| match observation {
            Observation::Read(bytes) => Some(bytes),
            Observation::Wait | Observation::Timed(_) | Observation::Poll(_) => None,
        });
    Ok(completed.value.complete(raw))
}

// ---- EGL query enums the string getters key on (the GL_* query enums live in hl_gl::glconst) ------

const EGL_VENDOR: i32 = 0x3053;
const EGL_VERSION_Q: i32 = 0x3054;
const EGL_EXTENSIONS_Q: i32 = 0x3055;
const EGL_CLIENT_APIS: i32 = 0x308D;
const EGL_NONE: i32 = 0x3038;
const EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT: i32 = 0x30BF;
const EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT: i32 = 0x3138;
const EGL_CONTEXT_OPENGL_NO_ERROR_KHR: i32 = 0x31B3;
const EGL_CONTEXT_MINOR_VERSION_KHR: i32 = 0x30FB;
// `EGL_KHR_create_context` (advertised in the display extension string) defines a flags word alongside the
// major/minor pair, plus its own spelling of the reset-notification attribute. A client that passes the
// flags word — even the default 0 — must not be refused, or NO version can be created at all.
const EGL_CONTEXT_FLAGS_KHR: i32 = 0x30FC;
const EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_KHR: i32 = 0x31BD;
const EGL_CONTEXT_OPENGL_DEBUG_BIT_KHR: i32 = 0x0001;
// `EGL_CONTEXT_OPENGL_FORWARD_COMPATIBLE_BIT_KHR` is defined only for OpenGL contexts, so setting it on an
// OpenGL ES context is `EGL_BAD_ATTRIBUTE`.
const EGL_CONTEXT_OPENGL_FORWARD_COMPATIBLE_BIT_KHR: i32 = 0x0002;
const EGL_CONTEXT_OPENGL_ROBUST_ACCESS_BIT_KHR: i32 = 0x0004;
const EGL_NO_RESET_NOTIFICATION_EXT: i32 = 0x31BE;
const EGL_LOSE_CONTEXT_ON_RESET_EXT: i32 = 0x31BF;

// `eglGetCurrentSurface` selectors: which of the current binding's two surfaces to report.
#[cfg(test)]
const EGL_DRAW: i32 = 0x3059;
const EGL_READ: i32 = 0x305A;

// ---- small C-ABI marshalling helpers -------------------------------------------------------------

struct RawBytes;

impl RawBytes {
    /// Borrow a raw C span, treating null and non-positive lengths as empty.
    unsafe fn read<'a>(pointer: *const c_void, length: isize) -> &'a [u8] {
        if pointer.is_null() || length <= 0 {
            &[]
        } else {
            std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize)
        }
    }
}

/// Concatenate a `glShaderSource` string array (`count` NUL-or-length-delimited fragments) into a String.
unsafe fn join_source(count: i32, string: *const *const c_char, length: *const i32) -> String {
    let mut s = String::new();
    if string.is_null() || count <= 0 {
        return s;
    }
    for i in 0..count as isize {
        let frag = *string.offset(i);
        if frag.is_null() {
            continue;
        }
        let len = if length.is_null() {
            -1
        } else {
            *length.offset(i)
        };
        if len < 0 {
            if let Ok(t) = std::ffi::CStr::from_ptr(frag).to_str() {
                s.push_str(t);
            }
        } else {
            let raw = std::slice::from_raw_parts(frag as *const u8, len as usize);
            s.push_str(&String::from_utf8_lossy(raw));
        }
    }
    s
}

/// Borrow a NUL-terminated C string as an owned `String` (`None` if null or not valid UTF-8). Used by the
/// `glGet*Location` name lookups.
struct Text;

impl Text {
    unsafe fn read(pointer: *const c_char) -> Option<String> {
        if pointer.is_null() {
            return None;
        }
        core::ffi::CStr::from_ptr(pointer)
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

/// Convert an uploaded `glTexImage2D` image to the RGBA8 (`w*h*4`) plane the frame builder consumes.
/// Handles the common RGBA/BGRA/RGB `UNSIGNED_BYTE` uploads; an unmodeled format uploads no pixels
/// (returns empty — the texture stays data-less and is truthfully skipped at draw time).
unsafe fn to_rgba8(
    ctx: &GlContext,
    format: u32,
    type_: u32,
    w: i32,
    h: i32,
    pixels: *const c_void,
) -> Vec<u8> {
    // GLES3.0 3.8.3: `width`/`height` above GL_MAX_TEXTURE_SIZE is GL_INVALID_VALUE, so the record layer
    // will reject this upload. Refuse the extent HERE, before the client pointer is read: the source span
    // is `w * h * bpp`, and a guest passing 65536x65536 with a small buffer made the shim read ~17 GiB out
    // of bounds and segfault before the record layer ever saw the size.
    if w < 0 || h < 0 || w > query::MAX_TEXTURE_SIZE || h > query::MAX_TEXTURE_SIZE {
        hl_log::hl_count!(hl_log::tag::GL, "upload_extent_rejected");
        crate::stub::Diagnostics::upload(format, type_, crate::stub::UploadOutcome::Rejected);
        return Vec::new();
    }
    match (format, type_) {
        (GL_RED, GL_UNSIGNED_BYTE) => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_red_u8");
        }
        (GL_RG, GL_UNSIGNED_BYTE) => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_rg_u8");
        }
        (GL_RGB, GL_UNSIGNED_BYTE) => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_rgb_u8");
        }
        (GL_RGBA, GL_UNSIGNED_BYTE) => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_rgba_u8");
        }
        (GL_BGRA_EXT, GL_UNSIGNED_BYTE) => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_bgra_u8");
        }
        (GL_ALPHA | GL_LUMINANCE | GL_LUMINANCE_ALPHA, GL_UNSIGNED_BYTE) => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_legacy_u8");
        }
        (GL_RGB, GL_UNSIGNED_SHORT_5_6_5) => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_rgb_565");
        }
        _ => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_unsupported");
        }
    }
    let Some(upload) = Upload::new(format, type_, w, h, ctx.pixel_store_state()) else {
        hl_log::hl_count!(hl_log::tag::GL, "upload_layout_rejected");
        crate::stub::Diagnostics::upload(format, type_, crate::stub::UploadOutcome::Rejected);
        return Vec::new();
    };
    let span = upload.source_len();
    hl_log::hl_add!(hl_log::tag::GL, "upload_source_bytes", span as u64);
    let unpack = ctx.buffer_for_target(GL_PIXEL_UNPACK_BUFFER);
    let owned;
    let src = if unpack != 0 {
        let offset = pixels as usize;
        owned = match ctx.buffers.range_bytes(unpack, offset, span) {
            Some(data) if data.len() == span => data,
            _ => {
                hl_log::hl_count!(hl_log::tag::GL, "upload_pbo_rejected");
                crate::stub::Diagnostics::upload(
                    format,
                    type_,
                    crate::stub::UploadOutcome::Rejected,
                );
                return Vec::new();
            }
        };
        owned.as_slice()
    } else {
        if pixels.is_null() {
            hl_log::hl_count!(hl_log::tag::GL, "upload_null_storage");
            crate::stub::Diagnostics::upload(format, type_, crate::stub::UploadOutcome::Null);
            return Vec::new();
        }
        RawBytes::read(pixels, span as isize)
    };
    match upload.rgba8(src) {
        Some(rgba) => {
            hl_log::hl_add!(hl_log::tag::GL, "upload_rgba_bytes", rgba.len() as u64);
            crate::stub::Diagnostics::upload(format, type_, crate::stub::UploadOutcome::Converted);
            rgba
        }
        None => {
            hl_log::hl_count!(hl_log::tag::GL, "upload_decode_rejected");
            crate::stub::Diagnostics::upload(format, type_, crate::stub::UploadOutcome::Rejected);
            Vec::new()
        }
    }
}

/// Default window-surface dimensions (`$HL_GL_SURFACE_W` / `_H`; 1280x720 fallback). The native window
/// handle carries no size in this model, so the surface is sized from the environment.
fn default_surface_wh() -> (u32, u32) {
    let g = |k: &str, d: u32| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    (g("HL_GL_SURFACE_W", 1280), g("HL_GL_SURFACE_H", 720))
}

// ==================================================================================================
// EGL: display / initialization / query
// ==================================================================================================

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetError() -> i32 {
    GlobalState::access(|s| s.take_egl_error())
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetDisplay(display_id: *mut c_void) -> *mut c_void {
    crate::stub::trace("eglGetDisplay", "returning the hl display");
    // `display_id` is the app's OWN `wl_display*` — keep it. The app-surface presenter marshals on that
    // connection, and the only other way to reach it is `wl_proxy_get_display` (Wayland 1.23+, absent on
    // 24.04-era guests). `EGL_DEFAULT_DISPLAY` is null and stays legal: it simply teaches us nothing.
    GlobalState::access(|s| s.record_app_display(display_id as usize));
    DISPLAY_TOKEN as *mut c_void
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglInitialize(_dpy: *mut c_void, major: *mut i32, minor: *mut i32) -> u32 {
    crate::stub::trace("eglInitialize", "advertising EGL 1.4");
    hl_log::hl_info!(hl_log::tag::EGL, "eglInitialize egl=1.4");
    if GlobalState::access(|s| {
        s.initialize().map_err(|error| {
            hl_log::hl_error!(
                hl_log::tag::EGL,
                "eglInitialize could not open projected GPU endpoint: {}",
                error
            );
            s.set_egl_error(EGL_NOT_INITIALIZED);
        })
    })
    .is_err()
    {
        return EGL_FALSE;
    }
    // EGL 1.5 makes sync objects core. This driver cannot yet order an EGL fence after deferred
    // default-framebuffer commands without presenting them, so advertise the last version whose complete
    // core contract we implement. Platform entry points remain available through their KHR/EXT extensions.
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

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglTerminate(_dpy: *mut c_void) -> u32 {
    GlobalState::access(|state| state.terminate());
    EGL_TRUE
}

/// `eglQueryString(dpy, name)` — vendor / version / client-APIs / **extensions**. The extension string is
/// keyed on `dpy`: with `EGL_NO_DISPLAY` (null) it returns the CLIENT extensions (the platform-base +
/// `EGL_*_platform_wayland` set a toolkit probes before opening a display), otherwise the per-display set.
/// Advertising `EGL_EXT_platform_wayland` / `EGL_KHR_platform_wayland` is what makes a Wayland app take the
/// `eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, …)` window path instead of surfaceless/pbuffer.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglQueryString(dpy: *mut c_void, name: i32) -> *const c_char {
    crate::stub::trace(
        "eglQueryString",
        if dpy.is_null() {
            "client query"
        } else {
            "display query"
        },
    );
    match name {
        EGL_VENDOR => b"hl-gl\0".as_ptr() as *const c_char,
        EGL_VERSION_Q => b"1.4 hl-gl\0".as_ptr() as *const c_char,
        EGL_CLIENT_APIS => b"OpenGL_ES\0".as_ptr() as *const c_char,
        EGL_EXTENSIONS_Q => Extensions::get(dpy.is_null()),
        // EGL 1.4 §3.3: an unrecognized name is EGL_BAD_PARAMETER and returns NULL. The empty string told
        // the caller the query succeeded and the answer was "nothing".
        _ => {
            GlobalState::access(|state| state.set_egl_error(EGL_BAD_PARAMETER));
            core::ptr::null()
        }
    }
}

/// The NUL-terminated `EGL_EXTENSIONS` string (built once from [`hl_gl::adapter::wayland`], process-static
/// so the returned pointer is valid for the app's lifetime).
struct Extensions;

impl Extensions {
    fn get(client: bool) -> *const c_char {
        use std::ffi::CString;
        use std::sync::OnceLock;
        static CLIENT: OnceLock<CString> = OnceLock::new();
        static DISPLAY: OnceLock<CString> = OnceLock::new();
        let cell = if client { &CLIENT } else { &DISPLAY };
        cell.get_or_init(|| {
            let s = if client {
                hl_gl::adapter::wayland::egl_client_extensions()
            } else {
                hl_gl::adapter::wayland::egl_display_extensions()
            };
            CString::new(s).unwrap_or_default()
        })
        .as_ptr()
    }
}
