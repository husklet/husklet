#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetInternalformativRobustANGLE(
    target: u32,
    internalformat: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetInternalformativ(target, internalformat, pname, 1, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetMultisamplefvRobustANGLE(
    pname: u32,
    index: u32,
    param_count: i32,
    length: *mut i32,
    values: *mut f32,
) {
    const REQUIRED: usize = 2;
    if values.is_null() || !writable(param_count, REQUIRED) {
        reject(length);
        return;
    }
    glGetMultisamplefv(pname, index, values);
    unsafe { write_length(length, REQUIRED) };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryivRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetQueryiv(target, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryObjectuivRobustANGLE(
    id: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut u32,
) {
    scalar_u32(param_count, length, params, |out| {
        glGetQueryObjectuiv(id, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryObjectivRobustANGLE(
    id: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        let mut value = 0;
        glGetQueryObjectuiv(id, pname, &mut value);
        unsafe { *out = value as i32 };
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryObjecti64vRobustANGLE(
    id: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i64,
) {
    scalar_i64(param_count, length, params, |out| {
        let mut value = 0;
        glGetQueryObjectuiv(id, pname, &mut value);
        unsafe { *out = i64::from(value) };
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryObjectui64vRobustANGLE(
    id: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut u64,
) {
    scalar_u64(param_count, length, params, |out| {
        let mut value = 0;
        glGetQueryObjectuiv(id, pname, &mut value);
        unsafe { *out = u64::from(value) };
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameterivRobustANGLE(
    sampler: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetSamplerParameteriv(sampler, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameterfvRobustANGLE(
    sampler: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut f32,
) {
    scalar_f32(param_count, length, params, |out| {
        glGetSamplerParameterfv(sampler, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexLevelParameterivRobustANGLE(
    target: u32,
    level: i32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetTexLevelParameteriv(target, level, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexLevelParameterfvRobustANGLE(
    target: u32,
    level: i32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut f32,
) {
    scalar_f32(param_count, length, params, |out| {
        glGetTexLevelParameterfv(target, level, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribfvRobustANGLE(
    index: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut f32,
) {
    scalar_f32(param_count, length, params, |out| {
        glGetVertexAttribfv(index, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribivRobustANGLE(
    index: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetVertexAttribiv(index, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribIivRobustANGLE(
    index: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetVertexAttribIiv(index, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribIuivRobustANGLE(
    index: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut u32,
) {
    scalar_u32(param_count, length, params, |out| {
        glGetVertexAttribIuiv(index, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribPointervRobustANGLE(
    index: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    pointer: *mut *mut c_void,
) {
    scalar_pointer(param_count, length, pointer, |out| {
        glGetVertexAttribPointerv(index, pname, out)
    });
}

fn uniform<T: Copy + Default>(
    program: u32,
    location: i32,
    buf_size: i32,
    length: *mut i32,
    params: *mut T,
) {
    let bytes =
        GlobalState::context(|state| intro::get_uniform_bytes(&state.gl, program, location));
    let count = bytes
        .as_ref()
        .map_or(1, |value| value.len().div_ceil(core::mem::size_of::<T>()));
    let required = count.saturating_mul(core::mem::size_of::<T>());
    if params.is_null() || buf_size < 0 || (buf_size as usize) < required {
        reject(length);
        return;
    }
    unsafe {
        core::ptr::write_bytes(params, 0, count);
        if let Some(bytes) = bytes {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), params.cast(), bytes.len());
        }
        write_length(length, count);
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformfvRobustANGLE(
    program: u32,
    location: i32,
    buf_size: i32,
    length: *mut i32,
    params: *mut f32,
) {
    uniform(program, location, buf_size, length, params);
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformivRobustANGLE(
    program: u32,
    location: i32,
    buf_size: i32,
    length: *mut i32,
    params: *mut i32,
) {
    uniform(program, location, buf_size, length, params);
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformuivRobustANGLE(
    program: u32,
    location: i32,
    buf_size: i32,
    length: *mut i32,
    params: *mut u32,
) {
    uniform(program, location, buf_size, length, params);
}
use super::*;
