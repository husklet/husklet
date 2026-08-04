# AArch64 performance baseline

This is the first execution-mode-proven comparison between the retained C
engine and the Rust native adapter. It is a regression baseline, not an
acceptance result: every Rust slowdown below is an open performance defect.

The run used five repetitions of each selected phase from the same retained
`combined-bench` static-PIE guest with `--divisor 20`. Guest medians are in
`us`; `wall_us` is the median complete engine-process duration. The Rust runner
required nonzero native diagnostics and stamped every Rust row
`native-verified`.

| Phase | C guest | Rust guest | Guest ratio | Wall ratio |
|---|---:|---:|---:|---:|
| compute | 2.442 ms | 314.223 ms | 128.674x | 15.287x |
| integer division | 15.349 ms | 49.872 ms | 3.249x | 1.680x |
| memory | 6.564 ms | 5,010.645 ms | 763.352x | 198.484x |
| pipe | 69.421 ms | 3,145.238 ms | 45.307x | 35.181x |
| syscall | 20.991 ms | 794.143 ms | 37.833x | 22.553x |

Compute, integer division, memory, and pipe checksums match across engines.
The syscall phase checksum includes syscall results that currently diverge and
is tracked as both a compatibility and performance defect.

## Provenance

- Date: 2026-08-03
- Host: Linux AArch64 under OrbStack, 18 logical CPUs
- C source: `7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`
- C executable SHA-256: `9c2701b36a46050909b12498eb0b47673f301bcb35d57e552d343b029cf3a67a`
- Rust source: `13d2fb7a4bca892de8d1f52ddf582801177f76c9`
- Rust executable SHA-256: `fbc2bf68c2f89d930e6e58c8ae4e953a79600d3c0a72a95ba2d07599be90f443`
- Guest SHA-256: `07756eb451ec3c063a6ffed129db76b8702d5a37be8ba9cbf79eb77944d052ee`

The dominant observed memory defect is more than 8.5 million native fallback
boundaries in libc memory loops. The retained C translator keeps the equivalent
guest loads, stores, and recognized loops inside translated execution. Pipe has
few instruction fallbacks; its cost is concentrated in syscall transitions and
the descriptor, marshalling, and in-memory pipe path.

## Optimization checkpoints

Commit `5cbbb322` added generic native scalar floating-point traces and exact
FPCR/FPSR preservation. A clean five-repeat rerun reduced compute guest time
from 314.223 ms to 13.039 ms and wall time from 349.550 ms to 34.880 ms with the
same checksum. That is a 24.10x Rust improvement; the remaining gap is 5.34x in
guest time and 1.53x in complete-process wall time versus the pinned C row.
`compute.csv` retains the raw checkpoint. Its Rust executable SHA-256 is
`ba9d257237e4970a3e70c20435f25a5346ee592dc559c8d56ebf8f8324ea8a20`.

The scheduler/pipe series `1589ca7a` + `6eefbcad`, merged on main as
`2bde9a45` + `aebaa40f`, keeps provably ready single-runnable-thread pipe
operations on the scheduler and accounts for fragmented atomic-write capacity.
A clean five-repeat run reduced pipe guest time from 3,180.805 ms to 495.234 ms
and wall time from 3,572.050 ms to 558.425 ms with checksum 75,000. This is a
6.42x guest and 6.40x wall improvement; the remaining gap is 7.13x guest and
5.67x wall versus C. `pipe.csv` retains the raw checkpoint.

The exact-write projection series `3f4fea1b` + `e3807faf` retains validated
writable operand views and journals disjoint writes without publishing the
holes between them. Journal capacity is checked before a host store, while the
dirty interval is committed only after the store succeeds. A clean five-repeat
run from exact commit `e3807faf` reduced memory guest time from 5,010.645 ms to
2,958.133 ms and wall time from 5,558.537 ms to 3,171.877 ms with checksum
36,526. This is a 1.69x guest and 1.75x wall improvement; the remaining gap is
450.66x guest and 113.26x wall versus C. `memory.csv` retains the raw checkpoint.
The Rust executable SHA-256 is
`673062c90ac260f2fac3f0fabbf1bf2298b43a86e5d0ade9e467ef1f93c92e70`.

