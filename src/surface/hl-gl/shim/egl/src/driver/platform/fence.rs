const EGL_SYNC_PRIOR_COMMANDS_COMPLETE_KHR: i32 = 0x30F0;
pub(super) const EGL_SYNC_STATUS_KHR: i32 = 0x30F1;
pub(super) const EGL_SIGNALED_KHR: i32 = 0x30F2;
const EGL_UNSIGNALED_KHR: i32 = 0x30F3;
const EGL_TIMEOUT_EXPIRED_KHR: i32 = 0x30F5;
const EGL_SYNC_TYPE_KHR: i32 = 0x30F7;
const EGL_SYNC_CONDITION_KHR: i32 = 0x30F8;
pub(super) const EGL_SYNC_FENCE_KHR: u32 = 0x30F9;
const EGL_SYNC_FLUSH_COMMANDS_BIT_KHR: i32 = 0x0001;

/// The remote command sink is synchronous: returning from `flush_pending` is this driver's completion
/// boundary. A fence created after that flush is therefore already signaled, but remains a tracked object
/// with normal EGL lifetime and validation semantics.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreateSync(
    dpy: *mut c_void,
    type_: u32,
    attrib_list: *const isize,
) -> *mut c_void {
    let attributes_empty = attrib_list.is_null() || unsafe { *attrib_list } == EGL_NONE as isize;
    Sync::create(dpy, type_, attributes_empty)
}

struct Sync;

impl Sync {
    fn create(dpy: *mut c_void, type_: u32, attributes_empty: bool) -> *mut c_void {
        if dpy as usize != DISPLAY_TOKEN {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
            return core::ptr::null_mut();
        }
        if current::context() == 0 || current::display() != dpy as usize {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_MATCH));
            return core::ptr::null_mut();
        }
        if type_ != EGL_SYNC_FENCE_KHR || !attributes_empty {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            return core::ptr::null_mut();
        }
        let context = current::context();
        let max_buffer_bytes = GlobalState::access(|state| state.max_buffer_bytes);
        let gl = GlobalState::gpu_submit_for(context, move |group, sink| {
            storage::flush_pending(group, sink, max_buffer_bytes)?;
            sync::fence_sync(&mut group.gl, sink, GL_SYNC_GPU_COMMANDS_COMPLETE, 0)
                .ok_or(hl_gpu::GpuError::Invalid("EGL fence sync"))
        });
        match gl {
            Ok(gl) => GlobalState::access(|state| state.create_sync(context, gl)) as *mut c_void,
            Err(error) => {
                GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
                core::ptr::null_mut()
            }
        }
    }
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroySync(dpy: *mut c_void, sync: *mut c_void) -> u32 {
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return EGL_FALSE;
    }
    let object = GlobalState::access(|state| state.destroy_sync(sync as usize));
    let Some(object) = object else {
        GlobalState::access(|state| state.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    };
    GlobalState::context_for(object.context, |group| group.gl.delete_sync(object.gl));
    EGL_TRUE
}

