//! memx — virtual-memory syscall coverage (task #311). Owner: memx-coverage agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! The mmap/mprotect surface beyond ext/posix's anon+file basics: MAP_FIXED replacement, page locking
//! (mlock/mlockall), and the Linux-only mremap grow/move + mincore residency (diffed vs native oracle).
#![allow(unused_imports)]
use crate::{group, src, port, fixture, in_rootfs, Case, Engine, Group};

pub fn groups() -> Vec<Group> { vec![memx()] }

fn memx() -> Group {
    group("ext-memmap", vec![
        // portable — same golden emulated-on-Linux and native-on-macOS
        port("mapfixed", "ext_mm/mapfixed.c").out("mapfixed placed=1 zeroed=1 wrote=1 neighbours=1\n"),
        port("mlock", "ext_mm/mlock.c").out("mlock lock=1 usable=1 unlock=1 lockall=1 unlockall=1\n"),
        // Linux-only (no macOS mremap) -> native oracle
        src("mremap", "ext_mm/mremap.c").oracle(),
        // mincore now projects host-page (16 KB) residency onto the guest's page granularity, so the
        // x86_64 guest's 4 KB-page residency vector is filled correctly (resident=4), matching aarch64
        // and the native oracle (#319).
        src("mincore", "ext_mm/mincore.c").oracle(),
        // #397: the mmap/munmap/mincore/madvise semantics surface behind LTP mmap08/mmap12/munmap01/
        // munmap03/mincore02/mincore04/madvise10 -- mmap len0 & bad-fd errors, munmap len0/misaligned/
        // out-of-range EINVAL, MAP_POPULATE zero-fill, mincore residency + EINVAL, MADV_DONTNEED zeroing,
        // MADV_WILLNEED/FREE, and MADV_WIPEONFORK/KEEPONFORK. Diffed vs native on both Linux arches.
        src("mmsem", "ext_mm/mmsem.c").oracle(),
        // #417: the mmap/msync/madvise/mremap errno+flag surface not pinned above -- MAP_NORESERVE usable,
        // MAP_SHARED-write of an O_RDONLY fd -> EACCES (private ok), msync flag validation (MS_SYNC/ASYNC
        // ok, both-set/unknown-bit -> EINVAL), MADV_DONTFORK/DOFORK accepted, and mremap(MREMAP_FIXED)
        // relocate-to-exact-addr + its FIXED-without-MAYMOVE / mis-aligned EINVAL guards. Byte-exact vs
        // native on both Linux arches (fixes: mem.c mremap MREMAP_FIXED + msync flag EINVAL).
        src("mmerr", "ext_mm/mmerr.c").oracle(),
    ])
}
