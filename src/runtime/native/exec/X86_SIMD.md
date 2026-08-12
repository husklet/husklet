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
| VPUNPCKL/H B/W/D/Q | map-1/66 byte, word, dword, qword; VEX.128/256 register/memory | implemented | per-128-bit-lane ZIP ordering; non-destructive `vvvv` source |
| PSHUFD/PSHUFHW/PSHUFLW/SHUFPS | every immediate lane selection, including destructive overlap | legacy SSE and VEX 128/256 non-destructive forms implemented | broader AVX families remain separate |
| PAND/PANDN/POR/PXOR | full 128-bit register/memory | implemented | none known |
| PCMPEQB/W/D, PMOVMSKB | all lane widths; mask to GPR | implemented; VEX equal B/W/D/Q implemented | legacy PCMPEQQ remains absent |
| PCMPGTB/W/D, min/max, average | signed compare; signed/unsigned min/max and rounded average | VEX signed B/W/D/Q compare and VEX packed min/max implemented | legacy compare/min/max and average remain separate |
| PADDB/W/D/Q, PSUBB/W/D/Q | wrapping lane arithmetic, no flags or exceptions | implemented by this lane | saturating forms remain separate |
| PADDS/PADDUS/PSUBS/PSUBUS | signed/unsigned saturating byte/word arithmetic | missing | QC must not leak into guest-visible state |
| PMULLW/PMULHW/PMULHUW/PMULUDQ | low/high and widening products with exact lane selection | legacy SSE and VEX 128/256 non-destructive forms implemented | MMX remains separate |
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

## VEX packed unpack/interleave slice

The retained oracle for this slice is `translate.c::translate_one` (map-1/66
`60/61/62/6c/68/69/6a/6d` ZIP lowering), `avx.c::do_avx2` (interpreter source
acquisition and per-128-bit-lane loop), and `interp.c::interp_punpck` (the
destructive legacy semantic ancestor). Decoder state and XMM/YMM storage belong
to the per-CPU dispatcher; these operations allocate no state, take no locks,
cannot block, and have no teardown or host-specific behavior beyond the AArch64
native lowering. The memory source is acquired across its complete 16- or
32-byte span before either destination half is published; a miss returns the
instruction PC and read fault metadata to the existing dispatcher path.

All eight retained integer forms are now native for VEX.128 and VEX.256,
register and memory operands. Source one is the encoded `vvvv` register and
source two is ModRM r/m; each 128-bit lane independently interleaves its low or
high half at 1/2/4/8-byte element width. Aliasing is safe because both inputs
are consumed before the destination half is stored. VEX.128 clears the
destination's upper half, while VEX.256 computes both lanes without changing
EFLAGS or MXCSR. Unsupported map/prefix combinations still fall back at the
original PC. The focused contract covers the complete opcode/width matrix,
extended registers, guarded memory admission, exact word capacity and
required-minus-one atomic rejection, plus executable AArch64 source
preservation, lane ordering, upper-lane publication, flags, and MXCSR.
The 128-bit memory form's sizing includes the five words emitted beyond the
generic vector-memory estimate, so admission cannot begin with a short buffer.

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

## PCMPISTRI equal-each byte slice

The retained correctness owner audited for this slice is
`src/translator/guest/x86_64/avx.c::do_sse3b`, including `sse42_ilen`,
`sse42_intres`, `sse42_index`, and `sse42_flags`. The retained native shortcut
is `src/translator/guest/x86_64/translate.c::emit_pcmpistri_eqeach_byte` and its
admission call site. It owns implicit-length, byte, equal-each PCMPISTRI for
register or guarded 16-byte memory operand two, both signedness spellings, all
four polarity encodings, and both index directions: the 16 encodings `08/0a`
plus each combination of `10`, `20`, and `40`.

The audit also inspected `avx_ea`, `avx_get_rm`, and the guest-access fault
bracket in `avx.c`, plus the validator contract in `rep_runtime.h`. State is
owned by one `struct cpu`; the operation has no independent identity,
allocation, lock, or teardown. Register operands are borrowed from the CPU
vector file. A memory operand is validated as one 16-byte read before ECX or
flags are published. A rejected read records instruction PC, address, width,
and access type, then abandons with `R_SOFTMISS`; dispatcher retry or guest
fault delivery remains outside this operation. The shortcut is an AArch64
emitter; effective-address bias folding is shared with the C fallback.

Rust ownership maps to `frontend.c` for prefix/opcode admission and dirty
architectural state, `frontend/memory.c` for guarded memory and AArch64
emission, and the executor for block accounting. Other PCMPxSTR widths,
aggregations, explicit lengths, and mask outputs remain unsupported by this
native frontend slice.

The frontend reserves at most 96 AArch64 words for the operation. Focused tests
run all 16 controls across every 17-by-17 implicit-length pair, explicit exact
and no-match results plus mismatches before, at, and after logical lengths,
register aliasing, an RCX-based SIB address, and a real two-page mapping with an
exact readable-page edge and `PROT_NONE` successor. They also check fault
address/PC/state/accounting atomicity, F2/F3/LOCK/redundant-66 prefix decisions,
invalid/truncated admission, and zero/one-unit executor budget boundaries.
These are correctness and bounded-code-size claims only; performance requires
a separately recorded exact-tree A/B run.

