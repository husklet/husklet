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

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct EngineExit {
    pub abi: u32,
    pub size: u32,
    pub kind: u32,
    pub guest_status: i32,
    pub detail: u64,
}

const _: () = assert!(size_of::<MainImagePlan>() == 48);
const _: () = assert!(offset_of!(MainImagePlan, link_start) == 16);
const _: () = assert!(offset_of!(MainImagePlan, interpreter_identity) == 40);
const _: () = assert!(size_of::<EngineExit>() == 24);
const _: () = assert!(offset_of!(EngineExit, detail) == 16);

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

pub(super) unsafe fn hl_engine_abi() -> c_uint {
    crate::loader::api()
        .ok()
        .and_then(|api| api.engine_abi)
        .map_or(0, |function| unsafe { function() })
}

pub(super) unsafe fn hl_engine_version() -> *const c_char {
    crate::loader::api()
        .ok()
        .and_then(|api| api.engine_version)
        .map_or(std::ptr::null(), |function| unsafe { function() })
}

pub(super) unsafe fn hl_c_backend_leak_check_nonvacuity() -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.leak_check_nonvacuity)
        .map_or(3, |function| unsafe { function() })
}

#[cfg(unix)]
pub(super) unsafe fn hl_c_backend_checkpoint_broker_pair(parent: *mut c_int, child: *mut c_int) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.checkpoint_broker_pair)
        .map_or(3, |function| unsafe { function(parent, child) })
}

#[cfg(unix)]
#[allow(dead_code)]
pub(super) unsafe fn hl_c_backend_checkpoint_broker_accept(
    broker: c_int,
    timeout_ms: c_int,
    host_pid: *mut u64,
) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.checkpoint_broker_accept)
        .map_or(3, |function| unsafe { function(broker, timeout_ms, host_pid) })
}

#[cfg(unix)]
pub(super) unsafe fn hl_c_backend_checkpoint_broker_accept_authenticated(
    broker: c_int,
    timeout_ms: c_int,
    host_pid: *mut u64,
    host_birth: *mut u64,
    host_generation: *mut u64,
    process_handle: *mut c_int,
) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.checkpoint_broker_accept_authenticated)
        .map_or(3, |function| unsafe {
            function(
                broker,
                timeout_ms,
                host_pid,
                host_birth,
                host_generation,
                process_handle,
            )
        })
}

#[cfg(unix)]
pub(super) unsafe fn hl_c_backend_checkpoint_trigger_create(
    descriptor: *mut c_int,
    mapping: *mut *mut c_void,
) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.checkpoint_trigger_create)
        .map_or(3, |function| unsafe { function(descriptor, mapping) })
}

#[cfg(unix)]
pub(super) unsafe fn hl_c_backend_checkpoint_trigger_bump(mapping: *mut c_void) -> c_uint {
    crate::loader::api()
        .ok()
        .and_then(|api| api.checkpoint_trigger_bump)
        .map_or(0, |function| unsafe { function(mapping) })
}

#[cfg(unix)]
pub(super) unsafe fn hl_c_backend_checkpoint_trigger_destroy(mapping: *mut c_void, descriptor: c_int) {
    if let Some(function) = crate::loader::api().ok().and_then(|api| api.checkpoint_trigger_destroy) {
        unsafe { function(mapping, descriptor) };
    }
}

#[cfg(unix)]
pub(super) unsafe fn hl_c_backend_checkpoint_adopt(isa: c_uint, broker: c_int, trigger: c_int) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.checkpoint_adopt)
        .map_or(3, |function| unsafe { function(isa, broker, trigger) })
}

#[cfg(unix)]
pub(super) unsafe fn hl_c_backend_checkpoint_interrupt_signal(isa: c_uint) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.checkpoint_interrupt_signal)
        .map_or(0, |function| unsafe { function(isa) })
}

#[cfg(unix)]
pub(super) unsafe fn hl_c_backend_checkpoint_configure(backend: *mut Backend, broker: c_int, trigger: c_int) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.checkpoint_configure)
        .map_or(3, |function| unsafe { function(backend, broker, trigger) })
}

