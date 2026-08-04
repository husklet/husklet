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