## Packed integer multiply slice

After packed shifts, missing coherent integer families ranked by legacy-XMM
coverage and hot-path impact as: (1) packed multiply (`D5/E4/E5/F4`, four
opcodes and eight register/memory paths), (2) saturating add/sub (eight
opcodes), (3) compare/min/max/average (nine), and (4) immediate shuffles
(four distinct merge rules). Multiply ranks first because PMULUDQ occurs in the
compiler-vectorized `string` initializer and all four operations share one
bounded widening-product mechanism; the larger groups do not occur there.

The direct read-only C audit covered
`../engine/src/translator/guest/x86_64/translate.c::translate_one` at the
`D5/E4/E5/F4` arms and
`../engine/src/translator/guest/x86_64/interp.c::interp_step_sse`, including
`interp_simd_get`, `interp_simd_rm_get`, and `interp_simd_put`. PMULLW retains
the low 16 bits of eight 16x16 products; PMULHW/PMULHUW select signed/unsigned
high halves; PMULUDQ multiplies only dword lanes 0 and 2 into two qwords. The
instruction borrows CPU-owned XMM state, allocates and locks nothing, and owns
no independent identity, lifetime, or teardown. Register forms cannot fail or
block. Memory forms validate and load the complete 16-byte source before
destination publication. There are no partial results, flags, MXCSR, errno,
cancellation, or signal-ordering effects. The fast branch is AArch64 NEON; the
interpreter is portable. MMX and VEX forms remain separate branches.

| Retained capability | Husklet owner | State after this lane |
|---|---|---|
| PMULLW low-word products | `frontend.c` + `frontend/memory.c` | implemented, register/memory |
| PMULHW signed high-word products | same | implemented, register/memory |
| PMULHUW unsigned high-word products | same | implemented, register/memory |
| PMULUDQ even-dword widening products | same | implemented, register/memory |
| Full memory validation before commit | generic vector guard | implemented |
| MMX forms | native frontend | remaining separate gap |

The VEX extension re-audited the read-only entry path in
`../engine/src/translator/guest/x86_64/decode.c::hl_x86_decode` through the
`R_AVX` dispatch, `avx.c::hl_x86_avx_run`, `do_avx`, `avx_get`, `avx_get_rm`,
and `avx_put`, plus `translate.c::translate_avx_inline`. Map-one mandatory-66
opcodes `D5/E4/E5/F4` use decoded VEX.vvvv as the non-destructive first source,
accept register or full-width memory as source two, and apply the same word-low,
signed/unsigned word-high, and even-dword widening rules independently to both
128-bit lanes when L is set. C4/C5 field inversion and WIG behavior remain
shared decoder contracts.

The CPU owns XMM and YMM-high storage for the dispatcher lifetime; these
operations allocate and lock nothing and have no blocking, cancellation, errno,
signal, partial-result, or teardown behavior. `avx_get_rm` completes the entire
16/32-byte read before `avx_put` publishes the destination. VEX.128 clears the
destination upper half, VEX.256 commits both halves, and neither source, EFLAGS,
nor MXCSR changes. The portable C loop and AArch64 inline owner have the same
architectural contract.

| Retained VEX capability | Husklet owner | State after this lane |
|---|---|---|
| VPMULLW, VPMULHW, VPMULHUW | `frontend.c` + `frontend/memory.c` | implemented, XMM/YMM register and memory |
| VPMULUDQ even-dword selection | same | implemented, XMM/YMM register and memory |
| non-destructive vvvv first source | decoded vector source + scratch-first emitter | implemented |
| VEX.128 upper zero / VEX.256 upper result | VEX completion + generated CPU layout | implemented |
| full memory validation before either lane commits | generic vector guard | implemented |
| MMX packed multiply spellings | native frontend | remaining separate gap |

`test/x86_translation.c::packed_integer_multiply` exercises every register and
unaligned guarded-memory legacy form plus all four VEX operations at 128 and
256 bits, checks every lane against a software oracle, verifies non-destructive
sources, EFLAGS/MXCSR preservation and VEX upper-lane rules, and rejects the MMX
spelling.

## VEX map-two packed dword multiply extension

The retained read-only audit covered `../engine/src/translator/guest/x86_64/decode.c`
(`hl_x86_decode`, C4/C5 map, `pp`, `W`, `L`, inverted register fields, and
bounded ModRM/SIB ownership), `../engine/src/translator/guest/x86_64/avx.c`
(`do_avx`, map-two `0x28` and `0x40`, `avx_get`, `avx_get_rm`, and `avx_put`),
and `../engine/src/translator/guest/x86_64/translate.c` (`emit_avx`, the
map-two `VPMULLD` AArch64 lowering). The CPU owns both halves of each vector
until task teardown; these operations allocate no state, acquire no locks, and
publish the destination only after a complete register or guarded 16/32-byte
memory read. Retained `VPMULDQ` sign-extends even dwords before producing two
qwords per 128-bit lane; `VPMULLD` keeps every dword's wrapping low half.
Neither changes EFLAGS or MXCSR. C4 map two with `66` is required, `W` is
ignored, `vvvv` names the independent first source, `L` selects one or two
128-bit lanes, and VEX.128 zeros the CPU-owned YMM high half.

