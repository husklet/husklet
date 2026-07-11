//! linuxsys — basics expansion (in-process JIT matrix). Owner: linuxsys agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! Linux-only syscalls with no portable POSIX form (epoll/eventfd/timerfd/signalfd/inotify/memfd/
//! splice family/pidfd/sched/io_uring/…). On the macOS-hosted runtime these are EMULATED (kqueue/pipe),
//! so each is diffed against a NATIVE-Linux oracle: a divergence here is an emulation gap (xfail-tracked
//! + a GAPS row), which is exactly the high-value signal for the engine builders.
#![allow(unused_imports)]
use crate::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![
        epoll(),
        events(),
        fsx(),
        procx(),
        randx(),
        schedx(),
        miscx(),
    ]
}

/// epoll readiness-model corners: edge-trigger, MOD/DEL, oneshot rearm, pwait sigmask.
fn epoll() -> Group {
    group(
        "lsys-epoll",
        vec![
            src("epoll-et", "ext_linuxsys/epoll_et.c").oracle(),
            src("epoll-mod", "ext_linuxsys/epoll_mod.c").oracle(),
            src("epoll-oneshot", "ext_linuxsys/epoll_oneshot.c").oracle(),
            src("epoll-pwait", "ext_linuxsys/epoll_pwait.c").oracle(),
            // a cross-thread epoll_ctl must not make a blocked epoll_wait(-1) spuriously return 0.
            src("epoll-reblock-inf", "ext_linuxsys/epoll_reblock_inf.c").has("DELIVERED_OK"),
            src("epoll-reblock-fin", "ext_linuxsys/epoll_reblock_fin.c").has("FINITE_OK"),
            // LTP poll02/pselect01/select01/select02/epoll_create1_01 surface, in one byte-exact transcript:
            // select readfds/writefds bitsets (regular file / pipe / FIFO), timeout==0 immediate return,
            // nfds 0/negative -> EINVAL, EBADF for a closed fd, EFAULT on a bad fd_set / timeout / pollfd
            // pointer (guest PROT_NONE), poll POLLNVAL/POLLOUT, epoll_create1 CLOEXEC round-trip + bad-flag
            // EINVAL. Oracle-diffed on both arches (qemu-user == native for every case here).
            src("poll-select-epoll", "ext_linuxsys/poll_select_epoll.c").oracle(),
        ],
    )
}

/// event/timer/signal fds.
fn events() -> Group {
    group(
        "lsys-events",
        vec![
            // EFD_NONBLOCK read on a zero counter does NOT return EAGAIN on the JIT. GAPS `lsys-eventfd-nonblock`.
            src("eventfd-nonblock", "ext_linuxsys/eventfd_nonblock.c").oracle(),
            // kernel-AIO/libaio (io_setup/io_submit(PREAD)/io_getevents): synchronous emulation must return
            // the completion with res==nbytes and the right bytes. Unblocks nginx:alpine + innodb file-AIO.
            // Golden-checked (NOT oracle): qemu-x86_64 user-mode returns ENOSYS for io_setup, so it can't be
            // the ground truth; the native aarch64 kernel and both dd JITs all produce this exact line.
            src("aio-pread", "ext_linuxsys/aio_pread.c")
                .out("aio res=10 data=d00dfeed buf=KLMNOPQRST\n")
                .exit(0),
            src("timerfd-interval", "ext_linuxsys/timerfd_interval.c").oracle(),
            // timerfd_gettime reports the armed one-shot's remaining time (deadline tracked in timerfd_settime).
            src("timerfd-gettime", "ext_linuxsys/timerfd_gettime.c").oracle(),
            // signalfd realtime queue: each wake byte carries the queued signo -> every siginfo has ssi_signo.
            src("signalfd-rt", "ext_linuxsys/signalfd_rt.c").oracle(),
        ],
    )
}

