use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetError() -> u32 {
    GlobalState::access(|s| s.ctx.take_gl_error())
}

/// `glGetString(name)` — the driver's GLES3 identity strings (`GL_VERSION` = "OpenGL ES 3.0 …", vendor /
/// renderer / GLSL version / extensions). Served from [`query::gl_string`] so the guest-visible identity
/// is defined once and unit-tested. Never null: a GLES app dereferences the result unconditionally.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetString(name: u32) -> *const u8 {
    query::gl_string(name).as_ptr()
}

/// `glGetStringi(name, index)` — the ES3 indexed extension enumeration. Served from the SAME inventory as
/// `glGetString(GL_EXTENSIONS)` + `glGetIntegerv(GL_NUM_EXTENSIONS)` (see [`query::string_i`]), so the
/// three never disagree. An out-of-range index raises `GL_INVALID_VALUE` and returns null (never a
/// dangling pointer); a non-`GL_EXTENSIONS` name raises `GL_INVALID_ENUM`. With no extensions advertised
/// an app that honors the count of `0` never calls this.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetStringi(name: u32, index: u32) -> *const u8 {
    match query::string_i(name, index) {
        Some(bytes) => bytes.as_ptr(),
        None => {
            let e = if name == GL_EXTENSIONS {
                GL_INVALID_VALUE
            } else {
                GL_INVALID_ENUM
            };
            GlobalState::access(|s| s.ctx.set_gl_error(e));
            core::ptr::null()
        }
    }
}

// ==================================================================================================
// GLES: state / capability queries (glGet*) — read-only; served from the modeled limits + live state
// ==================================================================================================

/// `glGetIntegerv(pname, data)` — capability limits + bound-object / fixed-function state. Writes the
/// modeled value(s) for `pname` (1, 2, or 4 ints); a null `data` or unknown `pname` is handled by
/// [`query::get_integerv`] (unknown → a single `0`).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetIntegerv(pname: u32, data: *mut i32) {
    if data.is_null() {
        return;
    }
    let mut buf = [0i32; 4];
    let n = GlobalState::access(|s| query::get_integerv(&s.ctx, pname, &mut buf));
    unsafe {
        for i in 0..n {
            *data.add(i) = buf[i];
        }
    }
}

/// `glGetFloatv(pname, data)` — the float-typed state (clear color, depth-clear value, line width, …).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetFloatv(pname: u32, data: *mut f32) {
    if data.is_null() {
        return;
    }
    let mut buf = [0f32; 4];
    let n = GlobalState::access(|s| query::get_floatv(&s.ctx, pname, &mut buf));
    unsafe {
        for i in 0..n {
            *data.add(i) = buf[i];
        }
    }
}

/// `glGetBooleanv(pname, data)` — the boolean-typed state (fixed-function enables + depth write mask).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBooleanv(pname: u32, data: *mut u8) {
    if data.is_null() {
        return;
    }
    let mut buf = [0u8; 4];
    let n = GlobalState::access(|s| query::get_booleanv(&s.ctx, pname, &mut buf));
    unsafe {
        for i in 0..n {
            *data.add(i) = buf[i];
        }
    }
}

/// `glPixelStorei(pname, param)` — record a pack/unpack pixel-store parameter (e.g. `GL_UNPACK_ALIGNMENT`,
/// which affects texture-upload row packing). An out-of-range value raises `GL_INVALID_VALUE` (first-error
/// wins); see [`record::pixel_store`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPixelStorei(pname: u32, param: i32) {
    GlobalState::access(|s| record::pixel_store(&mut s.ctx, pname, param));
}

