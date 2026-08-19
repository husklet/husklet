#![cfg(feature = "native-test-hooks")]

//! A checkpoint must never publish an image that silently omits state the format
//! does not carry. SysV IPC objects and fcntl/flock record locks live outside the
//! guest descriptor table, so the descriptor scan cannot see them; without an
//! explicit gate a live PostgreSQL cluster (which holds both) checkpointed and
//! restored without its shared memory or its data-directory interlock.
//!
//! SysV is now captured rather than refused, so scenarios 1-3 drive a real
//! capture/restore round trip through a fresh namespace hash; the lock domain is
//! still uncaptured and scenarios 4-5 still assert its refusal.

/// Nothing held -> the checkpoint is admitted.
#[test]
fn checkpoint_admits_a_process_holding_neither_sysv_nor_lock_state() {
    for isa in [1, 2] {
        hl_native::checkpoint_ipc_admission_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} empty-state admission failed with status {status}"));
    }
}

/// A shared-memory segment, a semaphore set with its SEM_UNDO list, and a message queue
/// survive a capture/restore round trip into a different IPC namespace -- and the segment
/// comes back at its original attach address with its original bytes.
#[test]
fn sysv_objects_survive_a_capture_and_restore_round_trip() {
    for isa in [1, 2] {
        for scenario in 1..=3 {
            hl_native::checkpoint_ipc_admission_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} scenario {scenario} did not round-trip: status {status}"));
        }
    }
}

/// Each still-uncaptured lock object refuses the checkpoint, and the refusal is not sticky.
#[test]
fn checkpoint_refuses_every_uncaptured_file_lock_object() {
    for isa in [1, 2] {
        for scenario in 4..=5 {
            hl_native::checkpoint_ipc_admission_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} scenario {scenario} did not fail closed: status {status}"));
        }
    }
}

#[test]
fn checkpoint_ipc_admission_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_ipc_admission_test(isa, 6), Err(99));
    }
}
