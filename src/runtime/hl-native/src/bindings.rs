#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_uint, c_ulonglong, c_void};
use std::mem::offset_of;

// These declarations mirror native/bridge/api.h. Including that header in the
// defining C translation unit makes signature drift a compile-time error.
const _: () = assert!(size_of::<c_int>() == size_of::<i32>());
const _: () = assert!(size_of::<c_uint>() == size_of::<u32>());
const _: () = assert!(size_of::<c_ulonglong>() == size_of::<u64>());

#[repr(C)]
pub struct Backend {
    _private: [u8; 0],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct MainImagePlan {
    pub abi: u32,
    pub size: u32,
    pub architecture: u32,
    pub kind: u32,
    pub link_start: u64,
    pub link_end: u64,
    pub has_interpreter: u32,
    pub flags: u32,
    pub interpreter_identity: u64,
}

const _: () = assert!(size_of::<MainImagePlan>() == 48);
const _: () = assert!(offset_of!(MainImagePlan, link_start) == 16);
const _: () = assert!(offset_of!(MainImagePlan, interpreter_identity) == 40);

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct SyscallCpuAarch64 {
    pub abi: u32,
    pub size: u32,
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub tls: u64,
    pub nzcv: u64,
    pub task: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub(super) struct SyscallTrapResult {
    pub abi: u32,
    pub size: u32,
    pub outcome: u32,
    pub exit_status: i32,
    pub image_generation: u64,
}

const _: () = assert!(size_of::<SyscallCpuAarch64>() == 296);
const _: () = assert!(offset_of!(SyscallCpuAarch64, x) == 8);
const _: () = assert!(offset_of!(SyscallCpuAarch64, task) == 288);
const _: () = assert!(size_of::<SyscallTrapResult>() == 24);
const _: () = assert!(offset_of!(SyscallTrapResult, image_generation) == 16);

pub(super) type SyscallDispatch =
    unsafe extern "C" fn(*mut c_void, c_uint, *mut SyscallCpuAarch64, *mut SyscallTrapResult) -> c_int;

unsafe extern "C" {
    #[cfg(test)]
    pub(super) fn hl_engine_abi() -> c_uint;
    #[cfg(test)]
    pub(super) fn hl_engine_version() -> *const c_char;
    pub(super) fn hl_c_backend_leak_check_nonvacuity() -> c_int;
    pub(super) fn hl_c_backend_checkpoint_broker_pair(parent: *mut c_int, child: *mut c_int) -> c_int;
    pub(super) fn hl_c_backend_checkpoint_broker_accept(broker: c_int, timeout_ms: c_int, host_pid: *mut u64) -> c_int;
    pub(super) fn hl_c_backend_checkpoint_trigger_create(descriptor: *mut c_int, mapping: *mut *mut c_void) -> c_int;
    pub(super) fn hl_c_backend_checkpoint_trigger_bump(mapping: *mut c_void) -> c_uint;
    pub(super) fn hl_c_backend_checkpoint_trigger_destroy(mapping: *mut c_void, descriptor: c_int);
    pub(super) fn hl_c_backend_checkpoint_adopt(broker: c_int, trigger: c_int) -> c_int;
    pub(super) fn hl_c_backend_checkpoint_interrupt_signal() -> c_int;
    pub(super) fn hl_c_backend_create(
        isa: c_uint,
        rootfs: *const c_char,
        executable_host: *const c_char,
        executable_fd: c_int,
        image_plan: *const MainImagePlan,
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
    #[cfg(test)]
    pub(super) fn hl_c_backend_translation_count(backend: *const Backend) -> c_ulonglong;
    pub(super) fn hl_c_backend_destroy(backend: *mut Backend);
}

#[cfg(test)]
pub(super) fn engine_metadata_is_valid() -> bool {
    // SAFETY: both functions are immutable metadata queries exported by the
    // package-owned shared library and take no caller-provided pointers.
    unsafe { hl_engine_abi() == 5 && !hl_engine_version().is_null() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn exported_bridge_contract_is_callable() {
        // SAFETY: these calls exercise the C bridge's documented null-input
        // contract: queries and destroy accept null, while create, run, and
        // request reject it without dereferencing any supplied pointer.
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

    #[test]
    fn create_clears_output_before_rejecting_inputs() {
        let mut output = ptr::dangling_mut::<Backend>();
        // SAFETY: output is writable and every other pointer is null. The invalid image input must be
        // rejected before it is dereferenced; this test also proves the documented failure ownership state.
        let status = unsafe {
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
                &raw mut output,
            )
        };
        assert_ne!(status, 0);
        assert!(output.is_null());
    }
}
