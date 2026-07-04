//! processx — process-creation/lifecycle coverage (task #311). Owner: processx-coverage agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! Beyond ext/posix's fork/wait/exec: posix_spawn (with file actions + re-exec of self), vfork, waitid
//! siginfo (CLD_EXITED/CLD_KILLED), and getrusage accounting — portable golden verdicts. Plus the
//! Linux-only prlimit, clone3, and direct futex, diffed against a native oracle.
#![allow(unused_imports)]
use crate::{group, src, port, fixture, in_rootfs, Case, Engine, Group};

const LIN: &[Engine] = &[Engine::LinuxAarch64, Engine::LinuxX86_64];

pub fn groups() -> Vec<Group> { vec![processx_portable(), processx_linux()] }

fn processx_portable() -> Group {
    group("ext-process", vec![
        port("posix-spawn", "ext_proc/posix_spawn.c").out("posix_spawn spawned=1 exit7=1 captured=1\n"),
        port("vfork", "ext_proc/vfork.c").out("vfork reaped=1 exit9=1\n"),
        port("waitid", "ext_proc/waitid.c").out("waitid exited=1 killed=1\n"),
        port("getrusage", "ext_proc/getrusage.c").out("getrusage self=1 maxrss=1 child=1 child_cpu=1\n"),
    ])
}

fn processx_linux() -> Group {
    group("ext-process-lin", vec![
        // prlimit(2) set now persists into the per-resource limit store (g_ulimit), so a subsequent get
        // reflects the lowered soft limit (lowered=1) on both Linux engines (#315).
        src("prlimit", "ext_proc/prlimit.c").oracle(),
        // clone3(2): the aarch64 JIT does it correctly vs the native kernel. On x86_64 the JIT ALSO does
        // it correctly (made/reaped/exit11 all 1) but the qemu-x86_64 oracle lacks clone3 (ENOSYS →
        // made=0) — an oracle artifact, not an engine gap (cf. pidfd/process_vm). xfail x86_64.
        src("clone3", "ext_proc/clone3.c").oracle().xfail(&[Engine::LinuxX86_64]),
        src("futex", "ext_proc/futex.c").oracle(),       // direct FUTEX_WAIT/FUTEX_WAKE
        // #400: cross-process futex on a MAP_SHARED page across fork() (LTP fork04 / tst_checkpoint).
        // A FUTEX_WAKE in one process must wake a FUTEX_WAIT in another on the same shared physical
        // page (both directions), plus WAIT-timeout=ETIMEDOUT and WAKE(N) of N cross-process waiters.
        src("futex-xproc", "ext_proc/futex_xproc.c").oracle(),
        // ptrace(2) real tracer/tracee coordination (#238). dd emulates the ptrace relationship BETWEEN two
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
    ])
}
