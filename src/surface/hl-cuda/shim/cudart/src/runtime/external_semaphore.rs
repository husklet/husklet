use core::ffi::c_void;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};

use hl_cuda::model::external_semaphore::ExternalSemaphore;
use hl_cuda::result::*;
use hl_cuda::service::external_semaphore;
use hl_gpu::transport::adapter::unix::OpaqueSyncFd;

use crate::state::ShimState;

const TIMELINE_SEMAPHORE_FD: i32 = 9;

#[repr(C)]
pub struct ExternalSemaphoreHandleDesc {
    type_: i32,
    _padding: u32,
    handle: [u64; 2],
    flags: u32,
    reserved: [u32; 16],
}

#[repr(C)]
pub struct ExternalSemaphoreParams {
    payload: [u64; 2],
    flags: u32,
    reserved: [u32; 16],
}

impl ExternalSemaphoreHandleDesc {
    fn fd(&self) -> Option<i32> {
        (self.type_ == TIMELINE_SEMAPHORE_FD
            && self.flags == 0
            && self.reserved.iter().all(|value| *value == 0))
        .then_some(self.handle[0] as i32)
        .filter(|fd| *fd >= 0)
    }
}

impl ExternalSemaphoreParams {
    fn value(&self) -> Option<u64> {
        (self.flags == 0 && self.reserved.iter().all(|value| *value == 0))
            .then_some(self.payload[0])
    }
}

#[no_mangle]
pub extern "C" fn cudaImportExternalSemaphore(
    output: *mut *mut c_void,
    desc: *const ExternalSemaphoreHandleDesc,
) -> i32 {
    let (Some(output), Some(desc)) = ((unsafe { output.as_mut() }), (unsafe { desc.as_ref() }))
    else {
        return CUDART_ERROR_INVALID_VALUE;
    };
    let Some(fd) = desc.fd() else {
        return CUDART_ERROR_INVALID_VALUE;
    };
    let duplicate = match unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned() {
        Ok(fd) => fd,
        Err(_) => return CUDART_ERROR_INVALID_VALUE,
    };
    let export = match OpaqueSyncFd::from_owned(duplicate).and_then(OpaqueSyncFd::consume) {
        Ok(export) => export,
        Err(_) => return CUDART_ERROR_INVALID_VALUE,
    };
    let result = ShimState::with(|state| {
        match external_semaphore::import(&mut state.ctx, &mut state.sink, export) {
            Ok(handle) => {
                *output = handle.0 as *mut c_void;
                CUDART_SUCCESS
            }
            Err(error) => state.fail(RuntimeStatus::from(&error).code()),
        }
    });
    if result == CUDART_SUCCESS {
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    result
}

#[no_mangle]
pub extern "C" fn cudaDestroyExternalSemaphore(handle: *mut c_void) -> i32 {
    ShimState::with(|state| {
        match external_semaphore::destroy(
            &mut state.ctx,
            &mut state.sink,
            ExternalSemaphore(handle as u64),
        ) {
            Ok(()) => CUDART_SUCCESS,
            Err(error) => state.fail(RuntimeStatus::from(&error).code()),
        }
    })
}

#[no_mangle]
pub extern "C" fn cudaSignalExternalSemaphoresAsync(
    handles: *const *mut c_void,
    params: *const ExternalSemaphoreParams,
    count: u32,
    stream: *mut c_void,
) -> i32 {
    operate(handles, params, count, stream, true)
}

#[no_mangle]
pub extern "C" fn cudaWaitExternalSemaphoresAsync(
    handles: *const *mut c_void,
    params: *const ExternalSemaphoreParams,
    count: u32,
    stream: *mut c_void,
) -> i32 {
    operate(handles, params, count, stream, false)
}

fn operate(
    handles: *const *mut c_void,
    params: *const ExternalSemaphoreParams,
    count: u32,
    stream_handle: *mut c_void,
    signal: bool,
) -> i32 {
    if count != 0 && (handles.is_null() || params.is_null()) {
        return CUDART_ERROR_INVALID_VALUE;
    }
    let handles = if count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(handles, count as usize) }
    };
    let params = if count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(params, count as usize) }
    };
    ShimState::with(|state| {
        let Some(stream) = state.stream(stream_handle) else {
            return state.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        };
        for (handle, params) in handles.iter().zip(params) {
            let Some(value) = params.value() else {
                return state.fail(CUDART_ERROR_INVALID_VALUE);
            };
            let semaphore = ExternalSemaphore(*handle as u64);
            let result = if signal {
                external_semaphore::signal(&state.ctx, &mut state.sink, semaphore, value, stream)
            } else {
                external_semaphore::wait(&state.ctx, &mut state.sink, semaphore, value, stream)
            };
            if let Err(error) = result {
                return state.fail(RuntimeStatus::from(&error).code());
            }
        }
        CUDART_SUCCESS
    })
}
