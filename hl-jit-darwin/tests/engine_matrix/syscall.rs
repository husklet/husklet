//! Syscall surface — portable POSIX, Linux-specific syscalls, and obscure edge corners.

use crate::support::{group, port, src, Engine, Group};

/// Portable POSIX syscalls — the event/IO/IPC surface a real daemon leans on, expressed in pure POSIX
/// so it runs (and must agree) on Linux AND macOS. Golden-checked.
pub(super) fn posix() -> Group {
    group("posix", vec![
        port("pollselect", "pollselect.c").out("poll=1 select=1 timeout=1\n"), // poll() + select() + timeout
        // poll/select/pselect readiness (read+write), ready-fd COUNT, and 0/finite-timeout return-0,
        // all as deterministic booleans -> golden across x86/aarch64/darwin.
        port("pollselect-ext", "pollselect_ext.c")
            .out("poll rd=1 wr=1 to0=1 to=1\nselect rd=1 wr=1 count2=1 to=1\npselect rd=1 to=1 nfds0=1\n"),
        port("mmapshared", "mmapshared.c").out("mmapshared ok=1 sum=520192\n"), // file-backed MAP_SHARED + msync
        port("filelock", "filelock.c").out("filelock blocked=1 free_after=1\n"), // fcntl F_SETLK/F_GETLK + fork
        port("clock", "clockmono.c").has("mono_ok=1 slept_ge=1 realtime_ok=1"),  // clock_gettime + nanosleep
        port("realpath", "realpath.c").out("realpath readlink=1 resolve=1\n"),   // symlink/readlink/realpath
        port("getdents", "getdents.c").out("getdents count=4 namechk=2306\n"),   // opendir/readdir
        port("tmpfile", "tmpfile.c").out("tmpfile mkstemp=1 data=1 tmpfile=1 val=4242\n"), // mkstemp/tmpfile
        port("statvfs", "statvfs.c").out("statvfs ok=1 bsize_pow2=1 blocks_ok=1 consistent=1\n"), // statfs
    ])
}

/// Linux-specific syscalls — epoll/eventfd/signalfd/inotify/sendfile/getrandom have no portable POSIX
/// form (macOS uses kqueue/getentropy), so they're Linux-engine only and diffed against a native oracle.
/// On the macOS-hosted runtime these are emulated (kqueue/pipe), so this group is where emulation gaps
/// surface against real-Linux ground truth.
pub(super) fn linuxsys() -> Group {
    group(
        "linuxsys",
        vec![
            src("epoll", "epoll.c").oracle(), // epoll_create1/ctl/wait readiness loop
            // a WATCHED fd whose number exceeds 1024 (Chromium registers hundreds of fds) must still get
            // correct readiness delivery + EEXIST/ENOENT membership -- the per-instance membership bitmap
            // spans the full guarded fd range, not just the first 1024 (a narrower bitmap OOBs and strands
            // the fd's readiness = the load-dependent renderer node-connect stall).
            src("epoll-highfd", "epoll_highfd.c").oracle(),
            // epoll surface: create1/create flag+size validation, EPOLLIN/OUT readiness, oneshot re-arm,
            // and the epoll_ctl EEXIST/ENOENT/EINVAL/EPERM error return values.
            src("epoll-edge", "epoll_edge.c").oracle(),
            // epoll interest follows the OPEN FILE DESCRIPTION: closing a watched fd whose OFD stays alive via
            // a dup KEEPS readiness (re-homed onto the surviving alias), returning the original udata.
            src("epoll-dup-lifetime", "epoll_dup_lifetime.c").oracle(),
            // a fork child inherits the parent's epoll interest list; the child epoll_waits WITHOUT
            // re-registering and must still see the inherited registration fire (dd rebuilds an empty kqueue
            // in the child and re-arms the inherited interest from its per-instance table).
            src("epoll-fork-inherit", "epoll_fork_inherit.c").oracle(),
            // poll/select/pselect/ppoll signal+timeout corners: the select02 HANG regression (a blocked,
            // dd-hooked signal must NOT restart the full timeout), EINTR on a delivered handler, EFAULT/EINVAL.
            src("pollselect-eintr", "pollselect_eintr.c").oracle(),
            src("eventfd", "eventfd.c").oracle(), // eventfd2 counter semantics
            src("eventfd-sema", "eventfd_sema.c").oracle(), // EFD_SEMAPHORE decrement-by-1 contract
            // Cross-thread eventfd->epoll message-pump wakeup under contention (Chrome's MessagePumpEpoll +
            // ScheduleWork). 6 producers cross-wake a level-triggered epoll pump + a futex mutex; asserts
            // every wake lands AND the eventfd counter accounts for every write EXACTLY. Regression guard for
            // the unsynchronized eventfd counter+pipe race that stalled chromium's first paint (a stranded
            // pipe-readable-with-count-0 -> pump busy-spin / lost wakeup). Fails deterministically pre-fix.
            src("pump-wakeup", "pump_wakeup.c").has("pump OK"),
            src("signalfd", "signalfd.c").oracle(), // sigprocmask + signalfd4 read of a raised signal
            // Two INDEPENDENT signalfds (SIGUSR1 vs SIGUSR2): each has its own mask + delivery queue, so a
            // raised SIGUSR1 is readable on the USR1 fd only. Regression guard for the old single-shared-pipe
            // model that aliased distinct signalfds and ORed their masks.
            src("signalfd-multi", "signalfd_multi.c").oracle(),
            src("inotify", "inotify.c").oracle(),   // inotify watch -> IN_CREATE event read
            src("sendfile", "sendfile.c").oracle(), // sendfile + readv/writev scatter-gather
            src("timerfd", "timerfd.c").oracle(),   // timerfd one-shot expiration
            src("memfd", "memfd.c").oracle(),       // memfd_create + ftruncate + mmap
            src("wlshm_pool", "wlshm_pool.c").oracle(), // wl_shm pool: memfd -> MAP_SHARED -> SCM_RIGHTS -> cross-proc mmap coherency (display M0)
            src("splice", "splice.c").oracle(),     // splice file->pipe->file (zero copy)
            src("prctl", "prctl.c").oracle(),       // prctl PR_SET_NAME/PR_GET_NAME
            src("sigqueue", "sigqueue.c").oracle(), // realtime signal payload via si_value
            src("getrandom", "getrandom.c").has("nonzero=1 differ=1"), // entropy verdict (bytes not reproducible)
        ],
    )
}