Rust ownership maps decoding and operand identity to `frontend.c`, operation
identity to `decode.h`, guarded memory access and lane lowering to
`frontend/memory.c`, and architectural vector lifetime to the generated CPU
layout. Both multiply families, register aliases, both widths, and atomic
32-byte memory failure are implemented; legacy multiply families remain owned
by their existing SSE path.

## VEX integer blend/select slice

The retained read-only audit covered
`../engine/src/translator/guest/x86_64/decode.c::hl_x86_decode`,
`avx.c::do_avx` and its `avx_get`, `avx_get_rm`, and `avx_put` operand
staging, plus `translate.c::translate_avx_inline` at map-three opcodes
`02/0c/0d/0e/4a/4b/4c`. The coherent W0, mandatory-66 family comprises
VPBLENDD, VBLENDPS, VBLENDPD, VPBLENDW, VBLENDVPS, VBLENDVPD, and
VPBLENDVB. Immediate selectors choose r/m when their bit is set; the word
selector repeats in each 128-bit lane, while dword and qword controls span
the encoded vector width. Variable forms take their mask register from
is4 bits 7:4 and select by each byte/dword/qword sign bit.

The per-CPU vector image owns XMM and YMM-high identity until task teardown;
these operations allocate nothing, lock nothing, cannot block, and have no
errno, cancellation, flag, or MXCSR effect. Register aliases are staged before
destination publication. A memory operand is validated and loaded across the
complete 16- or 32-byte span before either destination half changes. VEX.128
clears the destination upper half; VEX.256 applies the operation independently
to both stored halves. `frontend.c` owns bounded C4 decode and operand fields,
while `frontend/memory.c` owns AArch64 INS or SSHR/BSL lowering, guarded loads,
exact register-form capacity, and architectural publication. W1 and other
map/prefix spellings remain interpreter fallbacks. The focused matrix covers
all seven operations, both widths, register and memory sources, truncation,
invalid W, provenance, and exact/required-minus-one register capacity.

Memory address decoding temporarily owns scalar address-operation width, so the
blend decoder restores the preserved vector width before vector sizing and VEX
completion. Without that restoration, VEX.128 memory forms take the wide upper
operation instead of clearing YMM-high. Live AArch64 coverage seeds YMM-high
immediately before entry for every form, executes register and guarded memory
sources, and verifies that a one-byte-short mapping faults before either
destination half is published.

## VEX packed saturating narrow slice

The retained oracle is `../engine/src/translator/guest/x86_64/avx.c::do_avx`
(map-one opcodes `63`, `67`, and `6B`, and map-two opcode `2B`) together with
`../engine/src/translator/guest/x86_64/translate.c::emit_avx_inline` and its
`SQXTN`/`SQXTUN` lowering. `avx_get`, `avx_get_rm`, and `avx_put` own operand
staging and publication. Per-CPU `v` and `vhi` storage owns register identity
until task teardown; this family allocates no state, takes no lock, blocks on
no operation, and changes neither integer flags nor MXCSR. A rejected ordinary
memory read abandons before publication, while a completed VEX.128 operation
clears the destination YMM upper half and VEX.256 publishes two independent
128-bit results.

Each 128-bit lane narrows all signed elements from `vvvv` first and all signed
elements from r/m second. `VPACKSSWB` and `VPACKSSDW` clamp to the signed target
range; `VPACKUSWB` and `VPACKUSDW` clamp negative inputs to zero and positive
overflow to the unsigned target maximum. The Rust native frontend maps these
operations to a scratch `SQXTN`/`SQXTN2` or `SQXTUN`/`SQXTUN2` pair before
publishing the destination, preserving non-destructive source and all alias
forms. The shared vector-memory owner stages the full 16/32-byte source before
the operation.

| Retained capability | Rust native status |
|---|---|
| C4 VEX.128/256, WIG, extended registers, non-destructive `vvvv` | implemented |
| `VPACKSSWB`, `VPACKSSDW`, `VPACKUSWB`, `VPACKUSDW` | implemented |
| per-128-lane first/second ordering and signed/unsigned saturation | implemented |
| register aliases and unchanged flags/MXCSR | implemented by scratch-result publication |
| whole-span register/memory fault atomicity | implemented by shared guarded vector load |
| VEX.128 upper-zero and VEX.256 upper publication | implemented |

`test/x86_translation.c::vex_packed_saturating_contract` admits every opcode at
both widths with register and memory sources and rejects wrong-map, wrong-pp,
and misplaced `VPACKUSDW` encodings. The `hl-execution` scalar contract checks
all saturation boundaries, lane ordering, source selection, upper-zeroing, and
fault-before-commit behavior.
## VEX packed unsigned average and byte SAD slice

