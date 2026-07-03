//! fsx — extended filesystem-syscall coverage (task #311). Owner: fsx-coverage agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! Fills gaps left by ext/posix: the dirfd-relative *at() family (fchmodat/fchownat/faccessat/linkat/
//! symlinkat/renameat/mkdirat/mkfifoat), extended attributes (portable via an __APPLE__ branch), and
//! the Linux-only file corners posix_fadvise/close_range/O_PATH which have no portable POSIX shape and
//! are diffed against a native oracle.
#![allow(unused_imports)]
use crate::{group, src, port, fixture, in_rootfs, Case, Engine, Group};

const LIN: &[Engine] = &[Engine::LinuxAarch64, Engine::LinuxX86_64];

pub fn groups() -> Vec<Group> { vec![fsx_portable(), fsx_linux()] }

/// Portable fs surface — golden verdict identical emulated-on-Linux and native-on-macOS.
fn fsx_portable() -> Group {
    group("ext-fsx", vec![
        port("atflags", "ext_fsx/atflags.c")
            .out("atflags chmod=1 mode=1 chown=1 acc=1 accmiss=1 unlink=1 gone=1\n"),
        // readlinkat(dirfd, relpath) returns the wrong result on the Linux JIT (readlink=0: the link
        // text isn't read back), though the symlink resolves for other ops — passes native-on-macOS.
        // xfail Linux; GAPS `fsx-readlinkat-dirfd`.
        port("linkat", "ext_fsx/linkat.c")
            .out("linkat sym=1 readlink=1 link=1 nlink2=1 rename=1 oldgone=1 newhas=1 mkdir=1 isdir=1\n")
            .xfail(LIN),
        port("mkfifoat", "ext_fsx/mkfifoat.c").out("mkfifoat made=1 isfifo=1 ready=1 got=1\n"),
        // xattr round-trips on the Linux JIT too: guest xattrs are namespaced under `user.ddx.` on the
        // host backing inode (set/get/list/remove all persist), and copy-up carries them (overlay G5).
        // (darwin uses the 6-arg setxattr, branched in-source.)
        port("xattr", "ext_fsx/xattr.c").out("xattr set=1 roundtrip=1 listed=1 removed=1 gone=1\n"),
    ])
}

/// Linux-only fs syscalls with no portable form — diffed vs the native oracle (both Linux engines).
fn fsx_linux() -> Group {
    group("ext-fsx-lin", vec![
        src("fadvise", "ext_fsx/fadvise.c").oracle(),        // posix_fadvise hints
        src("close-range", "ext_fsx/closerange.c").oracle(), // close_range(2) bulk fd close
        // An O_PATH fd is readable on the Linux JIT (read_ebadf=0) — Linux rejects reads through it with
        // EBADF. xfail Linux; GAPS `fsx-opath-readable`.
        src("opath", "ext_fsx/opath.c").oracle().xfail(LIN),
    ])
}
