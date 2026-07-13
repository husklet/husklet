//! dentry — positive dentry/path-resolution cache coherence. Owner: dentry-cache agent. Edit ONLY this file.
//! The engine memoizes successful path resolutions per directory (fscache.c `dc_*`, epoch-gated on the
//! container-shared namespace epoch, exactly like the rc_/oc_/updirneg caches). These cases interleave
//! every name->object mutation (create/unlink/rename/symlink-flip/hardlink/dir-chain-rename/rmdir+
//! recreate/cross-process create) with lookups and assert a lookup NEVER sees a stale resolution, plus
//! the O_NOFOLLOW/follow cache-mixing corner (must ELOOP right after a follow-mode open). They run
//! inside the alpine rootfs because the path caches only exist under a container jail; both Linux
//! engines are covered. The darwin container resolves paths via darwinjail + the host FS — this
//! resolver/cache does not exist there, so a darwin variant would exercise none of the code under test.
//! Overlay-mode coverage (copy-up relocation, whiteouts, opaque dirs) comes from the daemon scenarios
//! (utilities/overlay-metadata + filesystem/dentry-storm-overlay), which run both guest arches.
#![allow(unused_imports)]
use dd_tests::{group, src, Case, Engine, Group};

const LIN: &[Engine] = &[Engine::LinuxAarch64, Engine::LinuxX86_64];

pub fn groups() -> Vec<Group> {
    vec![group(
        "ext-dentry",
        vec![
            // 200-iteration mutation<->lookup interleave storm (doubles as the soak for this cache):
            // any positive dentry/path entry that outlives its invalidating mutation fails a check.
            src("dentry-storm", "dentry/storm.c")
                .rootfs("alpine")
                .only(LIN)
                .out("dentry-storm iters=200 readdir=7 ghost=0 fails=0\n")
                .exit(0),
        ],
    )]
}
