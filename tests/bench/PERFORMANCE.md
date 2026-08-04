# Engine performance checkpoint

## 2026-08-04: retained C versus Rust native execution

This checkpoint measures the folder-owned `combined/main.c`, which is byte-for-byte
identical to `../engine/tests/perf/combined_bench.c`. The Rust binaries were built
from clean detached Husklet commit
`2b731a4337ddd7ad1d4d96b2a7ec4b124508e3e3`. The retained C source paths used by
the benchmark were clean at commit
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`; the executed C binaries are
content-bound below because their build directory is not a clean checkout.

| artifact | SHA-256 |
|---|---|
| Rust `hl-engine` | `ea0cd2d28f1e4a1eae2ee06b064ca7ed4760794b051be16e2535dca1e9007fe6` |
| Rust `testing` | `5dd399743d9513c33685872163e273778971330d4309298403684265388a4980` |
| ARM64 guest | `b39066b2ae7468d8a57359f963515729f92cbfd7bc53fb7af68980909e3be08f` |
| AMD64 guest | `d3be5c156c27b3069a0a90d3788d4eef6a77e1cf67a06ea55d79f9a64953755c` |
| C ARM64 engine | `9c2701b36a46050909b12498eb0b47673f301bcb35d57e552d343b029cf3a67a` |
| C AMD64 engine | `d25cc62b03b9baf68f7edea8ec8f87146ca5f57ffc380fa30555020dbedf30a2` |

### Method

- Linux ARM64 host, performance governor, one cell at a time pinned to CPU 17.
- Five samples per cell, `--divisor 20`, 120-second per-sample timeout, and a
  1 MiB output bound. With five samples, the observed maximum is nearest-rank
  p90.
- Rust used `HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`; every measured Rust
  cell reported `native-verified` with nonzero native diagnostics.
- Host start: 18 logical CPUs, 15 GiB available RAM, 9.9 GiB swap used, 192 GiB
  disk free, and 7.7 GiB `/tmp` free. Concurrent repository builds later raised
  load average to 26 and swap use to 14 GiB. In-guest phase time is therefore
  the comparison metric; wall time is retained only as diagnostic evidence.

Times are microseconds. Ratios are Rust divided by retained C.

| ISA | phase | C median | C p90 | Rust median | Rust p90 | median ratio | p90 ratio |
|---|---:|---:|---:|---:|---:|---:|---:|
| ARM64 | compute | 3,596 | 6,539 | 16,052 | 20,147 | 4.464x | 3.081x |
| ARM64 | float SIMD | 10,540 | 11,748 | 15,526,078 | 19,704,347 | **1,473.062x** | **1,677.251x** |
| ARM64 | memory | 7,044 | 7,766 | 2,203,672 | 2,824,412 | 312.844x | 363.689x |
| ARM64 | syscall | 26,495 | 27,288 | 238,371 | 270,094 | 8.997x | 9.898x |
| ARM64 | signal | 23,969 | 25,187 | 3,166,848 | 3,340,777 | 132.123x | 132.639x |
| ARM64 | pipe | 72,562 | 73,899 | 291,401 | 307,204 | 4.016x | 4.157x |
| AMD64 | compute | 5,003 | 5,158 | 972,729 | 994,547 | 194.429x | 192.816x |
| AMD64 | float SIMD | 18,712 | 19,530 | 20,095,445 | 21,035,868 | 1,073.934x | 1,077.105x |
| AMD64 | memory | 6,531 | 7,477 | 303,537 | 305,902 | 46.476x | 40.912x |
| AMD64 | syscall | 26,004 | 30,696 | 490,431 | 520,288 | 18.860x | 16.950x |
| AMD64 | signal | 15,261 | 16,235 | 2,701,050 | 3,324,126 | 176.990x | 204.751x |
| AMD64 | pipe | 78,726 | 79,765 | 1,010,559 | 1,060,462 | 12.836x | 13.295x |

Checksums matched for every comparable row. The syscall checksum contains the
guest TID and is intentionally not compared. The file phase is excluded from
performance ratios because it exposed a correctness failure: Rust returned zero
successful operations while C returned 20,000. The complete ARM64 C sample ran
all phases, while the complete Rust sample reached the fixed 120-second bound;
the table uses individually selected phases with identical internal warmups.

The largest measured hot-path gap is ARM64 float SIMD. Root-cause work must first
identify its generic translation or fallback mechanism; this checkpoint does not
justify an application-specific fast path.

## Candidate evidence after the checkpoint

The results below come from the shared dirty tree and are diagnostic candidate
evidence, not evidence for `HEAD` or a stable revision.

### ARM64 baseline SIMD admission

Disassembly of `phase_float_simd` identified vector MOV/ORR and FMLA in the hot
loop and vector SCVTF during setup. All three instruction families terminated
the native trace. The scheduler then suppressed the failed entry and repeatedly
executed the loop through lane-by-lane SoftFloat. Current diagnostics confirmed
13 native fallbacks across 13 builds before the candidate.

The candidate admits the complete baseline AdvSIMD logical family and the S/D
vector FMLA/FMLS and SCVTF/UCVTF families. Optional FP16, BF16, and I8MM remain
excluded pending explicit host/guest feature policy. A warning-strict trace test
covering AND, MOV, FMLA 4S, FMLS 2D, SCVTF 4S, UCVTF 2D, and SVC failed before
the change and passes afterward.

One full `--divisor 20 --phase float_simd` candidate row completed
`native-verified` with the same checksum at 14,251,204 us. This is 8.2% faster
than the clean checkpoint's 15,526,078-us median but remains about 1,352 times
the retained-C median. It proves SIMD admission helps without identifying it as
the dominant remaining cost. Vector memory guards and remaining fallback
boundaries are the next measured owners.

### AMD64 REP bulk accounting

The retained domain audit covered `lower/repstr.c` and `rep_runtime.c` for MOVS
and STOS decoding, largest pinned-span selection, direct and indirect mappings,
forward-overlap smear, backward DF order, partial faults, register publication,
store observation, and pin release on every path. It also covered
`guest_memory.c` for pin lifetime and `dispatch.c` for helper return ordering.
The current Rust-owned comparison covered x86 `run.c`, `projection.c`, the Rust
projection lease, and scheduler slice selection. The lease holds mapping
identity for the synchronous run; native code owns only borrowed bounded views,
and post-success dirty publication precedes any executable-write epoch exit.

The checkpoint's AMD64 memory row was 46.476 times the retained-C median.
Diagnostics isolated a deterministic amplification: one memory iteration made
1,537 public exits and four made 2,305, exactly 256 additional exits for each
additional 1 MiB copy. Projection already supplies a bounded contiguous 1 MiB
view, but native REP charged every copied byte against the 4,096-instruction
scheduler budget.

A candidate charged one authenticated, at-most-1-MiB REP chunk as one guest
instruction. Its five-repeat native-verified row had checksum 36,526 throughout
and a 24,599-us median, 12.34 times faster than the checkpoint Rust median.
Independent review rejected and reverted it: the established interpreter and
native contract charges every REP element against the instruction budget. The
experiment let budget one copy up to 1 MiB, changed `executed` semantics, and
weakened interrupt/checkpoint latency by up to 1,048,576 times. Its timing is
useful causal evidence for the exit amplification, not an acceptable product
result.

The accepted follow-up must either preserve per-element accounting while
eliminating public exits, or explicitly redesign the common interpreter/native
string-budget contract. It requires differential coverage for a greater-than-
1-MiB partial chunk and resume, the next-view fault, interrupt/checkpoint
latency, DF and overlap order, exact RCX/RSI/RDI, dirty publication, and
executable-write epoch behavior before another benchmark.

### AMD64 scalar SSE2 audit

The compute loop's hot instruction sequence is MULSD, integer XOR/IMUL/ROL,
ADDSD, COMISD, conditional SUBSD, and CVTTSD2SI. Current native lowering admits
only scalar square root, addition, multiplication, and division. Retained C
owns the coherent scalar SSE/SSE2 arithmetic, comparison, conversion, MXCSR,
NaN, EFLAGS, and indefinite-result semantics. The missing family, rather than
one benchmark opcode, was assigned to a separate implementation lane.

That candidate implements SUBSD, COMISD/UCOMISD, and 32/64-bit CVTTSD2SI for
register and guarded-memory forms, including ordered/unordered flags, signaling
versus quiet NaNs, integer-indefinite overflow results, and preservation of the
upper XMM lane. Its warning-strict focused structural and differential test
passes. Five native-verified compute repeats produced a 616,183-us median and
539,149--663,124-us range with checksum 7,097,455,804,780,747,230 throughout.
That is 1.58 times faster than the checkpoint Rust median but still 123 times
the retained-C median. The result confirms the coherent scalar family removes
real fallback cost; packed and scalar-single SSE remain the larger gap. This is
shared dirty-tree evidence. The observed post-build `HEAD` was
`2114daca34f3790f0ce2c887dcaf3ad6551567eb`, and the candidate engine SHA-256
was `a9a544aadfb9b7ec69c8c4c59036f24eed6fb60d60d8240396b37e7e8b2dbf92`.
