/// Whether a compressed upload's extent may be allocated. GLES3.0 3.8.6: `width`/`height` above
/// GL_MAX_TEXTURE_SIZE is GL_INVALID_VALUE, and the `w*h*4` RGBA8 reservation these entry points make is
/// unbounded arithmetic on guest input — `glCompressedTexImage2D(…, i32::MAX, i32::MAX, …)` panicked the
/// allocator with a capacity overflow, which `panic = "abort"` turns into a dead driver.
fn allocatable_extent(width: i32, height: i32) -> bool {
    if width > query::MAX_TEXTURE_SIZE || height > query::MAX_TEXTURE_SIZE {
        GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_VALUE));
        return false;
    }
    true
}

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
    if !allocatable_extent(width, height) {
        return;
    }
    GlobalState::context(|group| {
        group.redefine_texture(|ctx| {
            let name = ctx.bound_texture();
            if name != 0 && width > 0 && height > 0 {
                ctx.textures.alloc_plane(name, width, height);
            }
        });
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
    if !allocatable_extent(width, height) {
        return;
    }
    GlobalState::context(|group| {
        let name = group.gl.bound_texture();
        if name != 0 && width > 0 && height > 0 {
            group.gl.textures.alloc_plane(name, width, height);
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
    let v = GlobalState::context(|group| query::get_buffer_parameteriv(&group.gl, target, pname));
    unsafe { *params = v };
}

/// `glGetBufferParameteri64v` — the 64-bit view of `glGetBufferParameteriv` (size/usage widened).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferParameteri64v(target: u32, pname: u32, params: *mut i64) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::context(|group| query::get_buffer_parameteriv(&group.gl, target, pname));
    unsafe { *params = v as i64 };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::context(|group| query::get_tex_parameteriv(&group.gl, target, pname));
    unsafe { *params = v };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameterfv(target: u32, pname: u32, params: *mut f32) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::context(|group| query::get_tex_parameteriv(&group.gl, target, pname));
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
    let v = GlobalState::context(|group| query::get_tex_parameteriv(&group.gl, target, pname));
    unsafe { *params = v as u32 };
}

/// `glGetVertexAttrib{f,i}v(index, pname)` — the vertex attribute ARRAY state (`glVertexAttribPointer`'s
/// size/type/stride/normalized/binding, the `glEnableVertexAttribArray` flag, and the
/// `glVertexAttribDivisor` rate) plus `GL_CURRENT_VERTEX_ATTRIB`. An out-of-range index or an untracked
/// `pname` raises the spec error and leaves `params` untouched, rather than writing a `0` an app would
/// then restore as real state.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribfv(index: u32, pname: u32, params: *mut f32) {
    if params.is_null() {
        return;
    }
    GlobalState::context(|s| {
        if pname == GL_CURRENT_VERTEX_ATTRIB {
            match query::get_current_vertex_attrib(&s.gl, index) {
                // SAFETY: GL_CURRENT_VERTEX_ATTRIB writes exactly four components; `params` is non-null.
                Some(v) => unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), params, 4) },
                None => s.gl.set_gl_error(GL_INVALID_VALUE),
            }
            return;
        }
        match query::get_vertex_attrib(&s.gl, index, pname) {
            // SAFETY: every remaining pname is a single value and `params` is non-null.
            Some(v) => unsafe { *params = v as f32 },
            None => s.gl.set_gl_error(attrib_query_error(&s.gl, index)),
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribiv(index: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    GlobalState::context(|s| {
        if pname == GL_CURRENT_VERTEX_ATTRIB {
            match query::get_current_vertex_attrib(&s.gl, index) {
                // SAFETY: four components into a non-null `params`.
                Some(v) => unsafe {
                    for (slot, value) in v.iter().enumerate() {
                        *params.add(slot) = *value as i32;
                    }
                },
                None => s.gl.set_gl_error(GL_INVALID_VALUE),
            }
            return;
        }
        match query::get_vertex_attrib(&s.gl, index, pname) {
            // SAFETY: a single value into a non-null `params`.
            Some(v) => unsafe { *params = v },
            None => s.gl.set_gl_error(attrib_query_error(&s.gl, index)),
        }
    });
}

/// `GL_INVALID_VALUE` when the attribute index is out of range, `GL_INVALID_ENUM` for an unrecognized
/// `pname` at a valid index — the split ES 3.0 §6.1.10 requires.
fn attrib_query_error(ctx: &GlContext, index: u32) -> u32 {
    if index as usize >= ctx.attributes().len() {
        GL_INVALID_VALUE
    } else {
        GL_INVALID_ENUM
    }
}

/// `glGetVertexAttribIiv`/`Iuiv` — the integer forms of the same array state.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribIiv(index: u32, pname: u32, params: *mut i32) {
    glGetVertexAttribiv(index, pname, params);
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribIuiv(index: u32, pname: u32, params: *mut u32) {
    if params.is_null() {
        return;
    }
    let mut value = 0i32;
    glGetVertexAttribiv(index, pname, &mut value);
    // SAFETY: `params` is non-null and holds one value for every pname this form accepts.
    unsafe { *params = value as u32 };
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
    GlobalState::context(|group| {
        record::copy_buffer_sub_data(
            &mut group.gl,
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
    let fail = |e: u32| GlobalState::context(|s| s.gl.set_gl_error(e));
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
    let row = width as usize * bpp;
    // The buffer has to hold the alignment padding between rows as well as the pixels; checking the
    // tightly packed size would accept a buffer the aligned write then runs past.
    let need = GlobalState::context(|s| s.gl.pixel_store_state().pack_size(row, height as usize));
    if (buf_size as usize) < need {
        fail(GL_INVALID_OPERATION);
        return;
    }
    let packed = gpu_read_pixels(
        x,
        y,
        width,
        height,
        readpixels::PixelFormat::new(format, GL_UNSIGNED_BYTE),
    );
    match packed {
        Ok(bytes) => {
            #[cfg(feature = "verbose")]
            {
                let pbo = GlobalState::context(|s| s.gl.buffer_for_target(GL_PIXEL_PACK_BUFFER));
                let head = bytes
                    .iter()
                    .take(16)
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                hl_log::hl_error!(
                    hl_log::tag::GL,
                    "glReadnPixels destination={data:p} pbo={pbo} region={width}x{height} \
                     buf_size={buf_size} need={need} returned={} head=[{head}]",
                    bytes.len()
                );
            }
            unsafe { crate::driver::drawing::write_packed_rows(&bytes, row, height as usize, data) }
        }
        Err(e) => {
            GlobalState::context(|s| s.gl.set_gl_error(GL_OUT_OF_MEMORY));
            GlobalState::access(|s| s.set_egl_error(egl_error_from_gpu_error(&e)));
        }
    }
}

// ==================================================================================================
// ES3 vertex-attribute constants + integer attribute pointers
// ==================================================================================================
//
// The shim sources vertex attributes from bound arrays (`glVertexAttribPointer`), so a CONSTANT generic
// attribute (`glVertexAttrib*f`) has no array slot to feed and is an honest no-op (matches the reference
use super::*;
