# JIT, Cache, and Opcode Gaps

This file covers instruction fidelity, opcode coverage, stale translation, and hidden completeness holes.

## Thread-Directed Signals Do Not Interrupt Blocking Reads

Priority: P2
Impact: wrong `EINTR`/restart behavior and delayed signal handling
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-I-jit-runtime-20260710`.

Evidence:

- `tgkill` marks the target thread pending and sets `irq`: `dd-jit-darwin/src/runtime/os/linux/thread.c:1014`.
- It only wakes published futex waits: `dd-jit-darwin/src/runtime/os/linux/thread.c:1021`.
- Blocking `read` stays in the host syscall loop: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:552`.

Why this is bad:

A thread-directed signal should interrupt a target blocked in read/accept/recv-style syscalls when restart rules allow. dd delays delivery until the host read returns.

Isolated proof:

```sh
timeout 5 qemu-x86_64 target-worker-I/poc/tgkill_read_eintr
timeout 10 mac bash -lc "exec '$PWD/target-worker-I/release/build/dd-jit-darwin-5b0dabfbe6f0af2e/out/ddjit-linux_x86_64' '$PWD/target-worker-I/poc/tgkill_read_eintr'"
```

Observed: qemu `read_ret=-1 errno=4 delayed=0 rc=0`; dd `read_ret=1 errno=0 delayed=1 rc=1`.

## MXCSR Sticky Exception Flags Are Not Modeled

Priority: P2
Impact: `stmxcsr`/exception tests see stale or default flags
Confidence: Medium

Evidence:

- `ldmxcsr` maps only rounding-control bits into host FPCR: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3338`.
- `stmxcsr` reports default MXCSR plus current rounding mode: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3359`.

Why this is bad:

MXCSR includes sticky exception flags and control bits beyond rounding mode. Code using `stmxcsr` or `fetestexcept` after FP operations can observe wrong state.

Verification:

Add probes for invalid/divide-by-zero/overflow sticky bits after SSE operations and compare dd against native/qemu.

## Opcode Completeness Is Still Curated, Not Exhaustive

Priority: P2
Impact: false confidence in opcode/syscall completeness
Confidence: Medium-high

Evidence:

- `dd-tests/src/cases/ext/completeness/mod.rs` describes systematic syscall-table and opcode-space coverage.
- Current registered probes are a curated subset; the static backstop is also suspect because `coverage.sh` uses stale paths (see [daemon-tests-docs.md](daemon-tests-docs.md#coverage-tool-uses-stale-engine-paths-and-exits-green)).

Why this is bad:

The suite can be valuable while still failing to prove completeness. The hidden AVX/F16C/SSE4.2 issues above are examples: implemented op families are present, but narrow tests miss important semantic variants.

Suggested improvement:

Generate a checked-in matrix with at least:

- syscall number/name
- handler location or explicit unsupported verdict
- dynamic test coverage
- xfail/gap row
- opcode family/subform
- semantic dimensions tested, such as flags, MXCSR/rounding, memory vs register, VEX vs legacy, scalar merge, fault behavior

## x87 Long Double Precision Is Truncated

Priority: P2
Impact: extended-precision computations can underflow to binary64 results
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bf2-audit-20260710`.

Evidence:

- x87 stack is represented as double, not ext80: `dd-jit-darwin/src/runtime/translate/x86_64/translate/x87.c:15`.
- `fldt/fstpt` narrow through double: `dd-jit-darwin/src/runtime/translate/x86_64/x86_ops.c:293`.

Why this is bad:

x87 `long double` has extended precision. Narrowing through binary64 can turn positive extended-precision results into zero and break numerics that intentionally use x87 precision.

Isolated proof:

```sh
qemu-x86_64 target/bf2/x87_ext_precision
ddjit-linux_x86_64 target/bf2/x87_ext_precision
```

qemu printed `positive=1 out=1.08420217248550443401e-19`; dd printed `positive=0 out=0`.

## `fxsave` / `fxrstor` Do Not Preserve x87/MMX Register Data Or FSW

Priority: P2
Impact: a context that saves/restores actual FPU register values via fxsave/fxrstor gets stale registers
Confidence: High

Status: Remainder of a previously-fixed finding. The proven impact (MXCSR + FCW control/rounding round-trip) is FIXED — fxsave now writes MXCSR@24 + FCW@0 and fxrstor restores both (see `dd-jit-darwin/src/runtime/translate/x86_64/translate.c`, the `0F AE` fxsave/fxrstor block), with regression `dd-tests` case `comp-x86-misc/fxsave-mxcsr`. This section tracks only the register-DATA remainder.

Evidence:

- fxsave/fxrstor move the XMM lanes (@160) and now MXCSR/FCW, but NOT the x87 register stack ST0-7 (@32, ten bytes each), MMX register contents, or the FSW (@2): `dd-jit-darwin/src/runtime/translate/x86_64/translate.c` (fxsave/fxrstor `0F AE` block).

Why this is bad:

`fxrstor` should restore the full x87/MMX register file and status word. A program that fxsaves actual FPU register values and later fxrstors them keeps the values the handler/other code left, not the saved ones. (Restoring ST0-7 also intersects the separate "x87 long double precision is truncated" gap, since the engine models the x87 stack as double.)

Verification:

Extend a probe like `dd-tests/guests/completeness/x86_fxsave_mxcsr.c` to load distinct ST0-7 values, fxsave, clobber the x87 stack, fxrstor, and compare the restored ST values against native/qemu.

## 4K Guest `munmap` Subpage Remains Readable

Priority: P1
Impact: guest SIGSEGV is not delivered for unmapped 4K subpages
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-jitfault-audit-20260710`.

Evidence:

- Guest `munmap` handles subpage bookkeeping: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:228`, `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:245`, `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:256`.
- x86 data loads dereference host memory directly: `dd-jit-darwin/src/runtime/translate/x86_64/decode.c:433`.

Why this is bad:

After a guest 4K page is unmapped, scalar loads and `rep movsb` should fault and deliver guest `SIGSEGV`. dd continues reading the host backing bytes, so guest fault handlers never run and memory safety/probing semantics are wrong.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-jitfault-audit-20260710/target-jitfault-audit/release/build/dd-jit-darwin-16122afd27b6bb64/out cargo run -q -p dd-tests --target-dir /Users/x/dd/dd-jitfault-audit-cargo -- -e x86_64 repmovs-fault
```

qemu observed `repmovs_fault scalar=1 sig=1 copied4=1 tail=1 regs=1 addr=1 rip_nonzero=1`; dd observed `scalar=0 sig=0 ... addr=0 rip_nonzero=0`.

## `ICEBP` And Invalid `0x62` Bytes Abort Instead Of Guest Traps

Priority: P2
Impact: unsupported x86 trap encodings terminate instead of delivering guest signals
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-jitfault-audit-20260710`.

Evidence:

- x86 decode covers single-byte and prefix paths: `dd-jit-darwin/src/runtime/translate/x86_64/decode.c:65`, `dd-jit-darwin/src/runtime/translate/x86_64/decode.c:175`, `dd-jit-darwin/src/runtime/translate/x86_64/decode.c:209`.
- Unhandled instruction paths abort as unimplemented: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3808`, `dd-jit-darwin/src/runtime/translate/x86_64/avx.c:1958`.

Why this is bad:

`ICEBP` should deliver a guest trap, and invalid `0x62`/BOUND-family byte sequences should produce guest fault behavior. dd aborts with unimplemented errors and exit code `70`.

Isolated proof:

```sh
timeout 5 qemu-x86_64 target/dd-tests/x86_64/completeness/x86_fault_traps icebp
timeout 8 /Users/x/dd/dd-jitfault-audit-20260710/target-jitfault-audit/release/build/dd-jit-darwin-16122afd27b6bb64/out/ddjit-linux_x86_64 target/dd-tests/x86_64/completeness/x86_fault_traps icebp
```

qemu reported guest traps; dd reported `UNIMPL 1B opcode 0xf1` or `UNIMPLEMENTED EVEX` and exited `70`.

## aarch64 Threaded Self-Modifying Code Executes Stale Translations

Priority: P1
Impact: patched code keeps executing old translated bytes when another guest thread is live
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-jit-memorder-cache-20260710`.

Evidence:

- aarch64 `smc_icflush()` skips in-place SMC drop when another guest thread is live: `dd-jit-darwin/src/runtime/translate/aarch64/translate.c:1184`.
- Translation cache drop behavior is in the shared engine cache: `dd-jit-darwin/src/runtime/engine/cache.c:999`.

Why this is bad:

Guest code that patches an already-executed function and calls `__clear_cache` should execute the new bytes. dd keeps running the old translation in threaded cases, breaking JITs, trampolines, and hot patching.

Observed proof:

```text
Linux: threaded-smc before=1 after=2 expected=2
dd:    threaded-smc before=1 after=1 expected=2
```

## x86 Persistent Cache Key Ignores Codegen Env Modes

Priority: P2
Impact: persistent cache can reuse translations emitted under different codegen settings
Confidence: Medium-high

Verification status: Source-proven in isolated worktree `/Users/x/dd/dd-audit-jit-memorder-cache-20260710`.

Evidence:

- x86 pcache effective version keys only the IRQSLIM layout bit: `dd-jit-darwin/src/runtime/translate/x86_64/pcache.c:59`.
- `pcache_make_id()` does not include many env-driven codegen gates: `dd-jit-darwin/src/runtime/translate/x86_64/pcache.c:196`.
- Codegen reads env-controlled modes in translation setup: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:394`.

Why this is bad:

With `DDJIT_PCACHE=1`, switching env modes such as `NOLAZY`, `NOSTITCH`, `NOXALUDIRECT`, `NOXSHIFTDIRECT`, `NOREPCMP`, `NOSSEOPT`, `NOEAOPT`, or `NOGUESTFOLD` can load cached translations generated under a different mode. That makes env-driven compatibility/debug settings silently ineffective or inconsistent.