The read-only oracle audit followed
`../engine/src/translator/guest/x86_64/decode.c::hl_x86_decode` through the
`R_AVX` dispatch, `avx.c::hl_x86_avx_run`, `do_avx`, `avx_get`, `avx_get_rm`,
and `avx_put`, and compared the AArch64 inline owners in
`translate.c::translate_avx_inline` and `translate_one`. The complete related
map-one, mandatory-66 family is VPAVGB (`E0`), VPAVGW (`E3`), and VPSADBW
(`F6`), all WIG and L=0/1. VEX.vvvv is source one; ModRM r/m is a register or
the complete 16/32-byte source two. Average computes unsigned `(a+b+1)/2` per
byte or word. SAD sums eight unsigned byte absolute differences independently
in each qword and zero-extends the at-most-2040 result in its low word.

The dispatcher-owned CPU image supplies XMM/YMM state for the block lifetime.
These operations have no separate identity, allocation, lock, teardown,
blocking, cancellation, errno, flags, or MXCSR effect. `avx_get_rm` validates
the full memory span before `avx_put` publishes any destination lane; register
aliases are safe because both sources are obtained before publication.
VEX.128 clears the destination YMM upper half and VEX.256 applies identical
independent 128-bit lowering to both halves. Portable C owns other hosts; the
NEON path uses URHADD or UABD followed by three pairwise widening reductions.

| Retained capability | Husklet owner | Status after this lane |
|---|---|---|
| VPAVGB/VPAVGW rounded unsigned lanes | `frontend.c` and `frontend/memory.c` | implemented, XMM/YMM register and memory |
| VPSADBW qword-grouped reductions | same | implemented, XMM/YMM register and memory |
| full-span fault before destination commit | generic vector read guard | implemented |
| VEX.128 upper zero and VEX.256 upper result | VEX completion and CPU layout | implemented |
| VMPSADBW map-three immediate windows | native frontend | separate, still unsupported |

The structural contract enumerates all twelve width/register-memory forms,
rejects wrong-map and wrong-prefix aliases, and finds the minimum capacity that
still publishes the instruction before checking required-minus-one. This
explicit search reflects the frontend's evidenced non-monotonic shorter
interpreter-exit fallback; it does not claim a global capacity monotonicity
rule. AArch64 execution tests remain the proportional semantic gate for source
preservation, upper-lane rules, flags/MXCSR, boundary memory, and exact results.

## VEX packed integer compare slice

The retained read-only audit covered
`../engine/src/translator/guest/x86_64/decode.c::hl_x86_decode` and the `R_AVX`
dispatch, `avx.c::hl_x86_avx_run`, `do_avx`, `avx_get`, `avx_get_rm`, and
`avx_put`, plus `translate.c::translate_avx_inline`. Map-one opcodes `64/65/66`
and `74/75/76` own signed greater-than and equality for byte, word, and dword
lanes; map-two opcodes `37` and `29` provide the corresponding qword forms.
Every form requires mandatory prefix `66`, takes its non-destructive first
source from decoded VEX.vvvv, and accepts C4 or C5 where the selected map is
representable. W is ignored and L selects a 16- or 32-byte operation.

The CPU owns XMM state and the YMM-high tail for the dispatcher lifetime. The
operation allocates and locks nothing and has no independent identity,
blocking, cancellation, errno, signal, or teardown behavior. Register operands
cannot fail. `avx_get_rm` validates and reads an entire memory operand before
`avx_put` publishes either destination half, so faults have no partial result.
Equality compares bits; greater-than is signed at every lane width. Each true
lane becomes all ones and each false lane zero. VEX.128 clears the destination
YMM high half; VEX.256 commits both halves without changing EFLAGS or MXCSR.
The retained portable loop and AArch64 inline path have the same state contract.

Rust ownership maps C4/C5 inversion, map, pp, L, W, vvvv, and ModRM admission to
`frontend.c`; the complete 16/32-byte guarded read and destination publication
belong to `frontend/memory.c`; generated CPU layout owns YMM-high lifetime.
`VECTOR_COMPARE_EQUAL` and `VECTOR_COMPARE_GREATER_SIGNED` select AArch64 CMEQ
and CMGT at all four widths. Focused tests admit register and memory forms for
VPCMPEQ{B,W,D,Q} and VPCMPGT{B,W,D,Q}, both vector widths, and both VEX prefix
lengths; reject wrong map and mandatory-prefix encodings; execute signed values
at the sign boundaries; check all-ones masks, non-destructive sources,
unchanged flags/MXCSR, VEX.128 upper-zeroing, YMM results, and full-width memory
fault-before-commit behavior.

The retained packed-extrema audit additionally covered `avx.c::do_avx` and
`lower/crypto.c`'s AArch64 SMIN/SMAX/UMIN/UMAX selection. It has the compare
slice's dispatcher-owned lifetime, vvvv first source, full-width `avx_get_rm`
before `avx_put`, VEX.128 upper clearing, and flags/MXCSR preservation.

