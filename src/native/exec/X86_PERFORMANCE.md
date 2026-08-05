# AMD64 native performance audit

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
`src/native/exec/src/arch/x86_64/entry.S`; `NativeX86` maps guest MXCSR into
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
sources=$(find src/native/exec/src src/native/exec/cache -type f -name '*.c' -print)
cc -std=c11 -Wall -Wextra -Werror \
  -I src/native/exec/include -I src/native/exec/src -I src/native/cpu/include \
  $sources src/native/exec/src/arch/aarch64/entry.S \
  src/native/exec/src/arch/x86_64/entry.S src/native/exec/test/x86_run.S \
  src/native/exec/test/x86_translation.c -lpthread -o /tmp/hl_x86_sse_new
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
