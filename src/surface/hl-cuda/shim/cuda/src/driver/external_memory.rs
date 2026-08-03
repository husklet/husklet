use core::ffi::c_void;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};

use hl_cuda::model::external_memory::ExternalMemory;
use hl_cuda::result::*;
use hl_cuda::service::external_memory;
use hl_gpu::transport::adapter::unix::OpaqueResourceFd;

use crate::state::ShimState;

const CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD: i32 = 1;

#[repr(C)]
pub struct ExternalMemoryHandleDesc {
    type_: i32,
    _padding: u32,
    handle: [u64; 2],
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}

#[repr(C)]
pub struct ExternalMemoryBufferDesc {
    offset: u64,
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}

#[no_mangle]
pub extern "C" fn cuImportExternalMemory(
    output: *mut *mut c_void,
    descriptor: *const ExternalMemoryHandleDesc,
) -> i32 {
    let (Some(output), Some(descriptor)) =
        (unsafe { output.as_mut() }, unsafe { descriptor.as_ref() })
    else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    *output = core::ptr::null_mut();
    if descriptor.type_ != CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD
        || (descriptor.handle[0] as i32) < 0
        || descriptor.size == 0
        || descriptor.flags != 0
        || descriptor.reserved.iter().any(|value| *value != 0)
    {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let fd = descriptor.handle[0] as i32;
    let duplicate = match unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned() {
        Ok(fd) => fd,
        Err(_) => return CUDA_ERROR_INVALID_VALUE,
    };
    let export = match OpaqueResourceFd::from_owned(duplicate).and_then(OpaqueResourceFd::consume) {
        Ok(export) => export,
        Err(_) => return CUDA_ERROR_INVALID_VALUE,
    };
    let result = ShimState::with_context(|state| {
        match external_memory::import(&mut state.ctx, &mut state.sink, export, descriptor.size) {
            Ok(handle) => {
                *output = handle.0 as *mut c_void;
                CUDA_SUCCESS
            }
            Err(error) => DriverStatus::from(&error).code(),
        }
    });
    if result == CUDA_SUCCESS {
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    result
}

#[no_mangle]
pub extern "C" fn cuExternalMemoryGetMappedBuffer(
    pointer: *mut u64,
    memory: *mut c_void,
    descriptor: *const ExternalMemoryBufferDesc,
) -> i32 {
    let (Some(pointer), Some(descriptor)) =
        (unsafe { pointer.as_mut() }, unsafe { descriptor.as_ref() })
    else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    *pointer = 0;
    if memory.is_null()
        || descriptor.flags != 0
        || descriptor.reserved.iter().any(|value| *value != 0)
    {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|state| {
        match external_memory::mapped_buffer(
            &mut state.ctx,
            ExternalMemory(memory as u64),
            descriptor.offset,
            descriptor.size,
        ) {
            Ok(mapped) => {
                *pointer = mapped.0;
                CUDA_SUCCESS
            }
            Err(error) => DriverStatus::from(&error).code(),
        }
    })
}

#[no_mangle]
pub extern "C" fn cuDestroyExternalMemory(memory: *mut c_void) -> i32 {
    if memory.is_null() {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    ShimState::with_context(|state| {
        match external_memory::destroy(
            &mut state.ctx,
            &mut state.sink,
            ExternalMemory(memory as u64),
        ) {
            Ok(()) => CUDA_SUCCESS,
            Err(error) => DriverStatus::from(&error).code(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptors_match_cuda_lp64_abi() {
        assert_eq!(core::mem::size_of::<ExternalMemoryHandleDesc>(), 104);
        assert_eq!(core::mem::size_of::<ExternalMemoryBufferDesc>(), 88);
    }
}
