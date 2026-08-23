#![allow(unsafe_code)]

use super::{c_int, no_argument_status, scenario_status, test_api};

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_continuation_contract_test(isa: u32) -> Result<(), i32> {
    type Hook = unsafe extern "C" fn() -> c_int;
    let (signal, registers): (Hook, Hook) = match isa {
        1 => (
            test_api().aarch64_checkpoint_signal_precedence,
            test_api().aarch64_checkpoint_restart_register,
        ),
        2 => (
            test_api().x86_64_checkpoint_signal_precedence,
            test_api().x86_64_checkpoint_restart_register,
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
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_restore_claim,
        test_api().x86_64_checkpoint_restore_claim,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_restore_fd_reset_test(isa: u32, scenario: u32) -> Result<u64, i32> {
    let hook = match isa {
        1 => test_api().aarch64_checkpoint_restore_fd_reset,
        2 => test_api().x86_64_checkpoint_restore_fd_reset,
        _ => return Err(-22),
    };
    let mut inspected = 0;
    // SAFETY: the hook owns its descriptor fixture and writes one initialized scalar to this live pointer.
    let status = unsafe { hook(scenario, &raw mut inspected) };
    (status == 0).then_some(inspected).ok_or(status)
}

/// Exercise the restore-side host-page slicing that keeps a rounded claim off a neighbouring guest
/// region's already-claimed host page.
#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_restore_slice_test(isa: u32, scenario: u32) -> Result<(), i32> {
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_restore_slice,
        test_api().x86_64_checkpoint_restore_slice,
        scenario,
    )
}

/// Exercise the registry teardown a re-forked restorer runs before it claims its own image, and the
/// host-granularity rounding that teardown depends on.
#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_gmap_release_test(isa: u32, scenario: u32) -> Result<(), i32> {
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_gmap_release,
        test_api().x86_64_checkpoint_gmap_release,
        scenario,
    )
}

/// Exercise the anonymous `MAP_SHARED` identity and its restore-side republication.
#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_anon_shared_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // objects it publishes, runs its cross-process arm in a forked child it reaps, borrows no caller
    // memory, and returns a scalar.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_anon_shared,
        test_api().x86_64_checkpoint_anon_shared,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn pid_namespace_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // process-wide container identity state it seeds never reaches this process. It borrows no caller
    // memory and returns a scalar.
    scenario_status(
        isa,
        test_api().aarch64_pid_namespace,
        test_api().x86_64_pid_namespace,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_identity_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // touches, runs the scenario in a forked child it reaps, borrows no caller memory, and returns a scalar.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_identity,
        test_api().x86_64_checkpoint_identity,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_election_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // restores, forks nothing, opens no descriptor, borrows no caller memory, and returns a scalar status.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_election,
        test_api().x86_64_checkpoint_election,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_rendezvous_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // opens and closes them, restores the process-wide checkpoint broker descriptor it swapped, reaps
    // every child it created, and returns a scalar status. It borrows no caller memory.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_rendezvous,
        test_api().x86_64_checkpoint_rendezvous,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_refusal_order_test(isa: u32) -> Result<(), i32> {
    no_argument_status(
        isa,
        test_api().aarch64_checkpoint_refusal_order,
        test_api().x86_64_checkpoint_refusal_order,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_launch_identity_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // before returning, and answers a scalar status.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_launch_identity,
        test_api().x86_64_checkpoint_launch_identity,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_membership_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // the process-wide HL_PROCESS_DOMAIN option it also sets, kills the orphan it created, and returns a
    // scalar status. It borrows no caller memory.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_membership,
        test_api().x86_64_checkpoint_membership,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_pipe_capture_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // owns no caller memory, restores the previous sink and the destructive-capture flag, and returns a
    // scalar status.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_pipe_capture,
        test_api().x86_64_checkpoint_pipe_capture,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_stdio_alias_capture_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // swaps the process-wide guest box for the duration of the call, restores it, releases everything it
    // created, borrows no caller memory, and returns a scalar status.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_stdio_alias_capture,
        test_api().x86_64_checkpoint_stdio_alias_capture,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_socket_halfclose_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // is its own static buffer, clears the identity it assigned, restores the previous sink and the
    // destructive-capture flag, and returns a scalar status.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_socket_halfclose,
        test_api().x86_64_checkpoint_socket_halfclose,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_ipc_admission_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // exercising, evaluates the gate, and restores the previous contents before
    // returning a scalar status; it allocates nothing the caller owns.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_ipc_admission,
        test_api().x86_64_checkpoint_ipc_admission,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_restore_rollback_test(isa: u32) -> Result<(), i32> {
    no_argument_status(
        isa,
        test_api().aarch64_checkpoint_restore_rollback,
        test_api().x86_64_checkpoint_restore_rollback,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn terminal_termios_install_test(fd: c_int, image: &[u8; 36]) {
    // SAFETY: `image` is a live 36-byte buffer for the call and C copies it before returning.
    unsafe { (test_api().aarch64_terminal_termios_install)(fd, image.as_ptr()) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn terminal_termios_store_test(isa: u32) -> Result<(), i32> {
    no_argument_status(
        isa,
        test_api().aarch64_terminal_termios_store,
        test_api().x86_64_terminal_termios_store,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn unix_identity_test(isa: u32, operation: u32, fd: i32, object: u64) -> Result<(u64, u64, u32), i32> {
    let mut local = 0;
    let mut peer = 0;
    let mut hidden = 0;
    let hook = match isa {
        1 => test_api().aarch64_unix_identity,
        2 => test_api().x86_64_unix_identity,
        _ => return Err(-libc::EINVAL),
    };
    // SAFETY: `local`, `peer` and `hidden` are this frame's locals, live for the call and borrowed
    // by nobody else, so the three out-pointers are unaliased and correctly aligned for the widths
    // the hook writes; it stores nothing beyond the call. `fd` is borrowed, not consumed.
    let status = unsafe { hook(operation, fd, object, &raw mut local, &raw mut peer, &raw mut hidden) };
    if status == 0 || ((operation == 1 || operation == 16) && status == 1) {
        Ok((local, peer, hidden))
    } else {
        Err(status)
    }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn socket_shape_test(isa: u32, operation: u32, fd: i32, capacity: u32, out: &mut [u8]) -> Result<u32, i32> {
    let mut length = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let hook = match isa {
        1 => test_api().aarch64_socket_shape,
        2 => test_api().x86_64_socket_shape,
        _ => return Err(libc::EINVAL),
    };
    // SAFETY: `out` is a caller-owned slice held mutably for the call, and `length` carries its
    // true element count in, so the hook cannot write past it; `length` is a local, so the buffer
    // and its bound are unaliased. The hook retains neither after returning a scalar status.
    let status = unsafe { hook(operation, fd, capacity, out.as_mut_ptr(), &raw mut length) };
    if status == 0 { Ok(length) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn unix_identity_capture_test(isa: u32, fd: i32) -> Result<(), i32> {
    let hook = match isa {
        1 => test_api().aarch64_unix_identity_capture,
        2 => test_api().x86_64_unix_identity_capture,
        _ => return Err(-libc::EINVAL),
    };
    // SAFETY: the hook takes one borrowed descriptor and no pointers, so there is nothing for it
    // to alias or outlive; it captures the descriptor's identity into engine-owned storage and
    // returns a scalar status.
    let status = unsafe { hook(fd) };
    if status == 0 { Ok(()) } else { Err(status) }
}
