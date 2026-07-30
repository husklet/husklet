use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDispatchCompute(num_groups_x: u32, num_groups_y: u32, num_groups_z: u32) {
    if let Err(error) = GlobalState::gpu_submit(move |group, sink| {
        compute::dispatch_compute(
            &mut group.gl,
            sink,
            num_groups_x,
            num_groups_y,
            num_groups_z,
        )
    }) {
        GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
    }
}

/// `glDispatchComputeIndirect(indirect)` — dispatch with the group counts read from the buffer bound to
/// `GL_DISPATCH_INDIRECT_BUFFER` at byte offset `indirect` (see [`compute::dispatch_compute_indirect`]).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDispatchComputeIndirect(indirect: isize) {
    if let Err(error) = GlobalState::gpu_submit(move |group, sink| {
        compute::dispatch_compute_indirect(&mut group.gl, sink, indirect)
    }) {
        GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
    }
}

// ==================================================================================================
// GLES3.0: sync objects (GLsync) over the IR fence timeline
// ==================================================================================================

/// `glFenceSync(condition, flags)` — insert a fence into the command stream + return its `GLsync` token
/// (an opaque non-null pointer), or null on a bad `condition`/`flags`. See [`sync::fence_sync`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFenceSync(condition: u32, flags: u32) -> *mut c_void {
    GlobalState::gpu_submit(move |group, sink| {
        Ok(sync::fence_sync(&mut group.gl, sink, condition, flags))
    })
    .ok()
    .flatten()
    .map_or(core::ptr::null_mut(), |token| token as *mut c_void)
}

/// `glClientWaitSync(sync, flags, timeout)` — client-side wait on the fence value `sync` marks. See
/// [`sync::client_wait_sync`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClientWaitSync(sync: *mut c_void, flags: u32, timeout: u64) -> u32 {
    let sync = sync as usize;
    let deadline = if timeout == GL_TIMEOUT_IGNORED {
        std::time::Duration::from_secs(31)
    } else {
        std::time::Duration::from_nanos(timeout)
            .saturating_add(std::time::Duration::from_secs(2))
            .min(std::time::Duration::from_secs(31))
    };
    let completed = GlobalState::gpu_io(deadline, move |group, sink| {
        Ok(sync::client_wait_sync(
            &mut group.gl,
            sink,
            sync,
            flags,
            timeout,
        ))
    });
    let Ok(completed) = completed else {
        return GL_WAIT_FAILED;
    };
    let signaled = completed
        .observations
        .iter()
        .any(|observation| matches!(observation, Observation::Timed(hl_gpu::FenceWait::Complete)));
    if signaled {
        GlobalState::context(|group| {
            if let Some(value) = group.gl.sync_value(sync) {
                group.gl.mark_fence_signaled(value);
            }
        });
        GL_CONDITION_SATISFIED
    } else {
        completed.value
    }
}

/// `glWaitSync(sync, flags, timeout)` — device-side (queue) wait; lowers to a `WaitFence`. See
/// [`sync::wait_sync`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glWaitSync(sync: *mut c_void, flags: u32, timeout: u64) {
    let sync = sync as usize;
    let _ = GlobalState::gpu_submit(move |group, sink| {
        sync::wait_sync(&mut group.gl, sink, sync, flags, timeout);
        Ok(())
    });
}

/// `glDeleteSync(sync)` — drop the sync object (an unknown non-null sync raises `GL_INVALID_VALUE`).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteSync(sync: *mut c_void) {
    GlobalState::context(|s| s.gl.delete_sync(sync as usize));
}

