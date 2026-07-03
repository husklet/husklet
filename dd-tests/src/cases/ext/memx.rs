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
    ])
}