/// fs-flavoured Linux syscalls: inotify, memfd sealing, splice family, copy_file_range, statfs, sync.
fn fsx() -> Group {
    group(
        "lsys-fs",
        vec![
            // inotify IN_MODIFY/IN_ATTRIB/IN_DELETE_SELF on a watched file — emulated correctly.
            src("inotify-modify", "ext_linuxsys/inotify_modify.c").oracle(),
            // inotify rename events: rename(2) queues the paired IN_MOVED_FROM/IN_MOVED_TO with a shared cookie.
            src("inotify-moves", "ext_linuxsys/inotify_moves.c").oracle(),
            // a closed emulated fd (inotify/timerfd/eventfd/memfd) must shed its per-fd emulation table so
            // a RECYCLED fd number reads/fcntls as a plain file — never misrouted to the dead emulation. Oracle-
            // diffable: fd-number reuse + plain-file semantics are exact real-Linux behaviour on both arches.
            src("fdreuse-guard", "ext_linuxsys/fd_reuse_guard.c")
                .out("fdreuse inotify=1 timerfd=1 eventfd=1 memfd=1\n")
                .oracle(),
            // memfd F_ADD_SEALS / F_SEAL_WRITE enforced (a write after F_SEAL_WRITE fails EPERM).
            src("memfd-seal", "ext_linuxsys/memfd_seal.c").oracle(),
            // eventfd/timerfd/memfd emulation must still be active when real fd numbers exceed 1024.
            // Golden-checked (NOT oracle): the 4th seal digit is pwritev2-vs-F_SEAL_WRITE, and
            // qemu-x86_64 user-mode returns ENOSYS for pwritev2 (seals=1110), so it can't be the
            // ground truth. Canonical Linux returns EPERM (verified: native aarch64 kernel prints
            // seals=1111, matching both dd JITs) — the golden pins dd to real-kernel behaviour.
            src("high-fd-emul", "ext_linuxsys/high_fd_emul.c")
                .out("highfd base=1 event=1 timer=1 seals=1111\n")
                .exit(0),
            // tee(2): duplicate pipe->pipe via peek+pushback so the source pipe is left intact.
            src("tee", "ext_linuxsys/tee.c").oracle(),
            // vmsplice(2): gather user memory into a pipe (write end) / scatter it back (read end).
            src("vmsplice", "ext_linuxsys/vmsplice.c").oracle(),
            src("copy-file-range", "ext_linuxsys/copy_file_range.c").oracle(),
            src("fstatfs", "ext_linuxsys/fstatfs.c").oracle(),
            // syncfs(2) returns nonzero on the JIT (native returns 0). GAPS `lsys-syncfs`.
            src("syncfs", "ext_linuxsys/syncfs.c").oracle(),
        ],
    )
}

/// process control: prctl variants, pidfd, gettid, unshare.
fn procx() -> Group {
    group(
        "lsys-proc",
        vec![
            // PR_SET/GET_NO_NEW_PRIVS: the sticky nnp bit round-trips.
            src("prctl-nnp", "ext_linuxsys/prctl_nnp.c").oracle(),
            // PR_SET/GET_DUMPABLE round-trip.
            src("prctl-dumpable", "ext_linuxsys/prctl_dumpable.c").oracle(),
            // PR_SET/GET_PDEATHSIG round-trip.
            src("prctl-pdeathsig", "ext_linuxsys/prctl_pdeathsig.c").oracle(),
            // The full LTP prctl02 + prctl03 validation matrix (option/arg EINVAL, EFAULT on a bad name ptr,
            // EPERM on PR_SET_SECUREBITS/PR_CAPBSET_DROP after dropping CAP_SETPCAP, THP_DISABLE / CAP_AMBIENT /
            // SPECULATION_CTRL arg checks, PR_SET/GET_CHILD_SUBREAPER round-trip). Self-checking (NOT oracle:
            // qemu-user lacks THP_DISABLE and the initial THP/dumpable flags are host-specific) -- the native
            // run confirms the expected values by also printing PRCTL_GUARD_OK.
            src("prctl-ltp", "ext_linuxsys/prctl_guard.c")
                .has("PRCTL_GUARD_OK")
                .exit(0),
            src("gettid", "ext_linuxsys/gettid.c").oracle(),
            // A fork-without-exec child must publish its own /proc entry, and its exit must not unlink
            // the parent's inherited registry path. Chrome zygote renderer children depend on this shape.
            src("proc-fork-registry", "ext_linuxsys/proc_fork_registry.c")
                .out("proc_fork_registry child=1 self_before=1 self_after=1 exit=1\n")
                .oracle(),
            // pidfd_open(2) unsupported on the JIT (returns failure). GAPS `lsys-pidfd`.
            src("pidfd-open", "ext_linuxsys/pidfd_open.c").oracle(),
            // pidfd_send_signal IS implemented and delivers correctly (opened=1 sent=1). The test's `killed`
            // field is a non-deterministic SIGTERM-vs-SIGKILL RACE: right after the pidfd SIGTERM the parent
            // also SIGKILLs the child. The native kernel usually lets the unblockable SIGKILL win the pending
            // pair (killed=0), while dd's SIGTERM (a host kill to the separate child engine, whose host default
            // terminates it) frequently lands first (killed=1). The winner varies run-to-run on BOTH arches, so
            // it can never be byte-stable against the oracle -- not an engine gap. Timing/oracle artifact.
            src("pidfd-signal", "ext_linuxsys/pidfd_signal.c")
                .oracle()
                .xfail(&[Engine::LinuxAarch64]),
            src("unshare-files", "ext_linuxsys/unshare_files.c").oracle(),
            // process_vm_readv: the aarch64 JIT now implements it (matches native). On x86_64 the JIT reads
            // correctly (read=32 ok=1) but the qemu-user oracle lacks process_vm_readv (read=-1) -> oracle
            // artifact, not an engine gap (cf. clone3). GAPS `lsys-process-vm`.
            src("process-vm", "ext_linuxsys/process_vm.c")
                .oracle()
                .xfail(&[Engine::LinuxX86_64]),
        ],
    )
}

