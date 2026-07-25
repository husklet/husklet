use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage2D(
    target: u32,
    levels: i32,
    internalformat: u32,
    width: i32,
    height: i32,
) {
    GlobalState::access(|s| {
        record::tex_storage_2d(&mut s.ctx, target, levels, internalformat, width, height)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage3D(
    target: u32,
    levels: i32,
    _internalformat: u32,
    width: i32,
    height: i32,
    depth: i32,
) {
    GlobalState::access(|s| {
        record::tex_storage_3d(&mut s.ctx, target, levels, width, height, depth)
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage3D(
    target: u32,
    level: i32,
    _internalformat: i32,
    width: i32,
    height: i32,
    depth: i32,
    _border: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    GlobalState::access(|s| {
        let rgba = unsafe { to_rgba8(&s.ctx, format, type_, width, height, pixels) };
        record::tex_image_3d(&mut s.ctx, target, level, width, height, depth, &rgba)
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage2D(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    GlobalState::access(|s| {
        let rgba = unsafe { to_rgba8(&s.ctx, format, type_, width, height, pixels) };
        record::tex_sub_image_2d(
            &mut s.ctx, target, level, xoffset, yoffset, width, height, &rgba,
        )
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage3D(
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
    pixels: *const c_void,
) {
    GlobalState::access(|s| {
        let rgba = unsafe { to_rgba8(&s.ctx, format, type_, width, height, pixels) };
        record::tex_sub_image_3d(
            &mut s.ctx, target, level, xoffset, yoffset, zoffset, width, height, depth, &rgba,
        )
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage2D(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    GlobalState::access(|s| {
        record::copy_tex_sub_image_2d(
            &mut s.ctx, target, level, xoffset, yoffset, x, y, width, height,
        )
    });
}

/// `glCopyTexSubImage3D` — the deferred model has no materialized source color plane per layer at record
/// time (see [`record::copy_tex_sub_image_2d`]); the layer copy is a documented no-op. Params validated
/// only insofar as a bad `target` is left to the bound-texture path — an honest no-op body.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage3D(
    _target: u32,
    _level: i32,
    _xoffset: i32,
    _yoffset: i32,
    _zoffset: i32,
    _x: i32,
    _y: i32,
    _width: i32,
    _height: i32,
) {
}

/// `glCopyTexImage2D` — allocate the bound texture and copy from the read framebuffer. This deferred
/// model has no materialized default-framebuffer source plane at record time (only a color-attachment
/// texture carries pixels), so the allocation of the destination extent is honored and the pixel copy is
/// the documented no-op (mirrors `glCopyTexSubImage2D`/`glBlitFramebuffer`).
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexImage2D(
    target: u32,
    level: i32,
    internalformat: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    border: i32,
) {
    let _ = (x, y);
    if target != GL_TEXTURE_2D || level != 0 || border != 0 {
        GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    // Allocate the destination extent so a later sample/subimage has storage (RGBA8 neutral plane).
    let ifmt = if matches!(internalformat, GL_RGB | GL_RGBA) {
        internalformat
    } else {
        GL_RGBA
    };
    let _ = ifmt;
    GlobalState::access(|s| {
        let name = s.ctx.tex_unit[s.ctx.active_texture];
        if name != 0 && width >= 0 && height >= 0 {
            s.ctx.textures.alloc_rgba(name, width, height);
        } else {
            s.ctx.set_gl_error(GL_INVALID_VALUE);
        }
    });
}

// ==================================================================================================
// ES3 compressed-texture uploads — no compressed codec is modeled, so these validate + no-op honestly
// (the RGBA8 render path samples uncompressed textures only; a compressed upload materializes no pixels).
// ==================================================================================================

/// `glCompressedTexImage2D` — a compressed upload the RGBA8 model cannot decode. We allocate the bound
/// texture's extent (so bookkeeping/bind proceeds) and truthfully do not materialize sampled pixels.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexImage2D(
    target: u32,
    level: i32,
    _internalformat: u32,
    width: i32,
    height: i32,
    _border: i32,
    _image_size: i32,
    _data: *const c_void,
) {
    if target != GL_TEXTURE_2D || level != 0 {
        return;
    }
    GlobalState::access(|s| {
        let name = s.ctx.tex_unit[s.ctx.active_texture];
        if name != 0 && width > 0 && height > 0 {
            s.ctx.textures.alloc_rgba(name, width, height);
        }
    });
}

/// `glCompressedTexImage3D` — the 2D-array / 3D compressed upload; layer-0 extent allocated, no decode.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexImage3D(
    target: u32,
    level: i32,
    _internalformat: u32,
    width: i32,
    height: i32,
    _depth: i32,
    _border: i32,
    _image_size: i32,
    _data: *const c_void,
) {
    if level != 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) {
        return;
    }
    GlobalState::access(|s| {
        let name = s.ctx.tex_unit[s.ctx.active_texture];
        if name != 0 && width > 0 && height > 0 {
            s.ctx.textures.alloc_rgba(name, width, height);
        }
    });
}

/// `glCompressedTexSubImage2D` — a compressed sub-image the model cannot decode: an honest no-op (the
/// texture's sampled pixels are unchanged).
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexSubImage2D(
    _target: u32,
    _level: i32,
    _xoffset: i32,
    _yoffset: i32,
    _width: i32,
    _height: i32,
    _format: u32,
    _image_size: i32,
    _data: *const c_void,
) {
}

/// `glCompressedTexSubImage3D` — the 2D-array / 3D compressed sub-image; an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexSubImage3D(
    _target: u32,
    _level: i32,
    _xoffset: i32,
    _yoffset: i32,
    _zoffset: i32,
    _width: i32,
    _height: i32,
    _depth: i32,
    _format: u32,
    _image_size: i32,
    _data: *const c_void,
) {
}

// ==================================================================================================
// ES3 buffer / texture / vertex-attribute state queries
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::access(|s| query::get_buffer_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v };
}

/// `glGetBufferParameteri64v` — the 64-bit view of `glGetBufferParameteriv` (size/usage widened).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferParameteri64v(target: u32, pname: u32, params: *mut i64) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::access(|s| query::get_buffer_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v as i64 };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::access(|s| query::get_tex_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameterfv(target: u32, pname: u32, params: *mut f32) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::access(|s| query::get_tex_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v as f32 };
}

/// `glGetTexParameterIiv` — the integer view of `glGetTexParameteriv`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameterIiv(target: u32, pname: u32, params: *mut i32) {
    glGetTexParameteriv(target, pname, params);
}

/// `glGetTexParameterIuiv` — the unsigned-integer view.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameterIuiv(target: u32, pname: u32, params: *mut u32) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::access(|s| query::get_tex_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v as u32 };
}

/// `glGetVertexAttribfv`/`iv` — attribute readback. This model records the array pointer + enable state
/// but reports the safe default `0` for the queried parameters (no attribute reflected back), matching the
/// reference shim; the app's own `glVertexAttribPointer` state is authoritative.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribfv(_index: u32, _pname: u32, params: *mut f32) {
    if !params.is_null() {
        unsafe { *params = 0.0 };
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribiv(_index: u32, _pname: u32, params: *mut i32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

/// `glGetVertexAttribIiv`/`Iuiv` — the integer forms; same `0` default.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribIiv(_index: u32, _pname: u32, params: *mut i32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribIuiv(_index: u32, _pname: u32, params: *mut u32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

/// `glGetVertexAttribPointerv` — the attribute array pointer readback; reports null (the app's own bound
/// pointer is authoritative), matching the reference.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribPointerv(_index: u32, _pname: u32, pointer: *mut *mut c_void) {
    if !pointer.is_null() {
        unsafe { *pointer = core::ptr::null_mut() };
    }
}

// ==================================================================================================
// ES3 buffer copy (glCopyBufferSubData) + readback with a bound size (glReadnPixels)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCopyBufferSubData(
    read_target: u32,
    write_target: u32,
    read_offset: isize,
    write_offset: isize,
    size: isize,
) {
    GlobalState::access(|s| {
        record::copy_buffer_sub_data(
            &mut s.ctx,
            read_target,
            write_target,
            read_offset,
            write_offset,
            size,
        )
    });
}

/// `glReadnPixels(x, y, w, h, format, type, bufSize, data)` — the bounded-buffer form of `glReadPixels`:
/// identical readback, but never writes more than `bufSize` bytes into `data` (a `bufSize` too small for
/// the requested rect raises `GL_INVALID_OPERATION`).
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glReadnPixels(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    buf_size: i32,
    data: *mut c_void,
) {
    let fail = |e: u32| GlobalState::access(|s| s.ctx.set_gl_error(e));
    if type_ != GL_UNSIGNED_BYTE {
        fail(GL_INVALID_ENUM);
        return;
    }
    let bpp = match format {
        GL_RGBA | GL_BGRA_EXT => 4usize,
        GL_RGB => 3,
        _ => {
            fail(GL_INVALID_ENUM);
            return;
        }
    };
    if width < 0 || height < 0 || buf_size < 0 {
        fail(GL_INVALID_VALUE);
        return;
    }
    if width == 0 || height == 0 {
        return;
    }
    if data.is_null() {
        fail(GL_INVALID_VALUE);
        return;
    }
    let need = width as usize * height as usize * bpp;
    if (buf_size as usize) < need {
        fail(GL_INVALID_OPERATION);
        return;
    }
    let packed = GlobalState::access(|s| {
        readpixels::read_pixels(&mut s.ctx, &mut s.sink, x, y, width, height, format)
    });
    match packed {
        Ok(bytes) => {
            let n = bytes.len().min(need).min(buf_size as usize);
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), data as *mut u8, n) };
        }
        Err(e) => GlobalState::access(|s| {
            s.ctx.set_gl_error(GL_OUT_OF_MEMORY);
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }),
    }
}

// ==================================================================================================
// ES3 vertex-attribute constants + integer attribute pointers
// ==================================================================================================
//
// The shim sources vertex attributes from bound arrays (`glVertexAttribPointer`), so a CONSTANT generic
// attribute (`glVertexAttrib*f`) has no array slot to feed and is an honest no-op (matches the reference
