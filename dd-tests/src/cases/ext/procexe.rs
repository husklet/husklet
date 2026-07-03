//! procexe — the /proc/self/exe + readlink/readlinkat surface (#370/#317). Owner: procselfexe agent.
//! Edit ONLY this file. One comprehensive guest (guests/procexe/selfexe.c) covers: readlink of
//! /proc/self/exe in every spelling (absolute, pid alias, thread-self, dirfd-relative, cwd-relative),
//! open/stat/lstat/access THROUGH the magic link, /proc/self/fd/N link text (file/pipe/socket/closed),
//! cwd/root/mounts/ns links, EINVAL/ENOENT/EBADF/bufsiz<=0 semantics, real-symlink readlink-vs-readlinkat
//! consistency incl. truncation, and seven re-exec stages (execve of /proc/self/exe, relative path,
//! symlink, shebang-script, and execveat abs/dirfd-rel/AT_EMPTY_PATH) each asserting the child's
//! /proc/self/exe is the absolute CANONICAL binary path. darwin is excluded: there is no /proc surface
//! in the macOS container (BSD guests use KERN_PROCARGS2 — see guests/darwin/bsd_procpath.c).
#![allow(unused_imports)]
use crate::{group, src, port, fixture, in_rootfs, Case, Engine, Group};

pub fn groups() -> Vec<Group> { vec![procexe()] }

// The whole-surface golden: every boolean must be 1. This exact text is ALSO what a real Linux kernel
// (the native aarch64 oracle) emits byte-for-byte — see pe-selfexe below — so it is a proven oracle truth,
// not just an internal expectation.
const SELFEXE_GOLDEN: &str =
    "exe abs=1 canon=1 alias=1 tself=1 dirfd=1 cwdrel=1 selfpid=1 open=1 lstat=1 stat=1 access=1\n\
     fdlink file=1 pipe=1 sock=1 closed=1\n\
     magic cwd=1 root=1 mounts=1 ns=1 einval=1 eproc=1 zerobuf=1\n\
     sym rel=1 dirfd=1 cwd=1 abs=1 long=1 trunc=1 zero=1 dangle=1 efile=1 edir=1 enoent=1 ebadf=1\n\
     stage proc exe=1\nstage rel exe=1\nstage lnk exe=1\nstage shb exe=1\n\
     stage at exe=1\nstage atrel exe=1\nstage empty exe=1\nprocexe done\n";

fn procexe() -> Group {
    use Engine::{LinuxAarch64, LinuxX86_64};
    group("procexe", vec![
        // aarch64: the native oracle IS a real Linux kernel (the guest runs directly on the aarch64 host),
        // so this is a full byte-exact differential of the entire surface vs a real kernel — AND golden.
        src("pe-selfexe", "procexe/selfexe.c")
            .out(SELFEXE_GOLDEN)
            .oracle()
            .only(&[LinuxAarch64]),
        // x86_64: qemu-user is NOT a faithful oracle for this surface. It special-cases only the literal
        // readlink("/proc/self/exe") and leaks the HOST qemu-x86_64 identity for /proc/thread-self/exe,
        // readlinkat(dirfd,"exe"), the cwd-relative spelling and stat()-THROUGH the link — so an oracle
        // diff here would compare the JIT (correct) against qemu's own broken /proc emulation. This is the
        // same limitation that makes pe-comm golden-only. Instead assert the GOLDEN, which the aarch64 case
        // above proves is exactly what a real Linux kernel returns — a transitive oracle validation.
        src("pe-selfexe-x86", "procexe/selfexe.c")
            .out(SELFEXE_GOLDEN)
            .only(&[LinuxX86_64]),
        // comm fidelity: Linux sets comm from the LAST component of the path PASSED to execve
        // (/proc/self/exe -> "exe"; ./x -> "x"; a #! script keeps ITS name, not the interpreter's).
        // Golden, not oracle-diffed: under the qemu-x86 oracle comm is set by the HOST kernel/binfmt
        // (qemu-x86_64/fd-number strings), so the truth here is native-Linux semantics — verified
        // against a native aarch64 Linux run — enforced on both engines.
        src("pe-comm", "procexe/selfexe.c").arg("comm")
            .out("commchk self=selfexe\nstage proc comm=exe\nstage rel comm=selfexe\n\
                  stage lnk comm=lnk-selfexe\nstage shb comm=shb.sh\ncommchk done\n"),
    ])
}