| Retained VEX extrema capability | Rust native owner | State |
|---|---|---|
| map-one `DA/DE/EA/EE`: unsigned byte and signed word min/max | frontend admission + vector emitter | implemented, 128/256-bit register/memory |
| map-two `38..3F`: signed byte/dword and unsigned word/dword min/max | same | implemented, 128/256-bit register/memory |
| complete read before destination publication; VEX.128 upper clearing | generic vector guard + VEX completion | implemented |

Focused translation tests cover all twelve opcodes, register/memory sources,
VEX.128/256, and both VEX prefix lengths where representable.

## VEX packed sign and absolute-value slice

The retained read-only audit covered
`../engine/src/translator/guest/x86_64/decode.c::hl_x86_decode`, the `R_AVX`
dispatch, and `avx.c::hl_x86_avx_run`, `do_avx`, `avx_get`, `avx_get_rm`, and
`avx_put`, including the complete legacy SSSE3 `do_sse3b` definitions used as
the two-operand semantic cross-check. Map-two, mandatory-prefix-66 opcodes
`08/09/0A` are VPSIGNB/W/D: VEX.vvvv supplies the value and ModRM r/m supplies
the signed control. Opcodes `1C/1D/1E` are VPABSB/W/D, read only ModRM r/m, and
require the reserved encoded VEX.vvvv field. W is ignored and L selects 16 or
32 bytes. Negative control negates modulo the element width, zero control
produces zero, and positive control preserves the value. Absolute value also
negates modulo width, so each signed minimum remains its original bit pattern.

The dispatcher owns CPU/XMM/YMM-high state and translated-block lifetime. These
operations allocate and lock nothing and have no identity, blocking,
cancellation, errno, signal, or teardown behavior. `avx_get_rm` completes the
entire register or memory read before `avx_put` commits either destination half;
there is no partial result on a memory fault. Destination aliases are safe
because sources are captured first. VEX.128 clears the destination YMM-high
half; VEX.256 publishes both halves. EFLAGS and MXCSR are unchanged, and the
only host-specific distinction is the retained portable loop versus the Rust
AArch64 NEON lowering.

| Retained capability | Rust native owner | State |
|---|---|---|
| VPSIGNB/W/D, VEX.128/256, register/memory | frontend admission + vector emitter | implemented |
| VPABSB/W/D, VEX.128/256, register/memory | same | implemented |
| full-width fault-before-commit, alias safety, VEX upper handling | generic vector guard/completion | implemented |

Focused tests cover the full opcode/width/register-memory matrix, invalid
map/prefix/reserved-vvvv encodings, exact and one-word-short capacity contracts,
and live AArch64 sign zero/negative/minimum behavior, absolute signed minima,
aliasing, upper clearing, flags/MXCSR preservation, and guarded memory faults.

## Consolidated retained-oracle evidence (2026-08-05)

### Legacy SSE shuffle

The retained implementation was inspected read-only at
`../engine/src/translator/guest/x86_64/translate.c` (`translate_one`, legacy
`0F 70` and `0F C6` cases), `interp.c` (the complete `PSHUFD`, `PSHUFHW`,
`PSHUFLW`, `SHUFPS`, and `SHUFPD` definitions), `avx.c` (the corresponding
non-destructive VEX ownership), `emit.c` (`e_ins_s`, `e_ins_d`, vector-copy
and broadcast emitters), and `lower/trace.c` (immediate-bearing instruction
length admission). The benchmark instruction sequence was checked against
`tests/perf/x86_flag_sse_diff.c` and the Rust-owned benchmark inventory in
`X86_PERFORMANCE.md`.

These operations own no allocation, locks, blocking, cancellation, signals,
errno, or teardown. The dispatcher owns the CPU and translated-block
lifetime. The instruction owns one immediate permutation and commits one XMM
destination only after any memory operand has passed the complete guarded read.
Register source/destination overlap is destructive but must read every selected
input lane before architectural commit. None of the family changes integer
flags or MXCSR. REX.R/REX.B extend XMM identities; address-size and segment
prefixes remain owned by the generic effective-address path. The retained
AArch64 path uses scratch vectors for overlap and general immediates, while the
x86 interpreter performs the same byte-exact permutation. There are no
host-specific branches beyond those two execution owners.

| Retained capability | Rust native owner | State |
|---|---|---|
| `66 0F 70 /r ib` PSHUFD, every immediate | frontend decode + vector emitter | implemented |
| `F2/F3 0F 70 /r ib` low/high word shuffle | frontend decode + vector emitter | implemented |
| `0F C6 /r ib` SHUFPS | frontend decode + vector emitter | implemented |
| `66 0F C6 /r ib` SHUFPD | frontend decode + vector emitter | implemented |
| register, destructive alias, REX XMM identity | scratch-first vector emitter | implemented |
| unaligned memory read and fault-before-commit | generic vector memory guard | implemented |
| truncated ModRM/address/immediate | frontend bounded decoder | implemented |
| VEX 128/256 non-destructive forms | bounded C4/C5 frontend + lane-local vector emitter | implemented |