fn egl_sync(dpy: *mut c_void, sync: *mut c_void) -> Option<crate::state::EglSync> {
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return None;
    }
    let object = GlobalState::access(|s| s.sync(sync as usize));
    if object.is_none() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
    }
    object
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglClientWaitSync(
    dpy: *mut c_void,
    sync: *mut c_void,
    flags: i32,
    _timeout: u64,
) -> i32 {
    if flags & !EGL_SYNC_FLUSH_COMMANDS_BIT_KHR != 0 {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE as i32;
    }
    let Some(object) = egl_sync(dpy, sync) else {
        return EGL_FALSE as i32;
    };
    let deadline = if _timeout == GL_TIMEOUT_IGNORED {
        std::time::Duration::from_secs(31)
    } else {
        std::time::Duration::from_nanos(_timeout)
            .saturating_add(std::time::Duration::from_secs(2))
            .min(std::time::Duration::from_secs(31))
    };
    let result = GlobalState::gpu_io_for(object.context, deadline, move |group, sink| {
        Ok(sync::client_wait_sync(
            &mut group.gl,
            sink,
            object.gl,
            flags as u32,
            _timeout,
        ))
    });
    match result {
        Ok(completed) => {
            let signaled = completed.observations.iter().any(|observation| {
                matches!(observation, Observation::Timed(hl_gpu::FenceWait::Complete))
            });
            if signaled {
                GlobalState::context_for(object.context, |group| {
                    if let Some(value) = group.gl.sync_value(object.gl) {
                        group.gl.mark_fence_signaled(value);
                    }
                });
            }
            match if signaled {
                GL_CONDITION_SATISFIED
            } else {
                completed.value
            } {
                GL_ALREADY_SIGNALED | GL_CONDITION_SATISFIED => EGL_CONDITION_SATISFIED,
                GL_TIMEOUT_EXPIRED => EGL_TIMEOUT_EXPIRED_KHR,
                _ => {
                    GlobalState::access(|state| state.set_egl_error(EGL_BAD_MATCH));
                    EGL_FALSE as i32
                }
            }
        }
        Err(error) => {
            GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
            EGL_FALSE as i32
        }
    }
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglWaitSync(dpy: *mut c_void, sync: *mut c_void, flags: i32) -> u32 {
    if flags != 0 {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let Some(object) = egl_sync(dpy, sync) else {
        return EGL_FALSE;
    };
    match GlobalState::gpu_submit_for(object.context, move |group, sink| {
        sync::wait_sync(&mut group.gl, sink, object.gl, 0, GL_TIMEOUT_IGNORED);
        Ok(())
    }) {
        Ok(()) => EGL_TRUE,
        Err(error) => {
            GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
            EGL_FALSE
        }
    }
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetSyncAttrib(
    dpy: *mut c_void,
    sync: *mut c_void,
    attribute: i32,
    value: *mut isize,
) -> u32 {
    if value.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let Some(object) = egl_sync(dpy, sync) else {
        return EGL_FALSE;
    };
    let result = match attribute {
        EGL_SYNC_TYPE_KHR => EGL_SYNC_FENCE_KHR as isize,
        EGL_SYNC_CONDITION_KHR => EGL_SYNC_PRIOR_COMMANDS_COMPLETE_KHR as isize,
        EGL_SYNC_STATUS_KHR => {
            let status = GlobalState::gpu_io_for(
                object.context,
                std::time::Duration::from_secs(5),
                move |group, sink| {
                    Ok(sync::get_synciv(
                        &mut group.gl,
                        sink,
                        object.gl,
                        GL_SYNC_STATUS,
                    ))
                },
            );
            match status {
                Ok(completed) => {
                    let signaled = completed
                        .observations
                        .iter()
                        .any(|observation| matches!(observation, Observation::Poll(true)));
                    if signaled {
                        GlobalState::context_for(object.context, |group| {
                            if let Some(value) = group.gl.sync_value(object.gl) {
                                group.gl.mark_fence_signaled(value);
                            }
                        });
                    }
                    match completed.value {
                        Some(status) if signaled || status == GL_SIGNALED as i32 => {
                            EGL_SIGNALED_KHR as isize
                        }
                        Some(_) => EGL_UNSIGNALED_KHR as isize,
                        None => {
                            GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
                            return EGL_FALSE;
                        }
                    }
                }
                Err(error) => {
                    GlobalState::access(|state| {
                        state.set_egl_error(egl_error_from_gpu_error(&error))
                    });
                    return EGL_FALSE;
                }
            }
        }
        _ => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            return EGL_FALSE;
        }
    };
    unsafe { *value = result };
    EGL_TRUE
}

// Keep the suffixed symbols fail-closed too. They are not advertised.
pub(crate) extern "C" fn eglCreateSyncKHR(
    dpy: *mut c_void,
    type_: u32,
    attrib_list: *const i32,
) -> *mut c_void {
    let attributes_empty = attrib_list.is_null() || unsafe { *attrib_list } == EGL_NONE;
    Sync::create(dpy, type_, attributes_empty)
}

pub(crate) extern "C" fn eglDestroySyncKHR(dpy: *mut c_void, sync: *mut c_void) -> u32 {
    eglDestroySync(dpy, sync)
}

pub(crate) extern "C" fn eglClientWaitSyncKHR(
    dpy: *mut c_void,
    sync: *mut c_void,
    flags: i32,
    timeout: u64,
) -> i32 {
    eglClientWaitSync(dpy, sync, flags, timeout)
}

pub(crate) extern "C" fn eglWaitSyncKHR(dpy: *mut c_void, sync: *mut c_void, flags: i32) -> u32 {
    eglWaitSync(dpy, sync, flags)
}

pub(crate) extern "C" fn eglGetSyncAttribKHR(
    dpy: *mut c_void,
    sync: *mut c_void,
    attribute: i32,
    value: *mut i32,
) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    let mut core = 0_isize;
    let result = eglGetSyncAttrib(dpy, sync, attribute, &mut core);
    if result == EGL_TRUE {
        unsafe { *value = core as i32 };
    }
    result
}
use super::*;
