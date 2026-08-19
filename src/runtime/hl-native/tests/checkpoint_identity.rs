#![cfg(feature = "native-test-hooks")]

//! What a captured member records about itself, and whether the image can then be restored.
//!
//! Election and group naming were widened for container exec sessions; identity was not. `container_pid()`
//! answers 1 for EVERY launch top -- `target/{aarch64,x86_64}.c` set `g_init_hostpid` to `getpid()` per
//! launch -- so an exec session's top process recorded `self_gpid = 1` and, through that arm,
//! `ppid_gpid = 0`, while its image was filed under `proc.<host pid>` by `ckpt_self_group`.
//!
//! Restore is whole-image: `ckpt_scan_procs` takes each member's gpid from the DIRECTORY NAME and its
//! parent from the META, and `ckpt_validate_proc_tree` refuses a non-root member with no parent. So a
//! capture that reported `checkpoint OK: 12 process(es)` on a live `PostgreSQL` cluster produced an image
//! that was rejected before the first fork -- and the capture did not know it.
//!
//! An exec session has no parent to name. hl-container forks it out of its own daemon, so it is a sibling
//! of guest pid 1; measured on Docker 29.1.3, an exec top reads `pid=7 ppid=0 pgrp=7 sid=7` inside the
//! container's pid namespace. It is parentless-but-in-domain, and the image now says exactly that.

/// A container exec session's top process records the gpid its group is named with, no parent, its own
/// group and session, and the container process domain it belongs to -- and the image validates.
#[test]
fn an_exec_session_member_validates() {
    for isa in [1, 2] {
        hl_native::checkpoint_identity_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} rejected a captured exec-session identity at {status}"));
    }
}

/// The validator was not weakened to accept the broken image. A parentless member that declares no domain
/// is still refused: membership has to be positively recorded by the capture, not assumed by the restore.
#[test]
fn a_parentless_member_declaring_no_domain_is_refused() {
    for isa in [1, 2] {
        hl_native::checkpoint_identity_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} admitted an undeclared parentless member at {status}"));
    }
}

/// An unknown scenario is refused rather than silently treated as scenario 0.
#[test]
fn an_unknown_scenario_is_refused() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_identity_test(isa, 2), Err(-22));
    }
}
