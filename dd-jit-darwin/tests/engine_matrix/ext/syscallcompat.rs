//! syscall-compat — regression probes for the syscall lifecycle / edge-case bugs tracked in
//! docs/bugs/syscall-compat.md. Owner: syscall-compat bug agent. Edit ONLY this file.
//!
//! Each guest issues a syscall with bad flags / bad fds / bad pointers / bad alignment and prints the
//! raw errno it observed. `.oracle()` diffs the JIT run against the same binary run natively, so the
//! native Linux errno IS the expected value -- a fake-success regression diverges and fails. aarch64
//! only (native gcc + native oracle), which is the reliable path in this harness.
#![allow(unused_imports)]
use dd_tests::{group, src, Case, Engine, Group};

/// aarch64-only syscall regression probe, oracle-diffed vs native.
fn p(name: &'static str, file: &'static str) -> Case {
    src(name, file).only(&[Engine::LinuxAarch64]).oracle()
}

/// Like `p`, but runs the compiled guest INSIDE a container rootfs (alpine) so the guest-pid namespace is
/// active (g_init_hostpid set) -- required for the container-scoped signal routing (kill(0) group delivery)
/// to engage. Still oracle-diffed vs native aarch64 (where kill(0) signals the process group unconditionally).
fn pc(name: &'static str, file: &'static str) -> Case {
    src(name, file).rootfs("alpine").only(&[Engine::LinuxAarch64]).oracle()
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
            // mlock honors RLIMIT_MEMLOCK: soft 0 -> EPERM, exceeding the limit -> ENOMEM, within -> ok.
            // Arch-neutral fix, so oracle-diffed on BOTH Linux engines (x86_64 native under qemu-user).
            src("sc-mlock-rlimit", "syscallbug/mlock_rlimit.c")
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .oracle(),
            // inotify_rm_watch bad wd -> EINVAL, never closes an unrelated fd.
            p("sc-inotify-rm", "syscallbug/inotify_rm.c"),
            // SIGKILL/SIGSTOP can never be blocked via rt_sigprocmask.
            p("sc-sigkill-unmaskable", "syscallbug/sigkill_unmaskable.c"),
            // eventfd counter overflow -> EAGAIN + prior value preserved (no wrap to zero).
            p("sc-eventfd-overflow", "syscallbug/eventfd_overflow.c"),
            // periodic timerfd with a distinct earlier it_value fires FIRST at it_value, then every it_interval
            // (not collapsed into the interval) -- readable within a 200ms budget << the 10s interval.
            p("sc-timerfd-first-interval", "syscallbug/timerfd_first_interval.c"),
            // short read() of timerfd/inotify/signalfd -> EINVAL, event NOT consumed.
            p("sc-shortread-timerfd", "syscallbug/shortread_timerfd.c"),
            p("sc-shortread-signalfd", "syscallbug/shortread_signalfd.c"),
            p("sc-shortread-inotify", "syscallbug/shortread_inotify.c"),
            // read(eventfd, NULL, 8) -> EFAULT, counter not consumed.
            p("sc-eventfd-nullread", "syscallbug/eventfd_nullread.c"),
            // pselect6/ppoll invalid tv_nsec -> EINVAL.
            p("sc-badtimeout", "syscallbug/badtimeout.c"),
            // prlimit64 invalid resource -> EINVAL, dead pid -> ESRCH.
            p("sc-prlimit-invalid", "syscallbug/prlimit_invalid.c"),
            // sigaltstack invalid flags -> EINVAL, tiny stack -> ENOMEM.
            p("sc-sigaltstack-validate", "syscallbug/sigaltstack_validate.c"),
            // rt_sigprocmask invalid how / wrong sigsetsize -> EINVAL.
            p("sc-sigprocmask-validate", "syscallbug/sigprocmask_validate.c"),
            // kill(0, sig) signals the caller's whole process GROUP (parent + child), not just the caller.
            // Container-mode so the guest-pid-namespace group routing engages (see docs/bugs/syscall-compat.md).
            pc("sc-kill-zero-group", "syscallbug/kill_zero_group.c"),
            // Sentry close-on-exec: under DDJIT_UNTRUSTED the guest's fds are sentry-owned VIRTUAL fds; a
            // guest execve (worker-local image reload) must still close the FD_CLOEXEC ones so a peer sees
            // EOF, while plain fds survive. Oracle-diffed vs native (the kernel's own close-on-exec).
            p("sc-sentry-cloexec-exec", "syscallbug/sentry_cloexec_exec.c").untrusted(),
        ],
    )]
}
