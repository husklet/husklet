# AMD64 native performance audit

> Historical replacement-engine audit. Rust/native measurements below are not
> current product baselines; use the C-primary campaign described in
> [`../README.md`](../README.md).

## Write projection publication, 2026-08-05

The writable-view cache now admits scalar, vector, and RMW stores against the
four authenticated run views while preserving exact post-success publication.
Permission-homogeneous cache windows prevent adjacent RW and executable views
from contaminating executable-write state. The durable oracle, ownership
matrices, and coarse-publication blocker are recorded in `WRITE_PUBLICATION.md`;
no standalone performance result was claimed for these correctness changes.

## Scalar SSE/SSE2, 2026-08-04

The retained implementation was inspected read-only at
`../engine/src/translator/guest/x86_64/translate.c` (`translate_one`,
`emit_ldmxcsr`, `emit_stmxcsr`, and the legacy `0F` scalar conversion,
comparison, and arithmetic cases), `emit.c` (`e_nzcv_save_fcmp`), `interp.c`
(host-MXCSR ownership and scalar SSE fallback), and `lower/sse4x.c`
(`hl_x86_lower_sse4x`). The dispatcher owns the CPU lifetime and translation
cache; scalar operations own no allocations, locks, host calls, or teardown.
Memory operands preserve effective-address calculation, guarded access, and
fault-before-commit ordering. Register forms have no blocking, cancellation,
or errno behavior. On AArch64 the retained translator maps MXCSR rounding and
flush control into FPCR, accumulates exceptions in FPSR, maps comparison NZCV
to x86 CF/ZF/PF while clearing AF/SF/OF, and repairs ARM's saturating
float-to-integer result to x86's integer-indefinite value for NaN and positive
overflow. Its x86-host interpreter instead makes host MXCSR authoritative.

The Rust-owned native boundary already saves and restores host FPCR/FPSR in
`src/runtime/native/exec/src/arch/x86_64/entry.S`; `NativeX86` maps guest MXCSR into
the native CPU record and merges FPSR exceptions back. Before this lane,
`frontend.c` and `frontend/memory.c` lowered scalar-double SQRT, ADD, MUL, and
DIV but rejected SUB, COMISD/UCOMISD, and CVTTSD2SI. This lane maps that
coherent hot family into the same FPCR/FPSR owner: SUB preserves the upper XMM
lane, comparisons implement ordered and unordered EFLAGS and signaling versus
quiet NaN behavior, and truncating conversion preserves flags and substitutes
the correct 32- or 64-bit integer-indefinite result.

Fail-first used one block containing `SUBSD; COMISD; CVTTSD2SI`. Linking the
new structural test to the prior archive failed at `x86_translation:78` because
the frontend returned unsupported. The warning-strict candidate build and its
focused structural/differential test passed:

```sh
sources=$(find src/runtime/native/exec/src src/runtime/native/exec/cache -type f -name '*.c' -print)
cc -std=c11 -Wall -Wextra -Werror \
  -I src/runtime/native/exec/include -I src/runtime/native/exec/src -I src/runtime/native/cpu/include \
  $sources src/runtime/native/exec/src/arch/aarch64/entry.S \
  src/runtime/native/exec/src/arch/x86_64/entry.S src/runtime/native/exec/test/x86_run.S \
  src/runtime/native/exec/test/x86_translation.c -lpthread -o /tmp/hl_x86_sse_new
/tmp/hl_x86_sse_new
```

The semantic cases cover finite subtraction, upper-lane preservation, greater
and unordered comparison flags, COMISD invalid status for a quiet NaN, finite
truncation, positive infinity and NaN integer-indefinite conversion, and
preservation of pre-existing integer flags by conversion. Remaining gaps are
CVTSD2SI rounding and precision-flag corner cases, CVTSI2SD, scalar single
precision, packed floating arithmetic/conversion, MXCSR load/store in the
native frontend, and the retained implementation's fuller generated-NaN sign
repair. The combined float benchmark is dominated by those packed/single
families, so this focused lane is correctness and fallback-reduction evidence,
not yet an end-to-end parity claim.

