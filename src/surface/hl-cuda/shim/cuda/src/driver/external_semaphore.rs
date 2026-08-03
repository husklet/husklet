use core::ffi::c_void;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};

use hl_cuda::model::external_semaphore::ExternalSemaphore;
use hl_cuda::result::*;
use hl_cuda::service::external_semaphore;
use hl_gpu::transport::adapter::unix::OpaqueSyncFd;

use crate::state::ShimState;

const CU_EXTERNAL_SEMAPHORE_HANDLE_TYPE_TIMELINE_SEMAPHORE_FD: i32 = 9;

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
        (self.type_ == CU_EXTERNAL_SEMAPHORE_HANDLE_TYPE_TIMELINE_SEMAPHORE_FD
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
pub extern "C" fn cuImportExternalSemaphore(
    external_semaphore_out: *mut *mut c_void,
    semaphore_handle_desc: *const ExternalSemaphoreHandleDesc,
) -> i32 {
    let (Some(output), Some(desc)) = (
        (unsafe { external_semaphore_out.as_mut() }),
        (unsafe { semaphore_handle_desc.as_ref() }),
    ) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let Some(fd) = desc.fd() else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let duplicate = match unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned() {
        Ok(fd) => fd,
        Err(_) => return CUDA_ERROR_INVALID_VALUE,
    };
    let export = match OpaqueSyncFd::from_owned(duplicate).and_then(OpaqueSyncFd::consume) {
        Ok(export) => export,
        Err(_) => return CUDA_ERROR_INVALID_VALUE,
    };
    let result = ShimState::with_context(|state| {
        let handle = match external_semaphore::import(&mut state.ctx, &mut state.sink, export) {
            Ok(handle) => handle,
            Err(error) => return DriverStatus::from(&error).code(),
        };
        *output = handle.0 as *mut c_void;
        CUDA_SUCCESS
    });
    if result == CUDA_SUCCESS {
        // SAFETY: CUDA takes ownership of a POSIX descriptor after a successful import.
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    result
}

#[no_mangle]
pub extern "C" fn cuDestroyExternalSemaphore(external_semaphore_handle: *mut c_void) -> i32 {
    ShimState::with_context(|state| {
        match external_semaphore::destroy(
            &mut state.ctx,
            &mut state.sink,
            ExternalSemaphore(external_semaphore_handle as u64),
        ) {
            Ok(()) => CUDA_SUCCESS,
            Err(error) => DriverStatus::from(&error).code(),
        }
    })
}

#[no_mangle]
pub extern "C" fn cuSignalExternalSemaphoresAsync(
    external_semaphores: *const *mut c_void,
    params: *const ExternalSemaphoreParams,
    count: u32,
    stream: *mut c_void,
) -> i32 {
    operate(external_semaphores, params, count, stream, true)
}

#[no_mangle]
pub extern "C" fn cuWaitExternalSemaphoresAsync(
    external_semaphores: *const *mut c_void,
    params: *const ExternalSemaphoreParams,
    count: u32,
    stream: *mut c_void,
) -> i32 {
    operate(external_semaphores, params, count, stream, false)
}

fn operate(
    handles: *const *mut c_void,
    params: *const ExternalSemaphoreParams,
    count: u32,
    stream_handle: *mut c_void,
    signal: bool,
) -> i32 {
    if count != 0 && (handles.is_null() || params.is_null()) {
        return CUDA_ERROR_INVALID_VALUE;
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
    ShimState::with_context(|state| {
        let Some(stream) = state.stream(stream_handle) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        for (handle, params) in handles.iter().zip(params) {
            let Some(value) = params.value() else {
                return CUDA_ERROR_INVALID_VALUE;
            };
            let semaphore = ExternalSemaphore(*handle as u64);
            let result = if signal {
                external_semaphore::signal(&state.ctx, &mut state.sink, semaphore, value, stream)
            } else {
                external_semaphore::wait(&state.ctx, &mut state.sink, semaphore, value, stream)
            };
            if let Err(error) = result {
                return DriverStatus::from(&error).code();
            }
        }
        CUDA_SUCCESS
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_semaphore_abi_and_reserved_fields_are_exact() {
        assert_eq!(core::mem::size_of::<ExternalSemaphoreHandleDesc>(), 96);
        assert_eq!(core::mem::size_of::<ExternalSemaphoreParams>(), 88);
        let mut desc = ExternalSemaphoreHandleDesc {
            type_: CU_EXTERNAL_SEMAPHORE_HANDLE_TYPE_TIMELINE_SEMAPHORE_FD,
            _padding: 0,
            handle: [7, 0],
            flags: 0,
            reserved: [0; 16],
        };
        assert_eq!(desc.fd(), Some(7));
        desc.flags = 1;
        assert_eq!(desc.fd(), None);
        let mut params = ExternalSemaphoreParams {
            payload: [11, 0],
            flags: 0,
            reserved: [0; 16],
        };
        assert_eq!(params.value(), Some(11));
        params.reserved[15] = 1;
        assert_eq!(params.value(), None);
    }
}
