//! Cargo-owned build and linkage boundary for Husklet's native C engine.
//!
//! The package exposes a deliberately small Rust facade. The C layout and its
//! host-service callback tables remain private implementation details so that
//! individual service groups can later move to Rust without changing callers.

mod bindings;
#[cfg(test)]
mod build_support;
#[cfg(unix)]
mod checkpoint;
mod engine;
mod loader;
mod provider;

#[cfg(test)]
mod artifact;

#[cfg(unix)]
pub use checkpoint::{AuthenticatedCheckpointPeer, CheckpointBroker, CheckpointTransport};
pub use engine::{Engine, EngineConfig, Error, Exit};
pub use loader::{LoadError, LoadKind};
#[cfg(unix)]
pub use provider::artifact_lifecycle_smoke;
pub use provider::leak_check_nonvacuity;

/// Verifies that the dynamically loaded private engine exposes the ABI this Rust wrapper expects.
///
/// This hidden packaging probe crosses the real C boundary after artifact relocation.
#[doc(hidden)]
#[must_use]
pub fn artifact_smoke() -> bool {
    bindings::engine_metadata_is_valid()
}

/// Reports why the private engine failed to load, if it did.
///
/// `artifact_smoke` answers a bool, which is the wrong shape for a build-freshness failure:
/// the whole value of that diagnosis is the two fingerprints it names.
#[doc(hidden)]
#[must_use]
pub fn artifact_load_error() -> Option<&'static LoadError> {
    loader::path().err()
}

/// Returns the exact dynamic export contract for the Cargo-selected native library.
#[doc(hidden)]
#[must_use]
pub const fn artifact_export_manifest() -> &'static str {
    #[cfg(feature = "native-test-hooks")]
    {
        include_str!("native/bridge/test_exports.txt")
    }
    #[cfg(not(feature = "native-test-hooks"))]
    {
        include_str!("native/bridge/exports.txt")
    }
}

/// Returns the target-native filename of the private engine artifact selected by Cargo.
#[doc(hidden)]
#[must_use]
pub const fn artifact_filename() -> &'static str {
    env!("HL_NATIVE_LIBRARY_NAME")
}

/// Resolves the shared objects that supplied the linked engine lifecycle symbols.
#[cfg(unix)]
#[doc(hidden)]
#[must_use]
pub fn artifact_paths() -> Option<Vec<std::path::PathBuf>> {
    bindings::engine_library_paths()
}

/// Calls the private executable-authority boundary through the versioned bridge table.
#[doc(hidden)]
#[allow(unsafe_code)]
pub unsafe fn executable_authority_open_test(
    services: *const std::ffi::c_void,
    path: *const std::ffi::c_char,
    output: *mut std::ffi::c_void,
) -> i32 {
    let api = loader::api().expect("load native engine for executable-authority test");
    let function = api.executable_open.expect("validated executable_open");
    // SAFETY: this hidden ABI probe forwards the caller's C-compatible test records unchanged.
    unsafe { function(services, path, output) }
}

