#![cfg(feature = "native-test-hooks")]

//! An anonymous `MAP_SHARED` region is the one shared object with no descriptor anywhere: `map.c`
//! registers a mapping in `g_filemap` only when it is not `MAP_ANON`, and `backing_object` is only
//! ever set from `g_filemap`, so such a region reached the image with `backing_object == 0` and
//! `memory_restore.c` mapped it `MAP_ANON|MAP_PRIVATE` -- a per-process private copy of memory the
//! guest believes is shared. `PostgreSQL` 16 with `shared_memory_type=mmap` keeps its whole buffer
//! pool, `ProcArray`, lock tables and `PMChildFlags` there.

/// The kernel's shmem inode names the object: distinct mappings get distinct identities, a sub-range
/// is the same object at an offset, and a `MAP_PRIVATE` anonymous region gets no identity at all.
#[test]
fn anonymous_shared_mappings_have_a_kernel_object_identity_and_private_ones_do_not() {
    for isa in [1, 2] {
        hl_native::checkpoint_anon_shared_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} identity scenario failed with status {status}"));
    }
}

/// Two processes derive the same identity AND offset for the region they share and, through the
/// restore-side seed for it mapped at that offset, end up mapping ONE object: a write by one is
/// visible to the other. The offset is not always zero -- Darwin coalesces adjacent shared anonymous
/// mappings into one `vm_object` -- and carrying it is exactly what `memory_restore` does.
#[test]
fn two_processes_restore_one_shared_object_rather_than_two_private_copies() {
    for isa in [1, 2] {
        hl_native::checkpoint_anon_shared_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} round trip failed with status {status}"));
    }
}

#[test]
fn the_anonymous_shared_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_anon_shared_test(isa, 2), Err(99));
    }
}