## Packed and scalar-single SSE, 2026-08-04

The exact AMD64 `combined` guest at
`target/testing/bench/combined/amd64/combined` was disassembled at
`phase_float_simd` (`0x89c0..0x8b73`). Its initialization uses `MOVSS`,
`MOVD`, `SHUFPS`, `PSHUFD`, `MOVDQA`, `CVTDQ2PS`, `PADDD`, `MULPS`, and
`ADDPS`. The repeated kernel uses `CVTSI2SS`, `MULSS`, `SHUFPS`, `MOVAPS`,
`MULPS`, `ADDPS`, `COMISS`, `SUBSS`, and `CVTTSS2SI` in register and memory
forms. The loop is therefore not one missing opcode: moves, permutation,
integer-vector arithmetic, conversions, comparisons, and floating arithmetic
are distinct ownership families.

The retained implementation was read through
`../engine/src/translator/guest/x86_64/translate.c`: `translate_one` owns the
legacy `0F 58/59/5C/5E` prefix matrix, guarded memory access, packed
result-NaN gate, scalar input-NaN gate, upper-lane merge, and the `R_SSE3B`
precommit fallback; the `0F 10/11`, `0F 2A/2C`, `0F 2E/2F`, `0F 5B`, `0F
70`, `0F C6`, and `0F FE` cases own the other benchmark families. `emit.c`
owns comparison flag projection, `interp.c` owns x86-exact fallback under the
host MXCSR, and `avx.c` records the same NaN-priority and generated-indefinite
rules for non-destructive forms. These instructions allocate nothing and own
no locks or teardown; the CPU record and dispatcher own FPCR/FPSR lifetime,
while guarded memory owns fault-before-commit ordering.

| Benchmark family | Retained C | Native frontend before | Result |
|---|---|---|---|
| packed/scalar ADD and SUB | `.4s`, `.2d`, scalar S/D; NaN gate before commit | scalar D only | packed S/D and scalar S added; scalar D retained |
| packed/scalar MUL and DIV | same widths and exception rules | scalar D only | deliberately remains a fallback |
| MOVSS and broadcast/shuffle | low-lane load/store plus upper-lane rules | missing | unchanged |
| integer/FP conversion | packed lanes and scalar 32/64 widths | scalar truncating D-to-int only | unchanged |
| COMISS/UCOMISS | exact CF/ZF/PF and signaling split | double only | unchanged |
| PADDD | four wrapping 32-bit lanes | missing | unchanged |

ADD/SUB computes into scratch, checks the result before architectural commit,
and returns to the exact fallback when any scalar or packed lane is NaN.
Finite packed results commit all 128 bits; scalar-single results replace only
lane zero. Memory reads and permission checks precede either result path.
Tests cover packed-single ADD/SUB, packed-double ADD and memory SUB,
scalar-single SUB with upper lanes preserved, register and memory sources, and
generated-NaN rollback.

The wider four-operation experiment exposed a separate integration blocker:
admitting packed ADD and MUL together made the product benchmark fail to
finish, while each opcode alone and their direct semantic sequence completed.
That is consistent with crossing the last fallback inside the native loop and
exposing a continuation/self-loop defect; it is not evidence that either
arithmetic instruction is wrong. This lane retains the coherent additive
family and leaves MUL/DIV as the safe dispatcher boundary pending the separate
repetition/continuation owner.

With native execution and diagnostics proven, the three-repeat
`--divisor 2000` median fell from 218,576 us to 185,194 us (15.3%) with the
identical `1151364000` checksum. Public exits fell from 14 to 12 per
invocation. This is focused dirty-tree evidence rather than a clean-commit
parity claim. Warning-strict compilation and the complete `x86_translation`
executable passed on AArch64.

