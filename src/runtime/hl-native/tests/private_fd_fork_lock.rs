//! `hl_private_fork_lock` is a file-static `pthread_mutex_t` held across a
//! `/proc/<pid>/stat` read, so its hold window is milliseconds rather than nanoseconds.
//! `fork()` from a multi-threaded process copies it in whatever state a sibling thread
//! left it, and a child that then takes the same path used to block in
//! `pthread_mutex_lock` forever against a thread that does not exist in the child --
//! measured at 11 hangs in 30 runs before the `pthread_atfork` child handler existed.
//!
//! The C hook bounds its own wait, so a regression fails by name instead of wedging the
//! suite.
#![cfg(all(unix, feature = "native-test-hooks"))]

#[test]
fn a_fork_child_completes_the_private_fd_path_while_a_sibling_holds_the_lock() {
    match hl_native::private_fork_lock_test(1) {
        Ok(()) => {}
        Err(status) if status == -libc::ETIMEDOUT => panic!(
            "the fork child never returned from hl_host_process_fd_private_add: it inherited a locked \
             hl_private_fork_lock, so the pthread_atfork child handler is missing or ineffective"
        ),
        Err(status) => panic!("private fork-lock scenario failed with status {status}"),
    }
}

#[test]
fn an_ordinary_soft_limit_keeps_the_guest_ceiling_and_a_full_private_band() {
    hl_native::private_fork_lock_test(2)
        .unwrap_or_else(|status| panic!("guest/private descriptor split is inconsistent: {status}"));
}

#[test]
fn lowering_the_hard_limit_makes_the_reported_guest_ceiling_honest() {
    hl_native::private_fork_lock_test(3)
        .unwrap_or_else(|status| panic!("low-hard-limit descriptor split is inconsistent: {status}"));
}
