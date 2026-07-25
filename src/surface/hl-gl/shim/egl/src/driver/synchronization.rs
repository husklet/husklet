use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDispatchCompute(num_groups_x: u32, num_groups_y: u32, num_groups_z: u32) {
    GlobalState::access(|s| {
        if let Err(e) = compute::dispatch_compute(
            &mut s.ctx,
            &mut s.sink,
            num_groups_x,
            num_groups_y,
            num_groups_z,
        ) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
}

/// `glDispatchComputeIndirect(indirect)` — dispatch with the group counts read from the buffer bound to
/// `GL_DISPATCH_INDIRECT_BUFFER` at byte offset `indirect` (see [`compute::dispatch_compute_indirect`]).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDispatchComputeIndirect(indirect: isize) {
    GlobalState::access(|s| {
        if let Err(e) = compute::dispatch_compute_indirect(&mut s.ctx, &mut s.sink, indirect) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
}

// ==================================================================================================
// GLES3.0: sync objects (GLsync) over the IR fence timeline
// ==================================================================================================

/// `glFenceSync(condition, flags)` — insert a fence into the command stream + return its `GLsync` token
/// (an opaque non-null pointer), or null on a bad `condition`/`flags`. See [`sync::fence_sync`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFenceSync(condition: u32, flags: u32) -> *mut c_void {
    GlobalState::access(
        |s| match sync::fence_sync(&mut s.ctx, &mut s.sink, condition, flags) {
            Some(token) => token as *mut c_void,
            None => core::ptr::null_mut(),
        },
    )
}

/// `glClientWaitSync(sync, flags, timeout)` — client-side wait on the fence value `sync` marks. See
/// [`sync::client_wait_sync`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClientWaitSync(sync: *mut c_void, flags: u32, timeout: u64) -> u32 {
    GlobalState::access(|s| {
        sync::client_wait_sync(&mut s.ctx, &mut s.sink, sync as usize, flags, timeout)
    })
}

/// `glWaitSync(sync, flags, timeout)` — device-side (queue) wait; lowers to a `WaitFence`. See
/// [`sync::wait_sync`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glWaitSync(sync: *mut c_void, flags: u32, timeout: u64) {
    GlobalState::access(|s| {
        sync::wait_sync(&mut s.ctx, &mut s.sink, sync as usize, flags, timeout)
    });
}

/// `glDeleteSync(sync)` — drop the sync object (an unknown non-null sync raises `GL_INVALID_VALUE`).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteSync(sync: *mut c_void) {
    GlobalState::access(|s| s.ctx.delete_sync(sync as usize));
}

/// `glIsSync(sync)` — `GL_TRUE`/`GL_FALSE` as the codegen's `u8` (`GLboolean`) ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsSync(sync: *mut c_void) -> u8 {
    GlobalState::access(|s| s.ctx.has_sync(sync as usize)) as u8
}

/// `glGetSynciv(sync, pname, buf_size, length, values)` — write the single integer state value for
/// `pname` (see [`sync::get_synciv`]). Null-safe on both out-params.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSynciv(
    sync: *mut c_void,
    pname: u32,
    buf_size: i32,
    length: *mut i32,
    values: *mut i32,
) {
    let v = GlobalState::access(|s| sync::get_synciv(&mut s.ctx, sync as usize, pname));
    if let Some(v) = v {
        unsafe {
            if !values.is_null() && buf_size >= 1 {
                *values = v;
                if !length.is_null() {
                    *length = 1;
                }
            } else if !length.is_null() {
                *length = 0;
            }
        }
    }
}

// ==================================================================================================
// GLES3.0: indexed buffer bindings (UBO/SSBO) — glBindBufferBase / glBindBufferRange
// ==================================================================================================

/// `glBindBufferBase(target, index, buffer)` — bind the whole `buffer` to indexed slot `index`. See
/// [`record::bind_buffer_base`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindBufferBase(target: u32, index: u32, buffer: u32) {
    GlobalState::access(|s| record::bind_buffer_base(&mut s.ctx, target, index, buffer));
}

/// `glBindBufferRange(target, index, buffer, offset, size)` — bind `[offset, offset+size)` of `buffer` to
/// indexed slot `index`. See [`record::bind_buffer_range`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindBufferRange(
    target: u32,
    index: u32,
    buffer: u32,
    offset: isize,
    size: isize,
) {
    GlobalState::access(|s| {
        record::bind_buffer_range(&mut s.ctx, target, index, buffer, offset, size)
    });
}

// ==================================================================================================
// GLES3.0: PBO-style buffer mapping — glMapBufferRange / glUnmapBuffer / glFlushMappedBufferRange
// ==================================================================================================