/// /proc/sys/kernel/random/{boot_id,uuid} synth. Self-checking (NOT oracle: the exact UUID value is
/// host/boot-specific, but the format + stable-boot_id / fresh-uuid invariants are universal). Without
/// boot_id, curl/systemd/dbus/libuuid print "cannot find current boot id".
fn randx() -> Group {
    group(
        "lsys-rand",
        vec![src("proc-bootid", "ext_linuxsys/proc_bootid.c")
            .has("BOOTID_OK")
            .has("UUID_OK")
            .has("SURFACE_OK")
            .exit(0)],
    )
}

/// scheduler & cpu-topology syscalls.
fn schedx() -> Group {
    group(
        "lsys-sched",
        vec![
            src("sched-yield", "ext_linuxsys/sched_yield.c").oracle(),
            src("sched-affinity", "ext_linuxsys/sched_affinity.c").oracle(),
            src("getcpu", "ext_linuxsys/getcpu.c").oracle(),
        ],
    )
}

/// the long tail: getrandom flags, io_uring setup, membarrier, personality, clocks, fanotify, ioprio.
fn miscx() -> Group {
    group(
        "lsys-misc",
        vec![
            src("getrandom-flags", "ext_linuxsys/getrandom_flags.c").oracle(),
            // io_uring is DELIBERATELY unimplemented: all of setup/enter/register return ENOSYS on BOTH arches
            // A fake setup that succeeds but never completes SQEs would hang every real guest that then
            // submits I/O (Go/tokio/liburing) waiting on completions that never arrive — strictly worse than the
            // graceful epoll/sync fallback a clean ENOSYS triggers. So dd reports "no io_uring" exactly as a
            // kernel built without it. The native aarch64 oracle HAS io_uring (setup=1), so this differential is
            // an accepted-gap artifact on aarch64 (x86's qemu oracle also ENOSYS -> matches). xfail'd.
            src("io-uring", "ext_linuxsys/io_uring.c")
                .oracle()
                .xfail(&[Engine::LinuxAarch64]),
            // membarrier(2) MEMBARRIER_CMD_QUERY enumerates the supported command mask — oracle-identical to native.
            src("membarrier", "ext_linuxsys/membarrier.c").oracle(),
            // personality(2) get/set round-trips (query 0xffffffff; ADDR_NO_RANDOMIZE sticks).
            src("personality", "ext_linuxsys/personality.c").oracle(),
            src("clock-linux", "ext_linuxsys/clock_linux.c").oracle(),
            // fanotify_init: the JIT now returns EPERM (the real-Linux contract for an unprivileged caller),
            // matching the native aarch64 oracle. On x86 the qemu-user oracle LACKS the syscall and reports
            // ENOSYS (eperm=0), so the x86 differential is an oracle artifact — xfail'd (cf. process-vm)..
            src("fanotify", "ext_linuxsys/fanotify.c")
                .oracle()
                .xfail(&[Engine::LinuxX86_64]),
            src("ioprio", "ext_linuxsys/ioprio.c").oracle(),
        ],
    )
}