// ==================================================================================================
// GLES: buffers
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenBuffers(n: i32, buffers: *mut u32) {
    if buffers.is_null() || n <= 0 {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            *buffers.offset(i) = s.ctx.buffers.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindBuffer(target: u32, buffer: u32) {
    GlobalState::access(|s| record::bind_buffer(&mut s.ctx, target, buffer));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32) {
    // `glBufferData(target, size, NULL, usage)` RESERVES `size` bytes of (initially undefined) storage —
    // it does NOT leave the buffer empty. Model the NULL-data allocation as `size` zeroed bytes so a later
    // `glBufferSubData` / `glMapBufferRange` write lands WITHIN bounds. Without this a NULL-allocated-then-
    // filled buffer (Chrome/ANGLE's dynamic vertex path: `glBufferData(size, NULL)` then map/subData) kept
    // ZERO storage, its fill writes were rejected out-of-range (`GL_INVALID_VALUE`), so its `has_data()`
    // stayed false — and a draw whose vertex shader reads that VBO's attributes then lowered a render
    // pipeline that DECLARES vertex buffer 0 while binding none, which wgpu rejects at pass validation
    // (`MissingVertexBuffer`). Real data (`data != NULL`) is copied verbatim as before.
    let d = if data.is_null() && size > 0 {
        vec![0u8; size as usize]
    } else {
        unsafe { RawBytes::read(data, size) }.to_vec()
    };
    GlobalState::access(|s| record::buffer_data(&mut s.ctx, target, &d, usage));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBufferSubData(target: u32, offset: isize, size: isize, data: *const c_void) {
    let d = unsafe { RawBytes::read(data, size) }.to_vec();
    GlobalState::access(|s| {
        record::buffer_sub_data(&mut s.ctx, target, offset.max(0) as usize, &d)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteBuffers(n: i32, buffers: *const u32) {
    if buffers.is_null() || n <= 0 {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            s.ctx.delete_buffer(*buffers.offset(i));
        }
    });
}

// ==================================================================================================
// GLES: textures
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenTextures(n: i32, textures: *mut u32) {
    if textures.is_null() || n <= 0 {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            *textures.offset(i) = s.ctx.textures.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glActiveTexture(texture: u32) {
    GlobalState::access(|s| s.ctx.active_texture(texture));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindTexture(target: u32, texture: u32) {
    GlobalState::access(|s| record::bind_texture(&mut s.ctx, target, texture));
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage2D(
    _target: u32,
    _level: i32,
    _internalformat: i32,
    width: i32,
    height: i32,
    _border: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    GlobalState::access(|s| {
        let rgba = unsafe { to_rgba8(&s.ctx, format, type_, width, height, pixels) };
        record::tex_image_2d(&mut s.ctx, width, height, &rgba)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameteri(_target: u32, pname: u32, param: i32) {
    GlobalState::access(|s| record::tex_parameter(&mut s.ctx, pname, param as u32));
}

/// `glTexParameterf(target, pname, param)` — the float-typed setter. GL's texture filter/wrap parameters
/// are enum-valued; the app passes the enum as a float, so it is truncated back to the `GLenum` the
/// integer path records (`glTexParameteri` parity).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterf(_target: u32, pname: u32, param: f32) {
    GlobalState::access(|s| record::tex_parameter(&mut s.ctx, pname, param as u32));
}

/// `glTexParameterfv(target, pname, params)` — the single-element vector form; reads `params[0]`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterfv(_target: u32, pname: u32, params: *const f32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    GlobalState::access(|s| record::tex_parameter(&mut s.ctx, pname, v as u32));
}

/// `glTexParameteriv(target, pname, params)` — the single-element integer vector form; reads `params[0]`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameteriv(_target: u32, pname: u32, params: *const i32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    GlobalState::access(|s| record::tex_parameter(&mut s.ctx, pname, v as u32));
}

/// `glGenerateMipmap(target)` — validate + record (an honest no-op on the pixel data; this model samples
/// the base level only). See [`record::generate_mipmap`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenerateMipmap(target: u32) {
    GlobalState::access(|s| s.ctx.generate_mipmap(target));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteTextures(n: i32, textures: *const u32) {
    if textures.is_null() || n <= 0 {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            s.ctx.delete_texture(*textures.offset(i));
        }
    });
}