/// `glIsSync(sync)` — `GL_TRUE`/`GL_FALSE` as the codegen's `u8` (`GLboolean`) ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsSync(sync: *mut c_void) -> u8 {
    GlobalState::context(|s| s.gl.has_sync(sync as usize)) as u8
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
    let sync = sync as usize;
    let completed = GlobalState::gpu_io(std::time::Duration::from_secs(5), move |group, sink| {
        Ok(sync::get_synciv(&mut group.gl, sink, sync, pname))
    });
    let v = completed.ok().and_then(|completed| {
        let signaled = completed
            .observations
            .iter()
            .any(|observation| matches!(observation, Observation::Poll(true)));
        if signaled {
            GlobalState::context(|group| {
                if let Some(value) = group.gl.sync_value(sync) {
                    group.gl.mark_fence_signaled(value);
                }
            });
            Some(GL_SIGNALED as i32)
        } else {
            completed.value
        }
    });
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
    GlobalState::context(|s| record::bind_buffer_base(&mut s.gl, target, index, buffer));
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
    GlobalState::context(|s| {
        record::bind_buffer_range(&mut s.gl, target, index, buffer, offset, size)
    });
}

// ==================================================================================================
// GLES3.0: PBO-style buffer mapping — glMapBufferRange / glUnmapBuffer / glFlushMappedBufferRange
// ==================================================================================================

/// `glMapBufferRange(target, offset, length, access)` — map a range of the bound buffer and return a
/// pointer INTO its host storage (the app writes through it; `glUnmapBuffer` flushes). Null on error. The
/// pointer stays valid until the buffer's storage reallocates (the reference shim's fragile contract).
#[cfg_attr(gles_client, no_mangle)]
/// PRECONDITION: a CURRENT context. Like every GL entry point this resolves against the calling thread's
/// share group and answers its zero value — here a null pointer, with no GL error — when there is none.
pub extern "C" fn glMapBufferRange(
    target: u32,
    offset: isize,
    length: isize,
    access: u32,
) -> *mut c_void {
    crate::stub::trace(
        "glMapBufferRange",
        &format!("target={target:#x} offset={offset} length={length} access={access:#x}"),
    );
    GlobalState::context(|s| {
        match map::map_buffer_range(&mut s.gl, target, offset, length, access) {
            Some((name, off)) => {
                s.gl.buffers
                    .mapped_ptr(name, off)
                    .map_or(core::ptr::null_mut(), |pointer| pointer.cast())
            }
            None => core::ptr::null_mut(),
        }
    })
}

/// `glUnmapBuffer(target)` — flush the mapped range as a `WriteBuffer` + clear the mapping. Returns the
/// `GLboolean` (`u8`) result; a sink error is surfaced via `eglGetError`. See [`map::unmap_buffer`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUnmapBuffer(target: u32) -> u8 {
    match GlobalState::gpu_submit(move |group, sink| map::unmap_buffer(&mut group.gl, sink, target))
    {
        Ok(value) => value,
        Err(error) => {
            GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
            0
        }
    }
}

/// `glMapBufferOES(target, access)` — `GL_OES_mapbuffer`: map the WHOLE buffer bound to `target` and
/// return a pointer into its host storage. That extension defines exactly one legal access,
/// `GL_WRITE_ONLY_OES`; anything else is `GL_INVALID_ENUM`. Expressed as the ES 3 `glMapBufferRange` over
/// `[0, size)` with `GL_MAP_WRITE_BIT`, so unmap/flush behaviour is the one modeled path
/// ([`map::map_buffer_range`]). Null on error.
pub extern "C" fn glMapBufferOES(target: u32, access: u32) -> *mut c_void {
    const GL_WRITE_ONLY_OES: u32 = 0x88B9;
    GlobalState::context(|s| {
        if access != GL_WRITE_ONLY_OES {
            s.gl.set_gl_error(GL_INVALID_ENUM);
            return core::ptr::null_mut();
        }
        let name = s.gl.buffer_for_target(target);
        let size = s.gl.buffers.get(name).map_or(0, |buffer| buffer.data.len());
        if name == 0 || size == 0 {
            s.gl.set_gl_error(GL_INVALID_OPERATION);
            return core::ptr::null_mut();
        }
        match map::map_buffer_range(&mut s.gl, target, 0, size as isize, GL_MAP_WRITE_BIT) {
            Some((name, offset)) => {
                s.gl.buffers
                    .mapped_ptr(name, offset)
                    .map_or(core::ptr::null_mut(), |pointer| pointer.cast())
            }
            None => core::ptr::null_mut(),
        }
    })
}

