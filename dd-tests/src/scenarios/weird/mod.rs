//! Weird / edge software — the cases that hammer the engine's corners hardest: JIT-in-JIT runtimes
//! (V8/JVM/LuaJIT/PyPy/BEAM/RyuJIT/Julia emitting + executing their own machine code), self-modifying
//! code, exotic syscalls (io_uring, eBPF, seccomp, ptrace, userfaultfd, memfd, timerfd, inotify),
//! compression/crypto codegen (gzip/bzip2/xz/zstd/openssl), unusual languages (Haskell/Erlang/Forth/
//! Tcl/Lua/R), and CPU-feature probing (cpuid/NEON, getauxval AT_HWCAP, rdtsc/cntvct, cpu-topology).
//! These are where a translator is most likely to diverge. Both Linux arches. Owner: weird agent.
//!
//! Every scenario below is proven on the REAL oracle (Docker Desktop, arm64) — the marker matches the
//! real output, so the TEST is correct. `.xfail()` flags a *suspected* dd divergence (see GAPS.md):
//!   * gcc-compiled C cases → the documented toolchain fork-exec / exec-loader gap blocks `cc`/`ld`
//!     (and `ghc`/`dotnet build`), so they xfail on both Linux arches; each ALSO probes a deeper corner
//!     (RWX exec, SMC re-translation, signal-on-fault, rdtsc, futex, a syscall) — XPASS after the
//!     toolchain fix reveals whether that corner works.
//!   * python:3.12-slim cases → xfail amd64 only (jit86-opcode-1c: silent exit 255 on x86_64).
//!   * cpu-topology / non-PIE exec seed cases → the existing GAPS rows.

use crate::scenario::{scen, sgroup, ScenGroup, Scenario, Target};

mod codegen;
mod cpu;
mod gaps;
mod native;
mod runtimes;

/// A C program compiled+run inside `gcc:latest` (glibc, both arches). Compiling forks cc1/as/ld — the
/// documented toolchain fork-exec / exec-loader gap — so these xfail on both Linux arches; the comment
/// at each call site names the deeper corner the program additionally exercises.
pub(super) fn cc(id: &'static str, flags: &str, src: &str) -> Scenario {
    let script = format!("cat > /m.c <<'CEOF'\n{src}\nCEOF\ncc /m.c {flags} -o /m && /m");
    scen(id, "gcc:latest")
        .exec(&script)
        .long()
        .xfail(&Target::LINUX)
}

pub fn group() -> ScenGroup {
    sgroup(
        "weird",
        runtimes::items()
            .into_iter()
            .chain(native::items())
            .chain(cpu::items())
            .chain(codegen::items())
            .chain(gaps::items())
            .collect(),
    )
}