/// `glMapBufferRange(target, offset, length, access)` — map a range of the bound buffer and return a
/// pointer INTO its host storage (the app writes through it; `glUnmapBuffer` flushes). Null on error. The
/// pointer stays valid until the buffer's storage reallocates (the reference shim's fragile contract).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glMapBufferRange(
    target: u32,
    offset: isize,
    length: isize,
    access: u32,
) -> *mut c_void {
    GlobalState::access(|s| {
        match map::map_buffer_range(&mut s.ctx, target, offset, length, access) {
            Some((name, off)) => match s.ctx.buffers.get_mut(name) {
                Some(b) => unsafe { b.data.as_mut_ptr().add(off) as *mut c_void },
                None => core::ptr::null_mut(),
            },
            None => core::ptr::null_mut(),
        }
    })
}

/// `glUnmapBuffer(target)` — flush the mapped range as a `WriteBuffer` + clear the mapping. Returns the
/// `GLboolean` (`u8`) result; a sink error is surfaced via `eglGetError`. See [`map::unmap_buffer`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUnmapBuffer(target: u32) -> u8 {
    GlobalState::access(
        |s| match map::unmap_buffer(&mut s.ctx, &mut s.sink, target) {
            Ok(v) => v,
            Err(e) => {
                s.set_egl_error(egl_error_from_gpu_error(&e));
                0
            }
        },
    )
}

/// `glFlushMappedBufferRange(target, offset, length)` — flush a sub-range of a still-mapped buffer as a
/// `WriteBuffer`. See [`map::flush_mapped_range`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFlushMappedBufferRange(target: u32, offset: isize, length: isize) {
    GlobalState::access(|s| {
        if let Err(e) = map::flush_mapped_range(&mut s.ctx, &mut s.sink, target, offset, length) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
}

// ==================================================================================================
// GLES3.0: MRT draw/read buffer selection — glDrawBuffers / glReadBuffer
// ==================================================================================================

/// `glDrawBuffers(n, bufs)` — record the fragment-output color-buffer list. See [`record::draw_buffers`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawBuffers(n: i32, bufs: *const u32) {
    let list = if bufs.is_null() || n <= 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(bufs, n as usize) }.to_vec()
    };
    GlobalState::access(|s| record::draw_buffers(&mut s.ctx, &list));
}

/// `glReadBuffer(src)` — select the color buffer subsequent readbacks read from. See
/// [`record::read_buffer`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glReadBuffer(src: u32) {
    GlobalState::access(|s| record::read_buffer(&mut s.ctx, src));
}

// ==================================================================================================
// ES3 sampler objects (client-side state; no GPU IR) — glGen/Bind/Delete/SamplerParameter*
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenSamplers(count: i32, samplers: *mut u32) {
    if samplers.is_null() || count <= 0 {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..count as isize {
            *samplers.offset(i) = s.ctx.samplers.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteSamplers(count: i32, samplers: *const u32) {
    if count < 0 {
        GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if samplers.is_null() {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..count as isize {
            s.ctx.samplers.delete(*samplers.offset(i));
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindSampler(unit: u32, sampler: u32) {
    GlobalState::access(|s| es3::bind_sampler(&mut s.ctx, unit, sampler));
}

/// `glIsSampler(sampler)` — `GLboolean` in the codegen's `u8` ABI (low byte is the boolean).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsSampler(sampler: u32) -> u8 {
    GlobalState::access(|s| s.ctx.samplers.contains(sampler)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameteri(sampler: u32, pname: u32, param: i32) {
    GlobalState::access(|s| {
        es3::sampler_parameter(&mut s.ctx, sampler, pname, param, param as f32)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterf(sampler: u32, pname: u32, param: f32) {
    GlobalState::access(|s| {
        es3::sampler_parameter(&mut s.ctx, sampler, pname, param as i32, param)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameteriv(sampler: u32, pname: u32, param: *const i32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    GlobalState::access(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterfv(sampler: u32, pname: u32, param: *const f32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    GlobalState::access(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v as i32, v));
}

/// `glSamplerParameterIiv` — the integer (non-normalized) vector form; reads `param[0]` (same setter path).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterIiv(sampler: u32, pname: u32, param: *const i32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    GlobalState::access(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

/// `glSamplerParameterIuiv` — the unsigned integer vector form; reads `param[0]`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterIuiv(sampler: u32, pname: u32, param: *const u32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param } as i32;
    GlobalState::access(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameteriv(sampler: u32, pname: u32, params: *mut i32) {
    let v = GlobalState::access(|s| es3::get_sampler_parameter(&mut s.ctx, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v.round() as i32 };
        }
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameterfv(sampler: u32, pname: u32, params: *mut f32) {
    let v = GlobalState::access(|s| es3::get_sampler_parameter(&mut s.ctx, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v };
        }
    }
}

/// `glGetSamplerParameterIiv` — the integer view of `glGetSamplerParameteriv`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameterIiv(sampler: u32, pname: u32, params: *mut i32) {
    glGetSamplerParameteriv(sampler, pname, params);
}

/// `glGetSamplerParameterIuiv` — the unsigned-integer view of `glGetSamplerParameteriv`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameterIuiv(sampler: u32, pname: u32, params: *mut u32) {
    let v = GlobalState::access(|s| es3::get_sampler_parameter(&mut s.ctx, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v.round() as u32 };
        }
    }
}
