//! The engine's feature-gated diagnostic surface.
//!
//! Every export here exists only under `native-test-hooks`, and the C engine only defines the
//! symbols behind them under `HL_NATIVE_TEST_HOOKS`. They drive one C mechanism apiece from an
//! integration test — a scenario number in, a status out — and none of them is reachable from a
//! product build, which is what keeps [`crate`]'s facade the small production one it describes.

use crate::bindings;

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn bound_vector_io_test(isa: u32, scenario: u32) -> Result<(i64, u32, u64), i32> {
    // The production target runs each guest in its own child process. This test hook runs both target TUs
    // in the test process, where they deliberately share the process-global logical-VMA ledger.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::bound_vector_io_test(isa, scenario)
}

/// Drives the private-fd fork-lock scenario in the C engine.
///
/// Gated on the feature alone, like [`bindings`] and the loader field it reaches: the C engine defines
/// this export on every host with the hooks compiled in, refusing on the Windows arm rather than
/// omitting the symbol, and a module narrower than its own binding is what left the two disagreeing.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn private_fork_lock_test(scenario: u32) -> Result<(), i32> {
    bindings::private_fork_lock_test(scenario)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn process_identity_token_test(scenario: u32) -> Result<(), i32> {
    bindings::process_identity_token_test(scenario)
}

/// Drives `F_SETFL(O_APPEND)` on an adopted descriptor followed by a write, in the C engine.
///
/// Gated on the feature alone, for the reason given on [`private_fork_lock_test`].
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn setfl_append_write_test(scenario: u32) -> Result<(), i32> {
    bindings::setfl_append_write_test(scenario)
}

#[cfg(any(feature = "native-test-hooks", windows))]
#[doc(hidden)]
pub fn identity_registry_test(scenario: u32, iterations: u32) -> Result<(), i32> {
    #[cfg(windows)]
    {
        let _ = (scenario, iterations);
        return Err(libc::ENOTSUP);
    }
    #[cfg(not(windows))]
    {
        // Each scenario owns process-global fault injection state in the C test boundary.
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        bindings::identity_registry_test(scenario, iterations)
    }
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn namespace_transaction_test(isa: u32, scenario: u32) -> Result<(), i32> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::namespace_transaction_test(isa, scenario)
}

/// Reports whether the emitted direct-store guards preflight their exact span.
///
/// Returns `0` when every emitted guard preflights the span it stores, `1` when
/// one does not, and `4` when the host has no emitters to inspect -- the
/// lowerings this reads emit ARM64 and are compiled only on an AArch64 host.
/// `4` is deliberately not `0`: a clean zero would claim a scan that never ran.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn x86_store_preflight_test() -> i32 {
    bindings::x86_store_preflight_test()
}

/// Reports whether emitted aarch64 code keeps a live value in host `x18`.
///
/// Returns `0` when the emitted guard and guest-base fold name no reserved
/// register, `1` when one of them does, and `2`/`3` when the fixture emitted
/// nothing to inspect (a vacuous pass).
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn aarch64_reserved_register_test() -> i32 {
    bindings::aarch64_reserved_register_test()
}

/// Reports whether emitted x86-64 guest code keeps a live value in host `x18`.
///
/// Returns `0` when the flag lowerings name no reserved register, `1` when one
/// of them does, `2` when a sub-fixture emitted nothing, and `3` when the
/// witness instructions the scan is built around are absent (a vacuous pass).
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn x86_reserved_register_test() -> i32 {
    bindings::x86_reserved_register_test()
}

/// Reports whether an imported pathname operand still gets judged as a guest pointer.
///
/// `svc_fs` copies the guest pathname into engine storage before dispatch and
/// reaches the guest's `EFAULT` there. The fixture arms the production state
/// that broke `npm ci` -- a guest `PROT_NONE` ledger interval covering the
/// engine's own C stack -- and runs `fchmodat` over an absent pathname.
/// Returns `0` when the filesystem verdict (`ENOENT`) survives, `1` for the
/// spurious `EFAULT`, `2` for any other verdict, and `5` when the fixture could
/// not allocate its stand-in guest pathname (a vacuous pass).
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn aarch64_imported_path_guard_test() -> i32 {
    bindings::aarch64_imported_path_guard_test()
}

/// The x86-64 arm of [`aarch64_imported_path_guard_test`]; both target
/// translation units compile the same syscall layer, so both must answer.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn x86_imported_path_guard_test() -> i32 {
    bindings::x86_imported_path_guard_test()
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn linux_errno_from_host(domain: u32, host_errno: i32) -> i32 {
    bindings::linux_errno_from_host(domain, host_errno)
}