#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn hl_c_backend_create(
    isa: c_uint,
    rootfs: *const c_char,
    executable_host: *const c_char,
    executable_fd: c_int,
    image_plan: *const MainImagePlan,
    interpreter_image: *const c_void,
    interpreter_size: usize,
    option_count: c_uint,
    option_names: *const *const c_char,
    option_values: *const *const c_char,
    standard_fds: *const c_int,
    provider_fd: c_int,
    syscall_context: *mut c_void,
    syscall_dispatch: Option<SyscallDispatch>,
    output: *mut *mut Backend,
) -> c_int {
    let Ok(api) = crate::loader::api() else {
        if !output.is_null() {
            unsafe { output.write(std::ptr::null_mut()) };
        }
        #[cfg(unix)]
        if provider_fd >= 0 {
            // SAFETY: create owns every nonnegative provider descriptor even when loading fails.
            unsafe { libc::close(provider_fd) };
        }
        return 3;
    };
    let Some(function) = api.create else {
        return 3;
    };
    unsafe {
        function(
            isa,
            rootfs,
            executable_host,
            executable_fd,
            image_plan,
            interpreter_image,
            interpreter_size,
            option_count,
            option_names,
            option_values,
            standard_fds,
            provider_fd,
            syscall_context,
            syscall_dispatch,
            output,
        )
    }
}

pub(super) unsafe fn hl_c_backend_run(backend: *mut Backend, argc: c_int, argv: *const *const c_char) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.run)
        .map_or(3, |function| unsafe { function(backend, argc, argv) })
}

pub(super) unsafe fn hl_c_backend_request(backend: *mut Backend, request: c_uint, signal: c_int) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.request)
        .map_or(3, |function| unsafe { function(backend, request, signal) })
}

pub(super) unsafe fn hl_c_backend_exit(backend: *mut Backend, result: *mut EngineExit) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.exit)
        .map_or(3, |function| unsafe { function(backend, result) })
}

pub(super) unsafe fn hl_c_backend_guest_pid(backend: *const Backend) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.guest_pid)
        .map_or(0, |function| unsafe { function(backend) })
}

pub(super) fn hl_c_backend_process_identity_signal(handle: c_int, host_pid: u64, signal: c_int) -> c_int {
    crate::loader::api()
        .ok()
        .and_then(|api| api.process_identity_signal)
        .map_or(-1, |function| {
            // SAFETY: the descriptor is borrowed for the call and C retains nothing.
            unsafe { function(handle, host_pid, signal) }
        })
}

pub(super) fn hl_c_backend_terminal_termios_generation() -> u64 {
    crate::loader::api()
        .ok()
        .and_then(|api| api.terminal_termios_generation)
        // SAFETY: the call takes no arguments and reads one counter.
        .map_or(0, |function| unsafe { function() })
}

pub(super) fn hl_c_backend_terminal_termios(native_fd: c_int, out: &mut [u8; 36]) -> bool {
    crate::loader::api()
        .ok()
        .and_then(|api| api.terminal_termios)
        .is_some_and(|function| {
            // SAFETY: `out` is a live 36-byte buffer for the duration of the call, which is exactly
            // the width the bridge documents; the descriptor is borrowed and C retains neither.
            unsafe { function(native_fd, out.as_mut_ptr()) == 1 }
        })
}

/// The host's own termios for `native_fd`, as a Linux image.
pub(super) fn hl_c_backend_terminal_termios_capture(native_fd: c_int, out: &mut [u8; 36]) -> bool {
    crate::loader::api()
        .ok()
        .and_then(|api| api.terminal_termios_capture)
        .is_some_and(|function| {
            // SAFETY: `out` is a live 36-byte buffer for the duration of the call, which is exactly
            // the width the bridge documents; the descriptor is borrowed and C retains neither.
            unsafe { function(native_fd, out.as_mut_ptr()) == 1 }
        })
}

/// Records `image` as the guest view of `native_fd` against the host projection as it stands now.
pub(super) fn hl_c_backend_terminal_termios_adopt(native_fd: c_int, image: &[u8; 36]) -> bool {
    crate::loader::api()
        .ok()
        .and_then(|api| api.terminal_termios_adopt)
        .is_some_and(|function| {
            // SAFETY: `image` is a live 36-byte buffer for the duration of the call, which is exactly
            // the width the bridge documents; the descriptor is borrowed and C copies rather than
            // retains.
            unsafe { function(native_fd, image.as_ptr()) == 1 }
        })
}

pub(super) unsafe fn hl_c_backend_destroy(backend: *mut Backend) {
    if let Some(function) = crate::loader::api().ok().and_then(|api| api.destroy) {
        unsafe { function(backend) };
    }
}