Exact clean detached commit `c84cf0d81` confirms the end-to-end effect at the
standard `--divisor 20` workload. Five native-verified compute repeats held
checksum 7,097,455,804,780,747,230 and reported an 83,652-us median with an
82,216--97,228-us range. Diagnostics were identical across all repeats: 307
public exits, 187 builds, 819 hits, five fallbacks, and 19,780,505 completed
instructions. This is 11.63 times faster than the original 972,729-us Rust
checkpoint median and 10.12 times faster than the exact 846,456-us scalar-SSE
commit median. It remains 16.72 times slower than the retained-C median of
5,003 us. The exact `hl-engine` SHA-256 was
`a180e48ca2381618d1e4960fd2a8d7a4c73f6f46d625ac57869fa85b7a2e8132`;
the exact `testing` SHA-256 was
`aabeaf49d56f784abc7ecced06a22443267ae6dd14bd14a3371687a1f65428b2`.
## Self-loop fallback continuation

The retained-C SIMD and chaining audit covered
`../engine/src/translator/guest/x86_64/translate.c` (`translate_block`, the
legacy `0f 58/59/5c/5e` matrix, guarded memory, and the tiered self-loop path),
`emit.c` (direct-edge publication), `cache.c` (pending-edge ownership and hot
loop promotion), and `interp.c` (exact arithmetic fallback). The cache owns
published translations and pending edges; the CPU owns architectural vector
state and progress. Arithmetic commits only after guarded reads, and an
unordered result leaves the destination untouched before exact interpreter
fallback. No retained-C lock, allocation, errno, or cancellation behavior is
involved in this domain.

The Rust frontend already emits the exact seven-instruction benchmark loop
(`movaps; mulps; addps; movaps; add; cmp; jne`) as one conditional self-loop.
The nonprogress was instead in `run.c`: any return from folded-loop code was
treated as successful loop completion. A memory-guard or unordered-arithmetic
fallback therefore cleared loop state and immediately re-entered the same PC
without resolving the operand or returning to the typed interpreter; zero
instructions could be charged forever.

Folded-loop exits now add the current partial iteration's checkpoint count to
completed full iterations on fallback, then follow the ordinary fallback or
operand-resolution path. Only a completed non-fallback loop return re-enters
the folded body. `x86_budget` covers a NaN `mulps` self-loop and proves bounded,
zero-progress fallback without changing RCX. The complete warning-strict
`x86_translation` and `x86_budget` binaries pass. With the coherent SSE
arithmetic admission enabled, the previously nonterminating `float_simd`
workload completes: the divisor-2000 diagnostic run reported 172273 us instead
of timing out beyond 120 seconds. This is a correctness/unblocking result, not
a native-parity claim; diagnostics were enabled and the remaining fallback
rate requires a separate production-mode performance lane.

## Translated-chain lifecycle and performance evidence

This audit covers the x86-64 guest translated-block lookup and chaining domain.
The retained C tree was read only at `/Users/x/dd/engine`.

### Retained C implementation studied

- `src/translator/cache.c`: `map_idx`, `map_host`, `map_body`, `map_put`,
  `map_clear`, `add_pend3`, `patch_links_to`, `jit_flush_to_fresh`,
  `smc_inplace_drop`, and `jit_after_fork`.
- `src/core/dispatch.c`: `run_guest`, including lookup/translation locking,
  publication ordering, IBTC fill, arena-generation pinning, and block entry.
- `src/translator/guest/x86_64/emit.c`: `emit_chain_exit` and the indirect
  target probe emission.
- `src/translator/guest/x86_64/translate.c`: `translate_block` publication,
  `patch_links_to`, tier-two replacement, and cache/IBTC invalidation.
- `src/translator/guest/x86_64/dispatch.h`: `G_DISPATCH_ENTER`,
  `G_SHADOW_CLEAR`, `G_DISPATCH_CHAIN`, and `G_IBTC_FILL`.
- `src/translator/guest/x86_64/abi.h`, `glue.c`, and `glue.h`: x86 guest-PC
  identity, global cache ownership, and the two-way IBTC lifetime.

