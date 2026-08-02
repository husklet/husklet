fn upload_fits(
    format: u32,
    type_: u32,
    width: i32,
    height: i32,
    buf_size: i32,
    pixels: *const c_void,
) -> bool {
    GlobalState::context(|state| {
        let Some(upload) = Upload::new(format, type_, width, height, state.gl.pixel_store_state())
        else {
            return false;
        };
        let unpack = state.gl.buffer_for_target(GL_PIXEL_UNPACK_BUFFER);
        let unpack_len = if unpack == 0 {
            None
        } else {
            state.gl.buffers.get(unpack).map(|buffer| buffer.data.len())
        };
        robust_upload_fits(upload, buf_size, pixels as usize, unpack_len)
    })
}

pub(super) fn robust_upload_fits(
    upload: Upload,
    buf_size: i32,
    pointer: usize,
    unpack_len: Option<usize>,
) -> bool {
    if buf_size < 0 {
        return false;
    }
    if let Some(unpack_len) = unpack_len {
        // ANGLE_robust_client_memory protects CLIENT memory. With a PBO, `pixels` is an offset
        // and the extension requires `bufSize == 0`; the GL buffer's own bounds protect the read.
        return buf_size == 0
            && pointer
                .checked_add(upload.source_len())
                .is_some_and(|end| end <= unpack_len);
    }
    if pointer == 0 {
        buf_size == 0
    } else {
        upload.source_len() <= buf_size as usize
    }
}

fn has_unpack_buffer() -> bool {
    GlobalState::context(|state| state.gl.buffer_for_target(GL_PIXEL_UNPACK_BUFFER) != 0)
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterfvRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    params: *const f32,
) {
    if params.is_null() || !writable(param_count, 1) {
        reject(core::ptr::null_mut());
        return;
    }
    glTexParameterfv(target, pname, params);
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterivRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    params: *const i32,
) {
    if params.is_null() || !writable(param_count, 1) {
        reject(core::ptr::null_mut());
        return;
    }
    glTexParameteriv(target, pname, params);
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterfvRobustANGLE(
    sampler: u32,
    pname: u32,
    param_count: i32,
    params: *const f32,
) {
    if params.is_null() || !writable(param_count, 1) {
        reject(core::ptr::null_mut());
        return;
    }
    glSamplerParameterfv(sampler, pname, params);
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterivRobustANGLE(
    sampler: u32,
    pname: u32,
    param_count: i32,
    params: *const i32,
) {
    if params.is_null() || !writable(param_count, 1) {
        reject(core::ptr::null_mut());
        return;
    }
    glSamplerParameteriv(sampler, pname, params);
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage2DRobustANGLE(
    target: u32,
    level: i32,
    internalformat: i32,
    width: i32,
    height: i32,
    border: i32,
    format: u32,
    type_: u32,
    buf_size: i32,
    pixels: *const c_void,
) {
    if !upload_fits(format, type_, width, height, buf_size, pixels) {
        reject(core::ptr::null_mut());
        return;
    }
    glTexImage2D(
        target,
        level,
        internalformat,
        width,
        height,
        border,
        format,
        type_,
        pixels,
    );
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage2DRobustANGLE(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    buf_size: i32,
    pixels: *const c_void,
) {
    if (pixels.is_null() && !has_unpack_buffer())
        || !upload_fits(format, type_, width, height, buf_size, pixels)
    {
        reject(core::ptr::null_mut());
        return;
    }
    glTexSubImage2D(
        target, level, xoffset, yoffset, width, height, format, type_, pixels,
    );
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage3DRobustANGLE(
    target: u32,
    level: i32,
    internalformat: i32,
    width: i32,
    height: i32,
    depth: i32,
    border: i32,
    format: u32,
    type_: u32,
    buf_size: i32,
    pixels: *const c_void,
) {
    if !upload_fits(format, type_, width, height, buf_size, pixels) {
        reject(core::ptr::null_mut());
        return;
    }
    glTexImage3D(
        target,
        level,
        internalformat,
        width,
        height,
        depth,
        border,
        format,
        type_,
        pixels,
    );
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage3DRobustANGLE(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    zoffset: i32,
    width: i32,
    height: i32,
    depth: i32,
    format: u32,
    type_: u32,
    buf_size: i32,
    pixels: *const c_void,
) {
    if (pixels.is_null() && !has_unpack_buffer())
        || !upload_fits(format, type_, width, height, buf_size, pixels)
    {
        reject(core::ptr::null_mut());
        return;
    }
    glTexSubImage3D(
        target, level, xoffset, yoffset, zoffset, width, height, depth, format, type_, pixels,
    );
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glReadPixelsRobustANGLE(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    buf_size: i32,
    length: *mut i32,
    columns: *mut i32,
    rows: *mut i32,
    pixels: *mut c_void,
) {
    #[cfg(feature = "verbose")]
    {
        let pbo = GlobalState::context(|state| state.gl.buffer_for_target(GL_PIXEL_PACK_BUFFER));
        hl_log::hl_error!(
            hl_log::tag::GL,
            "glReadPixelsRobustANGLE entry region={width}x{height} format={format:#x} \
             type={type_:#x} buf_size={buf_size} pixels={pixels:p} pbo={pbo}"
        );
    }
    let bpp = match (format, type_) {
        (GL_RGBA | GL_BGRA_EXT, GL_UNSIGNED_BYTE) => 4usize,
        (GL_RGB, GL_UNSIGNED_BYTE) => 3,
        _ => {
            reject(length);
            return;
        }
    };
    // `GL_PACK_ALIGNMENT` pads between rows, so the buffer the caller must supply is larger than the
    // tightly packed pixels whenever a row does not fill a whole number of alignment units.
    let required = usize::try_from(width)
        .ok()
        .zip(usize::try_from(height).ok())
        .and_then(|(width, height)| Some((width.checked_mul(bpp)?, height)))
        .map(|(row, height)| {
            GlobalState::context(|state| state.gl.pixel_store_state().pack_size(row, height))
        });
    let Some(required) = required else {
        reject(length);
        return;
    };
    let pbo = GlobalState::context(|state| state.gl.buffer_for_target(GL_PIXEL_PACK_BUFFER));
    if (pbo == 0 && (pixels.is_null() || buf_size < 0 || (buf_size as usize) < required))
        || (pbo != 0 && buf_size < 0)
    {
        reject(length);
        return;
    }
    glReadPixels(x, y, width, height, format, type_, pixels);
    unsafe {
        write_length(length, required);
        if !columns.is_null() {
            *columns = width;
        }
        if !rows.is_null() {
            *rows = height;
        }
    }
}
use super::*;
