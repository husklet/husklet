#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_uint, c_ulonglong, c_void};

// These declarations mirror native/bridge/api.h. Including that header in the
// defining C translation unit makes signature drift a compile-time error.
const _: () = assert!(size_of::<c_int>() == size_of::<i32>());
const _: () = assert!(size_of::<c_uint>() == size_of::<u32>());
const _: () = assert!(size_of::<c_ulonglong>() == size_of::<u64>());

#[repr(C)]
pub struct Backend {
    _private: [u8; 0],
}

pub type SyscallDispatch = unsafe extern "C" fn(*mut c_void, c_uint, *mut c_void, *mut c_void) -> c_int;

unsafe extern "C" {
    pub(super) fn hl_engine_abi() -> c_uint;
    pub(super) fn hl_engine_version() -> *const c_char;
    pub(super) fn hl_c_backend_leak_check_nonvacuity() -> c_int;
    pub(super) fn hl_c_backend_create(
        isa: c_uint,
        rootfs: *const c_char,
        executable_host: *const c_char,
        executable_fd: c_int,
        image_plan: *const c_void,
        option_count: c_uint,
        option_names: *const *const c_char,
        option_values: *const *const c_char,
        standard_fds: *const c_int,
        provider_fd: c_int,
        syscall_context: *mut c_void,
        syscall_dispatch: Option<SyscallDispatch>,
        output: *mut *mut Backend,
    ) -> c_int;
    pub(super) fn hl_c_backend_run(backend: *mut Backend, argc: c_int, argv: *const *const c_char) -> c_int;
    pub(super) fn hl_c_backend_request(backend: *mut Backend, request: c_uint, signal: c_int) -> c_int;
    pub(super) fn hl_c_backend_exit_kind(backend: *const Backend) -> c_uint;
    pub(super) fn hl_c_backend_exit_status(backend: *const Backend) -> c_int;
    pub(super) fn hl_c_backend_exit_detail(backend: *const Backend) -> c_ulonglong;
    pub(super) fn hl_c_backend_destroy(backend: *mut Backend);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn exported_bridge_contract_is_callable() {
        unsafe {
            assert_ne!(hl_engine_abi(), 0);
            assert!(!hl_engine_version().is_null());
            assert_eq!(hl_c_backend_leak_check_nonvacuity(), 0);
            assert_ne!(
                hl_c_backend_create(
                    0,
                    ptr::null(),
                    ptr::null(),
                    -1,
                    ptr::null(),
                    0,
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    -1,
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                ),
                0
            );
            assert_ne!(hl_c_backend_run(ptr::null_mut(), 0, ptr::null()), 0);
            assert_ne!(hl_c_backend_request(ptr::null_mut(), 0, 0), 0);
            assert_eq!(hl_c_backend_exit_kind(ptr::null()), 0);
            assert_eq!(hl_c_backend_exit_status(ptr::null()), -1);
            assert_eq!(hl_c_backend_exit_detail(ptr::null()), 0);
            assert_eq!(hl_c_backend_translation_count(ptr::null()), 0);
            hl_c_backend_destroy(ptr::null_mut());
        }
    }
}
