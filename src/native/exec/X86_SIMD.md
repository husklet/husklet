# AMD64 SIMD and string capability audit

## Retained implementation ownership

The read-only oracle audit covered `../engine/src/translator/guest/x86_64/translate.c`
(`translate_one`, the legacy `0F` SSE matrix, `emit_pcmpistri_eqeach_byte`, and
the map-3 dispatch), `emit.c` (XMM spill/reload, MXCSR-to-FPCR/FPSR ownership,
flag materialization, memory guards, and block return), `interp.c` (exact SSE,
SSE2, SSE4.2, and string fallbacks), `avx.c` (shared floating exception, NaN,
conversion, and packed-string semantics), and `lower/repstr.c`
(`hl_x86_lower_repstr` and `emit_rep_string`).

The dispatcher owns the CPU/XMM/MXCSR image and translation-cache lifetime.
SIMD instructions allocate nothing and acquire no locks. Register forms cannot
block or return errno. Memory operands resolve and validate the complete access
before architectural commit. Retained REP helpers publish each completed
element, RCX/RSI/RDI, dirty writes, and a precise fault before returning; DF
selects increasing or decreasing order. PCMPISTRI atomically publishes RCX and
CF/ZF/SF/OF/PF/AF at the instruction boundary. Floating operations map MXCSR
rounding and flush control into host FPCR, merge exception status from FPSR,
and fall back before commit when the host result cannot reproduce x86 NaN
selection.

## Capability matrix

| Family | Retained C capability | Rust native status before this lane | Remaining contract |
|---|---|---|---|
| MOVAPS/MOVUPS, MOVDQA/MOVDQU | aligned/unaligned 128-bit register and guarded memory load/store | implemented | aligned faults and store observation covered by existing projection tests |
| MOVD/MOVQ, MOVSS/MOVSD | GPR transfer; scalar load merge and scalar register-copy upper-lane rules | partial: integer and broad 128-bit copy forms exist; scalar prefix semantics are incomplete | complete every scalar merge/zero rule |
| PUNPCKL/H B/W/D/Q | all four widths, register/memory | implemented | retained memory fault-before-commit contract |
| PSHUFD/PSHUFHW/PSHUFLW/SHUFPS | every immediate lane selection, including destructive overlap | missing except narrow insert support | implement general immediate permutation before claiming initialization coverage |
| PAND/PANDN/POR/PXOR | full 128-bit register/memory | implemented | none known |
| PCMPEQB/W/D, PMOVMSKB | all lane widths; mask to GPR | implemented | PCMPEQQ and wider compare families remain absent |
| PCMPGTB/W/D, min/max, average | signed compare; signed/unsigned min/max and rounded average | missing | coherent integer comparison/minmax slice |
| PADDB/W/D/Q, PSUBB/W/D/Q | wrapping lane arithmetic, no flags or exceptions | implemented by this lane | saturating forms remain separate |
| PADDS/PADDUS/PSUBS/PSUBUS | signed/unsigned saturating byte/word arithmetic | missing | QC must not leak into guest-visible state |
| PMULLW/PMULHW/PMULHUW/PMULUDQ | low/high and widening products with exact lane selection | missing | coherent packed multiply slice |
| PSLL/PSRL/PSRA | immediate and XMM scalar counts; x86 oversized-count saturation | missing | retained count saturation differs from raw AArch64 shifts |
| ADD/SUB packed and scalar S/D | all widths; scalar upper lanes; result/input NaN priority | ADD/SUB implemented for packed S/D and scalar S/D | fuller retained generated-NaN sign and payload rules |
| MUL/DIV packed and scalar S/D | all widths and the same exception/NaN contract | scalar double only | packed and scalar-single remain explicit fallback |
| COMISS/UCOMISS, COMISD/UCOMISD | exact CF/ZF/PF, cleared OF/SF/AF, signaling split | double implemented | single missing |
| CVTDQ2PS, CVTSI2SS/SD, CVTTSS/SD2SI | packed and scalar widths, MXCSR rounding or truncation, integer-indefinite overflow | truncating scalar double-to-integer only | conversions dominate benchmark initialization and checksum loop |
| LDMXCSR/STMXCSR | mask validation, control publication, accumulated exceptions | runtime CPU field exists; guest instructions missing | must preserve FPCR/FPSR across every native entry/exit |
| PCMPISTRI | all implicit-length aggregation/polarity/index/flags in fallback; retained native fast path serves equal-each byte forms | missing | generic SSE4.2 string compare owner; RCX and six flags change atomically |
| REP MOVS/STOS | widths 1/2/4/8, DF, bulk helper, partial faults, dirty/executable writes | implemented with bounded continuation | exact per-element budget is intentionally preserved |
| REP LODS | widths and DF scalar loop | implemented | no bulk shortcut because every element updates RAX |
| REPE/REPNE CMPS/SCAS | widths, DF, stop condition, exact terminal flags and registers | not admitted by the Rust native frontend | interpreter remains authoritative |

The `float_simd` kernel crosses missing conversion, permutation, packed MUL,
scalar-single conversion/comparison, and packed ADD paths. The `string` phase
first executes a compiler-vectorized initializer containing PMULUDQ, PADDD/Q,
packed shifts, and shuffles, then reaches libc REP and PCMPISTRI paths. Neither
phase is evidence for a one-opcode workaround.

## Bounded wrapping integer slice

This lane admits the complete legacy SSE2 wrapping add/sub family:
PADDB/PADDW/PADDD/PADDQ and PSUBB/PSUBW/PSUBD/PSUBQ, for register and guarded
memory sources. Every form is destructive `xmm(destination) op source`, wraps
independently at its 8/16/32/64-bit lane width, changes no EFLAGS or MXCSR
state, and owns no lifecycle or synchronization state. AArch64 ADD/SUB vector
instructions provide the exact modular arithmetic. Existing generic vector
memory lowering retains access validation and fault-before-commit ordering.

`test/x86_translation.c` admits all eight encodings in one block and checks
each operation independently across both 64-bit halves against a lane-width
software oracle. It also proves source preservation, destination/source
aliasing, unchanged flags and MXCSR, unaligned guarded memory, fault-before-
commit, and rejection of the same unprefixed MMX opcode bytes. Broader
benchmark evidence must not attribute unrelated unsupported SIMD/string
families to this slice.