Focused tests admit every legacy prefix and width, register and displaced
memory forms, reject truncated immediates without advancing the guest PC, and
execute a general self-independent PSHUFD permutation on AArch64 while checking
source, flags, and MXCSR preservation. AMD64 quiet comparison is unavailable on
this AArch64 host and is not represented as equivalent evidence.

### VEX shuffle extension

The VEX migration re-audited the retained read-only entry path
`../engine/src/translator/guest/x86_64/decode.c:hl_x86_decode` through the
`R_AVX` dispatch into `avx.c:hl_x86_avx_run` and `do_avx`, plus
`translate.c:translate_avx_inline`, `interp.c`'s complete shuffle definitions,
and `emit.c`'s insertion/copy primitives. C4 and C5 invert R/X/B and vvvv,
select opcode map and mandatory-prefix identity, and carry L/W independently.
Map-one opcode `70` admits pp 1/2/3 with reserved vvvv, while opcode `C6`
admits pp 0/1 and uses vvvv as its non-destructive first source. W is ignored
for this WIG family. Unsupported maps, pp values, and reserved-vvvv violations
return to the interpreter at the original guest PC.

The retained CPU owns XMM plus YMM-high storage for the dispatcher lifetime;
`avx_get_rm` completes the entire 16/32-byte register or guarded memory read
before `avx_put` commits. Each 256-bit operation applies the imm8 separately to
the two 128-bit lanes. A 128-bit VEX write commits the low lane and zeros the
destination's YMM high half; a 256-bit write commits both lanes. Temporary
vectors make destination/source aliasing safe. There is no allocation, lock,
blocking, cancellation, signal, errno, host-specific policy, or separate
teardown in this family.

Rust ownership maps C4/C5 admission to `frontend.c`, decoded non-destructive
source and VEX-width state to `decode.h`, full-width guarded memory access to
`frontend/memory.c`, and architectural YMM-high lifetime to the generated CPU
layout and Rust executor projection. The AArch64 emitter uses scratch vectors
for both lanes, zeros the upper half for VEX.128, and stores the independently
shuffled upper lane for VEX.256 only after a full memory operand is available.
Focused translation tests cover C4/C5, WIG, all five operations, register and
memory sources, both widths, and invalid map/pp/vvvv admission.

### VEX signed-dword conversion extension

The read-only oracle audit for VCVTDQ2PS covered
`../engine/src/translator/guest/x86_64/decode.c` (`hl_x86_decode`, VEX C4/C5
field inversion, and `R_AVX` dispatch), `avx.c` (`hl_x86_avx_run`, `do_avx`,
the map-one `5B` arm, `avx_get_rm`, and `avx_put`), `translate.c`
(`translate_avx_inline` and both legacy/native `CVTDQ2PS` arms), and `interp.c`
(`interp_step_sse` and its packed-conversion width classification). The audit
also traced the AArch64 `SCVTF .4S` emission in `translate.c` and the CPU-owned
FPCR/FPSR save and restore path in `emit.c`.

VCVTDQ2PS is map one, opcode `5B`, `pp=00`; its encoded `vvvv` field is
reserved and must decode to zero, W is ignored, and L selects four or eight
signed-dword lanes. It reads only ModRM r/m, so destination/source overlap is
non-destructive by construction. A VEX.128 write zeros YMM bits 128..255; a
VEX.256 operation converts both independent 128-bit halves. The retained
dispatcher owns the CPU, XMM/YMM, MXCSR-derived FP control, and translated
block lifetime. The instruction allocates and locks nothing and has no
blocking, cancellation, signal, errno, or teardown behavior. A register form
cannot fault. A memory form validates the complete 16/32-byte read before any
destination lane is published; a rejection retains both destination halves
and reports the original instruction PC, address, access kind, and full width.

Rust native ownership maps field validation and ModRM decoding to `frontend.c`,
the operation identity to `decode.h`, guarded atomic memory acquisition and
lane emission to `frontend/memory.c`, and XMM/YMM/FPCR/FPSR storage to the
generated CPU layout plus `entry.S`. AArch64 `SCVTF .4S` observes the guest
rounding mode already projected into FPCR and accumulates inexact status in
FPSR; the existing projection boundary maps those controls and status bits to
MXCSR. The same emitter is applied once per 128-bit half, and the existing VEX
completion path provides upper-zero or upper-store semantics.

| Retained capability | Rust native status |
|---|---|
| C4 and C5, WIG, XMM and YMM register sources | implemented |
| 16/32-byte unaligned memory source with full-span fault-before-commit | implemented by the shared guarded vector-load owner |
| MXCSR rounding and accumulated precision exception | implemented through FPCR/FPSR projection and `SCVTF` |
| reserved-vvvv, wrong map, and nonzero-pp rejection for this family | implemented |
| `VCVTPS2DQ` (`66`) and `VCVTTPS2DQ` (`F3`) sharing opcode `5B` | implemented for VEX.128/256 register and memory forms |