#[cfg(feature = "native-test-hooks")]
fn test_api() -> &'static crate::loader::TestApi {
    crate::loader::tests().unwrap_or_else(|error| panic!("native test bridge unavailable: {error}"))
}

#[cfg(feature = "native-test-hooks")]
#[allow(dead_code)]
pub(super) unsafe fn hl_c_backend_checkpoint_peer_authenticate_test(
    descriptor: c_int,
    claimed_pid: u64,
    host_pid: *mut u64,
    host_birth: *mut u64,
) -> c_int {
    unsafe { (test_api().checkpoint_peer_authenticate)(descriptor, claimed_pid, host_pid, host_birth) }
}

#[cfg(feature = "native-test-hooks")]
pub(super) unsafe fn hl_c_backend_checkpoint_channel_connect_test(broker_child: c_int) -> c_int {
    unsafe { (test_api().checkpoint_channel_connect)(broker_child) }
}

#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
pub(super) unsafe fn hl_c_backend_checkpoint_process_identity_open_test(
    pid: c_int,
    expected_birth: u64,
    expected_generation: u64,
    actual_birth: *mut u64,
    actual_generation: *mut u64,
) -> c_int {
    unsafe {
        (test_api().checkpoint_process_identity_open)(
            pid,
            expected_birth,
            expected_generation,
            actual_birth,
            actual_generation,
        )
    }
}

#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
pub(super) unsafe fn hl_c_backend_checkpoint_peer_identity_open_test(
    descriptor: c_int,
    claimed_pid: u64,
    actual_pid: *mut u64,
    actual_birth: *mut u64,
    actual_generation: *mut u64,
) -> c_int {
    unsafe {
        (test_api().checkpoint_peer_identity_open)(descriptor, claimed_pid, actual_pid, actual_birth, actual_generation)
    }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_bound_vector_io_test(
    scenario: c_uint,
    result: *mut i64,
    calls: *mut c_uint,
    bytes: *mut c_ulonglong,
) -> c_int {
    unsafe { (test_api().aarch64_bound_vector_io)(scenario, result, calls, bytes) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_bound_vector_io_test(
    scenario: c_uint,
    result: *mut i64,
    calls: *mut c_uint,
    bytes: *mut c_ulonglong,
) -> c_int {
    unsafe { (test_api().x86_64_bound_vector_io)(scenario, result, calls, bytes) }
}

#[cfg(all(test, feature = "native-test-hooks"))]
unsafe fn hl_aarch64_fdvis_path_publication_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_fdvis_path_publication)(scenario) }
}

