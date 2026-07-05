//! forkx — fork-path stress (preserved-arena fork). Owner: fork-cost agent. Edit ONLY this file.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! The change makes a fork child KEEP the parent's dual-mapped code cache (RX re-remapped from the
//! child's COW pages at the same VA) instead of rebuilding it, so the specific hazards are: a stale alias
//! (child emission invisible to the execute view -> wild exec), a torn inheritance, and drift across
//! nested fork generations. forkstorm drives 1000 sequential forks through five mixed patterns --
//! fork+exit, fork+warm-work, fork+COLD-translation (post-fork emission must reach the RX alias),
//! fork+execve(self) (in-process exec teardown after a preserved fork), and NESTED fork (generation-2
//! re-remap) -- and golden-checks one deterministic checksum over every child's exit code. Runs on both
//! Linux engines (golden cross-checked native aarch64 vs qemu-x86_64: identical).

use crate::{group, src, Group};

pub fn groups() -> Vec<Group> {
    vec![group(
        "forkx",
        vec![
            src("forkstorm", "forkstorm.c").out("forkstorm reaped=1000 sum=31283\n"),
            // fork from a THREADED parent: the child takes jit_after_fork's conservative rebuild branch
            // (torn-snapshot safety); guards the hole-reuse munmap regression described in the guest source.
            src("thrfork", "thrfork.c").out("thrfork sum=1225\n"),
        ],
    )]
}