The retained conversion oracle is
`../engine/src/translator/guest/x86_64/avx.c::do_avx` (opcode `5B`) and
`../engine/src/translator/guest/x86_64/translate.c::emit_avx_inline`,
`emit_ps2dq_128`. Per-CPU `v`/`vhi` storage owns register identity and lifetime;
the operation allocates no state, takes no lock, and has no teardown or blocking
transition. VEX `vvvv` must be reserved, C4.W is ignored, `L` selects four or
eight independent lanes, and pp selects signed-dword-to-float, MXCSR-rounded
float-to-dword, or truncating float-to-dword. The rounded path uses `FRINTX` so
inexact in-range inputs accumulate precision, while both paths replace NaN and
positive overflow saturation with `0x80000000`; negative overflow already has
that representation. Memory reads use the common whole-span 16/32-byte guard,
stage before publication, and therefore preserve atomic fault and destination
alias behavior. VEX.128 clears the CPU-owned upper half; VEX.256 publishes both
halves. The AArch64 lowering is the host-specific branch; architectural results
and accumulated MXCSR exception state remain x86-defined.

`test/x86_translation.c::vex_signed_dword_to_float_contract` checks C4/C5,
WIG, all three valid pp values, both vector lengths, register and memory
admission, reserved-pp/wrong-map/vvvv rejection, all eight signed source lanes,
source preservation, YMM-high publication, unchanged integer flags, and
accumulated inexact status. The completeness `cvt_flags` cohort supplies the
rounding, truncation, NaN, infinity, range-boundary, integer-indefinite, invalid,
and precision matrix. Shared vector-memory tests remain the authoritative exact
fault-state, alias, and whole-span atomicity coverage used by these operations.

### 256-bit vector memory

The read-only oracle was `../engine/src/translator/guest/x86_64/avx.c`, entry
points `avx_get`, `avx_put`, `avx_ea`, `avx_try_read`, `avx_try_write`,
`avx_memory_read`, and `avx_memory_write`, plus
`../engine/src/translator/guest/x86_64/translate.c`, entry points
`avx_cpu_ldr_q`, `avx_cpu_str_q`, `avx_zero_upper`, and the VEX memory lowering
inside `emit_avx_inline`.

The oracle owns low 128-bit state in `cpu.v`, YMM bits 128..255 in `cpu.vhi`,
and higher ZMM state in `cpu.vz`. Register state is CPU-instance-local and
survives until task teardown; these helpers neither allocate nor lock. A VEX
write commits its requested width and zeroes state above that width. The
dispatcher owns retry and fault delivery: a rejected whole-span access records
guest address, width, access type, and instruction PC, then abandons before any
architectural register or guest byte is changed. Gather's partial-result rule is
separate and does not apply to ordinary vector loads/stores.

The inline oracle validates `address + width` for overflow, active-view bounds,
and permissions before translating the guest address. The 256-bit path transfers
two 128-bit lanes. Loads stage both lanes before publishing the destination;
stores validate the full 32-byte interval before either lane is written. Both
unaligned accesses and spans crossing a page/view boundary use the same whole
span rule. AArch64 host lowering uses Q-register scratch state; no host-specific
branch changes the x86 architectural result.

| Retained capability | Rust-native owner | Status |
|---|---|---|
| Low 128-bit live register | host `v0..v15`, spilled to `cpu.vectors` | implemented |
| YMM upper 128-bit state | `hl_native_x86_64_cpu.vector_upper` | implemented |
| Upper load/store/zero | `hl_x86_emit_vector_upper_{load,store,zero}` | implemented |
| Whole-span cached read admission | `hl_x86_emit_read_cache` | implemented for 16/32 |
| Whole-span active-view guard | `hl_x86_emit_vector` | implemented for 16/32 |
| Two-lane load staging | `hl_x86_emit_vector` Q16/Q17 scratch | implemented |
| Fault-before-load commit | post-guard copy plus upper store | implemented |
| Fault-before-store commit | full-width guard before both Q stores | implemented |
| Exact dirty interval | `hl_x86_emit_dirty` with width-derived end | implemented |
| VEX shuffle opcode admission and operation cases | VEX frontend | implemented for map-one `70` and `C6` shuffle forms |

### YMM state

- `../engine/src/translator/guest/x86_64/cpu.h`, `struct cpu`: `v[32]`
  owns XMM0..15 and `vhi[32]` owns YMM0..15 bits 255:128. Both are
  per-CPU architectural storage, copied with the CPU on fork/checkpoint, and
  live until that CPU is torn down. No shared state or lock protects them.
- `../engine/src/translator/guest/x86_64/avx.c`, `avx_get`, `avx_put`, and the
  `0x77` VEX dispatch: reads and writes combine `v` with `vhi`; every VEX write
  zero-extends above its encoded width, so VEX.128 clears `vhi[destination]`.
  Legacy SSE paths write only `v` and preserve `vhi`.
- `../engine/src/translator/guest/x86_64/translate.c`, `avx_zero_upper` and the
  VEX lowering entry: inline translated VEX operations use the same split
  register ownership and upper-zero contract. Native faults spill live XMM
  lows; upper halves remain in their CPU-owned memory slots.
