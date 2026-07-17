//! Core ABI / codegen, libc breadth & C-runtime behaviour, and a heavy timing microbench.

use crate::support::{group, port, src, Engine, Group};

/// Core ABI / codegen — compiled guests, diffed against a native oracle.
pub(super) fn compat() -> Group {
    group(
        "compat",
        vec![
            src("hello", "hello.c").exit(42).out("hi\n"),
            src("math", "math.c").oracle(),
            src("strings", "strings.c").oracle(),
            src("bitops", "bitops.c").oracle(), // popcount/clz/ctz/bswap
            src("mov-moffs", "moffs.c")
                .only(&[Engine::LinuxX86_64])
                .oracle(), // MOV A0-A3 (acc<->abs64 addr; node/V8/mongosh)
            src("varargs", "varargs.c").oracle(), // stdarg + snprintf formats
            src("longjmp", "longjmp.c").out("longjmp r=42\n"),
            src("recursion", "recursion.c").oracle(), // fib(30) + ackermann (§B depth gate)
            src("fnptr", "fnptr.c").oracle(),         // function pointers -> IBTC / inline cache
            src("jumptable", "jumptable.c").oracle(), // dense switch -> jump table
            // IBTC stress: a 128-target MEGAMORPHIC computed-goto bytecode VM (CPython/VDBE
            // shape -> hammers the inline indirect-branch target cache) + a MONOMORPHIC deep recursion
            // (real call/ret). A wrong IBTC prediction that jumped to the wrong handler/return would corrupt
            // the deterministic checksum, so this golden runs byte-identically on both Linux engines.
            src("ibtc-dispatch", "ibtc_dispatch.c")
                .out("ibtc vm=10240120795314104034 rec=2178309 chk=12619423276023875997\n"),
            src("floatmath", "floatmath.c").oracle(),
            // FP-codegen edge differential: MIN/MAX NaN+-0 (H10), CMPNLT/NLE NaN (H12), float->int
            // indefinite (H13), ROUND MXCSR mode. x86 SSE codegen only; byte-exact vs the qemu oracle.
            src("fpedge", "fpedge.c")
                .only(&[Engine::LinuxX86_64])
                .oracle(),
            // Default/indefinite-NaN sign: x86 emits the NEGATIVE default NaN (0xFFC00000 / 0xFFF8..) on a
            // GENERATED NaN (0/0, inf/inf, 0*inf, inf-inf, sqrt<0) where ARM emits the positive default NaN;
            // propagated NaNs keep their input sign (no over-flip). div/mul/sub/add/sqrt scalar+packed sgl+dbl.
            src("fpdnan", "fpdnan.c")
                .only(&[Engine::LinuxX86_64])
                .oracle(),
            // DF (direction flag) is now a runtime cpu bit: a `std`/popfq-set direction persists across block
            // boundaries and is honored by `rep movs/stos` (backward). Was translate-time-only -> silent
            // forward copy for a cross-block string op. Cross-block forced via a noinline call.
            src("repmovsdf", "repmovsdf.c")
                .only(&[Engine::LinuxX86_64])
                .oracle(),
            // x87 m80 FLD/FSTP <-> double converters, byte-exact on the value classes independent of carrier
            // width (+-0/+-Inf/NaN/exact-in-double). Pins the Inf/NaN converter fixes. long-double arithmetic
            // precision (double-carrier drift, H11/#248/#249) is deliberately NOT tested.
            src("x87m80", "x87m80.c")
                .only(&[Engine::LinuxX86_64])
                .oracle(),
            // SHLD/SHRD now materialize real CF (last bit out) + PF (M item); capture RFLAGS after each
            // and diff CF|PF|ZF|SF vs qemu, incl by-CL and count==0 flag preservation. x86 codegen only.
            src("shldflags", "shldflags.c")
                .only(&[Engine::LinuxX86_64])
                .oracle(),
            // Stolen-register codegen surface (aarch64: x16/x17/x18/x28/x30 live in cpu->x[]): inline-asm
            // coverage of every mangle shape -- data-processing (1..3 distinct stolen regs), loads/stores
            // (stolen Rt/base/writeback/pairs), adr + ldr-literal INTO a stolen reg, TLS via tpidr_el0
            // (stolen + non-stolen), and cbz/tbz TESTING a stolen reg. Pins both the legacy mscratch dance
            // and the steal-mode fast paths (stealfast) byte-exactly against a native run.
            src("stolen-regs", "stolen_regs.c")
                .only(&[Engine::LinuxAarch64])
                .oracle(),
        ],
    )
}

/// libc-heavy paths.
pub(super) fn libc() -> Group {
    group(
        "libc",
        vec![
            src("heap", "heap.c").oracle(),
            src("qsort", "qsort.c").oracle(),
            src("files", "files.c").out("files n=7 data=payload\n"),
            src("statfile", "statfile.c").oracle(), // open/write/stat/access/unlink
            src("pipe", "pipe.c").out("pipe n=10 piped-data\n"),
            src("mmapanon", "mmapanon.c").oracle(), // mmap/munmap anon
            // #209: partial munmap (head/middle split) keeps the surviving sub-region(s) mapped + tracked.
            src("munmap_partial", "munmap_partial.c").out(
                "munmap_partial head: unmap=0 tail_a=17 tail_b=90 free=0\n\
                 munmap_partial middle: unmap=0 head=51 tail=68 free_h=0 free_t=0\n",
            ),
        ],
    )
}

/// libc breadth & C-runtime behaviour — regex, glob, float parsing, calendar math, environment, exit
/// handlers, and signal control flow. Portable across engines, golden-checked.
pub(super) fn clib() -> Group {
    group(
        "clib",
        vec![
            port("regex", "regex.c").out("regex hit=1 group=2026 miss=1\n"),
            port("glob", "globmatch.c").out("glob txt=3 all=5\n"),
            port("strtod", "strtod.c").out("strtod pi=1 sci=1 hex=1 inf=1 acc=10000\n"),
            port("timefmt", "timefmt.c").out("timefmt fmt=1 roundtrip=1 wday=2\n"),
            port("environ", "environ.c")
                .out("environ set=1 nooverwrite=1 overwrite=1 unset=1 haspath=1\n"),
            port("atexit", "atexit.c").out("atexit order=cba"),
            port("sigaction", "sigaction2.c").out("sigaction usr1=1 signo_ok=1 chld=1\n"), // SA_SIGINFO + SIGCHLD
            port("sigjmp", "sigjmp.c").out("sigjmp hops=3 from=3\n"), // sigsetjmp/siglongjmp
        ],
    )
}

/// Heavier workloads (also exercise the timing column).
pub(super) fn perf() -> Group {
    group(
        "perf",
        vec![
            src("sortbig", "sortbig.c").oracle(), // qsort 300k longs
        ],
    )
}
