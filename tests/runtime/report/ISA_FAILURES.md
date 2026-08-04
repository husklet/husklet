# ISA failure clusters

## Corpus accounting

`FULL_CORPUS_001.tsv` contains one header and 3,113 data rows. There is no fingerprint row.

| Measure | Count |
| --- | ---: |
| physical lines | 3,114 |
| data rows | 3,113 |
| unique suite/case/ISA keys | 3,113 |
| pass | 1,721 |
| fail | 1,392 |
| x86_64 rows | 1,573 |
| aarch64 rows | 1,540 |

The earlier 3,112 total and 1,720 pass count skipped line two under the incorrect assumption that the file had both a
fingerprint and a header. Line two is the first real result, `abi/ackermann/aarch64/pass`. The compatibility accounting
must therefore start after one line, not after two.

## Ranked terminal clusters

The report retains the final engine outcome but not always the originating guest instruction.

| Rank | ISA | Terminal result | Count | Interpretation |
| ---: | --- | --- | ---: | --- |
| 1 | x86_64 | exit 0, output mismatch | 367 | implemented path with incorrect observable semantics |
| 2 | aarch64 | exit 0, output mismatch | 366 | implemented path with incorrect observable semantics |
| 3 | x86_64 | signal 4 / exit 132 | 165 | unsupported or rejected x86 instruction |
| 4 | aarch64 | timeout | 112 | progress, wakeup, scheduling, or syscall readiness failure |
| 5 | x86_64 | timeout | 105 | progress, wakeup, scheduling, or syscall readiness failure |
| 6 | aarch64 | exit 1 | 59 | fixture assertion or ordinary program failure |
| 7 | x86_64 | exit 1 | 53 | fixture assertion or ordinary program failure |
| 8 | aarch64 | signal 4 / exit 132 | 43 | unsupported or rejected AArch64 instruction |
| 9 | x86_64 | worker error | 22 | runner or engine-control failure |
| 10 | aarch64 | worker error | 17 | runner or engine-control failure |
| 11 | x86_64 | signal 11 / exit 139 | 13 | incorrect memory, process, signal, or ISA semantics |
| 12 | aarch64 | signal 11 / exit 139 | 12 | incorrect memory, process, signal, or ISA semantics |

The remaining 70 failures are distributed across smaller exit-code and signal clusters.

## x86 illegal-instruction cluster

The largest directly actionable ISA terminal cluster is the 165 x86_64 signal-4 rows.

| Suite | Count |
| --- | ---: |
| abi/corpus | 65 |
| completeness | 49 |
| abi | 24 |
| core/abi | 8 |
| libc | 5 |
| isa/x86_64 | 3 |
| core/regress | 2 |
| memory | 2 |
| core/syscall | 1 |
| core/workload | 1 |
| ipc | 1 |
| isolation | 1 |
| soak | 1 |
| syscall | 1 |
| time | 1 |

This is not one opcode cluster. `completeness` and the explicit ISA fixtures deliberately contain unrelated instruction
families. An implementation choice must be based on repeated ordinary-program frontiers, not an arbitrary specialized
row.

### Diagnostic limitation

`hl-execution` converts a scalar-decode error to a synchronous SIGILL at the current RIP. The application layer queues
that signal and later reports the default signal disposition as:

```text
EngineExit { kind: Signal, guest_status: 4, detail: 0, fault: None }
```

The originating RIP and instruction bytes are lost before the TSV is written. Consequently, the existing TSV cannot
support an exact all-165 grouping by first fault PC. Exact grouping requires either preserving the synchronous trap RIP
and bytes in the report or tracing each immutable artifact. Treating every signal-4 row as the same decoder gap would be
false precision.

## Representative immutable artifacts

Static disassembly of ordinary ABI fixtures gives useful first reachable unsupported instructions:

| Fixture | First relevant PC | Bytes | Instruction | Rust state before this change |
| --- | ---: | --- | --- | --- |
| abi/float32/x86_64 | `0x4026de` | `f3 0f 2a cb` | `cvtsi2ss %ebx,%xmm1` | F3 scalar conversion unsupported |
| abi/switch/x86_64 | `0x402708` | `48 0f bf 0c 4e` | `movswq (%rsi,%rcx,2),%rcx` | `0f bf` word sign extension unsupported |
| abi/manyargs/x86_64 | `0x40271c` | `66 45 0f 14 f6` | `unpcklpd %xmm14,%xmm14` | packed-double unpack unsupported |

The `float32` path immediately continues through generic scalar-single SSE operations:

- MOVSS at `0x4026e6`;
- MULSS at `0x402710`;
- UCOMISS at `0x402734`;
- SQRTSS at `0x402741`;
- ADDSS at `0x40275a`;
- CVTSS2SD at `0x4027ce`.

UCOMISS already existed in the generic scalar comparator. The surrounding F3 move, arithmetic, and conversion family did
not. Implementing only `CVTSI2SS` would merely move this fixture to the next SIGILL, so the scalar-single family is the
highest-leverage cohesive frontier among the inspected ordinary ABI failures.

## Retained C comparison

The following retained implementation was read line-for-line for this family:

- `src/translator/guest/x86_64/decode.c` for mandatory-prefix and operand decoding;
- `src/translator/guest/x86_64/interp.c` for exact SSE memory widths, upper-lane rules, MXCSR behavior, and native
  x86 execution;
- `src/translator/guest/x86_64/translate.c` for the AArch64 projection and its NaN fixups;
- `src/translator/guest/x86_64/avx.c` for host-independent x86 NaN selection and generated-indefinite behavior.

The retained contract is:

1. F3 selects scalar binary32 and outranks a coexisting `66` prefix.
2. MOVSS register forms replace only the low 32 bits; a memory load clears the upper 96 bits; a memory store accesses
   exactly four bytes.
3. ADDSS, SUBSS, MULSS, DIVSS, and SQRTSS replace only the low 32 bits and preserve the rest of the destination.
4. MXCSR controls rounding, denormals-are-zero, flush-to-zero, and sticky invalid, denormal, divide-by-zero, overflow,
   underflow, and inexact flags.
5. For add/subtract/multiply/divide, an input NaN is selected with source-one priority and quieted. A NaN generated with
   no NaN input uses x86's negative indefinite `0xffc00000`.
6. CVTSI2SS reads signed r/m32 or r/m64 according to REX.W and preserves the destination's upper 96 bits.
7. CVTSS2SI and CVTTSS2SI use MXCSR rounding or truncation respectively, return the integer-indefinite bit pattern on
   invalid conversion, and set sticky exceptions.
8. CVTSS2SD and CVTSD2SS preserve the destination's untouched upper lane and preserve a converted NaN's sign and
   payload.

## Rust implementation

The prior Rust implementation had three binary64-specific owners:

- `x86/vector/scalar_double.rs` for MOVSD;
- `x86/scalar/double.rs` for ADDSD, SUBSD, and MULSD;
- `x86/convert_double.rs` for CVTSD2SI.

They were generalized into capability owners:

- `x86/scalar/transport.rs` owns scalar vector/register and memory transport for binary32 and binary64;
- `x86/scalar/arithmetic.rs` owns scalar binary32/binary64 add, subtract, multiply, divide, and square root;
- `x86/scalar/conversion.rs` owns signed integer/float, float/integer, and binary32/binary64 width conversion;
- `x86/scalar/vector.rs` owns the vector and floating IR vocabulary.

The new IR carries `FloatWidth`; it does not encode Go, libc, Chrome, or a fixture identity. Focused source tests cover:

- MOVSS register merge, memory-load clearing, four-byte store, and four-byte fault evidence;
- ADDSS, SUBSS, MULSS, DIVSS, and SQRTSS results with upper-lane preservation;
- MXCSR rounding and sticky inexact/divide-by-zero/invalid flags;
- source-one NaN propagation and negative generated indefinite;
- CVTSI2SS, CVTSS2SD, and CVTSS2SI;
- F2/F3 precedence over a coexisting `66` prefix.

The cohesive IR vocabulary move reduces `x86/scalar/ir.rs` from 517 to 452 production lines, resolving the design-lint
failure without suppression or an arbitrary test split.

## Remaining ranked ISA work

1. Preserve synchronous trap PC and instruction bytes in compatibility diagnostics, then rerun the full immutable
   corpus. This is required for exact first-opcode clustering rather than representative inference.
2. Validate the scalar-single source change with the focused `hl-execution` tests and the two `float32` fixture rows.
   This report records source inspection only until the manager grants a Cargo slot.
3. Implement generic `MOVSX r16/m16` (`0f bf`) to clear the `switch` frontier.
4. Audit packed-double unpack/arithmetic/conversion for `manyargs`; do not fold those semantics into scalar SSE.
5. Split the 733 output-mismatch rows by normalized expected/actual difference and dependency before choosing another
   ISA change. Exit-zero mismatches are a larger semantic population than SIGILL and must not be hidden by decoder work.
6. Cluster the 217 timeout rows by last guest progress, syscall, wait object, and process tree. They are not ISA decode
   failures merely because they occur under emulation.