/// `glUnmapBufferOES(target)` — `GL_OES_mapbuffer`'s unmap; identical semantics to the ES 3
/// `glUnmapBuffer` (flush the mapped range, clear the mapping).
pub extern "C" fn glUnmapBufferOES(target: u32) -> u8 {
    glUnmapBuffer(target)
}

/// `glFlushMappedBufferRange(target, offset, length)` — flush a sub-range of a still-mapped buffer as a
/// `WriteBuffer`. See [`map::flush_mapped_range`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFlushMappedBufferRange(target: u32, offset: isize, length: isize) {
    if let Err(error) = GlobalState::gpu_submit(move |group, sink| {
        map::flush_mapped_range(&mut group.gl, sink, target, offset, length)
    }) {
        GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
    }
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
    GlobalState::context(|s| record::draw_buffers(&mut s.gl, &list));
}

/// Desktop OpenGL single-target spelling used by Chromium's ANGLE/OpenGL dispatch.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawBuffer(buffer: u32) {
    glDrawBuffers(1, &buffer);
}

/// `glReadBuffer(src)` — select the color buffer subsequent readbacks read from. See
/// [`record::read_buffer`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glReadBuffer(src: u32) {
    GlobalState::context(|s| record::read_buffer(&mut s.gl, src));
}

// ==================================================================================================
// ES3 sampler objects (client-side state; no GPU IR) — glGen/Bind/Delete/SamplerParameter*
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenSamplers(count: i32, samplers: *mut u32) {
    if samplers.is_null() || count <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..count as isize {
            *samplers.offset(i) = s.gl.samplers.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteSamplers(count: i32, samplers: *const u32) {
    if count < 0 {
        GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if samplers.is_null() {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..count as isize {
            s.delete_sampler(*samplers.offset(i));
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindSampler(unit: u32, sampler: u32) {
    GlobalState::context(|s| es3::bind_sampler(&mut s.gl, unit, sampler));
}

/// `glIsSampler(sampler)` — `GLboolean` in the codegen's `u8` ABI (low byte is the boolean).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsSampler(sampler: u32) -> u8 {
    GlobalState::context(|s| s.gl.is_sampler_name(sampler)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameteri(sampler: u32, pname: u32, param: i32) {
    GlobalState::context(|s| {
        es3::sampler_parameter(&mut s.gl, sampler, pname, param, param as f32)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterf(sampler: u32, pname: u32, param: f32) {
    GlobalState::context(|s| {
        es3::sampler_parameter(&mut s.gl, sampler, pname, param as i32, param)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameteriv(sampler: u32, pname: u32, param: *const i32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    GlobalState::context(|s| es3::sampler_parameter(&mut s.gl, sampler, pname, v, v as f32));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterfv(sampler: u32, pname: u32, param: *const f32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    GlobalState::context(|s| es3::sampler_parameter(&mut s.gl, sampler, pname, v as i32, v));
}

/// `glSamplerParameterIiv` — the integer (non-normalized) vector form; reads `param[0]` (same setter path).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterIiv(sampler: u32, pname: u32, param: *const i32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    GlobalState::context(|s| es3::sampler_parameter(&mut s.gl, sampler, pname, v, v as f32));
}

/// `glSamplerParameterIuiv` — the unsigned integer vector form; reads `param[0]`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterIuiv(sampler: u32, pname: u32, param: *const u32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param } as i32;
    GlobalState::context(|s| es3::sampler_parameter(&mut s.gl, sampler, pname, v, v as f32));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameteriv(sampler: u32, pname: u32, params: *mut i32) {
    let v = GlobalState::context(|s| es3::get_sampler_parameter(&mut s.gl, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v.round() as i32 };
        }
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameterfv(sampler: u32, pname: u32, params: *mut f32) {
    let v = GlobalState::context(|s| es3::get_sampler_parameter(&mut s.gl, sampler, pname));
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
    let v = GlobalState::context(|s| es3::get_sampler_parameter(&mut s.gl, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v.round() as u32 };
        }
    }
}
