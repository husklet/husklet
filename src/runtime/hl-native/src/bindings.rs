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

/// Defines one call into the engine's resolved export table.
///
/// Every entry shares a single safety argument, so it is stated here once rather than repeated
/// verbatim at each site. `loader::api()` publishes the table only after the shared object's ABI
/// version and build fingerprint have been checked against this binary, so a `Some(function)` is
/// an export of the declared signature belonging to an object that stays mapped for the rest of
/// the process; no Rust storage is referenced, so nothing can be dropped underneath it. Each
/// generated function is itself `unsafe`, so its caller carries the validity, lifetime, alignment
/// and aliasing of the pointer arguments it forwards, and the engine may be entered concurrently
/// because every export takes its state through those arguments rather than through process
/// globals. The C side reports failure as a returned status and never unwinds, so no panic
/// crosses the boundary; an object that does not export the entry yields the absent value.
macro_rules! engine_entry {
    ($(#[$attribute:meta])* $name:ident($($argument:ident: $type:ty),* $(,)?) -> $result:ty, $absent:expr, $field:ident) => {
        $(#[$attribute])*
        pub(super) unsafe fn $name($($argument: $type),*) -> $result {
            crate::loader::api()
                .ok()
                .and_then(|api| api.$field)
                // SAFETY: a published table entry is a live export of this signature; the caller of
                // this `unsafe fn` owns the arguments it forwards. Stated in full on `engine_entry!`.
                .map_or($absent, |function| unsafe { function($($argument),*) })
        }
    };
    ($(#[$attribute:meta])* $name:ident($($argument:ident: $type:ty),* $(,)?), $field:ident) => {
        $(#[$attribute])*
        pub(super) unsafe fn $name($($argument: $type),*) {
            if let Some(function) = crate::loader::api().ok().and_then(|api| api.$field) {
                // SAFETY: a published table entry is a live export of this signature; the caller of
                // this `unsafe fn` owns the arguments it forwards. Stated in full on `engine_entry!`.
                unsafe { function($($argument),*) };
            }
        }
    };
}

engine_entry!(hl_engine_abi() -> c_uint, 0, engine_abi);

engine_entry!(hl_engine_version() -> *const c_char, std::ptr::null(), engine_version);

engine_entry!(hl_c_backend_leak_check_nonvacuity() -> c_int, 3, leak_check_nonvacuity);

engine_entry!(#[cfg(unix)]
hl_c_backend_checkpoint_broker_pair(parent: *mut c_int, child: *mut c_int) -> c_int, 3, checkpoint_broker_pair);

engine_entry!(#[cfg(unix)]
#[allow(dead_code)]
hl_c_backend_checkpoint_broker_accept(broker: c_int, timeout_ms: c_int, host_pid: *mut u64) -> c_int, 3, checkpoint_broker_accept);

engine_entry!(#[cfg(unix)]
hl_c_backend_checkpoint_broker_accept_authenticated(broker: c_int, timeout_ms: c_int, host_pid: *mut u64, host_birth: *mut u64, host_generation: *mut u64, process_handle: *mut c_int) -> c_int, 3, checkpoint_broker_accept_authenticated);

engine_entry!(#[cfg(unix)]
hl_c_backend_checkpoint_trigger_create(descriptor: *mut c_int, mapping: *mut *mut c_void) -> c_int, 3, checkpoint_trigger_create);

engine_entry!(#[cfg(unix)]
hl_c_backend_checkpoint_trigger_bump(mapping: *mut c_void) -> c_uint, 0, checkpoint_trigger_bump);

engine_entry!(#[cfg(unix)]
hl_c_backend_checkpoint_trigger_destroy(mapping: *mut c_void, descriptor: c_int), checkpoint_trigger_destroy);

engine_entry!(#[cfg(unix)]
hl_c_backend_checkpoint_adopt(isa: c_uint, broker: c_int, trigger: c_int) -> c_int, 3, checkpoint_adopt);

engine_entry!(#[cfg(unix)]
hl_c_backend_checkpoint_interrupt_signal(isa: c_uint) -> c_int, 0, checkpoint_interrupt_signal);

engine_entry!(#[cfg(unix)]
hl_c_backend_checkpoint_configure(backend: *mut Backend, broker: c_int, trigger: c_int) -> c_int, 3, checkpoint_configure);

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
            // SAFETY: `output` is the caller's out-parameter, checked non-null here and required by
            // this function's contract to be a writable, aligned `*mut Backend` slot owned by the
            // caller for the call. Clearing it is the failure half of that contract: the caller must
            // not read a stale handle out of a create that never ran.
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
    // SAFETY: `function` is a resolved export of the loaded engine, whose ABI version and build
    // fingerprint the loader checked before publishing it, and the object stays mapped for the rest
    // of the process. Every pointer below belongs to the caller of this `unsafe fn`, which its
    // contract requires to keep them valid for the call; the engine copies the plan, the option
    // vectors and the descriptor table before returning, takes ownership of `provider_fd`, and
    // stores the new handle through `output`. It reports failure as a status and never unwinds.
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

engine_entry!(hl_c_backend_run(backend: *mut Backend, argc: c_int, argv: *const *const c_char) -> c_int, 3, run);

engine_entry!(hl_c_backend_request(backend: *mut Backend, request: c_uint, signal: c_int) -> c_int, 3, request);

engine_entry!(hl_c_backend_exit(backend: *mut Backend, result: *mut EngineExit) -> c_int, 3, exit);

engine_entry!(hl_c_backend_guest_pid(backend: *const Backend) -> c_int, 0, guest_pid);

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

engine_entry!(hl_c_backend_destroy(backend: *mut Backend), destroy);

#[cfg(feature = "native-test-hooks")]
#[path = "test_hooks.rs"]
mod test_hooks;
#[cfg(feature = "native-test-hooks")]
pub(crate) use test_hooks::*;

#[cfg(feature = "native-test-hooks")]
#[path = "checkpoint_hooks.rs"]
mod checkpoint_hooks;
#[cfg(feature = "native-test-hooks")]
pub(crate) use checkpoint_hooks::*;

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
        for scenario in [0, 1, 2, 3, 4, 5, 6] {
            assert!(fdvis_path_publication_test(1, scenario), "arm64 scenario {scenario}");
            assert!(fdvis_path_publication_test(2, scenario), "x86 scenario {scenario}");
        }
        // 8: an abandoned reservation whose holder is gone is reclaimed -- until reserver_pid existed
        // nothing in the tree could reclaim one, because the UINT64_MAX marker decodes to owner -1 and
        // both sweeps skip a non-positive owner. 9: our own live reservation survives a sweep.
        // 10: a reservation with no recorded holder is left alone, which is what keeps an in-flight
        // proc_fdvis_fork_prepare() plan from being stolen out from under the fork.
        // 11 forks, SIGKILLs and waits for state 'Z' before sweeping, which is the only case that
        // separates "does this owner still exist" from "can this owner still run" -- 8 uses a pid the
        // kernel never issued and 9 uses this live process, and the old token comparison answers both
        // the same way. Without it the corpse-aware predicate has no probe that can fail.
        for scenario in [8, 9, 10] {
            assert!(
                fdvis_path_publication_test(1, scenario),
                "arm64 reservation scenario {scenario}"
            );
            assert!(
                fdvis_path_publication_test(2, scenario),
                "x86 reservation scenario {scenario}"
            );
        }
        // Windows cannot stage a zombie and the C hook explicitly refuses this arm there rather than
        // pretending it ran. Keep the unsupported host visible at the call site, as scenario 7 does.
        #[cfg(not(windows))]
        {
            assert!(fdvis_path_publication_test(1, 11), "arm64 reservation scenario 11");
            assert!(fdvis_path_publication_test(2, 11), "x86 reservation scenario 11");
        }
        // Scenario 7's second half forks and rendezvouses through a MAP_SHARED page, which the
        // Windows arm of path_runtime.c refuses. Naming the host here rather than dropping the
        // scenario keeps the refusal visible: the C hook returns 0 there, so this assertion would
        // fail rather than silently pass, and a Windows run says which case it did not cover.
        #[cfg(not(windows))]
        {
            assert!(fdvis_path_publication_test(1, 7), "arm64 scenario 7");
            assert!(fdvis_path_publication_test(2, 7), "x86 scenario 7");
        }
        #[cfg(windows)]
        assert!(
            !fdvis_path_publication_test(1, 7),
            "scenario 7 forks; the Windows arm must refuse rather than report a pass"
        );
        // Scenario 8 stages the double-fork shape: a lock holder killed inside its critical section and
        // deliberately left uncollected, while a second child asks for the same lock. It carries its own
        // deadline, so an engine that can only reclaim from a COLLECTED owner fails here rather than
        // spinning a core forever the way a guest does. Same host split as scenario 7.
        #[cfg(not(windows))]
        {
            assert!(fdvis_path_publication_test(1, 8), "arm64 scenario 8");
            assert!(fdvis_path_publication_test(2, 8), "x86 scenario 8");
        }
        #[cfg(windows)]
        assert!(
            !fdvis_path_publication_test(1, 8),
            "scenario 8 forks; the Windows arm must refuse rather than report a pass"
        );
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
