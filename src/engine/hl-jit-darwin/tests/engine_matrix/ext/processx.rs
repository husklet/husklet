//! processx — process-creation/lifecycle coverage. Owner: processx-coverage agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p hl-jit-darwin`).
//!
//! Beyond ext/posix's fork/wait/exec: posix_spawn (with file actions + re-exec of self), vfork, waitid
//! siginfo (CLD_EXITED/CLD_KILLED), and getrusage accounting — portable golden verdicts. Plus the
//! Linux-only prlimit, clone3, and direct futex, diffed against a native oracle.
#![allow(unused_imports)]
use crate::support::{fixture, group, in_rootfs, port, src, src_nopie, Case, Engine, Group};

const LIN: &[Engine] = &[Engine::LinuxAarch64, Engine::LinuxX86_64];

pub fn groups() -> Vec<Group> {
    vec![processx_portable(), processx_linux()]
}

fn processx_portable() -> Group {
    group(
        "ext-process",
        vec![
            port("posix-spawn", "ext_proc/posix_spawn.c")
                .out("posix_spawn spawned=1 exit7=1 captured=1\n"),
            port("vfork", "ext_proc/vfork.c").out("vfork reaped=1 exit9=1\n"),
            port("waitid", "ext_proc/waitid.c").out("waitid exited=1 killed=1\n"),
            port("getrusage", "ext_proc/getrusage.c")
                .out("getrusage self=1 maxrss=1 child=1 child_cpu=1\n"),
        ],
    )
}

fn processx_linux() -> Group {
    group(
        "ext-process-lin",
        vec![
        // prlimit(2) set now persists into the per-resource limit store (g_ulimit), so a subsequent get
        // reflects the lowered soft limit (lowered=1) on both Linux engines.
        src("prlimit", "ext_proc/prlimit.c").oracle(),
        // clone3(2): the aarch64 JIT does it correctly vs the native kernel. On x86_64 the JIT ALSO does
        // it correctly (made/reaped/exit11 all 1) but the qemu-x86_64 oracle lacks clone3 (ENOSYS →
        // made=0) — an oracle artifact, not an engine gap (cf. pidfd/process_vm). xfail x86_64.
        src("clone3", "ext_proc/clone3.c").oracle().xfail(&[Engine::LinuxX86_64]),
        src("futex", "ext_proc/futex.c").oracle(),       // direct FUTEX_WAIT/FUTEX_WAKE/FUTEX_WAKE_OP
        // Modern systemd-sysusers takes /etc/passwd's lock with Linux open-file-description fcntl
        // commands. Passing commands 36..38 through to macOS returns ENOTTY and prevents desktop
        // packages such as Chromium from installing in a workspace.
        src("ofd-lock", "ext_proc/ofd_lock.c").oracle(),
        // PI mutex (FUTEX_LOCK_PI/UNLOCK_PI under contention) + robust mutex (set_robust_list + OWNER_DIED
        // handoff). .out() golden not .oracle(): qemu-user x86_64 can't run PI futexes (hangs), so the golden
        // is the correct native-Linux result. Both hl Linux engines must give real mutual exclusion/recovery.
        port("pi-robust", "ext_proc/pi_robust.c").only(LIN)
            .out("pi_mutex sum=8000\nrobust eownerdead=1\n"),
        // (extends): non-PIE ET_EXEC pointer-arg rebase. Built static -no-pie (src_nopie) so the
        // loader biases the image high and dispatch.c's non-PIE g2h rebase switch (g_nonpie_lo) is armed —
        // the ONLY build that exercises it. Every syscall pointer here is a LOW static .bss/.data address;
        // the guard prints "name=ok" iff the valid pointer was NOT rejected with EFAULT, so a regressed
        // rebase flips a token (or crashes the run) and the .oracle() byte-compare vs native diverges. Covers
        // getgroups/semop/msgsnd (the report) plus the whole audited class: creds (capget/getres*), SysV IPC
        // (msgsnd/msgrcv/semop/semtimedop/semctl/shmctl), rt_signal (procmask/pending/action/altstack/
        // timedwait/sigqueue/tgsigqueueinfo), sched/rlimit (get/setrlimit/getaffinity/getattr/futex), wait4,
        // and poll/select (pselect/ppoll). Environment-independent verdicts keep native==JIT byte-exact.
        src_nopie("nonpie-ptrargs", "ext_proc/nonpie_ptrargs.c").oracle(),
        // cross-process futex on a MAP_SHARED page across fork (LTP fork04 / tst_checkpoint).
        // A FUTEX_WAKE in one process must wake a FUTEX_WAIT in another on the same shared physical
        // page (both directions), plus WAIT-timeout=ETIMEDOUT and WAKE(N) of N cross-process waiters.
        src("futex-xproc", "ext_proc/futex_xproc.c").oracle(),
        // cross-MAPPING futex: one MAP_SHARED page reached at two DIFFERENT virtual addresses (the Chrome
        // renderer<->GPU-service command-buffer split). A FUTEX_WAKE through mapping B must release a
        // FUTEX_WAIT parked through mapping A -- Linux keys the futex by the shared inode+offset, not the
        // VA. hl hashed the bucket on host VA, so the two mappings missed each other and the wake was lost.
        src("futex-shared-key", "ext_proc/futex_shared_key.c").oracle(),
        // a child killed by a fatal-default signal with no faithful fatal host mapping (SIGPOLL/
        // SIGSTKFLT map to host signals that default-ignore, SIGPWR maps to a different signo) must reach
        // the parent's wait4 as WIFSIGNALED/WTERMSIG=signo, not WIFEXITED(128+signo). Plus a real exit(157)
        // stays WIFEXITED (disambiguation) and the SIGKILL/SIGSEGV paths don't regress. LTP waitpid01 shape.
        src("waitsig", "ext_proc/waitsig.c")
            .out("waitsig sigpoll=1 sigsys=1 sigstkflt=1 sigpwr=1 exit157=1 sigkill=1 sigsegv=1\n")
            .oracle(),
        // ptrace(2) real tracer/tracee coordination. hl emulates the ptrace relationship BETWEEN two
        // guest processes (both translated) over a shared arena -- NOT the host macOS ptrace. A golden
        // verdict (both Linux arches): TRACEME + group-stop + PTRACE_SYSCALL entry/exit stops observe the
        // child's arch-native syscall numbers via GETREGS, with the TRACESYSGOOD 0x80 bit, and PEEKDATA
        // reads the tracee's memory. Linux-only (ptrace is Linux-specific; darwin uses a different model).
        // .out() golden rather than .oracle() because qemu-user's guest-ptrace-guest support is incomplete
        // (an oracle artifact); the golden is the correct native-Linux result.
        port("ptrace-tracer", "ext_proc/ptrace_tracer.c").only(LIN)
            .out("ptrace ok stopsig=1 sysgood=1 getpid=1 write=1 exit=1 peek=1 status=7\n"),
        // ^ peek=1: PEEKDATA reads the tracee's marker string out of its (separate host process) address
        // space via the stopped-tracee request/response channel.
        // ptrace exec-stop -- the strace -f initial event: TRACEME + execve stops the tracee with SIGTRAP
        // before the new image runs; PTRACE_CONT lets it finish.
        port("ptrace-exec", "ext_proc/ptrace_exec.c").only(LIN)
            .out("ptrace-exec ok execstop=1 exit=0\n"),
    ],
    )
}