- `../engine/src/translator/guest/x86_64/interp.c`, `interp_xmm_get` and
  `interp_xmm_put`: legacy interpreter writes explicitly preserve bits 128 and
  above.
- `../engine/src/translator/guest/x86_64/signal.c`,
  `hl_x86_signal_build`, `hl_x86_signal_restore`, and
  `hl_x86_signal_capture`: signal frames save and restore `vhi`; synchronous
  host-fault capture reconstructs live XMM lows while retaining the CPU-owned
  upper halves.

These paths perform no blocking operation, cancellation, partial result, or
errno conversion. Architecture-specific behavior is x86 VEX width zeroing;
the retained implementation's host-specific AArch64 fault capture does not
replace upper halves because they are never hosted in live vector registers.

| Capability | Rust owner | Status after this lane |
|---|---|---|
| XMM low halves | `hl_execution::CpuState::vectors` | implemented |
| YMM upper halves | `hl_execution::CpuState::vector_upper` | implemented |
| checkpoint/fork | `hl-execution` codec and `ExecutionMachine` clone | implemented |
| Linux signal save/restore | `hl-linux::X86SignalMachine` and engine signal-frame adapter | implemented |
| native C/Rust ABI | generated `hl_native_x86_64_cpu.vector_upper` / `X86_64Cpu::vector_upper` | implemented |
| native entry/exit transport | `NativeX86::capture` / `NativeX86::restore` | implemented |
| VEX.128 destination upper-zero | VEX shuffle emitter | implemented for map-one `70` and `C6` shuffle forms |

The ABI field is appended after all established native fields. Existing baked
emitter, trampoline, polling, dirty-publication, and fault offsets therefore do
not move. The generated C and Rust size/offset assertions cover the new tail.

### VEX packed horizontal add and subtract

The retained oracle audit covered
`../engine/src/translator/guest/x86_64/avx.c::hl_x86_avx_run`, `do_avx`
(map two opcodes `01/02/03/05/06/07`), `avx_get`, `avx_get_rm`, `avx_put`,
`avx_ea`, and `avx_try_read`, together with the `R_AVX` handoff in
`dispatch.h`/`interp_dispatch.h` and the generic VEX fallback in
`translate.c`. It also checked the legacy SSSE3 sibling in
`avx.c::do_sse3b` and the retained corpus model in
`tests/compat/abi/corpus/x_vex_ssse3.c`.

The six admitted instructions are VEX.128/256 `VPHADDW`, `VPHADDD`,
`VPHADDSW`, `VPHSUBW`, `VPHSUBD`, and `VPHSUBSW`, with `66` mandatory prefix
and register or full-width memory operand. Within each independent 128-bit
lane, adjacent pairs from vvvv precede adjacent pairs from r/m. Word and dword
non-saturating forms wrap; only opcodes `03/07` saturate signed word results.
They change neither integer flags nor MXCSR. Both sources are staged before the
destination, so either source may alias it. The current emitter maps wrapping
add directly to AArch64 `ADDP`; subtract and signed-saturating forms pair even
and odd elements with `UZP1/UZP2` before `SUB`, `SQADD`, or `SQSUB`.

Retained vector state is CPU-instance-owned (`v` low halves and `vhi` upper
halves), lives through the CPU/task lifetime, and is published only by
`avx_put`; this family allocates, blocks, cancels, and locks nowhere. The
dispatcher owns fallback and teardown. `avx_get_rm` validates a complete 16- or
32-byte read before `avx_put`, so a fault produces no partial destination. The
Rust owner is the x86 frontend decode plus vector memory guard/emitter: it keeps
the same fault-before-publication order, VEX.128 upper-zeroing, per-lane YMM
ordering, and architecture-independent guest result. Host differences are only
the retained C soft path versus AArch64 native lowering; no errno, signal,
checkpoint, or floating-point special case belongs to these operations.

| Retained capability | Rust-native owner | Status |
|---|---|---|
| Exact six map-two/`66` opcode forms | VEX frontend admission | implemented |
| 128/256 register and memory widths | vector decode and whole-span read guard | implemented |
| Per-128-lane vvvv-then-r/m ordering | horizontal vector emitter | implemented |
| Wrapping word/dword add/sub | `ADDP` or `UZP1/UZP2` plus `SUB` | implemented |
| Signed-word saturation for `03/07` only | `SQADD` / `SQSUB` | implemented |
| vvvv/rm destination aliases | scratch-first emitter ordering | implemented |
| Flags and MXCSR preservation | no status-state publication | implemented |
| Memory fault before destination publication | `hl_x86_emit_vector` read guard | implemented |
| VEX.128 upper zero and VEX.256 upper result | `emit_vex_completion` | implemented |

`test/x86_translation.c::vex_horizontal_add_sub_contract` covers all 24
opcode/width/register-memory combinations, both destination alias directions,
rejected map/prefix/opcode forms, exact-capacity byte/provenance equivalence,
required-minus-one atomic capacity failure, and all six 256-bit operations on
an AArch64 execution path with wrapping, saturation, lane ordering, and flags/
MXCSR preservation checks. The full warning-strict native translation test is
the lane gate.
