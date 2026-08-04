# AMD64 native performance audit

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