/// Checks that the macOS host's directory streams live in the engine-private descriptor band.
#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
#[doc(hidden)]
#[must_use]
pub fn directory_stream_private_test(scenario: u32) -> i32 {
    bindings::directory_stream_private_test(scenario)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn signal_errno_frame_test(isa: u32, domain: u32, redirect: bool, nr: u64, raw: i64) -> Result<(i64, i64), i32> {
    bindings::signal_errno_frame_test(isa, domain, redirect, nr, raw)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_continuation_contract_test(isa: u32) -> Result<(), i32> {
    bindings::checkpoint_continuation_contract_test(isa)
}

/// Serializes the restore hooks that mutate this process's own address space.
///
/// `hl_gmap_add`, `hl_gmap_reset` and `hl_gmap_unmap_range` share one process-wide guest-mapping table
/// across both guest-ISA namespaces -- `linux_abi/container/vfs/gmap.c` is a single translation unit, not
/// a per-target one -- and the entries it releases name host pages it then unmaps. Two of these hooks on
/// two libtest threads therefore tear each other's mappings down: measured on `x86_64` Linux as an
/// intermittent SIGSEGV in the test binary, roughly one run in three. The scenarios are unchanged; only
/// their concurrency is.
#[cfg(feature = "native-test-hooks")]
static GUEST_MAPPING_REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "native-test-hooks")]
fn guest_mapping_registry() -> std::sync::MutexGuard<'static, ()> {
    GUEST_MAPPING_REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_restore_claim_test(isa: u32, scenario: u32) -> Result<(), i32> {
    let _registry = guest_mapping_registry();
    bindings::checkpoint_restore_claim_test(isa, scenario)
}

/// Exercise the restore-side host-page slicing: scenario 0 is the observed Apple Silicon collision,
/// 1 and 2 are the file-backed refusals, 3 is the untouched single-claim case.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_restore_slice_test(isa: u32, scenario: u32) -> Result<(), i32> {
    bindings::checkpoint_restore_slice_test(isa, scenario)
}

/// Exercise the guest-mapping registry teardown a re-forked restorer runs before it claims its own image:
/// scenario 0 measures the real registry against the real host, scenario 1 pins the host-granularity
/// rounding against an explicit 16 KiB page so a 4 KiB host answers it too.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_gmap_release_test(isa: u32, scenario: u32) -> Result<(), i32> {
    let _registry = guest_mapping_registry();
    bindings::checkpoint_gmap_release_test(isa, scenario)
}

/// Exercise anonymous `MAP_SHARED` capture identity and restore-side republication: scenario 0 checks the
/// identity and the shared-vs-private discriminator, scenario 1 proves one object serves two processes.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_anon_shared_test(isa: u32, scenario: u32) -> Result<(), i32> {
    bindings::checkpoint_anon_shared_test(isa, scenario)
}

/// Exercise the container PID namespace: the launch top is guest pid 1 with no parent inside the
/// namespace, a forked child gets the next namespace-local pid rather than its host pid, and a host
/// process that is not a container member has no guest rendering at all.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn pid_namespace_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook seeds the process-wide container identity state in a forked child.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::pid_namespace_test(isa, scenario)
}

/// Exercise coordinator election: with one trigger word and one broker serving a whole process domain,
/// exactly one process may coordinate and every other must commit a group of its own under the name the
/// coordinator waits for.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_identity_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook mutates process-wide checkpoint identity state and the restore process table.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_identity_test(isa, scenario)
}

/// Exercise the identity a captured member records against the tree model that decides whether the image
/// can be restored: a container exec session's top process must record the gpid its group is named with,
/// and the container process domain it belongs to in place of a parent edge it does not have.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_election_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook mutates process-wide checkpoint identity state and the HL_CHECKPOINT_COORDINATOR option.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_election_test(isa, scenario)
}

/// Exercise the rendezvous exemption: whether the coordinator drops a peer that vanished before it could
/// contribute anything, and whether it still refuses for a peer that registered and then died.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_rendezvous_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook forks and swaps the process-wide checkpoint broker descriptor.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_rendezvous_test(isa, scenario)
}

/// Exercise which LAUNCH a capturing process belongs to: that a container `exec` session's top process is
/// recognised as a domain root by the launch that created it rather than by the guest pid it was handed,
/// and that an ordinary forked descendant of that same launch is still not one.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_launch_identity_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook writes and restores process-wide container identity statics.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_launch_identity_test(isa, scenario)
}

/// Exercise capture membership: whether the coordinator's peer filter admits a container `exec` session --
/// a SIBLING of guest pid 1 -- and refuses a live process of the same executable that belongs to another
/// container's process domain or to none.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_membership_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook forks and mutates the process-wide HL_PROCESS_DOMAIN option.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_membership_test(isa, scenario)
}

/// Exercise pipe capture: the image-wide election among the holders of one inherited pipe, the byte
/// fidelity of the drain, and the abort contract every holder falls under once it has run.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_pipe_capture_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook installs a process-wide checkpoint sink and forks.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_pipe_capture_test(isa, scenario)
}

/// Exercise the stdio-alias decision under capture: that a guest descriptor sharing an open file
/// description with the runtime's own stdin/stdout/stderr is captured as stdio rather than refused as a
/// shared pipe, and that a pipe the guest created is still refused.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_stdio_alias_capture_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook swaps the process-wide guest descriptor table for the duration of the call.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_stdio_alias_capture_test(isa, scenario)
}

