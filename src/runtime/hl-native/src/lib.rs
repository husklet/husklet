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
mod dynamic_library;
mod engine;
mod loader;
mod provider;
#[cfg(any(feature = "native-test-hooks", windows))]
mod test_hook;

#[cfg(test)]
mod artifact;

#[cfg(unix)]
pub use checkpoint::{AuthenticatedCheckpointPeer, CheckpointBroker, CheckpointTransport};
pub use engine::{Engine, EngineConfig, Error, Exit};
pub use loader::{LoadError, LoadKind};
#[cfg(unix)]
pub use provider::artifact_lifecycle_smoke;
pub use provider::leak_check_nonvacuity;
#[cfg(any(feature = "native-test-hooks", windows))]
pub use test_hook::*;

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

// Not offered on Windows: every function below is a descriptor capability, and the C side already
// answers this question the same way -- `hl_c_backend_process_identity_signal`'s `#else` arm refuses
// outright, because a pidfd (Linux) and a `NOTE_EXIT` kqueue watch (macOS) are the only two things
// that name one process INCARNATION rather than a reusable pid, and Windows has neither in a
// descriptor. A process HANDLE is the Windows object with that property, but it is a HANDLE and not
// an fd: it cannot be polled with `poll(2)`, it does not fit `RawFd`'s contract, and pretending
// otherwise would put a number that is not a descriptor through a descriptor API.
//
// Widening the signatures to `c_int` to make them compile everywhere was rejected for the same
// reason: it would drop the descriptor typing on Linux and macOS -- the platforms where these run --
// to accommodate a host that cannot supply the object at all. So the capability is absent on Windows
// rather than present and weaker, and a Windows consumer gets a name resolution error at the call
// site instead of a value it must not trust.
/// The guest's own view of the terminal `descriptor` names, as a Linux `struct termios` image.
///
/// Answers from the engine's record of what the guest last installed, not from the host terminal.
/// A pump that puts the host slave in raw mode -- so a Linux line discipline can run over a channel
/// that applies backpressure instead of flushing at `MAX_CANON` -- still needs to know what the
/// guest believes `ICANON`, `c_cc` and the echo flags to be, and the host no longer carries that.
///
/// Returns `None`, leaving `image` untouched, when no guest has configured this terminal.
#[cfg(unix)]
#[must_use]
pub fn terminal_termios(descriptor: std::os::fd::RawFd, image: &mut [u8; 36]) -> Option<()> {
    bindings::hl_c_backend_terminal_termios(descriptor, image).then_some(())
}

/// The host terminal's own `struct termios`, as the Linux image the guest ABI uses.
///
/// A pump that is about to make the host diverge from the guest -- putting the slave in raw mode so a
/// Linux line discipline can run over a channel that applies backpressure instead of flushing at
/// `MAX_CANON` -- reads this first, so it can record what the guest would otherwise have observed.
///
/// Returns `None`, leaving `image` untouched, when the descriptor is not a terminal.
#[cfg(unix)]
#[must_use]
pub fn terminal_termios_capture(descriptor: std::os::fd::RawFd, image: &mut [u8; 36]) -> Option<()> {
    bindings::hl_c_backend_terminal_termios_capture(descriptor, image).then_some(())
}

/// Records `image` as the guest's view of `descriptor`, paired with the host projection as it stands.
///
/// This is how the engine's terminal pump keeps the guest's own `TCGETS` answering with what the
/// guest installed after the pump has deliberately put the host slave in raw mode. Without it the
/// guest would read back the raw mode the pump imposed and stop line-editing.
///
/// Returns `None` when the host termios could not be read, in which case nothing was recorded.
#[cfg(unix)]
#[must_use]
pub fn terminal_termios_adopt(descriptor: std::os::fd::RawFd, image: &[u8; 36]) -> Option<()> {
    bindings::hl_c_backend_terminal_termios_adopt(descriptor, image).then_some(())
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
#[cfg(unix)]
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
#[cfg(unix)]
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

#[cfg(any(not(debug_assertions), test))]
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
