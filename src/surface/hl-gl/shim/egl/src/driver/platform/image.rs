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

pub extern "C" fn eglQueryDmaBufFormatsEXT(
    _dpy: *mut c_void,
    max_formats: i32,
    formats: *mut i32,
    count: *mut i32,
) -> u32 {
    if count.is_null() || max_formats < 0 || (max_formats > 0 && formats.is_null()) {
        return EGL_FALSE;
    }
    let supported = [
        crate::image::DRM_FORMAT_ARGB8888 as i32,
        crate::image::DRM_FORMAT_XRGB8888 as i32,
    ];
    unsafe {
        *count = supported.len() as i32;
        for (index, format) in supported.iter().take(max_formats as usize).enumerate() {
            *formats.add(index) = *format;
        }
    }
    EGL_TRUE
}

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
        || (max_modifiers > 0 && (modifiers.is_null() || external_only.is_null()))
        || !matches!(
            format as u32,
            crate::image::DRM_FORMAT_ARGB8888 | crate::image::DRM_FORMAT_XRGB8888
        )
    {
        return EGL_FALSE;
    }
    let external = GlobalState::access(|state| state.external_buffers_enabled());
    let supported = if external { 2 } else { 1 };
    unsafe {
        *count = supported;
        if max_modifiers > 0 {
            *modifiers = if external {
                hl_surface_protocol::buffer::MODIFIER
            } else {
                crate::image::DRM_FORMAT_MOD_LINEAR
            };
            *external_only = EGL_FALSE;
        }
        if external && max_modifiers > 1 {
            *modifiers.add(1) = crate::image::DRM_FORMAT_MOD_LINEAR;
            *external_only.add(1) = EGL_FALSE;
        }
    }
    EGL_TRUE
}
use super::*;