/// Exercise socket half-close across the checkpoint: that capture records the direction each endpoint
/// closed, that a drain reading end-of-stream from a peer that is still open does not record it as closed,
/// that a half-closed endpoint is admissible, and that the replay reproduces the kernel state measured on
/// the bare host.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_socket_halfclose_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook installs a process-wide checkpoint sink and writes the shared socket-identity arena.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_socket_halfclose_test(isa, scenario)
}

/// Exercise the checkpoint admission gate for SysV IPC and file-lock state.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_ipc_admission_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // The hook mutates the process-wide SysV registry and record-lock table.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::checkpoint_ipc_admission_test(isa, scenario)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_restore_rollback_test(isa: u32) -> Result<(), i32> {
    let _registry = guest_mapping_registry();
    bindings::checkpoint_restore_rollback_test(isa)
}

/// Exercise the engine-owned per-terminal termios store for `isa`.
///
/// The store is process-wide within each guest-ISA namespace, so the check
/// serializes against itself.
/// Install `image` as the guest view of `descriptor` in the aarch64 store, so a test can then read
/// it back through the bridge and check the whole path rather than only the C side of it.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn terminal_termios_install_test(descriptor: std::ffi::c_int, image: &[u8; 36]) {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::terminal_termios_install_test(descriptor, image);
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn terminal_termios_store_test(isa: u32) -> Result<(), i32> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::terminal_termios_store_test(isa)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn unix_identity_test(isa: u32, operation: u32, fd: i32, object: u64) -> Result<(u64, u64, u32), i32> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::unix_identity_test(isa, operation, fd, object)
}

/// The host->guest sockaddr and control-message translations, without a guest.
///
/// `operation` selects `getsockname` (0), `getpeername` (1), `recvmsg` into `capacity` host control
/// bytes (2), or the AF_UNIX datagram buffer policy (3). `out` receives the guest-visible bytes and the
/// returned length is the guest-visible length.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn socket_shape_test(isa: u32, operation: u32, fd: i32, capacity: u32, out: &mut [u8]) -> Result<u32, i32> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::socket_shape_test(isa, operation, fd, capacity, out)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn unix_identity_capture_test(isa: u32, fd: i32) -> Result<(), i32> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::unix_identity_capture_test(isa, fd)
}

/// Adopts the liveness capability a macOS identity hook minted, or reports why the host refused.
///
/// Both hooks below answer with a `kqueue` descriptor they opened themselves and registered on
/// `EVFILT_PROC`, or with -1 and `errno`. The adoption is the same on both paths and lives here once so
/// it carries one rationale rather than two copies of the same one.
#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
#[allow(unsafe_code)]
fn adopt_identity_capability(capability: i32) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    if capability < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a non-negative answer is a descriptor `hl_host_process_identity_open` obtained from
    // `kqueue()` and has already set `FD_CLOEXEC` on; it hands the only reference to this caller and
    // retains none, and it closes the descriptor itself on every path that returns -1. So this process
    // is the sole owner and `OwnedFd` closes it exactly once.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(capability) })
}

#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
#[doc(hidden)]
#[allow(unsafe_code)]
pub fn checkpoint_process_identity_open_test(
    pid: i32,
    expected_birth: u64,
    expected_generation: u64,
) -> std::io::Result<(std::os::fd::OwnedFd, u64, u64)> {
    let mut birth = 0;
    let mut generation = 0;
    // SAFETY: the hook is a resolved export of the loaded engine taking two out-parameters. `birth` and
    // `generation` are live locals for the whole call, written only through these exclusive pointers and
    // read only after it returns; the C sets them to zero before it can fail, so no path leaves them
    // uninitialised. `pid` names a process this caller does not own, which is why the hook fences the
    // answer to `expected_birth`/`expected_generation` rather than trusting the number. It is C to its
    // depth and cannot unwind.
    let capability = unsafe {
        bindings::hl_c_backend_checkpoint_process_identity_open_test(
            pid,
            expected_birth,
            expected_generation,
            &raw mut birth,
            &raw mut generation,
        )
    };
    Ok((adopt_identity_capability(capability)?, birth, generation))
}

#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
#[doc(hidden)]
#[allow(unsafe_code)]
pub fn checkpoint_peer_identity_open_test(
    descriptor: std::os::fd::RawFd,
    claimed_pid: u64,
) -> std::io::Result<(std::os::fd::OwnedFd, u64, u64, u64)> {
    let mut pid = 0;
    let mut birth = 0;
    let mut generation = 0;
    // SAFETY: three live locals behind three exclusive out-pointers, zeroed by the C before any failure
    // path, as above. What differs here is `descriptor`: the hook reads `LOCAL_PEERTOKEN` off it twice,
    // around the mint, so the caller must keep that socket open across the call -- it is a `RawFd`
    // borrowed from an owner the caller still holds, never one this function may close. The hook closes
    // the capability itself if the token moved between the two reads.
    let capability = unsafe {
        bindings::hl_c_backend_checkpoint_peer_identity_open_test(
            descriptor,
            claimed_pid,
            &raw mut pid,
            &raw mut birth,
            &raw mut generation,
        )
    };
    Ok((adopt_identity_capability(capability)?, pid, birth, generation))
}
