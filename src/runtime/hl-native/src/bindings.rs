#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_uint, c_ulonglong, c_void};

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
    pub(super) fn hl_c_backend_translation_count(backend: *const Backend) -> c_ulonglong;
    pub(super) fn hl_c_backend_destroy(backend: *mut Backend);
}