The retained engine owns one translation arena, open-addressed guest-PC map,
pending direct-edge index, and IBTC generation. Single-threaded lookup is
lock-free. After guest threads exist, `g_jit_lock` covers lookup, translation,
publication, and link mutation; stop-the-world generation pinning prevents an
arena from being reclaimed under an executing peer. Direct edges are patched
only after executable bytes and instruction-cache coherence are published.
Pending edges are indexed by target and removed when patched. Indirect edges
probe an identity-keyed IBTC and return to the dispatcher on a miss. Mapping
replacement, executable writes, cache rollover, fork repair, and tier-two
replacement retire or repair every host-code pointer according to their
generation. A block-boundary interrupt poll keeps indefinitely chained guest
control flow observable to signals and stop-the-world requests.

### Rust/native ownership comparison

| Retained capability | Rust/native owner | State |
| --- | --- | --- |
| guest-PC lookup and generation | `cache/cache.c` | implemented |
| pending and resolved direct edges | `cache/relocation.c` | implemented |
| W^X publication before link visibility | `arena.c`, `translation.c` | implemented |
| source-range invalidation and edge undo | `cache/cache.c`, `cache/relocation.c` | implemented |
| dynamic-target identity cache | `executor.c`, `arch/x86_64/run.c` | implemented, one-way |
| per-block interrupt checkpoint | `arch/x86_64/run.c` entry prefix | implemented |
| live remaining-budget checkpoint | `frontend/output.c`, entry prefix | implemented |
| guarded direct-cycle closure | `cache/relocation.c` | implemented |
| x86 declaration that its blocks are guarded | `arch/x86_64/run.c` emission | **missing** |

`cycle_requires_exit` correctly refuses to close an unguarded cycle. The x86
frontend requests `HL_X86_A64_CHECKPOINTS | HL_X86_A64_LIVE_CHAIN`; every
ordinary block entry loads the interrupt flag and compares the live remaining
budget, and each checkpoint subtracts its accounted instruction count from
that budget. Direct targets enter at word two, intentionally preserving both
checks while skipping only BTI and the redundant budget reload. Nevertheless,
`emit_block` left `hl_native_emission.cycle_safe` zero. Consequently every x86
control-flow cycle retained one host dispatcher exit despite already carrying
the required guards.

The repair marks these emissions cycle-safe. It does not weaken the relocation
admission algorithm: mixed cycles containing any future unguarded emission
still retain an exit, frontier saturation still fails closed, conditional
self-loops retain their separately bounded implementation, and invalidation
still restores the original dispatcher edge before retiring a target.

### Evidence contract

The focused two-block x86 cycle test must prove both edges close, a finite
budget still yields at the exact instruction count, an interrupt at entry makes
no guest progress, and invalidating either member restores the surviving edge.
The full native execution tests and warning-strict engine build remain required.

The host is AArch64 Linux, so an x86-64 host-native timing row is physically
unavailable. QEMU is retained only as the x86 semantic/timing control; it is not
reported as native. Performance acceptance compares the same pinned x86-64
guest, CPU affinity, options, checksums, and exact baseline/candidate Rust
executables, alongside the retained C engine.

On CPU 17, nine alternating calls-phase pairs using the same static PIE guest
(`bda1b267...`) produced a baseline median of 21,424 us and candidate median of
20,823 us: 2.81% lower, with the candidate winning all nine pairs. Both reported
checksum `11942663544271968902` and 3,580,664 completed instructions. Public
branch boundaries fell from 61,793 to 161; relocation-cycle refusals fell from
9 to 0. The retained C row was 230 us and QEMU was 1,463 us. This closes a real
avoidable crossing but does not claim x86 parity: instruction lowering and
remaining boundary costs still dominate.

### Immediate checkpoint accounting

