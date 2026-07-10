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