Commit `13080762`, merged on main as `2f931539`, resolves generation-checked
writable views from the bounded in-lease cache when a trace alternates between
already projected stack and heap ranges. It reduced native write-projection
boundaries from 2,263,021 to 151,130. A clean five-repeat exact-commit run
reduced memory guest time again, from 2,958.133 ms to 2,656.846 ms, and wall
time from 3,171.877 ms to 2,713.397 ms with the same checksum. The remaining
gap is 404.76x guest and 96.89x wall versus C. `memory.csv` now retains this
newest checkpoint.

Commit `367aab1e`, merged on main as `64ec7af2`, resumes a bounded sequence of
provably nonblocking synchronous services while retaining scheduler ownership.
Every iteration publishes and drops projection leases before service, rechecks
ptrace, interruption, runnable ownership, blocking classification, CPU timers,
and CPU-accounting observation, and restores the ordinary boundary after at
most 64 services. Against the same-tree boundary-disabled path, a clean
five-repeat run reduced syscall guest time from 1,041.824 ms to 288.876 ms and
wall time from 1,196.888 ms to 387.056 ms: 3.61x guest and 3.09x wall. The
remaining gap is 13.76x guest and 9.53x wall versus C. `syscall.csv` retains the
raw checkpoint.

Commit `b5d39355`, integrated as code-only main commit `68f98e3f`, adds an
explicit contiguous-extension fast path to both halves of the AArch64
two-phase dirty journal. Capacity is still checked before mutation and the
range is still committed only after a successful host store. On a clean
five-repeat integration run, memory guest time fell from 2,656.846 ms to
2,466.124 ms and wall time from 2,713.397 ms to 2,516.194 ms with checksum
36,526. This is another 7.18% guest and 7.27% wall reduction; the remaining gap
is 375.70x guest and 89.85x wall versus C. `memory.csv` retains the row. The
Rust executable SHA-256 is
`454630222e63533d11e1696b33da73db274e34ef0583e36647a0822d3410e172`.

Commit `4281c3fa`, integrated on main as `670be391`, removes a complete
memory-ledger snapshot clone and a heap allocation from every native re-entry.
It resolves the executable PC, branch targets, and writable stack with
generation-qualified, protection-checked point lookups and uses a bounded
256-byte source buffer. A pinned five-repeat same-tree comparison reduced the
syscall phase from 312.408 ms to 240.602 ms guest time and from 393.980 ms to
299.525 ms wall time, improvements of 22.98% and 23.97%, respectively. The
Rust checksum remained 1,000,000 and native diagnostics remained unchanged.
The fresh colocated C median was 30.850 ms guest and 58.815 ms wall, leaving a
7.80x guest and 5.09x wall gap. `syscall.csv` retains the new Rust checkpoint.

### Rejected eight-view cache experiment

The unmerged `df7c0cc5` + `6def5ed0` experiment expanded the AArch64 native
operand-view cache from four to eight entries. It reduced deterministic memory
fallback boundaries from 151,130 to 134,557 (10.97%) and resolver callbacks
from 686 to 398 (41.98%), but a quiet five-repeat comparison showed no material
speed improvement. Exact `867734f7` had a 2,521.502 ms guest median and
2,577.839 ms wall median; the candidate measured 2,523.733 ms guest and
2,572.541 ms wall with checksum 36,526 in every run. That is 0.089% slower in
guest time and 0.206% faster in wall time, both measurement noise. The pinned C
median was 6.793 ms guest and 28.775 ms wall, so the candidate remained 371.52x
slower by guest time. The capacity and CPU-schema changes were therefore
rejected and are not performance progress.

The experiment independently exposed a real pre-existing safety defect: after
assembler exhaustion, guard backpatching could dereference incomplete patch
slots. That fail-closed correction is being retained separately without the
rejected cache-capacity or large-scratch changes. This distinction prevents a
deterministic counter reduction from being reported as a benchmark win.
