//! pcachex — persistent cross-process translated-code cache (aarch64 + opt8 x86_64), matrix lane.
//! Owner: arm-pcache agent. Edit ONLY this file.
//!
//! These cases run with `HL_JIT_PCACHE=1` against a FIXED cache dir, so successive matrix runs alternate
//! cold (save) and warm (load) on the same files — the golden output must be byte-identical either way,
//! which is exactly the cache's correctness contract. The guests cover the three lifecycle shapes the
//! hardening exists for:
//!   * fork WITHOUT execve (forkpipe / the poisoned-fork-child-save crash class),
//!   * fork + execve of the same binary (selfexec / the go-build storm + in-process exec reload),
//!   * plain single-process cold/warm (hello shape, via the compute in both guests).
//! The full multi-invocation lifecycle (cold->warm identity, concurrent same-key processes, stale-binary
//! invalidation, kill-switch, corrupt-file rejection) lives in `hl-jit-darwin/tests/pcache.rs`.
//!
//! Cache keys include the engine build id, so parallel worktrees / rebuilds never share entries; the
//! fixed dir only makes REPEATED runs of one build warm. HL_JIT_NOPCACHE=1 in the ambient env would
//! disable the engine's cache, but the per-case env here is baked into the launch script and the `mac`
//! bridge drops ambient env, so these cases always exercise the cache path as written.
#![allow(unused_imports)]
use crate::support::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![pcachex()]
}

fn pc(c: Case) -> Case {
    c.env("HL_JIT_PCACHE", "1")
        .env("HL_JIT_PCACHE_DIR", "/tmp/ddjit-pcache-matrix")
}

fn pcachex() -> Group {
    group(
        "pcachex",
        vec![
            // fork child re-translates from a fresh arena and exits while the parent waits; deterministic
            // pipe-reported result. Cold run saves; every later run loads. Both must print exactly this.
            pc(src("pc-forkpipe", "pcachex/forkpipe.c")
                .out("pcache forkpipe r=8 h=5807372f99a952f5 exit=0\n")),
            // the go-build shape: 6 forked children each execve /proc/self/exe (in-process exec cache reload,
            // per-child cold-miss save on the first matrix run, per-child HIT afterwards).
            pc(src("pc-selfexec", "pcachex/selfexec.c")
                .out("pcache selfexec reaped=6 sum=75 bad=0\n")),
            // fork child that OUTLIVES the parent without execve — the exact crash repro: pre-fix, the
            // child's exit-time save (stale inherited reloc table vs its fresh arena) was the LAST rename and
            // poisoned the cache file; the NEXT matrix run then crashed/hung on the warm load.
            pc(src("pc-forkoutlive", "pcachex/forkoutlive.c").out("pcache forkoutlive forked=1\n")),
            // threaded guest: exit_group saves while peer threads are still live — the snapshot must be taken
            // under the translation lock (a torn arena here corrupts every later warm run).
            pc(src("pc-threads", "threads_mutex.c").out("queue produced=40000 consumed=40000\n")),
            // exec re-key: the driver epoch execve's itself as a DIFFERENT argv[0] basename (the
            // multicall applet switch). The driver epoch saves at the exec boundary under ITS key; the applet
            // epoch re-keys, reloads, and saves under its own — two distinct cache files, byte-identical
            // output on every cold/warm alternation. (The two-file protocol is asserted in tests/pcache.rs.)
            pc(src("pc-execchain", "pcachex/execchain.c")
                .out("pcache execchain applet ok argc=1\n")),
            // selective restore: a file-backed executable (library-like) map. Cold saves it in the
            // manifest; a warm run DEFERS its blocks and activates them only on the identity-matched re-map, so
            // a restored translation can never shadow different guest bytes. Output is deterministic either way
            // (the accumulator), which is the correctness contract this case guards on both engines.
            pc(src("pc-libmap", "pcachex/libmap.c").out("pcache libmap acc=506500\n")),
        ],
    )
}
