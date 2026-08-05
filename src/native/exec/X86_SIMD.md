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
| PSHUFD/PSHUFHW/PSHUFLW/SHUFPS | every immediate lane selection, including destructive overlap | legacy SSE and VEX 128/256 non-destructive forms implemented | broader AVX families remain separate |
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
| MMX and VEX forms | native frontend | remaining separate gaps |

`test/x86_translation.c::packed_integer_multiply` exercises every register and
unaligned guarded-memory form, checks every lane against a software oracle,
verifies source/EFLAGS/MXCSR preservation, and rejects the MMX spelling.

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
| `VCVTPS2DQ` and `VCVTTPS2DQ` sharing opcode `5B` under other pp values | remaining separate conversion families |

`test/x86_translation.c::vex_signed_dword_to_float_contract` checks C4/C5,
WIG, both vector lengths, register and memory admission, wrong-map/pp/vvvv
rejection, all eight signed lanes, source preservation, YMM-high publication,
unchanged integer flags, and accumulated inexact status. The shared vector
memory tests remain the authoritative exact fault-state and whole-span atomicity
coverage used by this operation.

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