#[cfg(all(test, feature = "native-test-hooks"))]
unsafe fn hl_x86_64_fdvis_path_publication_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_fdvis_path_publication)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_namespace_transaction_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_namespace_transaction)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_namespace_transaction_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_namespace_transaction)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_store_preflight_test() -> c_int {
    unsafe { (test_api().x86_64_store_preflight)() }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_reserved_register_test() -> c_int {
    unsafe { (test_api().aarch64_reserved_register)() }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_reserved_register_test() -> c_int {
    unsafe { (test_api().x86_64_reserved_register)() }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_signal_errno_frame_test(
    domain: c_uint,
    redirect: c_uint,
    nr: c_ulonglong,
    raw: i64,
    observed: *mut i64,
    completed: *mut i64,
) -> c_int {
    unsafe { (test_api().aarch64_signal_errno_frame)(domain, redirect, nr, raw, observed, completed) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_signal_errno_frame_test(
    domain: c_uint,
    redirect: c_uint,
    nr: c_ulonglong,
    raw: i64,
    observed: *mut i64,
    completed: *mut i64,
) -> c_int {
    unsafe { (test_api().x86_64_signal_errno_frame)(domain, redirect, nr, raw, observed, completed) }
}

macro_rules! test_no_argument {
    ($name:ident, $field:ident) => {
        #[cfg(feature = "native-test-hooks")]
        unsafe extern "C" fn $name() -> c_int {
            unsafe { (test_api().$field)() }
        }
    };
}

test_no_argument!(
    hl_aarch64_checkpoint_signal_precedence_test,
    aarch64_checkpoint_signal_precedence
);
test_no_argument!(
    hl_x86_64_checkpoint_signal_precedence_test,
    x86_64_checkpoint_signal_precedence
);
test_no_argument!(
    hl_aarch64_checkpoint_restart_register_test,
    aarch64_checkpoint_restart_register
);
test_no_argument!(
    hl_x86_64_checkpoint_restart_register_test,
    x86_64_checkpoint_restart_register
);
test_no_argument!(
    hl_aarch64_checkpoint_restore_rollback_test,
    aarch64_checkpoint_restore_rollback
);
test_no_argument!(hl_aarch64_terminal_termios_store_test, aarch64_terminal_termios_store);
test_no_argument!(hl_x86_64_terminal_termios_store_test, x86_64_terminal_termios_store);
test_no_argument!(
    hl_x86_64_checkpoint_restore_rollback_test,
    x86_64_checkpoint_restore_rollback
);

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_unix_identity_test(
    operation: c_uint,
    fd: c_int,
    object: c_ulonglong,
    local: *mut c_ulonglong,
    peer: *mut c_ulonglong,
    hidden: *mut c_uint,
) -> c_int {
    unsafe { (test_api().aarch64_unix_identity)(operation, fd, object, local, peer, hidden) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_unix_identity_test(
    operation: c_uint,
    fd: c_int,
    object: c_ulonglong,
    local: *mut c_ulonglong,
    peer: *mut c_ulonglong,
    hidden: *mut c_uint,
) -> c_int {
    unsafe { (test_api().x86_64_unix_identity)(operation, fd, object, local, peer, hidden) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_unix_identity_capture_test(fd: c_int) -> c_int {
    unsafe { (test_api().aarch64_unix_identity_capture)(fd) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_unix_identity_capture_test(fd: c_int) -> c_int {
    unsafe { (test_api().x86_64_unix_identity_capture)(fd) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_socket_shape_test(
    operation: c_uint,
    fd: c_int,
    capacity: c_uint,
    out: *mut u8,
    out_length: *mut c_uint,
) -> c_int {
    unsafe { (test_api().aarch64_socket_shape)(operation, fd, capacity, out, out_length) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_socket_shape_test(
    operation: c_uint,
    fd: c_int,
    capacity: c_uint,
    out: *mut u8,
    out_length: *mut c_uint,
) -> c_int {
    unsafe { (test_api().x86_64_socket_shape)(operation, fd, capacity, out, out_length) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_restore_claim_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_restore_claim)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_restore_claim_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_restore_claim)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_restore_slice_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_restore_slice)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_restore_slice_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_restore_slice)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_gmap_release_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_gmap_release)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_gmap_release_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_gmap_release)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_anon_shared_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_anon_shared)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_anon_shared_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_anon_shared)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_rendezvous_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_rendezvous)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_rendezvous_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_rendezvous)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_launch_identity_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_launch_identity)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_launch_identity_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_launch_identity)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_membership_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_membership)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_membership_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_membership)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_pid_namespace_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_pid_namespace)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_pid_namespace_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_pid_namespace)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_identity_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_identity)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_identity_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_identity)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_election_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_election)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_election_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_election)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_pipe_capture_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_pipe_capture)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_pipe_capture_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_pipe_capture)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_stdio_alias_capture_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_stdio_alias_capture)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_stdio_alias_capture_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_stdio_alias_capture)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_socket_halfclose_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_socket_halfclose)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_socket_halfclose_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_socket_halfclose)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_aarch64_checkpoint_ipc_admission_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().aarch64_checkpoint_ipc_admission)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_x86_64_checkpoint_ipc_admission_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().x86_64_checkpoint_ipc_admission)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_c_backend_errno_from_host_test(domain: c_uint, host_errno: c_int) -> c_int {
    unsafe { (test_api().errno_from_host)(domain, host_errno) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_c_backend_identity_registry_test(scenario: c_uint, iterations: c_uint) -> c_int {
    unsafe { (test_api().identity_registry)(scenario, iterations) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_c_backend_private_fork_lock_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().private_fork_lock)(scenario) }
}

#[cfg(feature = "native-test-hooks")]
unsafe fn hl_c_backend_process_identity_token_test(scenario: c_uint) -> c_int {
    unsafe { (test_api().process_identity_token)(scenario) }
}

#[cfg(all(test, feature = "native-test-hooks"))]
pub(super) unsafe fn hl_c_backend_checkpoint_test_prune_foreign_descriptors() -> c_uint {
    unsafe { (test_api().checkpoint_test_prune_foreign_descriptors)() }
}

#[cfg(all(test, feature = "native-test-hooks"))]
pub(super) unsafe fn hl_c_backend_checkpoint_test_fail_registry_allocation() {
    unsafe { (test_api().checkpoint_test_fail_registry_allocation)() };
}

#[cfg(all(test, feature = "native-test-hooks"))]
pub(super) unsafe fn hl_c_backend_checkpoint_test_fail_private_adopt(position: c_uint) {
    unsafe { (test_api().checkpoint_test_fail_private_adopt)(position) };
}

#[cfg(all(test, feature = "native-test-hooks"))]
pub(super) unsafe fn hl_c_backend_checkpoint_test_private_descriptor_count() -> u64 {
    unsafe { (test_api().checkpoint_test_private_descriptor_count)() }
}

#[cfg(all(test, feature = "native-test-hooks", unix))]
pub(super) unsafe fn hl_c_backend_host_process_force_test(pid: c_int) -> c_int {
    unsafe { (test_api().host_process_force)(pid) }
}

// Only `a_peer_that_leads_its_own_session_is_still_enumerated_as_a_peer` calls this, and that
// test is Linux-only because it reads the coordinator's /proc view; the C hook exists on both
// hosts, so the cfg tracks the caller rather than the symbol.
#[cfg(all(test, feature = "native-test-hooks", target_os = "linux"))]
pub(super) unsafe fn hl_c_backend_host_process_peer_enumerated_test(pid: c_int) -> c_int {
    unsafe { (test_api().host_process_peer_enumerated)(pid) }
}

#[cfg(all(test, feature = "native-test-hooks"))]
pub(super) unsafe fn hl_c_backend_activation_ready_pause(paused: c_int) {
    unsafe { (test_api().activation_ready_pause)(paused) };
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn bound_vector_io_test(isa: u32, scenario: u32) -> Result<(i64, u32, u64), i32> {
    let (mut result, mut calls, mut bytes) = (i64::MIN, u32::MAX, u64::MAX);
    let hook = match isa {
        1 => hl_aarch64_bound_vector_io_test,
        2 => hl_x86_64_bound_vector_io_test,
        _ => return Err(-22),
    };
    // SAFETY: the feature-gated C hook accepts writable scalar outputs and owns its fixture memory.
    let status = unsafe { hook(scenario, &raw mut result, &raw mut calls, &raw mut bytes) };
    if status == 0 {
        Ok((result, calls, bytes))
    } else {
        Err(status)
    }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn identity_registry_test(scenario: u32, iterations: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated native hook owns its private shared registry and child processes. Inputs are
    // scalar scenario controls, and the hook returns only after every child has been reaped.
    let status = unsafe { hl_c_backend_identity_registry_test(scenario, iterations) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn process_identity_token_test(scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated native hook reads only /proc records for this process and pid 1, and owns
    // the one child it forks, reaping it before returning. Its input is a scalar scenario selector.
    let status = unsafe { hl_c_backend_process_identity_token_test(scenario) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn private_fork_lock_test(scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated native hook owns its own descriptor, thread, and child process, and
    // returns only after the child has been reaped and the holder thread joined.
    let status = unsafe { hl_c_backend_private_fork_lock_test(scenario) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn namespace_transaction_test(isa: u32, scenario: u32) -> Result<(), i32> {
    let hook = match isa {
        1 => hl_aarch64_namespace_transaction_test,
        2 => hl_x86_64_namespace_transaction_test,
        _ => return Err(-22),
    };
    // SAFETY: each feature-gated hook owns its shared transaction fixture and
    // reaps every child before returning a scalar status.
    let status = unsafe { hook(scenario) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(all(test, feature = "native-test-hooks"))]
pub(crate) fn fdvis_path_publication_test(isa: u32, scenario: u32) -> bool {
    let hook = match isa {
        1 => hl_aarch64_fdvis_path_publication_test,
        2 => hl_x86_64_fdvis_path_publication_test,
        _ => return false,
    };
    // SAFETY: the feature-gated hook owns and restores its isolated descriptor-path fixture.
    unsafe { hook(scenario) == 1 }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn x86_store_preflight_test() -> bool {
    // SAFETY: the feature-gated hook owns its local emitter and CPU fixtures.
    unsafe { hl_x86_64_store_preflight_test() == 0 }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn aarch64_reserved_register_test() -> i32 {
    // SAFETY: the feature-gated hook owns its local emitter buffer and restores every global it moves.
    unsafe { hl_aarch64_reserved_register_test() }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn x86_reserved_register_test() -> i32 {
    // SAFETY: the feature-gated hook owns its local emitter buffer and restores every global it moves.
    unsafe { hl_x86_64_reserved_register_test() }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn linux_errno_from_host(domain: u32, host_errno: i32) -> i32 {
    // SAFETY: this pure test export accepts and returns one scalar value.
    unsafe { hl_c_backend_errno_from_host_test(domain, host_errno) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn signal_errno_frame_test(
    isa: u32,
    domain: u32,
    redirect: bool,
    nr: u64,
    raw: i64,
) -> Result<(i64, i64), i32> {
    let hook = match isa {
        1 => hl_aarch64_signal_errno_frame_test,
        2 => hl_x86_64_signal_errno_frame_test,
        _ => return Err(-22),
    };
    let (mut observed, mut completed) = (i64::MIN, i64::MIN);
    // SAFETY: the feature-gated hook owns its CPU fixture and writes two scalar outputs.
    let status = unsafe {
        hook(
            domain,
            u32::from(redirect),
            nr,
            raw,
            &raw mut observed,
            &raw mut completed,
        )
    };
    if status == 0 {
        Ok((observed, completed))
    } else {
        Err(status)
    }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_continuation_contract_test(isa: u32) -> Result<(), i32> {
    type Hook = unsafe extern "C" fn() -> c_int;
    let (signal, registers): (Hook, Hook) = match isa {
        1 => (
            hl_aarch64_checkpoint_signal_precedence_test,
            hl_aarch64_checkpoint_restart_register_test,
        ),
        2 => (
            hl_x86_64_checkpoint_signal_precedence_test,
            hl_x86_64_checkpoint_restart_register_test,
        ),
        _ => return Err(-22),
    };
    // SAFETY: feature-gated hooks own their local CPU fixtures and return scalar status only.
    for hook in [signal, registers] {
        let status = unsafe { hook() };
        if status != 0 {
            return Err(status);
        }
    }
    Ok(())
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_restore_claim_test(isa: u32, scenario: u32) -> Result<(), i32> {
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_restore_claim_test(scenario),
            2 => hl_x86_64_checkpoint_restore_claim_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

/// Exercise the restore-side host-page slicing that keeps a rounded claim off a neighbouring guest
/// region's already-claimed host page.
#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_restore_slice_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook reads only its own stack fixtures and returns a scalar.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_restore_slice_test(scenario),
            2 => hl_x86_64_checkpoint_restore_slice_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

/// Exercise the registry teardown a re-forked restorer runs before it claims its own image, and the
/// host-granularity rounding that teardown depends on.
#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_gmap_release_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook maps and releases only its own probe range and returns a scalar.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_gmap_release_test(scenario),
            2 => hl_x86_64_checkpoint_gmap_release_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

/// Exercise the anonymous `MAP_SHARED` identity and its restore-side republication.
#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_anon_shared_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook owns every mapping and descriptor it creates, unlinks the named
    // objects it publishes, runs its cross-process arm in a forked child it reaps, borrows no caller
    // memory, and returns a scalar.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_anon_shared_test(scenario),
            2 => hl_x86_64_checkpoint_anon_shared_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn pid_namespace_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook runs the scenario in a forked, setsid child it reaps, so the
    // process-wide container identity state it seeds never reaches this process. It borrows no caller
    // memory and returns a scalar.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_pid_namespace_test(scenario),
            2 => hl_x86_64_pid_namespace_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_identity_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook saves and restores the process-wide checkpoint identity state it
    // touches, runs the scenario in a forked child it reaps, borrows no caller memory, and returns a scalar.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_identity_test(scenario),
            2 => hl_x86_64_checkpoint_identity_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_election_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook mutates only process-wide checkpoint identity state it saves and
    // restores, forks nothing, opens no descriptor, borrows no caller memory, and returns a scalar status.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_election_test(scenario),
            2 => hl_x86_64_checkpoint_election_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_rendezvous_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook forks its own peer and its own responder, owns every descriptor it
    // opens and closes them, restores the process-wide checkpoint broker descriptor it swapped, reaps
    // every child it created, and returns a scalar status. It borrows no caller memory.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_rendezvous_test(scenario),
            2 => hl_x86_64_checkpoint_rendezvous_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_launch_identity_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook borrows no caller memory. It writes two engine statics, restores both
    // before returning, and answers a scalar status.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_launch_identity_test(scenario),
            2 => hl_x86_64_checkpoint_launch_identity_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_membership_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook forks its own orphan, owns every descriptor it opens, mutates only
    // the process-wide HL_PROCESS_DOMAIN option it also sets, kills the orphan it created, and returns a
    // scalar status. It borrows no caller memory.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_membership_test(scenario),
            2 => hl_x86_64_checkpoint_membership_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_pipe_capture_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook creates its own pipe and its own shared page, installs a sink that
    // owns no caller memory, restores the previous sink and the destructive-capture flag, and returns a
    // scalar status.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_pipe_capture_test(scenario),
            2 => hl_x86_64_checkpoint_pipe_capture_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_stdio_alias_capture_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook builds its own host services, descriptor tables and kernel pipes,
    // swaps the process-wide guest box for the duration of the call, restores it, releases everything it
    // created, borrows no caller memory, and returns a scalar status.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_stdio_alias_capture_test(scenario),
            2 => hl_x86_64_checkpoint_stdio_alias_capture_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_socket_halfclose_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook creates its own socketpair, installs a sink whose whole backing store
    // is its own static buffer, clears the identity it assigned, restores the previous sink and the
    // destructive-capture flag, and returns a scalar status.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_socket_halfclose_test(scenario),
            2 => hl_x86_64_checkpoint_socket_halfclose_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_ipc_admission_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated hook installs one object into the registry it is
    // exercising, evaluates the gate, and restores the previous contents before
    // returning a scalar status; it allocates nothing the caller owns.
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_ipc_admission_test(scenario),
            2 => hl_x86_64_checkpoint_ipc_admission_test(scenario),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_restore_rollback_test(isa: u32) -> Result<(), i32> {
    let status = unsafe {
        match isa {
            1 => hl_aarch64_checkpoint_restore_rollback_test(),
            2 => hl_x86_64_checkpoint_restore_rollback_test(),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn terminal_termios_install_test(fd: c_int, image: &[u8; 36]) {
    // SAFETY: `image` is a live 36-byte buffer for the call and C copies it before returning.
    unsafe { (test_api().aarch64_terminal_termios_install)(fd, image.as_ptr()) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn terminal_termios_store_test(isa: u32) -> Result<(), i32> {
    let status = unsafe {
        match isa {
            1 => hl_aarch64_terminal_termios_store_test(),
            2 => hl_x86_64_terminal_termios_store_test(),
            _ => return Err(-22),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn unix_identity_test(isa: u32, operation: u32, fd: i32, object: u64) -> Result<(u64, u64, u32), i32> {
    let mut local = 0;
    let mut peer = 0;
    let mut hidden = 0;
    let status = unsafe {
        match isa {
            1 => hl_aarch64_unix_identity_test(operation, fd, object, &raw mut local, &raw mut peer, &raw mut hidden),
            2 => hl_x86_64_unix_identity_test(operation, fd, object, &raw mut local, &raw mut peer, &raw mut hidden),
            _ => return Err(-libc::EINVAL),
        }
    };
    if status == 0 || ((operation == 1 || operation == 16) && status == 1) {
        Ok((local, peer, hidden))
    } else {
        Err(status)
    }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn socket_shape_test(isa: u32, operation: u32, fd: i32, capacity: u32, out: &mut [u8]) -> Result<u32, i32> {
    let mut length = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let status = unsafe {
        match isa {
            1 => hl_aarch64_socket_shape_test(operation, fd, capacity, out.as_mut_ptr(), &raw mut length),
            2 => hl_x86_64_socket_shape_test(operation, fd, capacity, out.as_mut_ptr(), &raw mut length),
            _ => return Err(libc::EINVAL),
        }
    };
    if status == 0 { Ok(length) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn unix_identity_capture_test(isa: u32, fd: i32) -> Result<(), i32> {
    let status = unsafe {
        match isa {
            1 => hl_aarch64_unix_identity_capture_test(fd),
            2 => hl_x86_64_unix_identity_capture_test(fd),
            _ => return Err(-libc::EINVAL),
        }
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

pub(super) fn engine_metadata_is_valid() -> bool {
    // SAFETY: both functions are immutable metadata queries exported by the
    // package-owned shared library and take no caller-provided pointers.
    unsafe { hl_engine_abi() == 5 && !hl_engine_version().is_null() }
}

#[cfg(unix)]
pub(super) fn engine_library_paths() -> Option<Vec<std::path::PathBuf>> {
    let path = crate::loader::path().ok()?.to_owned();
    Some(vec![path; 5])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    const STATUS_NOT_SUPPORTED: i32 = 3;

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn descriptor_path_publication_copies_and_clears_on_both_guest_isas() {
        for scenario in [0, 1, 2, 3, 4, 5, 6, 7] {
            assert!(fdvis_path_publication_test(1, scenario), "arm64 scenario {scenario}");
            assert!(fdvis_path_publication_test(2, scenario), "x86 scenario {scenario}");
        }
    }

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
                    ptr::null(),
                    0,
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
            let mut exit = EngineExit {
                abi: 5,
                size: size_of::<EngineExit>() as u32,
                kind: 0,
                guest_status: 0,
                detail: 0,
            };
            assert_ne!(hl_c_backend_exit(ptr::null_mut(), &raw mut exit), 0);
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
                ptr::null(),
                0,
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

    #[cfg(unix)]
    #[test]
    fn unsupported_provider_descriptor_is_consumed_after_output_is_cleared() {
        const PARKED_DESCRIPTOR: c_int = 900;
        let mut descriptors = [-1; 2];
        // SAFETY: the array has room for both descriptors produced by pipe.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        // Consumption is a property of *this* process's descriptor table, and it is read back from a
        // parked descriptor number rather than from the pipe. Two weaker proofs have already been
        // tried here and both are unsound:
        //
        //   * `fcntl(F_GETFD)` on the number `pipe` handed out. The kernel hands out the lowest free
        //     number, so a sibling test thread that opens anything between the call below and the
        //     check can be given exactly that number back, and the check then reads a live descriptor
        //     and passes.
        //   * writing to the other end of the pipe and demanding `EPIPE`. That asks whether *any*
        //     process still holds the read end, which is a strictly weaker question. Measured on
        //     macOS: with `armed_running_guest_reaches_checkpoint_broker` running concurrently, the
        //     write succeeds while `fcntl` on the parked number answers `EBADF` -- create had
        //     consumed our copy, and a child forked by the engine under test was holding an inherited
        //     one. The pipe cannot distinguish that from a leak.
        //
        // `F_DUPFD` with a high minimum answers the right question: it never clobbers an occupied
        // slot, and because allocation is lowest-free the parked number cannot be reissued to a
        // sibling thread unless the whole table below it fills, which no test here approaches.
        // SAFETY: both descriptors are owned here; `F_DUPFD` returns a fresh number at or above the
        // minimum without disturbing whatever occupies it.
        let parked = unsafe { libc::fcntl(descriptors[0], libc::F_DUPFD, PARKED_DESCRIPTOR) };
        assert!(parked >= PARKED_DESCRIPTOR, "parking the provider descriptor failed");
        // SAFETY: the original read end is owned here and is replaced by the parked duplicate.
        assert_eq!(unsafe { libc::close(descriptors[0]) }, 0);
        descriptors[0] = parked;
        let mut output = ptr::dangling_mut::<Backend>();
        // SAFETY: output is writable. Provider rejection happens before the deliberately invalid image inputs
        // are inspected, and ownership of descriptors[0] transfers to this call.
        let status = unsafe {
            hl_c_backend_create(
                0,
                ptr::null(),
                ptr::null(),
                -1,
                ptr::null(),
                ptr::null(),
                0,
                0,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                descriptors[0],
                ptr::null_mut(),
                None,
                &raw mut output,
            )
        };
        assert_eq!(status, STATUS_NOT_SUPPORTED);
        assert!(output.is_null());
        // SAFETY: reads the flags of the parked number, which this test no longer owns.
        let flags = unsafe { libc::fcntl(parked, libc::F_GETFD) };
        let error = std::io::Error::last_os_error().raw_os_error();
        assert_eq!(
            (flags, error),
            (-1, Some(libc::EBADF)),
            "create must consume the provider descriptor it rejected"
        );
        // SAFETY: the write end remains owned by this test.
        assert_eq!(unsafe { libc::close(descriptors[1]) }, 0);
    }
}
