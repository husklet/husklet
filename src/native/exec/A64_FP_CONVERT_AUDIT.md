# AArch64 scalar conversion audit

The retained C oracle was `/Users/x/dd/engine`. The scalar conversion domain
studied was `src/translator/guest/aarch64/translate.c` (`gpr_field_mask`, stolen
register rewriting and vector-dirty classification) and
`src/translator/guest/aarch64/interp.c` (the scalar FP integer-conversion box,
rounding modes, widths, invalid encodings, FPCR and FPSR behavior). The retained
native path emits allocated host-AArch64 conversions directly, rewriting a GPR
operand through scratch when it names engine-private x16, x17, x18, x28 or x30.
The interpreter owns exceptional and unavailable forms.

Husklet already owned FMOV in `fp_move.c`, vector conversions in `simd_float.c`,
FP state entry/exit, and exact guarded memory. It had no native owner for the
ordinary scalar integer-conversion family. In the combined float-SIMD loop,
`SCVTF s28,w1` and `FCVTZU x5,s31` therefore terminated native translation.
The engine completed only 164 native guest instructions and interpreted the
remaining nested array loop. Mapping lookup and guard selection were downstream
of that refusal and could not explain the seconds-scale result.

`fp_convert.c` now admits the allocated non-fixed scalar SCVTF, UCVTF, FCVTNS,
FCVTNU, FCVTPS, FCVTPU, FCVTMS, FCVTMU, FCVTZS, FCVTZU, FCVTAS and FCVTAU
width/type combinations. It executes the architectural instruction directly so
the installed guest FPCR/FPSR retain hardware rounding and exception semantics.
GPR sources and destinations naming stolen registers are loaded from or stored
to the generated CPU record. Half precision, fixed-point, FMOV and reserved
rounding combinations remain with their existing owners or fallback.

The structural trace test covers the two benchmark encodings. The focused
AArch64 FP executable test covers GPR-to-FP, FP-to-GPR, truncation, and stolen
x28 on both sides. The warning-strict Rust native executor suite passed 63 tests
with zero failures and two ignored captures; the standalone warning-strict
`aarch64_fp.c` executable passed on the AArch64 host.

One candidate runner drove both release engines with typed native execution and
diagnostics, the same guest SHA-256
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`, divisor
100 and three measured repetitions. Baseline guest time was 3,556,492 us median
(2,892,879--3,563,191); candidate was 1,805,477 us
(1,605,320--2,013,783), a 49.23% reduction. All samples produced checksum
23,027,281,045. Baseline completed only 164 native instructions; candidate
completed 35,976,315 with stable per-repeat counters. Remaining time is now
measurably dominated by 13,401,361 exact guards and 4,455,730 successful-store
dirty publications, which this change deliberately does not weaken.
