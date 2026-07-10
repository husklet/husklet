//! syscall-compat — regression probes for the syscall lifecycle / edge-case bugs tracked in
//! docs/bugs/syscall-compat.md. Owner: syscall-compat bug agent. Edit ONLY this file.
//!
//! Each guest issues a syscall with bad flags / bad fds / bad pointers / bad alignment and prints the
//! raw errno it observed. `.oracle()` diffs the JIT run against the same binary run natively, so the
//! native Linux errno IS the expected value -- a fake-success regression diverges and fails. aarch64
//! only (native gcc + native oracle), which is the reliable path in this harness.
#![allow(unused_imports)]
use crate::{group, src, Case, Engine, Group};

/// aarch64-only syscall regression probe, oracle-diffed vs native.
fn p(name: &'static str, file: &'static str) -> Case {
    src(name, file).only(&[Engine::LinuxAarch64]).oracle()
}

pub fn groups() -> Vec<Group> {
    vec![group(
        "bug-syscall-compat",
        vec![
            // close_range/pidfd_open/unshare/setns/inotify_init1/epoll_pwait/signalfd4 arg validation.
            p("sc-flag-einval", "syscallbug/flag_einval.c"),
            // getresuid/getresgid null output -> EFAULT.
            p("sc-getresid-efault", "syscallbug/getresid_efault.c"),
            // mprotect unaligned/unmapped + madvise invalid-advice/unaligned validation.
            p("sc-mem-validate", "syscallbug/mem_validate.c"),
            // unprivileged sched_setscheduler(SCHED_FIFO) -> EPERM.
            p("sc-sched-rt", "syscallbug/sched_rt.c"),
            // inotify_rm_watch bad wd -> EINVAL, never closes an unrelated fd.
            p("sc-inotify-rm", "syscallbug/inotify_rm.c"),
            // SIGKILL/SIGSTOP can never be blocked via rt_sigprocmask.
            p("sc-sigkill-unmaskable", "syscallbug/sigkill_unmaskable.c"),
            // eventfd counter overflow -> EAGAIN + prior value preserved (no wrap to zero).
            p("sc-eventfd-overflow", "syscallbug/eventfd_overflow.c"),
            // short read() of timerfd/inotify/signalfd -> EINVAL, event NOT consumed.
            p("sc-shortread-timerfd", "syscallbug/shortread_timerfd.c"),
            p("sc-shortread-signalfd", "syscallbug/shortread_signalfd.c"),
            p("sc-shortread-inotify", "syscallbug/shortread_inotify.c"),
        ],
    )]
}
