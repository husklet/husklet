/// Import a linear single-plane dma-buf as an independently owned `EGLImage`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreateImage(
    _dpy: *mut c_void,
    _ctx: *mut c_void,
    target: u32,
    _buffer: *mut c_void,
    attrib_list: *const isize,
) -> *mut c_void {
    GlobalState::access(|state| {
        let image = state
            .images
            .import(target, attrib_list, state.external_buffers_enabled())
            .unwrap_or(core::ptr::null_mut());
        if std::env::var_os("HL_SHIM_DEBUG").is_some() {
            eprintln!(
                "[hl-gl-shim] eglCreateImage target={target:#x} attributes_null={} result={:#x}",
                attrib_list.is_null(),
                image as usize
            );
        }
        image
    })
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroyImage(_dpy: *mut c_void, image: *mut c_void) -> u32 {
    GlobalState::access(|state| {
        if state.images.remove(image) {
            EGL_TRUE
        } else {
            EGL_FALSE
        }
    })
}

pub extern "C" fn eglCreateImageKHR(
    dpy: *mut c_void,
    ctx: *mut c_void,
    target: u32,
    buffer: *mut c_void,
    attrib_list: *const i32,
) -> *mut c_void {
    let _ = (dpy, ctx, buffer);
    GlobalState::access(|state| unsafe {
        let image = state
            .images
            .import_khr(target, attrib_list, state.external_buffers_enabled())
            .unwrap_or(core::ptr::null_mut());
        if std::env::var_os("HL_SHIM_DEBUG").is_some() {
            eprintln!(
                "[hl-gl-shim] eglCreateImageKHR target={target:#x} attributes_null={} result={:#x}",
                attrib_list.is_null(),
                image as usize
            );
        }
        image
    })
}

pub extern "C" fn eglDestroyImageKHR(dpy: *mut c_void, image: *mut c_void) -> u32 {
    eglDestroyImage(dpy, image)
}

/// The dma-buf formats this driver can import (`eglCreateImage` accepts exactly these fourccs).
///
/// `eglQueryDmaBufModifiersEXT` MUST answer every one of them: `EGL_EXT_image_dma_buf_import_modifiers`
/// defines its `EGL_BAD_PARAMETER` case as "<format> is not a supported format", so a format returned by
/// `eglQueryDmaBufFormatsEXT` and then rejected here is a contradiction that makes callers give up.
const SUPPORTED_FORMATS: [u32; 2] = [
    crate::image::DRM_FORMAT_ARGB8888,
    crate::image::DRM_FORMAT_XRGB8888,
];

/// The modifiers `eglCreateImage` will actually import, most preferred first.
///
/// Only two layouts are honestly importable: the Husklet-private modifier, whose plane fd is a memfd
/// carrying a token/serial header that the compositor joins against a host IOSurface, and `LINEAR` for a
/// plain CPU-visible allocation. The private modifier is only meaningful when native frame ingress was
/// negotiated (`external_buffers_enabled`); a headless session has no IOSurface path, so it advertises
/// `LINEAR` alone. Nothing tiled or vendor-specific exists behind this driver.
fn supported_modifiers(external: bool) -> &'static [u64] {
    const EXTERNAL: [u64; 2] = [
        hl_surface_protocol::buffer::MODIFIER,
        crate::image::DRM_FORMAT_MOD_LINEAR,
    ];
    const LINEAR: [u64; 1] = [crate::image::DRM_FORMAT_MOD_LINEAR];
    if external {
        &EXTERNAL
    } else {
        &LINEAR
    }
}

/// `eglQueryDmaBufFormatsEXT(dpy, max_formats, formats, num_formats)`.
///
/// Per `EGL_EXT_image_dma_buf_import_modifiers`: with `formats` NULL the call reports the total number of
/// supported formats in `num_formats` and writes nothing; otherwise it writes at most `max_formats` entries
/// and `num_formats` receives the number of entries actually written.
pub extern "C" fn eglQueryDmaBufFormatsEXT(
    _dpy: *mut c_void,
    max_formats: i32,
    formats: *mut i32,
    count: *mut i32,
) -> u32 {
    if count.is_null() || max_formats < 0 || (max_formats > 0 && formats.is_null()) {
        return EGL_FALSE;
    }
    let written = if formats.is_null() {
        SUPPORTED_FORMATS.len()
    } else {
        let written = SUPPORTED_FORMATS.len().min(max_formats as usize);
        for (index, format) in SUPPORTED_FORMATS.iter().take(written).enumerate() {
            // SAFETY: `formats` holds at least `max_formats >= written` `EGLint`s per the entry point's
            // contract; the index is bounded by `written`.
            unsafe { *formats.add(index) = *format as i32 };
        }
        written
    };
    // SAFETY: `count` was checked non-null above.
    unsafe { *count = written as i32 };
    EGL_TRUE
}

/// `eglQueryDmaBufModifiersEXT(dpy, format, max_modifiers, modifiers, external_only, num_modifiers)`.
///
/// Per `EGL_EXT_image_dma_buf_import_modifiers`: `modifiers` NULL means "report the count only";
/// `external_only` is OPTIONAL and may be NULL even when `modifiers` is not — Weston's
/// `simple-dmabuf-egl` passes exactly that, and rejecting it aborted the client before it allocated.
/// `num_modifiers` receives the number of entries written when `modifiers` is non-NULL.
pub extern "C" fn eglQueryDmaBufModifiersEXT(
    _dpy: *mut c_void,
    format: i32,
    max_modifiers: i32,
    modifiers: *mut u64,
    external_only: *mut u32,
    count: *mut i32,
) -> u32 {
    if count.is_null()
        || max_modifiers < 0
        || (max_modifiers > 0 && modifiers.is_null())
        || !SUPPORTED_FORMATS.contains(&(format as u32))
    {
        return EGL_FALSE;
    }
    let external = GlobalState::access(|state| state.external_buffers_enabled());
    let supported = supported_modifiers(external);
    let written = if modifiers.is_null() {
        supported.len()
    } else {
        let written = supported.len().min(max_modifiers as usize);
        for (index, modifier) in supported.iter().take(written).enumerate() {
            // SAFETY: both arrays hold at least `max_modifiers >= written` entries per the entry point's
            // contract; `external_only` is optional and only written when the caller supplied it.
            unsafe {
                *modifiers.add(index) = *modifier;
                if !external_only.is_null() {
                    // No modifier here is external-only: each imports as a sampler2D-capable image.
                    *external_only.add(index) = EGL_FALSE;
                }
            }
        }
        written
    };
    // SAFETY: `count` was checked non-null above.
    unsafe { *count = written as i32 };
    EGL_TRUE
}
use super::*;
