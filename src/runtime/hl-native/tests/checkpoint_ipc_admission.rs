#![cfg(feature = "native-test-hooks")]

//! A checkpoint must never publish an image that silently omits state the format
//! does not carry. SysV IPC objects and fcntl/flock record locks live outside the
//! guest descriptor table, so the descriptor scan cannot see them; without an
//! explicit gate a live PostgreSQL cluster (which holds both) checkpointed and
//! restored without its shared memory or its data-directory interlock.

/// Nothing held -> the checkpoint is admitted.
#[test]
fn checkpoint_admits_a_process_holding_neither_sysv_nor_lock_state() {
    for isa in [1, 2] {
        hl_native::checkpoint_ipc_admission_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} empty-state admission failed with status {status}"));
    }
}

/// Each uncaptured object refuses the checkpoint, and the refusal is not sticky.
#[test]
fn checkpoint_refuses_every_uncaptured_sysv_and_file_lock_object() {
    for isa in [1, 2] {
        for scenario in 1..=5 {
            hl_native::checkpoint_ipc_admission_test(isa, scenario).unwrap_or_else(|status| {
                panic!("ISA {isa} scenario {scenario} did not fail closed: status {status}")
            });
        }
    }
}

#[test]
fn checkpoint_ipc_admission_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_ipc_admission_test(isa, 6), Err(99));
    }
}
