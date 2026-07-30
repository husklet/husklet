//! Bounded client-memory entry points required by Chromium's ANGLE passthrough decoder.
//!
//! Unlike the core GLES getters, every ANGLE robust getter receives the destination capacity and reports
//! the number of elements written. Keep the bounds check here at the C boundary so an untrusted guest
//! cannot make the shim write beyond its supplied span.

use super::*;

mod getters;
mod upload;
pub use getters::*;
pub use upload::*;

#[cfg(test)]
#[path = "robust/tests.rs"]
mod tests;

fn writable(capacity: i32, required: usize) -> bool {
    capacity >= 0 && capacity as usize >= required
}

unsafe fn write_length(length: *mut i32, value: usize) {
    if !length.is_null() {
        *length = value as i32;
    }
}

fn reject(length: *mut i32) {
    GlobalState::context(|state| state.gl.set_gl_error(GL_INVALID_OPERATION));
    unsafe { write_length(length, 0) };
}

fn scalar_i32(param_count: i32, length: *mut i32, params: *mut i32, read: impl FnOnce(*mut i32)) {
    if params.is_null() || !writable(param_count, 1) {
        reject(length);
        return;
    }
    read(params);
    unsafe { write_length(length, 1) };
}

fn scalar_f32(param_count: i32, length: *mut i32, params: *mut f32, read: impl FnOnce(*mut f32)) {
    if params.is_null() || !writable(param_count, 1) {
        reject(length);
        return;
    }
    read(params);
    unsafe { write_length(length, 1) };
}

fn scalar_i64(param_count: i32, length: *mut i32, params: *mut i64, read: impl FnOnce(*mut i64)) {
    if params.is_null() || !writable(param_count, 1) {
        reject(length);
        return;
    }
    read(params);
    unsafe { write_length(length, 1) };
}

fn scalar_u32(param_count: i32, length: *mut i32, params: *mut u32, read: impl FnOnce(*mut u32)) {
    if params.is_null() || !writable(param_count, 1) {
        reject(length);
        return;
    }
    read(params);
    unsafe { write_length(length, 1) };
}

fn scalar_u64(param_count: i32, length: *mut i32, params: *mut u64, read: impl FnOnce(*mut u64)) {
    if params.is_null() || !writable(param_count, 1) {
        reject(length);
        return;
    }
    read(params);
    unsafe { write_length(length, 1) };
}

fn scalar_pointer(
    param_count: i32,
    length: *mut i32,
    params: *mut *mut c_void,
    read: impl FnOnce(*mut *mut c_void),
) {
    if params.is_null() || !writable(param_count, 1) {
        reject(length);
        return;
    }
    read(params);
    unsafe { write_length(length, 1) };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetIntegervRobustANGLE(
    pname: u32,
    param_count: i32,
    length: *mut i32,
    data: *mut i32,
) {
    let mut values = [0i32; 4];
    let count = GlobalState::context(|state| query::get_integerv(&state.gl, pname, &mut values));
    if data.is_null() || !writable(param_count, count) {
        GlobalState::context(|state| state.gl.set_gl_error(GL_INVALID_OPERATION));
        unsafe { write_length(length, 0) };
        return;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(values.as_ptr(), data, count);
        write_length(length, count);
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBooleanvRobustANGLE(
    pname: u32,
    param_count: i32,
    length: *mut i32,
    data: *mut u8,
) {
    let mut values = [0u8; 4];
    let count = GlobalState::context(|state| query::get_booleanv(&state.gl, pname, &mut values));
    if data.is_null() || !writable(param_count, count) {
        reject(length);
        return;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(values.as_ptr(), data, count);
        write_length(length, count);
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetFloatvRobustANGLE(
    pname: u32,
    param_count: i32,
    length: *mut i32,
    data: *mut f32,
) {
    let mut values = [0f32; 4];
    let count = GlobalState::context(|state| query::get_floatv(&state.gl, pname, &mut values));
    if data.is_null() || !writable(param_count, count) {
        reject(length);
        return;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(values.as_ptr(), data, count);
        write_length(length, count);
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferParameterivRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetBufferParameteriv(target, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramivRobustANGLE(
    program: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetProgramiv(program, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetShaderivRobustANGLE(
    shader: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetShaderiv(shader, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetRenderbufferParameterivRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetRenderbufferParameteriv(target, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetFramebufferAttachmentParameterivRobustANGLE(
    target: u32,
    attachment: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetFramebufferAttachmentParameteriv(target, attachment, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameterivRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetTexParameteriv(target, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameterfvRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut f32,
) {
    scalar_f32(param_count, length, params, |out| {
        glGetTexParameterfv(target, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetActiveUniformBlockivRobustANGLE(
    program: u32,
    uniform_block_index: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i32,
) {
    scalar_i32(param_count, length, params, |out| {
        glGetActiveUniformBlockiv(program, uniform_block_index, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferParameteri64vRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut i64,
) {
    scalar_i64(param_count, length, params, |out| {
        glGetBufferParameteri64v(target, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferPointervRobustANGLE(
    target: u32,
    pname: u32,
    param_count: i32,
    length: *mut i32,
    params: *mut *mut c_void,
) {
    scalar_pointer(param_count, length, params, |out| {
        glGetBufferPointerv(target, pname, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetInteger64vRobustANGLE(
    pname: u32,
    param_count: i32,
    length: *mut i32,
    data: *mut i64,
) {
    let mut values = [0i64; 4];
    let mut integers = [0i32; 4];
    let count = GlobalState::context(|state| query::get_integerv(&state.gl, pname, &mut integers));
    if data.is_null() || !writable(param_count, count) {
        reject(length);
        return;
    }
    for (value, integer) in values.iter_mut().zip(integers).take(count) {
        *value = i64::from(integer);
    }
    unsafe {
        core::ptr::copy_nonoverlapping(values.as_ptr(), data, count);
        write_length(length, count);
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetInteger64i_vRobustANGLE(
    target: u32,
    index: u32,
    param_count: i32,
    length: *mut i32,
    data: *mut i64,
) {
    scalar_i64(param_count, length, data, |out| {
        glGetInteger64i_v(target, index, out)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetIntegeri_vRobustANGLE(
    target: u32,
    index: u32,
    param_count: i32,
    length: *mut i32,
    data: *mut i32,
) {
    scalar_i32(param_count, length, data, |out| {
        glGetIntegeri_v(target, index, out)
    });
}
