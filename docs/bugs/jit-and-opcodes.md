# JIT, Cache, and Opcode Gaps

This file covers instruction fidelity, opcode coverage, stale translation, and hidden completeness holes.

## Stale Translation After Unmap/Remap

Priority: P1
Impact: wrong code execution after guest VA reuse
Confidence: High

Evidence:

- Dispatch reuses translated code by guest PC: `dd-jit-darwin/src/runtime/engine/dispatch.c:130`.
- `munmap` updates mapping registries but does not invalidate block map or IBTC entries: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:250`.
- `mmap` / `MAP_FIXED` tracks the new mapping but does not drop old translations for the same VA: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:617`.
- A broad invalidator exists (`smc_inplace_drop`) but is not called from unmap/remap: `dd-jit-darwin/src/runtime/engine/cache.c:1225`.

Why this is bad:

If code at VA `X` is translated, unmapped, and a different executable page is mapped at `X`, the dispatcher can jump to stale host code for the previous bytes. Existing SMC paths mainly cover write-fault/in-place rewrite, not VA reuse after unmap.

Verification:

Guest PoC:

1. Map executable code returning `111`.
2. Call it.
3. `munmap` the range.
4. `mmap(MAP_FIXED)` a different executable page at the same VA returning `222`.
5. Call it again.

Expected: `111 222`. Suspicious stale-cache result: `111 111`.

Coverage gap:

Existing SMC tests cover in-place rewrite and mprotect toggles. Add an unmap/remap reuse probe.

## F16C `vcvtps2ph` Ignores Rounding Immediate

Priority: P2
Impact: wrong half-float results for non-default rounding modes
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verifier2-wt`.

Evidence:

- Conversion helper is documented as round-to-nearest-even only: `dd-jit-darwin/src/runtime/translate/x86_64/avx.c:202`.
- `vcvtps2ph` emulation ignores `I.imm`: `dd-jit-darwin/src/runtime/translate/x86_64/avx.c:924`.

Why this is bad:

The x86 F16C immediate selects nearest, down, up, trunc, or MXCSR-controlled rounding. Ignoring it silently returns nearest-even results for all modes.

Isolated proof:

```sh
x86_64-linux-gnu-gcc -O2 -static-pie -pthread -o target/verifier2-probes/f16c_roundimm dd-tests/guests/completeness/x86_f16c_roundimm.c -lm
qemu-x86_64 target/verifier2-probes/f16c_roundimm
DDJIT_DIR=/Users/x/dd/dd-verifier2-wt/target-verifier2/release/build/dd-jit-darwin-16122afd27b6bb64/out \
  cargo run -q -p dd-tests -- --engine x86_64 f16c-roundimm
```

Observed dd result: all immediate modes returned `3c01 3c01 bc01 bc01`. The qemu oracle differs for down/trunc as expected.

Coverage gap:

`dd-tests/guests/completeness/x86_f16c.c` tests only `_mm_cvtps_ph(..., 0)`, so it cannot catch non-RNE modes.

## SSE4.2 String Compare Leaves AF Stale

Priority: P2
Impact: wrong flags after `PCMP*STR*`
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verifier2-wt`.

Evidence:

- The implementation documents Intel flag behavior: `AF=PF=0`: `dd-jit-darwin/src/runtime/translate/x86_64/avx.c:1191`.
- `sse42_flags` sets `nzcv` and `pf`, but does not set `af`: `dd-jit-darwin/src/runtime/translate/x86_64/avx.c:1194`.
- PTEST explicitly clears `af`, showing the expected style exists elsewhere: `dd-jit-darwin/src/runtime/translate/x86_64/avx.c:1377`.

Why this is bad:

If AF was set by a previous instruction, `PCMP*STR*` should clear it. Leaving stale AF can break code that reads full flags with `pushfq`/`lahf` after SSE4.2 string compares.

Isolated proof:

```sh
x86_64-linux-gnu-gcc -O2 -static-pie -pthread -o target/verifier2-probes/sse42_pcmp_flags dd-tests/guests/completeness/x86_sse42_pcmp_flags.c -lm
qemu-x86_64 target/verifier2-probes/sse42_pcmp_flags
DDJIT_DIR=/Users/x/dd/dd-verifier2-wt/target-verifier2/release/build/dd-jit-darwin-16122afd27b6bb64/out \
  cargo run -q -p dd-tests -- --engine x86_64 sse42-pcmp-flags
```

Observed: dd leaves `AF=1` (`raw=ad3`), while the qemu oracle clears it (`raw=ac3`).

Coverage gap:

`dd-tests/guests/completeness/x86_sse42.c` checks the comparison index, not full flag state.

## `fxsave` / `fxrstor` Skip MXCSR, x87, And MMX State

Priority: P1
Impact: restored rounding mode and floating-point state are wrong
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-b-jit-audit`.

Evidence:

- `fxsave` / `fxrstor` only save or restore XMM lanes from the memory image: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3377`.

Why this is bad:

`fxrstor` must restore MXCSR and x87/MMX state. Skipping MXCSR means code that saves a context, changes rounding mode, and restores it keeps the wrong rounding mode.

Isolated proof:

```sh
DDJIT_DIR="$PWD/target-worker-b-jit-audit/release/build/dd-jit-darwin-16122afd27b6bb64/out" \
  cargo run -q -p dd-tests --target-dir target-worker-b-jit-audit -- -e x86_64 fxrstor-mxcsr
```

Observed: dd `fxrstor-mxcsr r=2`; qemu/native `fxrstor-mxcsr r=1`.

## `mremap(MREMAP_FIXED)` Can Reuse Stale Translations

Priority: P1
Impact: old code can run after executable mapping relocation
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-b-jit-audit`.

Evidence:

- `mremap(MREMAP_FIXED)` can place a fresh mapping at the requested address: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:315`.
- Current SMC invalidation only drops code on write faults: `dd-jit-darwin/src/runtime/translate/x86_64/elf.c:807`.

Why this is bad:

This is the same class as `munmap`/`MAP_FIXED` stale code, but through `mremap`. A translated executable VA can be replaced and still dispatch old host code.

Isolated proof:

```sh
DDJIT_DIR="$PWD/target-worker-b-jit-audit/release/build/dd-jit-darwin-16122afd27b6bb64/out" \
  cargo run -q -p dd-tests --target-dir target-worker-b-jit-audit -- -e x86_64 smc-mremap-fixed
```

Observed: dd `first=11 second=11`; qemu/native `first=11 second=22`.

## VEX `vcvt*ss/sd2si` Lacks Legacy Overflow Fixups

Priority: P2
Impact: likely wrong integer-indefinite results for NaN/overflow
Confidence: Medium-high

Evidence:

- VEX scalar float-to-int conversion uses direct casts / rounding helper paths: `dd-jit-darwin/src/runtime/translate/x86_64/avx.c:436`.
- Legacy SSE conversion has explicit integer-indefinite fixups: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:2855`.

Why this is suspicious:

NaN and positive overflow cases for x86 float-to-int conversion have specific integer-indefinite behavior. If the VEX path skips the fixups present in the legacy path, AVX code can return host-C-cast artifacts instead of x86 results.

Verification:

Add qemu/native oracle probes for `vcvttss2si`, `vcvttsd2si`, `vcvtss2si`, and `vcvtsd2si` with NaN and out-of-range positive/negative values.

## `cmpxchg16b` Is Non-Atomic

Priority: P1
Impact: guest-thread race in 128-bit compare-exchange algorithms
Confidence: Medium-high

Evidence:

- `cmpxchg16b` is implemented as two loads plus stores: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3157`.
- Narrower `cmpxchg` uses a CAS primitive: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3526`.

Why this is bad:

`lock cmpxchg16b` is used by lock-free runtimes and atomics libraries. A non-atomic emulation can allow torn updates or incorrect success/failure races between guest threads.

Verification:

Add a multi-threaded guest stress test around a 16-byte compare-exchange loop and compare against native/qemu.

## SMC Tracking Has A Capacity Cliff

Priority: P2
Impact: stale code or write-fault handling failure after many protected code pages
Confidence: Medium

Evidence:

- `smc_protect` calls `mprotect` before checking whether the page can be recorded in the fixed SMC table: `dd-jit-darwin/src/runtime/translate/x86_64/dispatch_hooks.h:57`.
- If `g_smc_n >= SMC_MAX`, the page can be left read-only but untracked: `dd-jit-darwin/src/runtime/translate/x86_64/dispatch_hooks.h:58`.

Why this is bad:

Once the table is full, later translated pages can be protected without being recognizable by `smc_on_write`. A write fault to such a page may not invalidate translations or may fail the expected SMC recovery path.

Verification:

Generate more than `SMC_MAX` executable pages, execute each once, then patch and re-execute a late page.

## x86 Signal Return Drops AVX Upper And x87 State

Priority: P1
Impact: signal handlers corrupt vector/FPU state
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-I-jit-runtime-20260710`.

Evidence:

- Signal frame setup saves only `c->v`: `dd-jit-darwin/src/runtime/translate/x86_64/sigframe.c:48`.
- Signal return restores only `c->v`: `dd-jit-darwin/src/runtime/translate/x86_64/sigframe.c:86`.
- AVX upper state and x87 control/status live elsewhere: `dd-jit-darwin/src/runtime/include/cpu_x86_64.h:40`, `dd-jit-darwin/src/runtime/include/cpu_x86_64.h:48`.

Why this is bad:

Signals should preserve the interrupted machine state. Losing AVX upper lanes or x87 state can corrupt code that uses vectors/FPU across signal handlers.

Isolated proof:

```sh
timeout 5 qemu-x86_64 target-worker-I/poc/avx_sigreturn_upper
timeout 10 mac bash -lc "exec '$PWD/target-worker-I/release/build/dd-jit-darwin-5b0dabfbe6f0af2e/out/ddjit-linux_x86_64' '$PWD/target-worker-I/poc/avx_sigreturn_upper'"
```

Observed: qemu `high=1 rc=0`; dd `high=0 hi0=00 hi15=00 rc=1`.

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

## `LOCK BTS/BTR/BTC` Use Non-Atomic Bit-Op Path

Priority: P2
Impact: contended bitsets can lose updates
Confidence: High

Evidence:

- LOCK-aware ALU operations have an atomic LSE path: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:768`.
- Bit ops are handled by normal load/modify/store code: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3464`.
- Modified memory writes use `rm_store` without checking `I.lock`: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3516`.

Why this is bad:

`lock bts/btr/btc` are used for concurrent bitsets and synchronization. A non-atomic read-modify-write can lose updates across guest threads.

Verification:

Add a multi-threaded guest stress test that contends on the same bitset word with `lock bts`/`lock btr`.

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

## SSE2 `CVTPD2DQ` / `CVTTPD2DQ` Return Wrong Integer-Indefinite Values

Priority: P2
Impact: NaN and overflow conversions produce wrong integer results
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BM2-copy`.

Evidence:

- Packed double-to-int conversion is handled in the SSE path: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:2757`.
- It emits host `FCVTZS` / `FCVTNS`: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:2766`.
- It narrows with `SQXTN` without x86 integer-indefinite fixup: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:2769`.

Why this is bad:

x86 returns integer indefinite `0x80000000` for NaN and overflow cases. dd instead returns ARM saturation or zero for some lanes, causing silent numeric corruption.

Isolated proof:

```sh
x86_64-linux-gnu-gcc -O2 -static-pie -pthread -msse2 -o target/bm2-probes/x86_cvtpd_indef dd-tests/guests/completeness/x86_cvtpd_indef.c -lm
qemu-x86_64 target/bm2-probes/x86_cvtpd_indef
cargo run -q -p dd-tests --target-dir target-bm2-audit -- -e x86_64 cvtpd-indef
```

qemu returned `80000000` for positive overflow and NaN; dd returned values such as `7fffffff` and `00000000` for those lanes.

## SSE `UCOMISS` / `COMISD` Leave AF Stale

Priority: P2
Impact: flag consumers can observe stale auxiliary flag state
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BM2-copy`.

Evidence:

- The compare path calls `e_nzcv_save_fcmp`: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:3031`.
- The helper updates NZCV and parity only: `dd-jit-darwin/src/runtime/translate/x86_64/emit.c:269`.
- It stores parity but never clears AF: `dd-jit-darwin/src/runtime/translate/x86_64/emit.c:284`.

Why this is bad:

x86 SSE compare instructions clear AF. dd leaves AF from prior arithmetic, so code that saves or tests full flags can see impossible flag combinations.

Isolated proof:

```sh
x86_64-linux-gnu-gcc -O2 -static-pie -pthread -o target/bm2-probes/x86_comi_af_flags dd-tests/guests/completeness/x86_comi_af_flags.c -lm
qemu-x86_64 target/bm2-probes/x86_comi_af_flags
cargo run -q -p dd-tests --target-dir target-bm2-audit -- -e x86_64 comi-af-flags
```

qemu observed `comi-af ucomiss=001 comisd=040 fucomi=010`; dd observed `comi-af ucomiss=011 comisd=050 fucomi=010`.

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

## aarch64 Low-Address Exclusive And Pair Atomics Hang

Priority: P1
Impact: guest atomic instructions can spin forever at low non-PIE addresses
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-aarch64-atomics-perms-20260710`.

Evidence:

- Low non-PIE `LDXR`/`STXR` is intended to be handled by software LL/SC: `dd-jit-darwin/src/runtime/os/linux/elf.c:269`.
- `LDXR`/`LDAXR` records a software reservation: `dd-jit-darwin/src/runtime/os/linux/elf.c:285`.
- Pair forms including `CASP` are called out as rare and left to abort, but the observed behavior is a hang: `dd-jit-darwin/src/runtime/os/linux/elf.c:270`.

Why this is bad:

Low-address ordinary loads complete under dd, and native aarch64 completes the exclusive and pair atomic probes. dd times out on low-address `LDXR` and `CASP`, so guest code that uses atomics in a non-PIE low mapping can hang instead of completing or failing cleanly.

Observed proof:

```text
native: ldr_only_min ok; ldxr_only_min ok; casp_min ok
dd:     ldr_only_min ok; ldxr_only_min timed out; casp_min timed out
```

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

## x86 SMC Protection Table Overflow Can Hang On Code Rewrite

Priority: P1
Impact: code rewrite faults can become unhandled hangs after SMC table exhaustion
Confidence: High

Verification status: Proven with isolated proof patch in `/Users/x/dd/dd-audit-jit-memorder-cache-20260710`.

Evidence:

- `smc_protect()` calls `mprotect(PROT_READ)` before checking table capacity: `dd-jit-darwin/src/runtime/translate/x86_64/dispatch_hooks.h:44`, `dd-jit-darwin/src/runtime/translate/x86_64/dispatch_hooks.h:57`.
- SMC fault handling depends on recorded table entries: `dd-jit-darwin/src/runtime/translate/x86_64/elf.c:807`.

Why this is bad:

Once the SMC table is full, later translated pages can be made read-only without being recorded. A later guest code rewrite faults, but `smc_on_write()` cannot identify the page as SMC and dd hangs instead of invalidating or failing cleanly.

Observed proof:

```text
qemu: after=777777, exit 0
dd:   before-patch, then timeout 124
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
