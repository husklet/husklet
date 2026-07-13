//! fsx — extended filesystem-syscall coverage. Owner: fsx-coverage agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! Fills gaps left by ext/posix: the dirfd-relative *at() family (fchmodat/fchownat/faccessat/linkat/
//! symlinkat/renameat/mkdirat/mkfifoat), extended attributes (portable via an __APPLE__ branch), and
//! the Linux-only file corners posix_fadvise/close_range/O_PATH which have no portable POSIX shape and
//! are diffed against a native oracle.
#![allow(unused_imports)]
use dd_tests::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![fsx_portable(), fsx_xattr_edge(), fsx_linux()]
}

/// Portable fs surface — golden verdict identical emulated-on-Linux and native-on-macOS.
fn fsx_portable() -> Group {
    group("ext-fsx", vec![
        port("atflags", "ext_fsx/atflags.c")
            .out("atflags chmod=1 mode=1 chown=1 acc=1 accmiss=1 unlink=1 gone=1\n"),
        // readlinkat(dirfd, relpath): fixed by — a relative link path resolves against the
        // caller's dirfd now (fs.c case 78), so this passes on the Linux JIT too (xfail removed).
        port("linkat", "ext_fsx/linkat.c")
            .out("linkat sym=1 readlink=1 link=1 nlink2=1 rename=1 oldgone=1 newhas=1 mkdir=1 isdir=1\n"),
        port("mkfifoat", "ext_fsx/mkfifoat.c").out("mkfifoat made=1 isfifo=1 ready=1 got=1\n"),
        // xattr round-trips on the Linux JIT too: guest xattrs are namespaced under `user.ddx.` on the
        // host backing inode (set/get/list/remove all persist), and copy-up carries them (overlay G5).
        // (darwin uses the 6-arg setxattr, branched in-source.)
        port("xattr", "ext_fsx/xattr.c").out("xattr set=1 roundtrip=1 listed=1 removed=1 gone=1\n"),
    ])
}

/// xattr errno/edge fidelity (LTP setxattr/getxattr/removexattr family): ENODATA on a missing attr,
/// EEXIST for XATTR_CREATE on an existing one, ERANGE for a too-small buffer, the size==0 length-probe.
/// Linux-only errno shapes with no portable form — diffed vs the native oracle on both Linux engines.
fn fsx_xattr_edge() -> Group {
    group(
        "ext-fsx-xattr",
        vec![src("xattr-edge", "ext_fsx/xattr_edge.c").oracle()],
    )
}

/// Linux-only fs syscalls with no portable form — diffed vs the native oracle (both Linux engines).
fn fsx_linux() -> Group {
    group(
        "ext-fsx-lin",
        vec![
            src("fadvise", "ext_fsx/fadvise.c").oracle(), // posix_fadvise hints
            src("close-range", "ext_fsx/closerange.c").oracle(), // close_range(2) bulk fd close
            // O_PATH fds are now tracked (g_opath); the read/write family through one returns EBADF as Linux
            // does, while fstat and use as an openat dirfd still work.
            src("opath", "ext_fsx/opath.c").oracle(),
            // /dev/full: reads yield zeros, writes fail ENOSPC (macOS has no /dev/full; the container fs
            // synthesizes it). Verdict-only so the golden line is a plain Linux truth.
            src("devfull", "ext_fsx/devfull.c").out("devfull opened=1 zeros=1 enospc=1\n"),
            // /proc/self/{root,cwd} magic symlinks: root -> "/", cwd -> an absolute path (#container-fs).
            src("procself", "ext_fsx/procself.c").out("procself root=1 cwd=1\n"),
        ],
    )
}