The follow-up audit read the complete retained dispatcher and cache path in
`../engine/src/core/dispatch.c` (`run_guest`, `run_block`, and
`block_return`), `../engine/src/translator/cache.c` (`map_host`, `map_put`,
pending-link resolution, IBTC publication, cache rotation, stop-the-world
admission, and fork repair), and the x86 frontend in
`../engine/src/translator/guest/x86_64/{translate.c,emit.c,dispatch.h}`. The
dispatcher owns CPU registration and teardown; the cache owns translation
identity, arena generation, locking, W^X publication, pending edges, and
invalidation. Generated blocks own no locks, allocations, host calls, errno,
or cancellation paths. Fault-capable instructions commit only after their
guard succeeds, while every emitted block reaches an interrupt-visible
boundary.

The Rust owners are `src/arch/x86_64/run.c`,
`src/arch/x86_64/frontend.c`, `src/arch/x86_64/frontend/output.c`,
`src/translation.c`, and `cache/{cache.c,relocation.c}`. Translation identity,
publication, direct and indirect links, invalidation, rollover, and teardown
are implemented. Unlike the retained engine, Rust admits work with an exact
instruction budget. Before every instruction that may fault or fall back, the
frontend accounts the preceding segment so replay cannot double-commit
architectural state.

That accounting formerly materialised the segment length in x17 and then
added or subtracted x17. A decoded block is bounded by
`HL_X86_A64_MAX_INSTRUCTIONS == 64`, so every segment length fits AArch64's
12-bit add/sub immediate. The emitter now uses one immediate instruction; the
old register form remains as a fail-closed path if the frontend bound ever
grows beyond 4095. Live-chain checkpoints shrink from two host instructions
to one, and dispatcher-return checkpoints from four to three. Budget,
partial-result, fault-before-commit, interrupt, cache lifetime, and locking
semantics are unchanged.