/// Discards a private executable authority through the versioned bridge table.
#[doc(hidden)]
#[allow(unsafe_code)]
pub unsafe fn executable_authority_discard_test(services: *const std::ffi::c_void, executable: *mut std::ffi::c_void) {
    let api = loader::api().expect("load native engine for executable-authority test");
    let function = api.executable_discard.expect("validated executable_discard");
    // SAFETY: this hidden ABI probe forwards the caller's C-compatible test records unchanged.
    unsafe { function(services, executable) };
}

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
#[cfg(all(unix, feature = "native-test-hooks"))]
#[doc(hidden)]
pub fn private_fork_lock_test(scenario: u32) -> Result<(), i32> {
    bindings::private_fork_lock_test(scenario)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn process_identity_token_test(scenario: u32) -> Result<(), i32> {
    bindings::process_identity_token_test(scenario)
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

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn x86_store_preflight_test() -> bool {
    bindings::x86_store_preflight_test()
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn linux_errno_from_host(domain: u32, host_errno: i32) -> i32 {
    bindings::linux_errno_from_host(domain, host_errno)
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

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_restore_claim_test(isa: u32, scenario: u32) -> Result<(), i32> {
    bindings::checkpoint_restore_claim_test(isa, scenario)
}

/// Exercise the restore-side host-page slicing: scenario 0 is the observed Apple Silicon collision,
/// 1 and 2 are the file-backed refusals, 3 is the untouched single-claim case.
#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_restore_slice_test(isa: u32, scenario: u32) -> Result<(), i32> {
    bindings::checkpoint_restore_slice_test(isa, scenario)
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
pub fn terminal_termios_install_test(descriptor: std::os::fd::RawFd, image: &[u8; 36]) {
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

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn unix_identity_capture_test(isa: u32, fd: i32) -> Result<(), i32> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::unix_identity_capture_test(isa, fd)
}

#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
#[doc(hidden)]
#[allow(unsafe_code)]
pub fn checkpoint_process_identity_open_test(
    pid: i32,
    expected_birth: u64,
    expected_generation: u64,
) -> std::io::Result<(std::os::fd::OwnedFd, u64, u64)> {
    use std::os::fd::FromRawFd;
    let mut birth = 0;
    let mut generation = 0;
    let descriptor = unsafe {
        bindings::hl_c_backend_checkpoint_process_identity_open_test(
            pid,
            expected_birth,
            expected_generation,
            &raw mut birth,
            &raw mut generation,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        unsafe { std::os::fd::OwnedFd::from_raw_fd(descriptor) },
        birth,
        generation,
    ))
}

#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
#[doc(hidden)]
#[allow(unsafe_code)]
pub fn checkpoint_peer_identity_open_test(
    descriptor: std::os::fd::RawFd,
    claimed_pid: u64,
) -> std::io::Result<(std::os::fd::OwnedFd, u64, u64, u64)> {
    use std::os::fd::FromRawFd;
    let mut pid = 0;
    let mut birth = 0;
    let mut generation = 0;
    let capability = unsafe {
        bindings::hl_c_backend_checkpoint_peer_identity_open_test(
            descriptor,
            claimed_pid,
            &raw mut pid,
            &raw mut birth,
            &raw mut generation,
        )
    };
    if capability < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        unsafe { std::os::fd::OwnedFd::from_raw_fd(capability) },
        pid,
        birth,
        generation,
    ))
}

/// How many times any terminal's guest-authored termios has been installed.
///
/// The count only increases, so a reader that sees it unchanged may keep the image it last read.
/// That is the point: a terminal pump can check this on every wakeup for the price of one relaxed
/// load and consult [`terminal_termios`] only when it moves, instead of paying for a lookup per
/// keystroke.
#[must_use]
pub fn terminal_termios_generation() -> u64 {
    bindings::hl_c_backend_terminal_termios_generation()
}

/// The guest's own view of the terminal `descriptor` names, as a Linux `struct termios` image.
///
/// Answers from the engine's record of what the guest last installed, not from the host terminal.
/// A pump that puts the host slave in raw mode -- so a Linux line discipline can run over a channel
/// that applies backpressure instead of flushing at `MAX_CANON` -- still needs to know what the
/// guest believes `ICANON`, `c_cc` and the echo flags to be, and the host no longer carries that.
///
/// Returns `None`, leaving `image` untouched, when no guest has configured this terminal.
#[must_use]
pub fn terminal_termios(descriptor: std::os::fd::RawFd, image: &mut [u8; 36]) -> Option<()> {
    bindings::hl_c_backend_terminal_termios(descriptor, image).then_some(())
}

/// Delivers one signal to the exact process incarnation an authenticated peer capability names.
///
/// `handle` is that capability and `host_pid` the identity it authenticated. Delivery is refused
/// rather than retargeted once the incarnation is gone, which is what separates this from a `kill(2)`
/// on a remembered pid: the number can be reused, the capability cannot. Signal 0 probes reachability
/// without delivering.
///
/// # Errors
/// Returns `Err(())` when the incarnation has exited, the capability is not one this host can signal
/// through, or the host refused delivery.
pub fn process_identity_signal(handle: std::os::fd::RawFd, host_pid: u64, signal: i32) -> Result<(), ()> {
    if bindings::hl_c_backend_process_identity_signal(handle, host_pid, signal) == 0 {
        Ok(())
    } else {
        Err(())
    }
}

/// Whether the process incarnation an authenticated capability names is still running.
///
/// The capability becomes readable when its incarnation exits, so this answers about that exact
/// process and never about a later one that inherited its pid.
#[allow(unsafe_code)]
#[must_use]
pub fn process_identity_live(handle: std::os::fd::BorrowedFd<'_>) -> bool {
    use std::os::fd::AsRawFd;
    let mut waiting = libc::pollfd {
        fd: handle.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one writable poll record over a descriptor borrowed for the duration of the call.
        let ready = unsafe { libc::poll(&raw mut waiting, 1, 0) };
        if ready >= 0 {
            return ready == 0 && waiting.revents == 0;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
}

#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
#[doc(hidden)]
pub fn checkpoint_process_authority_test(pid: i32) -> std::io::Result<AuthenticatedCheckpointPeer> {
    let (process_handle, host_birth, host_generation) = checkpoint_process_identity_open_test(pid, 0, 0)?;
    Ok(AuthenticatedCheckpointPeer {
        host_pid: u64::try_from(pid).map_err(|_| std::io::ErrorKind::InvalidInput)?,
        host_birth,
        host_generation,
        process_handle,
    })
}

#[cfg(test)]
mod platform;

#[cfg(test)]
mod tests {
    use super::{artifact_filename, bindings};

    const LIBRARY_NAME: &str = env!("HL_NATIVE_LIBRARY_NAME");
    const LIBRARY_PATH: &str = env!("HL_NATIVE_LIBRARY_PATH");

    #[test]
    #[allow(unsafe_code)]
    fn shared_engine_exports_matching_abi() {
        assert!(bindings::engine_metadata_is_valid());
        assert!(LIBRARY_NAME.contains("hl_native_engine"));
        let library = std::path::Path::new(LIBRARY_PATH);
        assert!(
            library.is_file(),
            "Cargo-owned native library is missing: {}",
            library.display()
        );
        assert_eq!(
            library.file_name().and_then(|name| name.to_str()),
            Some(artifact_filename())
        );
    }
}
