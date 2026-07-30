use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetError() -> u32 {
    let error = GlobalState::context(|s| s.gl.take_gl_error());
    crate::stub::trace("glGetError", &format!("returning 0x{error:x}"));
    error
}

/// `glGetString(name)` — the driver's GLES3 identity strings (`GL_VERSION` = "OpenGL ES 3.0 …", vendor /
/// renderer / GLSL version / extensions). Served from [`query::gl_string`] so the guest-visible identity
/// is defined once and unit-tested. Never null: a GLES app dereferences the result unconditionally.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetString(name: u32) -> *const u8 {
    let version = GlobalState::context(|s| (s.gl.client_version().0, s.gl.client_version().1));
    let value = match (name, version) {
        (GL_VERSION, (2, 0)) => b"OpenGL ES 2.0 hl-gl\0".as_slice(),
        (GL_VERSION, (3, 0)) => b"OpenGL ES 3.0 hl-gl\0".as_slice(),
        (GL_VERSION, (3, 1)) => b"OpenGL ES 3.1 hl-gl\0".as_slice(),
        (GL_SHADING_LANGUAGE_VERSION, (2, 0)) => b"OpenGL ES GLSL ES 1.00\0".as_slice(),
        (GL_SHADING_LANGUAGE_VERSION, (3, 0)) => b"OpenGL ES GLSL ES 3.00\0".as_slice(),
        (GL_SHADING_LANGUAGE_VERSION, (3, 1)) => b"OpenGL ES GLSL ES 3.10\0".as_slice(),
        _ => query::gl_string(name),
    };
    crate::stub::trace(
        "glGetString",
        &format!("pname=0x{name:04x} pointer={:p}", value.as_ptr()),
    );
    crate::stub::Diagnostics::query(
        crate::stub::QueryKind::String,
        name,
        &String::from_utf8_lossy(value),
    );
    value.as_ptr()
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
            GlobalState::context(|s| s.gl.set_gl_error(e));
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
    crate::stub::trace("glGetIntegerv", "querying GLES state");
    if data.is_null() {
        return;
    }
    let mut buf = [0i32; 4];
    let n = GlobalState::context(|s| query::get_integerv(&s.gl, pname, &mut buf));
    crate::stub::Diagnostics::query(
        crate::stub::QueryKind::Integer,
        pname,
        &format!("{:?}", &buf[..n]),
    );
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
    let n = GlobalState::context(|s| query::get_floatv(&s.gl, pname, &mut buf));
    crate::stub::Diagnostics::query(
        crate::stub::QueryKind::Float,
        pname,
        &format!("{:?}", &buf[..n]),
    );
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
    let n = GlobalState::context(|s| query::get_booleanv(&s.gl, pname, &mut buf));
    crate::stub::Diagnostics::query(
        crate::stub::QueryKind::Boolean,
        pname,
        &format!("{:?}", &buf[..n]),
    );
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
    GlobalState::context(|s| record::pixel_store(&mut s.gl, pname, param));
}

// ==================================================================================================
// GLES: buffers
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenBuffers(n: i32, buffers: *mut u32) {
    if buffers.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            *buffers.offset(i) = s.gl.buffers.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindBuffer(target: u32, buffer: u32) {
    crate::stub::trace(
        "glBindBuffer",
        &format!("target={target:#x} buffer={buffer}"),
    );
    GlobalState::context(|s| record::bind_buffer(&mut s.gl, target, buffer));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32) {
    crate::stub::trace(
        "glBufferData",
        &format!(
            "target={target:#x} size={size} data_null={} usage={usage:#x}",
            data.is_null()
        ),
    );
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
    GlobalState::context(|s| record::buffer_data(&mut s.gl, target, &d, usage));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBufferSubData(target: u32, offset: isize, size: isize, data: *const c_void) {
    crate::stub::trace(
        "glBufferSubData",
        &format!("target={target:#x} offset={offset} size={size}"),
    );
    let d = unsafe { RawBytes::read(data, size) }.to_vec();
    GlobalState::context(|s| {
        record::buffer_sub_data(&mut s.gl, target, offset.max(0) as usize, &d)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteBuffers(n: i32, buffers: *const u32) {
    if buffers.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            s.delete_buffer(*buffers.offset(i));
        }
    });
}

// ==================================================================================================
// GLES: textures
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenTextures(n: i32, textures: *mut u32) {
    crate::stub::trace("glGenTextures", "allocating texture names");
    if textures.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            *textures.offset(i) = s.gl.textures.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glActiveTexture(texture: u32) {
    GlobalState::context(|s| s.gl.active_texture(texture));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindTexture(target: u32, texture: u32) {
    GlobalState::context(|s| record::bind_texture(&mut s.gl, target, texture));
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
    crate::stub::trace("glTexImage2D", "uploading a 2D texture");
    GlobalState::context(|s| {
        let rgba = unsafe { to_rgba8(&s.gl, format, type_, width, height, pixels) };
        s.redefine_texture(|ctx| record::tex_image_2d(ctx, width, height, &rgba))
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameteri(_target: u32, pname: u32, param: i32) {
    GlobalState::context(|s| record::tex_parameter(&mut s.gl, pname, param as u32));
}

/// `glTexParameterf(target, pname, param)` — the float-typed setter. GL's texture filter/wrap parameters
/// are enum-valued; the app passes the enum as a float, so it is truncated back to the `GLenum` the
/// integer path records (`glTexParameteri` parity).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterf(_target: u32, pname: u32, param: f32) {
    GlobalState::context(|s| record::tex_parameter(&mut s.gl, pname, param as u32));
}

/// `glTexParameterfv(target, pname, params)` — vector form. `GL_TEXTURE_SWIZZLE_RGBA` consumes four
/// components; all scalar parameters consume the first.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterfv(_target: u32, pname: u32, params: *const f32) {
    if params.is_null() {
        return;
    }
    let count = if pname == GL_TEXTURE_SWIZZLE_RGBA {
        4
    } else {
        1
    };
    let values = unsafe { std::slice::from_raw_parts(params, count) }
        .iter()
        .map(|value| *value as u32)
        .collect::<Vec<_>>();
    GlobalState::context(|s| record::tex_parameter_vector(&mut s.gl, pname, &values));
}

/// `glTexParameteriv(target, pname, params)` — vector form.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameteriv(_target: u32, pname: u32, params: *const i32) {
    if params.is_null() {
        return;
    }
    let count = if pname == GL_TEXTURE_SWIZZLE_RGBA {
        4
    } else {
        1
    };
    let values = unsafe { std::slice::from_raw_parts(params, count) }
        .iter()
        .map(|value| *value as u32)
        .collect::<Vec<_>>();
    GlobalState::context(|s| record::tex_parameter_vector(&mut s.gl, pname, &values));
}

/// `glGenerateMipmap(target)` — validate + record (an honest no-op on the pixel data; this model samples
/// the base level only). See [`record::generate_mipmap`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenerateMipmap(target: u32) {
    GlobalState::context(|s| s.gl.generate_mipmap(target));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteTextures(n: i32, textures: *const u32) {
    crate::stub::trace("glDeleteTextures", "deleting texture names");
    if textures.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            s.delete_texture(*textures.offset(i));
        }
    });
}