/// EDGE — obscure syscall corners mined from reading os/linux/service.c + the frontends: flag bits,
/// ancillary data, sparse-file/seek semantics, abstract sockets, packet pipes, and protection faults
/// that a runtime is easy to get subtly wrong. Differential vs native (oracle) where output matches
/// real Linux; golden verdict where it's a yes/no. These are where production software trips.
pub(super) fn edge() -> Group {
    // Confirmed JIT divergences vs real Linux (the engine-fixing lane owns these — see PLAN.md "Edge").
    // xfail-tracked so the gate stays green and XPASS fires the moment one is fixed. `msgflags`
    // (MSG_PEEK/MSG_DONTWAIT) is the only one that already matches Linux.
    let lin = &[Engine::LinuxAarch64, Engine::LinuxX86_64];
    group(
        "edge",
        vec![
            src("madvise", "edge_madvise.c").oracle(), // MADV_DONTNEED must drop anon pages
            src("renameat2", "edge_renameat2.c").oracle(), // RENAME_NOREPLACE / RENAME_EXCHANGE
            src("scmrights", "edge_scmrights.c").oracle(), // fd passing over AF_UNIX (SCM_RIGHTS) — FIXED (cmsg l2m/m2l)
            src("fallocate", "edge_fallocate.c").oracle(), // FALLOC_FL_PUNCH_HOLE sparse hole
            src("lseekhole", "edge_lseekhole.c").oracle(), // SEEK_HOLE / SEEK_DATA
            src("otmpfile", "edge_otmpfile.c").oracle(),   // O_TMPFILE unnamed file
            src("pipepacket", "edge_pipepacket.c").oracle(), // pipe2(O_DIRECT) packet boundaries
            src("msgflags", "edge_msgpeek.c").oracle(),    // recv MSG_PEEK + MSG_DONTWAIT — WORKS
            src("abstract", "edge_abstract.c").oracle(), // abstract-namespace AF_UNIX — FIXED (HL_NETNS fs-socket map)
            src("pipesz", "edge_pipesz.c").oracle(), // F_SET/GETPIPE_SZ (shadow-table emulation) + dup3 self-dup — FIXED
            // mprotect: portable — darwin (native) FAULTS correctly, the JIT no-ops it; xfail only Linux so
            // the darwin pass / Linux fail contrast is explicit.
            port("mprotect", "edge_mprotect.c")
                .out("mprotect faulted=1 readable_after=1\n")
                .xfail(lin),
            // clock_nanosleep TIMER_ABSTIME: emulated as (deadline - now) with an EINTR-recompute loop —
            // FIXED. Pinned to one engine to bound cost; previously hung to the 25s timeout when treated
            // as relative.
            src("clockabstime", "edge_clockabstime.c")
                .only(&[Engine::LinuxAarch64])
                .has("abstime_ok=1"),
            src("sigpipe", "edge_sigpipe.c").has("survived=1 epipe=1"), // SO_NOSIGPIPE set on every guest socket at creation (socket/socketpair/accept) -> write/send to a broken socket returns EPIPE, never a fatal SIGPIPE
            src("procfd", "edge_procfd.c").has("resolves=1 enough_fds=1"), // /proc/self/fd
            // /proc/self/task per-thread dir (crashpad/ThreadHelpers walks it): main tid listed, task/<tid>
            // stats as a dir, and per-thread stat/comm are served. The chromium thread_helpers.cc wall.
            src("proctask", "edge_proctask.c").has("proctask tids=1 isdir=1 stat=1 comm=1"),
            // times(): tms_utime works on x86_64 but is 0 on aarch64 (clock() works on both) — engine split.
            src("times", "edge_times.c").has("utime_ok=1 clock_ok=1 ret_ok=1"),
            // Legacy x86 time-setters with NO aarch64 canonical syscall number (utime=132/utimes=235/
            // futimesat=261) — used to return ENOSYS-by-normalization on x86 (arm64 261 is prlimit64). dd now
            // rewrites each to utimensat(dfd,path,timespec[2],flags) with the struct utimbuf/timeval[2] -> timespec
            // conversion, NULL times = "now", and honors utimensat's UTIME_OMIT/UTIME_NOW (Linux tv_nsec sentinels
            // translated to the macOS host's). Byte-identical vs native/qemu on both Linux engines.
            src("utimes-family", "utimes_family.c")
                .oracle()
                .has("utimes-family OK"),
            src("statfs", "edge_statfs.c").oracle().xfail(lin), // real fs geometry (not hardcoded)
            // statx (nr 291) must report the SAME uid/gid/mode/nlink/rdev/dev/size as fstat/newfstatat
            // for the same file -- through dd's cuid/cgid + guest-chown virtualization. Self-checking
            // agreement booleans, byte-identical native-vs-dd; before the fix statx diverged (raw uid, rdev 0:0).
            src("statx-agree", "statx_agree.c").oracle(),
            // Guest-pointer validation (access_ok): bad syscall buffers -> EFAULT, exactly as native Linux.
            // The differential test for host_range_mapped's fault-guarded probe fast path,
            // incl. the probe-guard re-arm (again=) and PROT_NONE (copy_from_user) cases.
            src("efault", "edge_efault.c").oracle(),
            // wall-clock RATE: REALTIME/MONOTONIC/gettimeofday must advance at the real host rate across a
            // nanosleep (regression guard for the x86 vDSO fast-syscall timebase — a 40x-slow REALTIME read
            // would fail real_ok/agree). Portable golden. The `-slow` sibling forces the svc_time slow path
            // (HL_JIT_NOFASTSYS) so both the inline and the syscall computations are covered.
            port("clockelapsed", "clockelapsed.c")
                .out("clockelapsed real_ok=1 mono_ok=1 gtod_ok=1 mono_fwd=1 agree=1\n"),
            port("clockelapsed-slow", "clockelapsed.c")
                .env("HL_JIT_NOFASTSYS", "1")
                .out("clockelapsed real_ok=1 mono_ok=1 gtod_ok=1 mono_fwd=1 agree=1\n")
                .only(lin),
            // bad/NULL RESULT pointer -> -EFAULT (never crash), via the x86 vDSO fast path AND the
            // slow path (the `-slow` sibling under HL_JIT_NOFASTSYS). Per-syscall NULL policy matches the kernel.
            // Diffed byte-exact vs the native/qemu oracle; before the fix the fast-path variant crashed (exit 255).
            src("clockefault", "clockefault.c").oracle(),
            src("clockefault-slow", "clockefault.c")
                .env("HL_JIT_NOFASTSYS", "1")
                .oracle(),
            // generic slow-path syscall ARGUMENT validation: a bad/unmapped guest pointer to a syscall
            // whose result the ENGINE fills via memcpy/struct-write (nanosleep/getrusage/mincore/fstat/
            // newfstatat/rt_sigaction) must return -EFAULT, exactly as native — never crash the engine, never
            // wrongly succeed. Complements clockefault.c (the clock family) and edge_efault.c (fcntl). RAW
            // syscalls hit dd's dispatch directly; the `-slow` sibling forces HL_JIT_NOFASTSYS. Byte-exact oracle.
            src("sysfault", "sysfault.c").oracle(),
            src("sysfault-slow", "sysfault.c")
                .env("HL_JIT_NOFASTSYS", "1")
                .oracle()
                .only(lin),
        ],
    )
}
