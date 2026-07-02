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
        // prlimit(2) set is not reflected on the Linux JIT: it returns success but a subsequent get
        // still shows the old soft limit (lowered=0). xfail Linux; GAPS `processx-prlimit-set`.
        src("prlimit", "ext_proc/prlimit.c").oracle().xfail(LIN),
        // clone3(2): the aarch64 JIT does it correctly vs the native kernel. On x86_64 the JIT ALSO does
        // it correctly (made/reaped/exit11 all 1) but the qemu-x86_64 oracle lacks clone3 (ENOSYS →
        // made=0) — an oracle artifact, not an engine gap (cf. pidfd/process_vm). xfail x86_64.
        src("clone3", "ext_proc/clone3.c").oracle().xfail(&[Engine::LinuxX86_64]),
        src("futex", "ext_proc/futex.c").oracle(),       // direct FUTEX_WAIT/FUTEX_WAKE
    ])
}