Exact-tree warning-strict direct builds and executions of `x86_budget` and the
complete `x86_translation` contract passed. The warning-strict `hl-engine`
suite completed 478 passing tests and two ignored tests; its sole failure was
the independently changing retained option registry (`41` retained entries
versus the test's expected `40`), not native execution. This change has exact
structural evidence (one fewer emitted host instruction at every checkpoint),
but makes no wall-time claim: the existing benchmark artifacts do not retain
per-checkpoint counts, and a timing claim without identical counters would be
false precision.

## Emitter-capacity contract

The retained C engine was inspected at
`../engine/src/translator/guest/x86_64/emit.c` and
`../engine/src/translator/guest/x86_64/translate.c`. Its emitter advances the
shared code-arena cursor directly; it has no 128-word per-vector contract and
therefore provides no justification for that test constant.

The Rust/native production owner is
`src/arch/x86_64/frontend.c::hl_x86_a64_emit`. It computes every instruction's
word count before emission, compares the complete block against the caller's
`host_capacity`, and returns `HL_X86_A64_CAPACITY` without emitting when the
block does not fit. `src/arch/x86_64/run.c::build_translation` supplies an
8,192-word array with explicit entry and relocation reserves. The vector
generator's sizing function and emitter are checked for equality by the native
translation suite.

The failing 128-word limit belonged only to `vector_fragment`, a test helper.
The helper's three callers all own 256-word arrays. Writable-view journal
coalescing increased `HL_X86_WRITE_CACHE_WORDS` from 71 to 105, legitimately
making the generated writable-vector fragment exceed the obsolete test-only
constant while remaining within each caller's real allocation and the
production preflight contract. Removing that stale early return then exposed a
second defect: `emit_write_cache` emits 109 words while
`HL_X86_WRITE_CACHE_WORDS` claimed 105. Production consequently under-reserved
four words for every generated writable-memory operation. The shared sizing
constant is corrected to the emitter's exact size.

Two sizing functions derive their exact count by emitting into bounded stack
scratch. Writable rotate and compare-exchange forms can now exceed their old
256-word scratch after the same cache expansion, so those buffers are raised to
the existing 512-word bound already used by the sibling bit, double-shift, and
exchange sizing paths. These buffers are private sizing workspace, not generated
block capacity.

The generic CALL/control sizing base also embedded the former 71-word writable
cache size. Its base is raised by the same 34-word delta, and the focused control
test supplies enough capacity for the now-larger valid fragment. This restores
the production rule that the sizing pass rejects capacity before any emission.

The correction passes each caller's actual capacity into the helper and
reserves one additional word for its appended return instruction. The generator
still emits identical code; its production capacity preflight now accounts for
the complete sequence.

## Indirect-branch table reset lifecycle

This bounded Linux optimization follows the retained engine's lazy reset of
large indirect-branch tables. It does not claim end-to-end parity.

### Retained oracle

The read-only implementation studied was
`../engine/src/translator/cache.c`, especially `cache_create`,
`cache_flush`, `cache_fork`, and the Linux `madvise(..., MADV_DONTNEED)` reset
path. The translator owns the table for the cache lifetime. Translation and
mapping mutation are serialized by the JIT lock; fork repair runs while peers
cannot execute translated code. A reset invalidates the complete table before
execution resumes. Linux supplies zero-filled pages on later access. Other
hosts retain their explicit clear path.

### Husklet mapping

`hl_native_executor` owns its 65,536-entry IBTC from construction through
destruction. `ibtc_clear` is called during construction, identity reset, arena
rollover, authority changes, and fork repair while exclusive mutation
admission excludes translated execution. Individual invalidation remains an
atomic publication operation and is unchanged.

The table is allocated at a 64 KiB boundary and occupies exactly 1 MiB, so the
Linux discard range is page aligned and does not include allocator metadata.
Successful discard has the same next-read zero contract as `memset` without
faulting every table page into the process. Failure and non-Linux hosts fall
back to `memset`. No guest-visible ordering, errno, cache identity, or table
lifetime changes.

This removes fixed construction/reset memory traffic. The dominant steady-
state gap remains generated projected-memory guards and write publication; it
must be addressed separately with exact compatibility evidence.

## Executable-write epoch exit diagnostics

The retained oracle audit covered `../engine/src/translator/guest/x86_64/dispatch.h`
(`smc_protect`, `smc_on_write`, and `G_DISPATCH_REASON`),
`../engine/src/translator/guest/x86_64/interp_dispatch.h`
(`smc_on_write` and `G_DISPATCH_REASON`),
`../engine/src/core/target/x86_64.c` (`jit86_smc_commit`), and
`../engine/src/linux_abi/x86.c` (`jit86_lazyguard`). The CPU record owns pending
SMC ranges until dispatcher commit. The JIT's process-wide protected-page table
is updated under the existing dispatcher/activation regime; a write fault first
unprotects and forgets its page, then invalidation retires stale translations.
The interpreter owns no emitted-code identity and re-decodes instead. Syscalls
commit queued shared writes before service and copyout writes afterward. Fault
classification and signal delivery precede the SMC/lazy-map path; there is no
partial syscall result or errno conversion in the SMC mechanism. Host-specific
signal-context reconstruction remains in `linux_abi/x86.c`; the x86 guest
semantics do not vary by host ISA.

The Rust owners are `src/arch/x86_64/projection.c` for bounded dirty and
executable-write classification, `src/arch/x86_64/run.c` for the typed epoch
exit and execution-scope teardown, and `src/executor.c` plus the public
diagnostics ABI for aggregate observation. The retained engine services SMC
inside its dispatcher, whereas Rust intentionally returns an epoch identity to
the memory/runtime owner before re-entry. The previously existing aggregate
`x86_public_exits` and syscall counters could not separate this boundary from
yield or fallback. `x86_public_epochs` now counts only typed
`HL_NATIVE_EXIT_EPOCH` returns, once per public return, with diagnostics disabled
remaining a single predictable branch and no logging or allocation. The ABI
extension is appended after the retained prefix. Focused coverage drives an
executable projection store to epoch, then separate yield, fallback, and syscall
returns, proving only the epoch changes the new counter.
