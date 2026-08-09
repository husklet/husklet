# Engine performance checkpoint

## 2026-08-08: BENCH-MAP-4 re-take after the scheduler, store-protocol and signal fixes

Head `3315d2a046b7` on `refactor/merge-rust-engine`, release build, Linux ARM64
host, 18 logical CPUs, `CARGO_TARGET_DIR` under `/var/tmp` with `target/release`
symlinked into the tree, lane-private scratch. Every row used the benchmark gate
at `--repeats 9 --divisor 1 --max-spread 0.05` against the retained C build
`/Users/x/dd/engine/build/unit-audit` (`c_build_id sha256:9c2701b3…`, unchanged
from all three previous maps). Rust engine `sha256:7f1a34880c25…`, revision
`3315d2a046b7` (clean, not `-dirty`).

**The guest is byte-identical to the previous three maps'.**
`git log d6861edf3..3315d2a04 -- tests/bench/combined/main.c` is empty, so every
number below is engine behaviour, not a changed guest. 32 commits touching
`src/containers/hl-engine` landed between BENCH-MAP-3's head and this one,
including all four fixes this lane existed to check.

**Completion was verified per arm, not by exit code.** All eleven rows recorded
9 sample files for each of `native`, `c-engine` and `rust-engine` (297 CSVs), and
the per-arm `ok` checksum (column 5 of `cycle-NNN-<env>-arm64.csv`) is identical
across all three arms on every row except `syscall`, whose checksum embeds the
guest TID and is intentionally not compared. No row came from an aborted
baseline. The gate also enforces this itself (`f419cf905`).

**Measurement caveat — the promised idle box did not materialise, and this is
the map's main limitation.** The lane was briefed that the host was idle at load
0.15. It was quiet only between roughly 08:50 and 09:05; for the rest of the
session a sibling lane ran `rustc` and its own `testing` binaries, and the
1-minute load average swung between 2.4 and 31.1. Per-row `host_load` is recorded
below and ranges 2.45 to 27.34 out of 18. Per the lane rule, `--max-spread` was
**not** relaxed. **Two rows returned a pass verdict** (`calls` at spread 0.016,
`memory` at 0.034); the other nine are **indicative** — treat `rust_x_c` and the
ordering as sound and the absolute microseconds as inflated.

**A structural note on `host_load` that previous maps did not record.** This host
is an **LXC container inside a Linux VM on macOS** (`systemd-detect-virt` reports
`lxc`; CPU implementer `0x61`). `/proc/loadavg` therefore reports only what is
visible *inside* the container — macOS-side contention is invisible to it. A
reported `host_load` of 2.4 is consistent with a busy machine, and this is the
most likely reason spread does not converge even when the box looks quiet. **An
"idle box" claim taken from inside this container is not verifiable**, which is
worth knowing before another lane is told to wait for one.

Ranked by absolute time lost against C (`rust_us - c_us`), which is the ordering
the optimisation lanes should prioritise by — not by ratio. Where a row was taken
twice, the take with the lower spread is shown and the other is noted.

| rank | workload | native_us | c_us | rust_us | rust_x_c | rust_x_native | spread | verdict | host_load | lost vs C (us) |
|---:|---|---:|---:|---:|---:|---:|---:|---|---:|---:|
| 1 | malloc | 195,753 | 315,530 | 72,777,986 | 230.653 | 371.785 | 0.414 | no verdict (spread) | 27.34 | 72,462,456 |
| 2 | signal | 599,605 | 501,100 | 10,947,690 | 21.847 | 18.258 | 0.139 | no verdict (spread) | 11.79 | 10,446,590 |
| 3 | tlb | 263,448 | 281,678 | 4,944,639 | 17.554 | 18.769 | 0.064 | no verdict (spread) | 5.19 | 4,662,961 |
| 4 | file | 168,483 | 257,339 | 4,128,889 | 16.045 | 24.506 | 0.147 | no verdict (spread) | 2.64 | 3,871,550 |
| 5 | pipe | 339,835 | 1,519,157 | 4,547,254 | 2.993 | 13.381 | 0.149 | no verdict (spread) | 7.63 | 3,028,097 |
| 6 | mmap | 249,975 | 358,528 | 2,644,525 | 7.376 | 10.579 | 0.150 | no verdict (spread) | 6.29 | 2,285,997 |
| 7 | syscall | 942,778 | 471,582 | 2,521,400 | 5.347 | 2.674 | 0.287 | no verdict (spread) | 2.45 | 2,049,818 |
| 8 | calls | 92,922 | 142,761 | 1,480,773 | 10.372 | 15.936 | 0.016 | **pass** | 3.20 | 1,338,012 |
| 9 | memory | 136,464 | 137,425 | 1,457,567 | 10.606 | 10.681 | 0.034 | **pass** | 3.16 | 1,320,142 |
| 10 | branch | 364,498 | 360,091 | 412,565 | 1.146 | 1.132 | 0.132 | no verdict (spread) | 13.50 | 52,474 |
| 11 | compute | 49,483 | 52,028 | 55,238 | 1.062 | 1.116 | 0.117 | no verdict (spread) | 2.47 | 3,210 |

Second takes, for the rows measured twice: `malloc` 255.651 (spread 0.340,
`host_load` 4.84 rising to ~20 mid-row), `signal` 23.528 (0.363, 8.00),
`file` 17.951 (0.222, 6.60), `tlb` 18.614 (0.283, 13.78), `branch` 1.185
(0.191, 2.43), `compute` 1.091 (0.207, 13.71). **Every row that was taken twice
reproduced its ratio to within about 10%**, which is the strongest available
evidence that the ordering below is real even though only two rows earn verdicts.

**Total time lost fell from about 126 seconds to about 101.5 seconds.** `malloc`
and `signal` together are 82.9 of the 101.5 seconds, or **82%**, so the
concentration is unchanged. The single largest remaining gap in the tree is
still `malloc`, by a factor of 7 over the next row.

**The ordering method continues to earn its keep, and mis-ranks worse than ever
by ratio.** `pipe` at 2.99x loses more than `mmap` at 7.4x, more than `syscall`
at 5.3x, and more than `calls` and `memory` at ~10.5x each. `branch` at 1.15x and
`compute` at 1.06x are last by both measures, `compute` losing 3.2 ms against
`malloc`'s 72.5 s — a factor of about 22,600.

### Before/after for the three rows the lane existed to check

The "before" column is the committed BENCH-MAP-3 row measured at `d6861edf3`.
**All three claims are directionally confirmed. Two are smaller than briefed.**

| workload | metric | BENCH-MAP-3 | briefed | BENCH-MAP-4 measured | verdict on the claim |
|---|---|---:|---:|---:|---|
| malloc | `rust_x_c` | 280.913 | 218.6 | **230.7 / 255.7** | confirmed, **smaller than briefed** |
| signal | `rust_x_c` | 58.727 | ~24.9 | **21.8 / 23.5** | **confirmed, slightly better than briefed** |
| syscall | `rust_x_c` | 5.733 | 4.958 | **5.347** | confirmed, **about half the briefed gain** |

**`malloc`: real but overstated.** Both takes (230.7 and 255.7) sit above the
briefed 218.6 and well below BENCH-MAP-3's 280.9. The honest statement is a
**1.10x–1.22x improvement**, not the 1.29x the briefed figure implies. Neither
take earns a verdict and both were contended, so 218.6 is not refuted — it is
simply not what this lane measured, twice. Its counters are unchanged from
BENCH-MAP-3 (`runs` 6,437,891, `builds` 165,579, `completed` 3,853,984,500),
which is the expected signature of a **scheduler and protocol** fix: the same
work, dispatched more cheaply.

**`signal`: confirmed, and the largest proportional win in the map.** 58.727 →
21.8–23.5 is a **2.5x–2.7x improvement**, slightly better than the briefed ~24.9.
Counters corroborate the mechanism rather than the volume: `builds` is 3,641 and
`completed` 19,800,525, both unchanged from BENCH-MAP-3, so the engine is doing
identical work — the saving is the 4,096-entry translated-block cache no longer
being cleared per delivery (`2a565c834`, `c92eae79c`).

**`syscall`: confirmed but half-sized.** 5.733 → 5.347 is a 1.07x improvement,
against the briefed 4.958 (1.16x). `services` is 10,830,786 and `completed`
9,500,030, both unchanged. The row's spread (0.287) is the second-widest in the
map, so a tighter re-take could plausibly land nearer 4.958; on this evidence it
does not.

**`calls`, `memory`, `branch`, `compute` all held their verdicts**, as briefed:
`calls` 10.592 → **10.372** (pass), `memory` 10.871 → **10.606** (pass),
`branch` 1.053 → **1.146**, `compute` 1.014 → **1.062**. The two `branch` and
`compute` figures drifted up by ~9% and ~5%, both without verdicts and both on a
contended box; they are consistent with no change.

**`tlb` improved without being a target**, 20.930 → **17.554**, consistent with
the per-boundary `getrusage`+`clock_gettime` pair having been gated away. Its
counters confirm the standing **adversarial-by-construction** attribution
exactly: `a64_dirty_merged` is **0** while `a64_dirty_overflow` is **1,591,601**,
i.e. every one of the 1.59 M runs fills all 16 dirty slots and merges nothing,
because the guest strides 4099 pages. `tlb` is rank 3 by time lost, but it is
measuring a case the guest was written to make worst.

### The one regression this map found

**`file` roughly tripled against C and is now rank 4.** BENCH-MAP-3 recorded
6.387; this map measures **16.045 and 17.951** across two independent takes 40
minutes apart, at `host_load` 2.64 and 6.60.

The attribution is unusually clean, and it is not contention:

* **The C and native arms did not move.** `c_us` 324,170 → 257,339/318,149 and
  `native_us` 209,184 → 168,483/241,409 — both flat within noise, on the same
  box, in the same runs. Only the Rust arm changed: `rust_us` 2,070,393 →
  **4,128,889 / 5,711,077**. A filesystem or I/O-contention explanation would
  have moved all three arms.
* **The counters are byte-identical to BENCH-MAP-3.** `runs=3`, `builds=9`,
  `hits=21`, `fallbacks=3`, `sites=3`, `services=866,490`, `completed=48`. The
  engine is translating and executing exactly the same amount of code and
  performing exactly the same number of syscall services as before.

Same work, same translations, twice the wall time, Rust arm only. `file` is
almost pure syscall-service cost (`services` 866,490 against `completed` 48), so
this points at the **host-side buffered read/write service path**, not at
translation. **This is unowned and worth a lane.** It is the only row in four
maps to have gone backwards.

### Native counters per workload (arm64, this head)

Counters are load-independent, so unlike the timings they are exact.

| workload | runs | builds | hits | fallbacks | sites | services | completed | dirty_committed | dirty_merged | dirty_overflow | reloc_invalid |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| malloc | 6,437,891 | 165,579 | 12,233,480 | 6 | 6 | 14 | 3,853,984,500 | 337,603,368 | 228,159,307 | 6,437,885 | 0 |
| signal | 3,300,008 | 3,641 | 3,306,696 | 3 | 3 | 4,400,022 | 19,800,525 | 1,100,058 | 31 | 1 | 0 |
| tlb | 1,591,616 | 38 | 1,591,681 | 12 | 6 | 20 | 229,183,481 | 27,057,256 | **0** | 1,591,601 | 0 |
| file | 3 | 9 | 21 | 3 | 3 | 866,490 | 48 | 0 | 0 | 0 | 0 |
| pipe | 4 | 14 | 31 | 4 | 3 | 3,249,259 | 149 | 7 | 3 | 0 | 0 |
| mmap | 7,504 | 30,007 | 67,519 | 5 | 5 | 487,405 | 180,056 | 7,499 | 0 | 0 | 0 |
| syscall | 500,002 | 9 | 500,021 | 3 | 3 | 10,830,786 | 9,500,030 | 0 | 0 | 0 | 0 |
| calls | 8,693 | 20,560 | 126,578 | 3,006 | 6 | 14 | 2,915,326,810 | 238,906,982 | 238,854,847 | 0 | 0 |
| memory | 1,129 | 144 | 36,205 | 4 | 4 | 22 | 1,175,973,419 | 288,521,929 | 288,518,517 | 0 | 0 |
| branch | 1,280 | 16 | 40,749 | 4 | 4 | 14 | 1,338,907,323 | 0 | 0 | 0 | 0 |
| compute | 364 | 17 | 11,576 | 4 | 4 | 14 | 377,955,317 | 7 | 3 | 0 | 0 |

Three attributions worth carrying forward:

**Nothing regressed in the counters.** `relocation_invalidations` is still 0 on
all eleven rows, and `builds` is flat or lower everywhere against BENCH-MAP-3.
The four fixes in this window were scheduler, protocol and cache-lifetime
changes; none of them changed what gets translated, and the counters say so.

**`tlb`'s adversarial signature is now explicit.** `dirty_merged=0` against
`dirty_overflow=1,591,601` is the whole attribution in two numbers, and it is
load-independent. Any future `tlb` work should change the guest's stride or the
slot count, not the dispatcher.

**`signal`'s `services` doubled, 3,300,020 → 4,400,022, while its time fell by
2.6x.** More syscall services for less time is the expected shape of removing a
fixed per-delivery memset, and is further evidence the win is real.

### First measured sqlite figure, and the six phases `--workload` rejects

**`sqlite` has never been measured in this file before.** All three previous maps
record it as "guarded by `#ifdef HL_BENCH_SQLITE` and not compiled in". It was
compiled here against `/usr/lib/aarch64-linux-gnu/libsqlite3.a`. **A figure of
"375x C / 5,981x native" was carried into this lane's brief as a prior
measurement; it appears nowhere in this file or its history, and should be
treated as unsourced.** The numbers below are the first committed measurement.

`sqlite` is not a `--workload` value. It, and the six phases `--workload`
rejects, are reachable only through `--workload full`. `full` at `--divisor 1`
is not affordable (it runs all 17 phases including the 12 M-iteration `malloc`),
so this was taken at **`--divisor 20`**, `--repeats 9`, `host_load` 14.70.
**No phase earned a verdict** and all are indicative. Ratios at divisor 20 are
not directly comparable to the divisor-1 table above — warm-up amortises over
20x fewer iterations, which inflates translation-heavy rows (`malloc` reads
300.4 here against 230.7 at divisor 1).

| phase | native_us | c_us | rust_us | rust_x_c | rust_x_native | spread | reachable via `--workload`? |
|---|---:|---:|---:|---:|---:|---:|---|
| **sqlite** | 2,462 | 6,417 | 3,695,756 | **575.932** | **1501.119** | 0.267 | no — `full` only, needs `-DHL_BENCH_SQLITE` |
| malloc | 10,062 | 16,171 | 4,857,761 | 300.400 | 482.783 | 0.205 | yes |
| mmap | 19,166 | 25,810 | 680,741 | 26.375 | 35.518 | 0.263 | yes |
| float_simd | 6,655 | 7,922 | 196,217 | **24.769** | 29.484 | 0.343 | **no — `full` only** |
| signal | 28,714 | 23,961 | 583,487 | 24.352 | 20.321 | 0.202 | yes |
| file | 11,131 | 14,088 | 263,759 | 18.722 | 23.696 | 0.336 | yes |
| tlb | 17,729 | 21,184 | 321,663 | 15.184 | 18.143 | 0.198 | yes |
| calls | 5,286 | 8,063 | 109,053 | 13.525 | 20.631 | 0.195 | yes |
| memory | 7,074 | 7,706 | 92,128 | 11.955 | 13.023 | 0.122 | yes |
| string | 10,148 | 11,810 | 138,408 | **11.720** | 13.639 | 0.207 | **no — `full` only** |
| atomics | 10,309 | 9,466 | 62,480 | **6.600** | 6.061 | 0.252 | **no — `full` only** |
| syscall | 45,254 | 25,522 | 137,140 | 5.373 | 3.030 | 0.217 | yes |
| pipe | 18,688 | 81,777 | 269,202 | 3.292 | 14.405 | 0.259 | yes |
| compute_cold | 2,458 | 2,548 | 3,983 | **1.563** | 1.620 | 0.490 | **no — `full` only** |
| crypto | 40,427 | 40,514 | 46,766 | **1.154** | 1.157 | 0.260 | **no — `full` only** |
| compute | 2,399 | 2,576 | 2,976 | 1.155 | 1.241 | 0.310 | yes |
| branch | 18,347 | 19,751 | 22,017 | 1.115 | 1.200 | 0.117 | yes |
| intdiv | 17,028 | 17,022 | 17,840 | **1.048** | 1.048 | 0.327 | **no — `full` only** |

**`sqlite` is by a wide margin the worst ratio in the tree — 576x C — and it is
the most realistic code in it.** It is 1.9x worse than `malloc` measured in the
same run under identical conditions. It is also the row with the largest
native-to-C gap (native 2,462 µs against C 6,417 µs), so the 1501x native figure
overstates the engine's deficit relative to a fair baseline; **576x C is the
number to quote.** A real database workload is a mix of pointer chasing,
branchy B-tree descent and small allocations — the three things `malloc`,
`calls` and `branch` measure separately — and it is worse than any of them
individually. **This should be the next lane's target, ahead of `malloc`.**

**Two of the six previously-unreachable phases are material.** `float_simd` at
**24.8x** would rank between `tlb` and `file` in the main table, and `string` at
**11.7x** would rank alongside `calls` and `memory`. Neither has ever appeared in
a ranked table. The other four are benign: `intdiv` 1.05x, `crypto` 1.15x,
`compute_cold` 1.56x and `atomics` 6.6x. **`crypto` at 1.15x is worth noting as a
positive** — the AES/SHA paths are essentially at parity with the C engine.

The prior lane's `full` figures (`malloc 465.7, signal 167.8, calls 89.4,
string 29.5, compute 3.4` at `host_load` 13.64) predate every fix in the last two
maps; against this run they are superseded on all five rows.

### Real container workloads, re-taken — and re-interpreted

Re-taken at this head with `--jobs 1`. All five cases passed their output
contracts. Both definitions run with `native: true, diagnostics: false`, so they
carry timings but **no native counters**.

| benchmark | case | cold_ms | warm median_ms | warm min_ms | p90_ms | materialize_us |
|---|---|---:|---:|---:|---:|---:|
| interp-python | startup | **8,679** | 3,458 | 3,443 | 3,554 | 1,247,070 |
| interp-python | import | **41,083** | 6,192 | 5,954 | 6,336 | 4,505,753 |
| interp-python | work | **104,818** | 102,486 | 78,104 | 229,632 | 1,047,723 |
| fs-churn | spawn (500 fork/exec) | **8,255** | 7,212 | 6,831 | 7,822 | 505,059 |
| fs-churn | unpack (2,000 files) | **11,246** | 9,180 | 8,922 | 9,219 | 163,887 |

**Read the cold column, not the warm one.** This inverts how the previous three
maps presented these rows, and the reason is a `__pycache__` artefact of the
harness rather than anything about the engine. `run_case` materializes **one**
rootfs and all five reps share it as `Rootfs::Directory`, so guest writes persist
across reps. `python:3.12-alpine` ships its stdlib with an **empty
`__pycache__`**; rep 0 compiles the imports and writes `.pyc`, and reps 1–4 read
them back. A sibling lane proved this by mutation: running `python -B` (never
write `.pyc`) collapses warm to cold's level — 6,098 → 45,816 ms and 8,627 →
38,850 ms across two alternating pairs — while cold is unchanged, because cold
never had a cache. **A real container, with a fresh rootfs and an empty
`__pycache__`, pays the ~40 s figure on every run.** The 6.2 s warm median is
reachable only because five reps share one mutated rootfs. The warm medians here
measure **a pre-warmed guest bytecode cache, not steady-state execution.**

**The container-setup hypothesis is dead, and this map's own numbers close it.**
`COLD_LIFECYCLE` instrumentation already existed and had simply never been read.
On `interp-python/import`: `create_us=165`, `attach_us=368`, `start_us=1,335`,
`output_read_us=226` — **about 2.1 ms of setup in total** — against
`wait_and_drain_us=41,081,914`. Setup is **0.005% of cold**. The same shape holds
on `startup` (1.48 ms against 8.68 s) and `work` (1.36 ms against 104.8 s).
**100% of the excess is inside guest execution.** No amount of container,
namespace or descriptor work will move these rows.

**Materialization is 0% of `cold_ms`.** It happens once, *before* rep 0, and is
not inside the measured interval — the `materialize_us` column above is reported
for completeness and **must not be divided into `cold_ms`**. An earlier
circulated figure of "~8% of cold" was arithmetic on these two columns and is
wrong. I/O is eliminated as a factor too: the whole 53 MB rootfs reads in 148 ms
and is page-cache resident by the time the guest starts.

**The `startup` improvement from the statx fix is confirmed.** `7b4513b83`
("answer the mapped-file EOF boundary from a cached length") landed 90 seconds
*after* BENCH-MAP-3 was taken, so that map's `interp-python` rows are pre-fix
baselines. A sibling lane A/B'd it at this head against a no-cache mutant:
**18,165,781 → 16,659 statx calls per python startup (1090x)**, warm median
4.45 → 3.47 s, cold 10.19 → 8.53 s. This lane's independent re-take measures
**3,458 ms warm and 8,679 ms cold**, within 1% and 2% of those post-fix
predictions. Against the *committed* BENCH-MAP-3 row (3,844 ms warm) the delta is
only −386 ms, because that row was taken under different load; the mutant A/B is
the sounder comparison and the fix is real.

**`interp-python/work` still cannot support a number.** Its three samples span
min 78,104 ms, median 102,486 ms and p90 229,632 ms — a **2.9x span**. A prior
baseline spanned min 51,938 ms against a median of 102,776 ms. **This case cannot
resolve a small effect and no conclusion should be drawn from a change in it**
until it gets more samples or a tighter workload.

**`fs-churn`'s "faster cold than warm" result did not reproduce.** BENCH-MAP-3
recorded `spawn` 6,107 cold against 6,487 warm and `unpack` 9,580 against 9,824,
and concluded there was no measurable first-run cost left. This map measures
`spawn` **8,255 cold against 7,212 warm** and `unpack` **11,246 against 9,180** —
a real cold penalty of 14% and 23% in both cases. Given the `__pycache__` finding
above, the honest reading is that **any warm-versus-cold delta on a shared-rootfs
bench is a statement about guest-visible state carried between reps**, not about
husklet's first-run cost, in whichever direction it points.

### The inflation control: what `combined` overstates

**`combined/main.c` overstates translation work by roughly three orders of
magnitude relative to a real container, and the ranked table must not be read as
if it predicted product behaviour.** Carried forward from the calibration lane
and *not* re-measured here: a real dotnet container run is about **86% image
materialization and 4.2% guest execution**, with roughly **2,100 translations for
the whole run against `combined/main.c`'s 9,591**.

**The 86% materialization figure is a property of that one dotnet workload and
does not generalize — state it that way, never as a general property.** On every
case in this map's container table, materialization happens outside the measured
interval entirely (0% of `cold_ms`), and guest execution is essentially 100% of
it. The materialization share is image size divided by run length, and
`alpine:3.20` or `python:3.12-alpine` with a ten-second workload sits at the
opposite end of that ratio from a large dotnet image with a short one. Both
numbers are correct about their own workload; neither generalises.

**Only 11% of `malloc`'s time runs guest code** — `hl-engine` is 80.4% of samples
— so that row measures **slice-dispatch machinery, not translation quality**, and
it produces 9,591 translations against a real dotnet container's ~2,100. **Do not
quote `malloc`'s 230x for container workloads.** The `sqlite` row is the better
proxy for realistic code, and it is worse.

Materialization here is measured on a filesystem **without reflink support**, so
the `materialize_us` column is an upper bound. **That bound has now been measured
and it does not matter.** On btrfs, `fs-churn/unpack` materialization falls 30%,
saving 31 ms — against a row that improves by 902 ms. Reflink is **3.4%** of the
gain; the other 871 ms is the guest's own file I/O against the cache filesystem.
Materialization is ~1% of that row either way.

The mechanism is the between-case split on identical infrastructure: `spawn` (500
fork/execs, little file I/O) moves −2.5%, `unpack` (2,000 file create/read/delete)
moves −9.2%. The effect tracks guest file I/O volume, because the materialized
rootfs — including guest `/tmp` — lives on the cache filesystem.

### Not measured, and why

* **`interp-python` with `diagnostics: true` aborts** with
  `*** stack smashing detected ***` (exit 134). This is a **real latent bug and
  currently unowned.** Its practical effect on this page is that the `hl-native:`
  counter route is **unavailable for every container row**, which is why the
  container table carries timings only. Fixing it would let the container rows be
  attributed the way the `combined` rows are.
* **`malloc`, amd64** — still not attempted; the gate's 600-second internal
  deadline fires before the phase completes under the x86 interpreter at
  `--divisor 1`. **Restated, not resolved**, for the fourth map running.
* **All amd64 timings** — not attempted. `native_baseline` is absent on amd64
  because the host is aarch64, so only `rust_x_c` would be meaningful there, and
  any "native" figure for amd64 comes from binfmt and **must never be quoted as a
  baseline**.
* **`+lse`** — measured and **rejected** by a sibling lane; it never landed.
  **Do not record any ~6.5% figure against it.**
* **A verdict on the nine unverdicted rows** — not obtainable in this session.
  The box was never quiet for long enough, and per the standing rule
  `--max-spread` was not relaxed and repeats were not raised to compensate.

### Rerun note

The ordering, the counter table, the `file` regression, the first `sqlite`
figure and the `__pycache__` re-interpretation of the container rows do not
depend on a quiet box and are this map's durable output. What a quiet box would
buy is verdicts on `tlb` (0.064), `signal` (0.139), `file` (0.147), `pipe`
(0.149) and `mmap` (0.150) — all five are within 3x of the 0.05 threshold and
would plausibly convert.

Three operational notes that cost this lane real time:

* **`/target/` in `.gitignore` is directory-only, so symlinking `target` itself
  leaves the tree `-dirty` and stamps `-dirty` into every `rust_revision`.**
  Make `target` a real directory and symlink `target/release` into it instead.
* **`--workload full --divisor 1` is not affordable** and `full` is the only
  route to `sqlite` and the six rejected phases. Use `--divisor 20` and say so.
* **This host is an LXC container in a VM**, so `/proc/loadavg` cannot see
  macOS-side contention. Treat any "the box is idle" instruction as unverifiable
  and record `host_load` per row regardless.

## 2026-08-08: BENCH-MAP-3 re-take after the dispatcher and trace-budget fixes

Head `d6861edf3` on `refactor/merge-rust-engine`, release build, Linux ARM64
host, 18 logical CPUs, `CARGO_TARGET_DIR` under `/var/tmp`, lane-private scratch.
Every row used the benchmark gate at `--repeats 9 --divisor 1 --max-spread 0.05`
against the retained C build `/Users/x/dd/engine/build/unit-audit`
(`c_build_id sha256:9c2701b3…`, unchanged from both previous maps). Rust engine
`sha256:45b5be6a7482…`, revision `d6861edf3704` (clean, not `-dirty`).

**The guest is byte-identical to the previous two maps'.**
`git log a1cf286dd..d6861edf3 -- tests/bench/combined/main.c` is empty, so every
number below is engine behaviour, not a changed guest. The CPU pin is now
per-PID rather than fixed to the highest core, so rows in this sweep ran on
CPUs 3, 5, 6, 7, 8, 9, 10, 12 and 17 rather than stacking on 17.

**Completion was verified per arm, not by exit code.** All eleven rows recorded
9 sample files for each of `native`, `c-engine` and `rust-engine`, and the
per-arm `ok=` checksum in `cycle-NNN-<env>-arm64.csv` is identical across all
three arms on every row except `syscall`, whose checksum embeds the guest TID and
is intentionally not compared. No row in this table came from an aborted
baseline. The gate now enforces this cross-arm itself (`f419cf905`).

**Measurement caveat — read before using the absolute numbers.** The box was
contended for most of the sweep by four sibling lanes, and per-row `host_load`
ranged 4.19 to 10.20 out of 18. It fell to ~2.8 only for the final `malloc` row.
Per the lane rule, `--max-spread` was **not** relaxed and repeats were not
raised. Unlike the two previous maps, which returned **no verdict on any row**,
this sweep returned a **pass verdict on three rows** (`branch`, `memory`,
`calls`); `malloc` and `pipe` missed only narrowly, at spread 0.063 and 0.058.
Rows without a verdict are **indicative**: treat `rust_x_c` and the ordering as
sound and the absolute microseconds as inflated run to run.

Ranked by absolute time lost against C (`rust_us - c_us`), which is the ordering
the optimisation lanes should prioritise by — not by ratio.

| rank | workload | native_us | c_us | rust_us | rust_x_c | rust_x_native | spread | verdict | host_load | lost vs C (us) |
|---:|---|---:|---:|---:|---:|---:|---:|---|---:|---:|
| 1 | malloc | 186,523 | 292,343 | 82,123,062 | 280.913 | 440.284 | 0.063 | no verdict (spread) | 9.51 | 81,830,719 |
| 2 | signal | 550,266 | 454,367 | 26,683,451 | 58.727 | 48.492 | 0.162 | no verdict (spread) | 10.20 | 26,229,084 |
| 3 | tlb | 270,770 | 253,374 | 5,303,049 | 20.930 | 19.585 | 0.123 | no verdict (spread) | 8.33 | 5,049,675 |
| 4 | mmap | 340,850 | 413,770 | 3,784,190 | 9.146 | 11.102 | 0.326 | no verdict (spread) | 6.69 | 3,370,420 |
| 5 | pipe | 344,855 | 1,471,969 | 4,275,257 | 2.904 | 12.397 | 0.058 | no verdict (spread) | 5.21 | 2,803,288 |
| 6 | syscall | 920,802 | 448,661 | 2,572,251 | 5.733 | 2.793 | 0.180 | no verdict (spread) | 5.09 | 2,123,590 |
| 7 | file | 209,184 | 324,170 | 2,070,393 | 6.387 | 9.897 | 0.323 | no verdict (spread) | 4.19 | 1,746,223 |
| 8 | calls | 94,873 | 145,610 | 1,542,359 | 10.592 | 16.257 | 0.038 | **pass** | 8.31 | 1,396,749 |
| 9 | memory | 135,266 | 136,378 | 1,482,602 | 10.871 | 10.961 | 0.046 | **pass** | 4.67 | 1,346,224 |
| 10 | branch | 335,459 | 356,554 | 375,365 | 1.053 | 1.119 | 0.029 | **pass** | 4.88 | 18,811 |
| 11 | compute | 49,118 | 53,323 | 54,056 | 1.014 | 1.101 | 0.140 | no verdict (spread) | 5.05 | 733 |

**Total time lost fell from about 382 seconds to about 126 seconds — a 3.0x
reduction across the board.** `malloc` and `signal` together are 108 of the 126
seconds, or 86%, so the concentration of the problem is unchanged even though its
size is not. The whole eleven-row sweep now completes in 25 minutes; the previous
map needed 48 minutes for `malloc` alone.

**The ordering method continues to earn its keep.** `compute` is still last, now
losing 0.7 ms against C while `malloc` loses 81.8 s — a factor of about 111,000.
Ranking by ratio would still mis-order the middle: `pipe` at 2.9x loses twice
what `memory` at 10.9x does and twice what `calls` at 10.6x does, and `mmap` at
9.1x loses more than `syscall`, `file`, `calls` and `memory` combined.

### Before/after for the two rows the lane existed to check

Both claims are **confirmed**, and both are larger than the recorded map showed.
The "before" column is the committed BENCH-MAP-2 row measured at `a1cf286dd`.

| workload | metric | BENCH-MAP-2 | BENCH-MAP-3 | change |
|---|---|---:|---:|---|
| malloc | `rust_x_c` | 536.416 | **280.913** | 1.91x better |
| malloc | `rust_us` | 263,421,862 | **82,123,062** | 3.21x faster |
| malloc | `runs` | 824 | **6,437,891** | 7,812x |
| malloc | `completed` | 506,533 | **3,853,984,494** | 7,608x |
| malloc | `fallbacks` | 824 | **6** | 137x fewer |
| malloc | `sites` | 257 | **6** | 43x fewer |
| malloc | suppressed-entry sites | 24 | **1** | — |
| calls | `rust_x_c` | 56.414 | **10.592** | 5.33x better |
| calls | `rust_us` | 9,920,061 | **1,542,359** | 6.43x faster |
| calls | `builds` | 250,071 | **20,560** | 12.2x fewer |
| calls | `relocation_invalidations` | (1,038,222 reported) | **0** | eliminated |
| calls | `completed` | 2,913,647,524 | 2,915,326,804 | flat |

**`malloc`: the recorded conclusion is overturned.** BENCH-MAP-2 concluded the
wall time "did not move" and, with `native_us` above `c_us`, that native was
intrinsically slower than the interpreter on this workload. That conclusion was
an artefact of native not running. With `HL_NATIVE_EXIT_EPOCH` no longer
rewritten as `FALLBACK` (`18811d6ef`, `bf41b444a`), `runs` goes from 824 to
6.4 million and `completed` from half a million to 3.85 billion — the engine is
now executing natively at all — and `native_us` (186,523) is now comfortably
below `c_us` (292,343), which is the opposite of the recorded relationship.
`malloc` remains rank 1 by a wide margin, but it is no longer evidence against
native execution.

**`calls`: confirmed, and the fix is broader than `calls`.** Sizing a cached
trace by the budget left in its slice (`659c7e126`) was invalidating traces,
their relocations and the whole IBTC whenever a wider entry arrived.
`relocation_invalidations` is now **0 on every one of the eleven workloads**, not
only on `calls`, and `calls` earns a gate verdict at spread 0.038 — the tightest
row in the sweep. `completed` is flat, confirming this was build churn and not a
change in work done.

### Rows that moved without being the lane's target

| workload | `rust_x_c` before | after | note |
|---|---:|---:|---|
| compute | 3.585 | **1.014** | now within 1.4% of the retained C engine |
| branch | 2.218 | **1.053** | passing verdict, spread 0.029 |
| file | 18.394 | **6.387** | 2.88x better |
| signal | 130.832 | **58.727** | `builds` 6,600,042 → 3,641; `fills` 2,199,965 → 1,195 |
| tlb | 33.576 | **20.930** | `runs` 28 → 1,590,747; `completed` 1,432 → 229,077,823 |
| memory | 13.509 | **10.871** | `builds` 1,527 → 76 |
| pipe | 3.089 | **2.904** | little changed |
| syscall | 5.439 | **5.733** | **the one row that got worse** |
| mmap | (no row) | **9.146** | first measurement ever taken; see below |

**`signal`'s BENCH-MAP-2 verdict is itself overturned.** That map recorded the
signal fix as "claim refuted, and worse", with `builds` rising to 6,600,042 and
`fills` to 2,199,965. Both counters have now collapsed to 3,641 and 1,195 — close
to the originally claimed 42 and 10 — and `completed` is flat at 19.8 million.
The restored inline syscall service is visible as `services` 17 → 3,300,020.
`signal` is still rank 2 and still the second-largest gap on the board.

**`syscall` is the only regression** (5.439 → 5.733, and it was 5.720 in the
first map). It is also the row where the retained C engine beats host-native
execution (448,661us against 920,802us), so its `rust_x_native` of 2.79
understates the real gap; use `rust_x_c`. Its diagnostics show a single
suppressed-entry site refusing 10,500,001 times. **That refusal has since been
measured and is correct**; the remaining gap is not a native-admission defect.

The site is `0x4164e8`, the instruction after `svc #0x0` in libc's `syscall()`
wrapper — the post-syscall resume point, the same shape as the `__syscall_cancel`
entry that `33517a682` fixed on x86. Isolating the phase (`--phase syscall`)
attributes it exactly: the harness calls `phase_syscall` with `iters/20 + 1 =
500,001` per warm call, and `runs` is 500,001 with `syscall` boundary exits
499,999. The entry runs natively for one whole warm call, then the return into
`run_phase`'s accumulator (`ldr x1, [x20, #2088]` at `0x402378`) raises a guard-read
fallback with `executed=17` against `budget=65536`, so `fallback_suppresses`
latches it; `500,001 + 10,000,000 = 10,500,001` is then every subsequent arrival.

Admitting it anyway is 2.9x slower. Letting an entry survive a fallback raised at
a block it merely chained into lifts `runs` 500,001 → 11,000,003 and `completed`
9,500,022 → 209,000,036 on the phase (and, on the full guest, `runs` 11.9M → 25.8M,
`completed` 25.88B → 27.86B with `builds` up only 8% and `services` unchanged) —
yet `rust_x_c` goes **5.213 → 15.224 with both arms earning gate verdicts** (spreads
0.009 and 0.017, `--max-spread` never relaxed), reproduced across two further
alternated rounds at 5.403 → 15.836 and 5.423 → 15.371. Each native run here retires only
19 instructions (`completed/runs`) before the syscall boundary ends it, so 11M
native entries each pay a full native-slice setup for 19 instructions of work,
which the interpreter slice absorbs far more cheaply. `executed * 2 < budget`
measured precisely the right thing. **This row is a counter-improvement that is a
wall-clock regression: lead with counters, but confirm with the gate.**

What is left is the per-syscall round trip, not admission: `services` is 10,830,786
for 10.5M guest `gettid` calls, so the engine leaves native code once per syscall
and pays roughly 231ns against the retained C engine's 43ns. That cost sits in the
scheduler's inline-service path, which is SIGRETURN-COST's territory, not native
suppression's.

This is a second, independent confirmation of the bar `8db3d7595` set on
`dbt-smc-hotpatch`: relaxing `fallback_suppresses` looks like a large counter win on
both cases and is not one. The mechanisms differ — that case ratchets through
successive entries, this one is a single cold fallback at `executed=17` that latches
under either the current rule or the proposed `budget.min(SLICE_BUDGET)` — but the
conclusion is the same, so the rule is left alone.

### Native counters per workload (arm64, this head)

Counters are load-independent, so unlike the timings they are exact.

| workload | runs | builds | hits | fallbacks | sites | services | fills | completed | a64_guard_fast | a64_guard_full | reloc_invalid |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| malloc | 6,437,891 | 165,579 | 12,233,480 | 6 | 6 | 14 | 17,838 | 3,853,984,494 | 146,504,775 | 985,644,326 | 0 |
| signal | 3,300,008 | 3,641 | 3,306,696 | 3 | 3 | 3,300,020 | 1,195 | 19,800,519 | 13 | 2,200,142 | 0 |
| tlb | 1,590,747 | 46 | 1,590,811 | 22 | 10 | 20 | 0 | 229,077,823 | 1,590,722 | 55,675,327 | 0 |
| mmap | 1 | 1 | 2 | 1 | 1 | 487,405 | 0 | 2 | 0 | 0 | 0 |
| pipe | 4 | 14 | 31 | 4 | 3 | 3,249,259 | 3 | 143 | 0 | 13 | 0 |
| syscall | 500,002 | 9 | 500,021 | 3 | 3 | 10,830,786 | 4 | 9,500,024 | 0 | 4 | 0 |
| file | 3 | 9 | 21 | 3 | 3 | 866,490 | 3 | 42 | 0 | 3 | 0 |
| calls | 8,693 | 20,560 | 126,578 | 3,006 | 6 | 14 | 3,551 | 2,915,326,804 | 62,998 | 477,877,014 | 0 |
| memory | 1,124 | 76 | 36,048 | 1 | 1 | 22 | 11 | 1,175,874,955 | 13,138,535 | 585,563,034 | 0 |
| branch | 1,280 | 16 | 40,749 | 4 | 4 | 14 | 3 | 1,338,907,317 | 0 | 1 | 0 |
| compute | 364 | 17 | 11,576 | 4 | 4 | 14 | 7 | 377,955,311 | 0 | 29 | 0 |

Three attributions worth carrying forward:

**Build churn is broadly gone.** `builds` fell on every row that had it:
`calls` 250,071 → 20,560, `memory` 1,527 → 76, `branch` 435 → 16, `compute`
57 → 17, `signal` 6,600,042 → 3,641. `malloc` is the exception, rising 9,757 →
165,579, but that is because it is now translating code it previously refused to
run at all; its `hits`/`builds` ratio is 74:1.

**The inline syscall service is restored and visible.** `services` was 17 across
the board in the previous map. It is now 10,830,786 on `syscall`, 3,300,020 on
`signal`, 3,249,259 on `pipe`, 866,490 on `file` and 487,405 on `mmap` — every
syscall-shaped workload.

**`tlb` is now genuinely executing natively.** `runs` 28 → 1,590,747 and
`completed` 1,432 → 229,077,823, against a `builds` count that barely moved
(41 → 46). The previous map's `tlb` numbers described an engine that was
translating and then discarding; this one describes an engine that runs.

### Real container workloads, re-taken

These are closer to the product than `combined` is, and they were re-taken at
this head with `--jobs 1` in the sweep's quiet window (`host_load` 2.7 to 3.1).
Both definitions run with `native: true, diagnostics: false`, so they carry
timings but no native counters. All five cases passed their output contracts.

| benchmark | case | cold_ms | warm median_ms | p90_ms | image materialize_us | image identity_us |
|---|---|---:|---:|---:|---:|---:|
| interp-python | startup | 7,959 | **3,844** | 3,874 | 1,531,570 | 1,289 |
| interp-python | import | 43,066 | **6,727** | 6,759 | 3,310,354 | 9,138,247 |
| interp-python | work | 133,925 | **111,920** | 236,629 | 1,430,585 | 1,135 |
| fs-churn | spawn (500 fork/exec) | 6,107 | **6,487** | 6,608 | 321,772 | 5,550,129 |
| fs-churn | unpack (2,000 files) | 9,580 | **9,824** | 9,928 | 107,548 | 1,604 |

Three things this shows that `combined` cannot.

**The cold-start penalty is gone on the file-churn cases.** `fs-churn/spawn` and
`fs-churn/unpack` are *faster* cold than warm (6,107 against 6,487; 9,580 against
9,824), i.e. there is no measurable first-run cost left. That is consistent with
no longer fsyncing every file of a throwaway container tree (`0791af30a`).
`interp-python` still shows a real cold penalty — `import` is 43.1 s cold against
6.7 s warm, a 6.4x ratio — and that first-run cost, not steady-state execution,
is where the remaining container-startup work is.

**The prior lane's "13.50 s → 0.79 s" startup figure was not reproduced.** The
closest analogue here, `interp-python/startup`, measures 3,844 ms warm and
7,959 ms cold. It is not the same case, so this is not a refutation; it is a
statement that no case in the committed suite currently measures 0.79 s, and the
figure should not be quoted against these benchmarks.

**`interp-python/work` is the least stable measurement on this page.** Its p90
(236.6 s) is more than double its median (111.9 s) across only three samples.
Treat it as an order-of-magnitude figure.

### The inflation control: what `combined` overstates

**`combined/main.c` overstates translation work by roughly three orders of
magnitude relative to a real container, and this table must not be read as if it
predicted product behaviour.** The finding this rests on, carried forward from
the calibration lane and *not* re-measured here: a real dotnet container run is
about **86% image materialization and 4.2% guest execution**, with roughly
**2,100 translations for the whole run against `combined/main.c`'s 9,591**.

Two honest qualifications on that control, both visible in the table above:

* **The 86%/4.2% split does not reproduce on these benchmarks, and should not be
  expected to.** Here guest execution dominates: `fs-churn/unpack` spends 9.82 s
  in the guest against 0.11 s materializing, and `interp-python/work` spends
  111.9 s against 1.43 s. The materialization share is a function of image size
  divided by run length, and `alpine:3.20` with a ten-second workload sits at the
  opposite end of that ratio from a large dotnet image with a short one. Both
  numbers are correct about their own workload; neither generalises.
* **Materialization here is measured on a filesystem without reflink support**,
  so the materialize figures above are an upper bound. Measured on btrfs, the
  bound is real and irrelevant: materialization is ~1% of the row, and reflink
  buys 3.4% of the 9.2% that moving the cache off virtiofs is actually worth.
  Retarget `target/testing/images`, which is per-ISA and so fixes both at once —
  not `HL_SCENARIO_IMAGE_CACHE`, which is an exact leaf serving only the ISA it
  names.

The practical consequence is unchanged: the synthetic ranking tells you where the
*engine* is slow, and `malloc` and `signal` are genuinely where that is. It does
not tell you what fraction of a real container run those rows control, and on the
evidence here that fraction is small for short-lived containers and large for
long-running ones.

### Not measured, and why

* **`mmap`, arm64 — resolved, not restated.** The deterministic
  `native execution was requested but native diagnostics are missing` failure,
  observed at `host_load` 4.14 and again at 9.58, **no longer reproduces at this
  head.** `mmap` produced a complete 9/9/9 row with matching `ok=` checksums for
  the first time. The likely fix is the a64 `read_cache` view-publication change
  (`d3860b637`). Its spread (0.326) is the widest in the sweep, so its absolute
  numbers are the least trustworthy of the eleven, but the harness gap is closed.
* **`malloc`, amd64** — still not attempted. The gate's 600-second internal
  deadline firing before the phase completes under the x86 interpreter at
  `--divisor 1` was not re-tested. arm64 `malloc` is now fast enough (about 15
  minutes) that an amd64 attempt is affordable for the first time, but it was not
  spent here. **Restated, not resolved.**
* **All amd64 timings** — not attempted. `native_baseline` is absent on amd64
  because the host is aarch64, so only `rust_x_c` would be meaningful there, and
  any "native" figure for amd64 comes from binfmt and must never be quoted as a
  baseline.
* **`compute_cold`, `intdiv`, `float_simd`, `crypto`, `atomics`, `string`** —
  **still holds, verified directly at this head.** `--workload intdiv` reports
  `[possible values: compute, branch, calls, memory, tlb, malloc, syscall, mmap,
  file, pipe, signal, full]`. The 17-phase guest defines the other six but they
  are reachable only through the guest's own `--phase` flag, so they carry no C
  baseline and no verdict. A prior lane reached them via `--workload full` and
  recorded `malloc 465.7, signal 167.8, calls 89.4, string 29.5, compute 3.4` at
  `host_load` 13.64 with no verdict on any row; those figures predate every fix
  in this map and should not be compared against the table above.
* **`sqlite`** — still guarded by `#ifdef HL_BENCH_SQLITE` and not compiled in.
* **`full`** — supported by the gate but not run here.

### Rerun note

The ordering, the counter table and the two before/after verdicts do not depend
on a quiet box and are this map's durable output. Three rows now carry gate
verdicts, which neither previous map achieved; the remaining eight want a quiet
re-take, and `malloc` at spread 0.063 and `pipe` at 0.058 would likely convert on
a genuinely idle host. Operational notes that cost previous lanes real time and
still apply: the agent scratchpad is shared between lanes, so drive long sweeps
from a lane-private directory with `setsid`; a foreground harness timeout has
reaped an in-flight gate run before; and `pkill -f "testing runtime"` matches
every lane's binary — scope kills to your own target-dir path.

## 2026-08-07: BENCH-MAP-2 re-take after the four counter-verified fixes

> **Superseded by the BENCH-MAP-3 section above.** Its `malloc` conclusion
> ("wall time unmoved", and native intrinsically slower than the interpreter) and
> its `signal` conclusion ("claim refuted, and worse") are both overturned there
> against a byte-identical guest.

Head `a1cf286dd` on `refactor/merge-rust-engine`, release build, Linux ARM64
host, 18 logical CPUs, harness-pinned CPU 17, `CARGO_TARGET_DIR` under
`/var/tmp`. Every row used the benchmark gate at
`--repeats 9 --divisor 1 --max-spread 0.05` against the retained C build
`/Users/x/dd/engine/build/unit-audit` (`c_build_id sha256:9c2701b3…`, unchanged
from the previous map). Rust engine `sha256:fc14eb0762df…`, revision
`a1cf286dd028` (clean, not `-dirty`). The guest is byte-identical to the
previous map's: `git log 3c805f9e5..a1cf286dd -- tests/bench/combined/main.c` is
empty, so every counter change below is engine behaviour, not a changed guest.

**Completion was verified per arm, not by exit code.** All ten rows recorded
9 files / 9 sample rows for each of `native`, `c-engine`, and `rust-engine`, and
the per-arm `ok=` checksum in `cycle-NNN-<env>-arm64.csv` is identical across all
three arms on every row except `syscall`, whose checksum embeds the guest TID and
is intentionally not compared. No row in this table came from an aborted
baseline. (One earlier `malloc` attempt was discarded precisely because it was
killed mid-run at 25 of 27 cycle files; it is not in this table.)

**Measurement caveat — read before using the absolute numbers.** The brief for
this lane was written against a quiet box (load ~1.1) and that window closed
before the sweep could use it. Load was 1.18 at 19:56; by 20:00 sibling lanes had
spun up a `testing runtime --jobs 4` soak suite, two release `rustc` builds, and
a *second* lane running its own `hl-engine --phase malloc` benchmark. All of
those competitors run unpinned (`sched_getaffinity` = 0-17), so they float onto
CPU 17 and contend directly with the pinned providers. Per-row `host_load` ranged
9.36 to 20.30 out of 18. The gate refused a spread verdict on **every** row. Per
the lane rule, `--max-spread` was **not** relaxed to force verdicts, and repeats
were not raised, because raising repeats does not fix contention.

The contention inflates absolute times but largely cancels in the ratios, since
all three arms are measured under the same load. `compute` remains the control
and was measured twice here: `rust_x_c 3.161` at `host_load 25.04` and
`rust_x_c 3.585` at `host_load 9.54`, against a quiet reference of 3.16. **Treat
`rust_x_c` and the ordering as sound; treat the absolute microseconds as
inflated, and treat any single row's absolutes as +/- ~15% run to run.**

Ranked by absolute time lost against C (`rust_us - c_us`), which is the ordering
the optimisation lanes should prioritise by — not by ratio.

| rank | workload | native_us | c_us | rust_us | rust_x_c | rust_x_native | spread | verdict | host_load | lost vs C (us) |
|---:|---|---:|---:|---:|---:|---:|---:|---|---:|---:|
| 1 | malloc | 343,755 | 491,078 | 263,421,862 | 536.416 | 766.307 | 0.302 | no verdict (spread) | 11.17 | 262,930,784 |
| 2 | signal | 691,388 | 631,445 | 82,612,899 | 130.832 | 119.488 | 0.195 | no verdict (spread) | 20.30 | 81,981,454 |
| 3 | tlb | 302,069 | 406,413 | 13,645,712 | 33.576 | 45.174 | 0.393 | no verdict (spread) | 12.72 | 13,239,299 |
| 4 | calls | 121,406 | 175,845 | 9,920,061 | 56.414 | 81.710 | 0.207 | no verdict (spread) | 15.36 | 9,744,216 |
| 5 | file | 256,196 | 367,993 | 6,768,818 | 18.394 | 26.420 | 0.641 | no verdict (spread) | 19.93 | 6,400,825 |
| 6 | pipe | 402,298 | 1,744,668 | 5,388,562 | 3.089 | 13.394 | 0.150 | no verdict (spread) | 11.25 | 3,643,894 |
| 7 | syscall | 948,509 | 484,383 | 2,634,412 | 5.439 | 2.777 | 0.236 | no verdict (spread) | 17.61 | 2,150,029 |
| 8 | memory | 140,979 | 129,519 | 1,749,706 | 13.509 | 12.411 | 0.274 | no verdict (spread) | 12.91 | 1,620,187 |
| 9 | branch | 395,535 | 421,865 | 935,754 | 2.218 | 2.366 | 0.087 | no verdict (spread) | 9.36 | 513,889 |
| 10 | compute | 60,101 | 60,026 | 215,219 | 3.585 | 3.581 | 0.123 | no verdict (spread) | 9.54 | 155,193 |

**The ordering survived the re-take.** Total time lost across the ten rows is
about 382 seconds; `malloc` and `signal` together are 345 of it, or 90%.
`compute` is still rank 10 and still the cheapest workload on the board — it
loses 0.16 s against C while `malloc` loses 263 s, a factor of about 1,690. The
only rank change against the previous map is `calls` and `file` swapping ranks 4
and 5. Ranking by ratio would still mis-order the middle: `pipe` at 3.1x loses
more than `memory` at 13.5x, and `calls` at 56.4x loses less than a third of what
`tlb` at 33.6x does.

### Before/after for the four fixed workloads

This is the gap the lane existed to close: all four fixes were counter-verified
but none was wall-clock verified. Three of the four do not survive wall-clock
measurement, and one does not survive its own counter claim.

| workload | claim | stale map | this map | verdict |
|---|---|---|---|---|
| malloc | fallbacks 757→6, native instructions 24,075→428,094 | `rust_x_c` 532.624 | `rust_x_c` 536.416 | **wall time unmoved** (+0.7%, inside noise) |
| signal | builds 4,400,030→42, fills 1,099,990→10 | builds 4,400,042, fills 1,100,007, `rust_x_c` 96.483 | builds **6,600,042**, fills **2,199,965**, `rust_x_c` 130.832 | **claim refuted, and worse** |
| tlb | completed 418→987, first guarded native execution | completed 180, `rust_x_c` 33.214 | completed 1,432, `rust_x_c` 33.576 | counters improved, **wall time unmoved** |
| compute | 4.02x→3.16x, gate verdict, spread 0.034 | `rust_x_c` 4.020 | `rust_x_c` 3.161 / 3.585 | **ratio confirmed**, but no gate verdict reproduced |

**`malloc`: the wall time did not move — confirm, not refute.** The finding
lane's "14.66 → 14.73 s, don't book the 157 s" is correct in direction and this
map confirms it at gate scale. `rust_x_c` moved 532.624 → 536.416, which is
smaller than the run-to-run variation of the control. The absolute `rust_us` rose
157.9 s → 263.4 s, but that is a load artefact (`host_load` 4.06 then versus
11.17 now) and must not be read as a regression. The counters do show the fix
landing: `completed` went 18,663 → 506,533, a 27x increase in guest instructions
retired natively, and `a64_guard_fast` is now 17,147 where the stale map had a
100% fallback rate. **Native instructions went up 27x and wall time did not
move**, which says the remaining `malloc` cost is not in the instructions that
were newly admitted. Note also that the claimed "fallbacks 757 → 6" conflates two
counters: the top-line `fallbacks` is still 824 against `runs=824`, a 100%
fallback rate exactly as before; it is the detail `a64_guard_fallback` that is 8.
The 263 s prize is still on the board and still unclaimed.

**`signal`: the counter claim does not hold at this head.** The fix
(`a37295b48 perf(a64): stop the per-entry direct-authority narrowing that reset
the cache`) is in the range, but at `a1cf286dd` the gate workload records
`runs=3,300,007 builds=6,600,042 fills=2,199,965` — still exactly two builds per
run, the same rebuild storm the stale map described, and roughly 1.5-2x *more* of
it in absolute terms. `rust_x_c` moved the wrong way, 96.483 → 130.832. Since the
guest is byte-identical, this is engine behaviour. The most likely reading is
that the later direct-authority work on this branch (`454780124`, `51dfd1ecc`,
`a1cf286dd`) re-broke what `a37295b48` fixed; that bisect was not run here and is
the obvious next step. Until it is, **the signal fix should not be booked.**

**`tlb`: counters improved, ratio did not.** `completed` went 180 → 1,432 and the
run now records real guarded native execution (`a64_guard_fast=7`,
`a64_guard_full=249`) where the stale map had none. But `rust_x_c` went
33.214 → 33.576, i.e. unmoved. `runs=28` against `fallbacks=28` is still a 100%
fallback rate, and `a64_fallback_form_memory=24` still classifies every one of
them as the aarch64 memory-guard form. Warming is no longer structurally
impossible, but it is not yet worth any measurable time.

**`compute`: the only fix that shows up in the ratio.** 4.020 → 3.161/3.585. It
is also, still, rank 10 of 10 and the smallest prize on the board. The claimed
`spread 0.034` and gate verdict did not reproduce under this load; the two
observations here spread 0.123 and 0.181.

### Native counters per workload (arm64, this head)

Counters are load-independent, so unlike the timings they are exact.

| workload | runs | builds | hits | fallbacks | sites | completed | fills | a64_guard_fast | a64_guard_full | a64_fallback_form_memory |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| malloc | 824 | 9,757 | 21,042 | 824 | 257 | 506,533 | 1,039 | 17,147 | 131,131 | 8 |
| signal | 3,300,007 | 6,600,042 | 15,400,096 | 3 | 3 | 19,800,288 | 2,199,965 | 1,100,005 | 2,200,075 | 5 |
| tlb | 28 | 41 | 82 | 28 | 13 | 1,432 | 0 | 7 | 249 | 24 |
| calls | 9,290 | 250,071 | 3,806,626 | 3,004 | 5 | 2,913,647,524 | 71,889 | 2 | 477,712,536 | 3,004 |
| file | 2 | 8 | 19 | 2 | 2 | 40 | 3 | 0 | 3 | 2 |
| syscall | 500,001 | 8 | 500,019 | 2 | 2 | 9,500,022 | 4 | 0 | 4 | 2 |
| memory | 1,123 | 1,527 | 23,938 | 0 | 0 | 1,175,797,129 | 292 | 47 | 585,526,360 | 16 |
| pipe | 3 | 13 | 29 | 3 | 2 | 141 | 3 | 0 | 13 | 3 |
| branch | 1,279 | 435 | 21,552 | 3 | 3 | 1,338,821,299 | 3 | 0 | 1 | 2 |
| compute | 363 | 57 | 5,945 | 3 | 3 | 377,955,309 | 7 | 1 | 29 | 2 |

Two attributions that are new since the previous map:

**`malloc` now suppresses entries rather than only falling back.** The run emits
24 distinct `hl-native-suppressed-entry` sites, the top five refusing
15,447 / 15,117 / 12,869 / 12,842 / 12,553 times each. That is the same
suppression signature the `tlb` lane identified and fixed, now visibly dominating
`malloc`. With `builds=9,757` against `runs=824` — about twelve builds per run —
the engine is still translating repeatedly and discarding the result. This, not
the view-hint replay that was just fixed, is where the 263 seconds now sits.

**`calls` regressed and is now rank 4.** `builds` went 159,314 → 250,071 and
`rust_x_c` went 28.753 → 56.414 while `completed` stayed flat at 2.91 billion.
`a64_guard_full=477,712,536` is unchanged, so this is build churn, not guard
cost. Nothing in the four fixes targeted `calls`; it should be checked against
the same direct-authority commits implicated in the `signal` regression.

### Not measured, and why

* **`mmap`, arm64** — still fails, still deterministically, and still not because
  of load: at `host_load=9.58` the gate exits with `testing: native execution was
  requested but native diagnostics are missing`. The previous map saw the
  identical failure at `host_load=4.14`. Two observations 5.4 load apart give the
  same message, so this is confirmed as an ARM64 native-diagnostics harness bug
  and not a contention artefact. It remains the one gate workload with no row.
* **`malloc`, amd64** — not re-attempted. The previous map's finding (the gate's
  600-second internal deadline fires before the phase completes under the x86
  interpreter at `--divisor 1`) was not re-tested here; arm64 `malloc` alone took
  48 minutes of the sweep under load, and an amd64 attempt would have cost a
  600-second timeout to re-learn a known result. **Restated, not resolved.**
* **All amd64 timings** — not attempted, same reasoning as the previous map, and
  additionally `native_baseline` is absent on amd64 because the host is aarch64.
  Only `rust_x_c` would be meaningful there, and any "native" number for amd64
  comes from binfmt and must not be quoted as a baseline.
* **`compute_cold`, `intdiv`, `float_simd`, `crypto`, `atomics`, `string`** —
  **still holds, verified directly at this head.** `--workload intdiv` reports
  `[possible values: compute, branch, calls, memory, tlb, malloc, syscall, mmap,
  file, pipe, signal, full]`. The 17-phase guest defines the other six but they
  are reachable only through the guest's own `--phase` flag, so they carry no C
  baseline or verdict.
* **`sqlite`** — still guarded by `#ifdef HL_BENCH_SQLITE` and not compiled in.
* **`full`** — supported by the gate but not run; at `--divisor 1` `malloc` and
  `signal` alone are about 346 seconds of rust time per repeat, so nine repeats
  cannot finish inside the gate's 600-second deadline.

### Rerun note

The ordering, the counter table, and the four before/after verdicts do not depend
on a quiet box and should be treated as this map's durable output. The absolute
microsecond columns still want a quiet re-take. Two operational hazards cost this
lane real time and are worth recording: the agent scratchpad directory is
**shared between lanes** and a sibling overwrote this lane's driver script
mid-sweep, and a foreground harness timeout reaped an in-flight gate run at 25 of
27 cycle files. Drive long sweeps from a lane-private directory with `setsid`,
and verify per-arm `ok=` counts before trusting any row.

## 2026-08-07: BENCH-MAP full-workload sweep against the retained C engine

> **Superseded for absolutes and for the four fixed rows by the BENCH-MAP-2
> section above.** Its ordering and its counter attributions still stand; its
> `malloc`, `signal`, `tlb`, and `compute` rows predate the fixes that target
> them.

Head `3c805f9e5` on `refactor/merge-rust-engine`, release build, Linux ARM64
host, 18 logical CPUs, harness-pinned CPU 17. Every row used the benchmark gate
at `--repeats 9 --divisor 1 --max-spread 0.05` against the retained C build
`/Users/x/dd/engine/build/unit-audit` (`c_build_id sha256:9c2701b3…`). Both arms
were checked for completion via the guest `PHASE` line and the per-arm `ok=`
checksum recorded in each `cycle-NNN-<env>-<arch>.csv`; no row in this table came
from an aborted baseline.

**Measurement caveat — read before using the absolute numbers.** The host was
never quiet during this sweep. Nine sibling agent worktrees were building and
benchmarking concurrently; a 60-minute wait for 1-minute load average below 2.0
timed out without ever succeeding, and per-row `host_load` ranged from 4.1 to
16.9 out of 18. The gate itself flagged the condition (`verdict suspect —
host_load … is too contended to compare timings against`) and refused a spread
verdict on every row but `pipe`. Per the lane rule, `--max-spread` was **not**
relaxed to force verdicts.

The contention inflates absolute times but largely cancels in the ratios, because
all three arms are measured under the same load. The control is `compute`, whose
quiet reference is `native 46,713us / C 46,944us / rust 185,223us` at spread
0.017: under load its absolutes inflated ~24% (native 58,164us) while
`rust_x_c` moved only from 3.95 to 4.02. **Treat `rust_x_c` and the ordering as
sound; treat the absolute microseconds as roughly 20-25% high.**

Ranked by absolute time lost against C (`rust_us - c_us`), which is the ordering
the optimisation lanes should prioritise by — not by ratio.

| rank | workload | native_us | c_us | rust_us | rust_x_c | rust_x_native | spread | verdict | host_load | lost vs C (us) |
|---:|---|---:|---:|---:|---:|---:|---:|---|---:|---:|
| 1 | malloc | 186,804 | 296,511 | 157,928,996 | 532.624 | 845.426 | 0.218 | no verdict (spread) | 4.06 | 157,632,485 |
| 2 | signal | 552,547 | 471,532 | 45,494,751 | 96.483 | 82.336 | 0.113 | no verdict (spread) | 6.55 | 45,023,219 |
| 3 | tlb | 283,916 | 332,963 | 11,059,064 | 33.214 | 38.952 | 0.381 | no verdict (spread) | 15.06 | 10,726,101 |
| 4 | file | 254,154 | 317,293 | 7,993,451 | 25.193 | 31.451 | 0.312 | no verdict (spread) | 16.57 | 7,676,158 |
| 5 | calls | 94,582 | 143,619 | 4,129,415 | 28.753 | 43.660 | 0.055 | no verdict (spread) | 8.86 | 3,985,796 |
| 6 | pipe | 364,675 | 1,568,764 | 5,067,411 | 3.230 | 13.896 | 0.033 | pass, flagged suspect on load | 9.82 | 3,498,647 |
| 7 | syscall | 1,039,508 | 596,193 | 3,410,347 | 5.720 | 3.281 | 0.170 | no verdict (spread) | 15.22 | 2,814,154 |
| 8 | memory | 123,584 | 120,929 | 1,772,418 | 14.657 | 14.342 | 0.127 | no verdict (spread) | 7.55 | 1,651,489 |
| 9 | branch | 367,188 | 388,069 | 894,279 | 2.304 | 2.435 | 0.238 | no verdict (spread) | 16.90 | 506,210 |
| 10 | compute | 58,164 | 56,431 | 226,878 | 4.020 | 3.901 | 0.131 | no verdict (spread) | 9.52 | 170,447 |

The headline result is that **`compute` is the cheapest measured workload on the
board.** It is where the current optimisation attention sits, and it is last of
ten by absolute time lost. `malloc` alone loses 157 seconds against C — about
925 times what `compute` loses, and more than every other row combined and
doubled. Total time lost across the ten measured rows is about 234 seconds;
`malloc` and `signal` together are 203 of it, or roughly 87%.

Ranking by ratio would have mis-ordered the middle of the board as well: `calls`
has a higher ratio than `file` (28.8x versus 25.2x) but loses half as much wall
time, and `pipe` at only 3.2x loses more than `memory` at 14.7x. The absolute
column is the one to prioritise by.

Note that `malloc` was measured at `host_load=4.06`, the quietest row in the
sweep, so its 532x is not a contention artefact.

`syscall` is the one row where the retained C engine beats host-native execution
(596,193us versus 1,039,508us), so its `rust_x_native` of 3.28 understates the
gap against the real baseline; use `rust_x_c` of 5.72 for that row.

### Native counters per workload

Counters are load-independent, so unlike the timings they are exact and were
harvested successfully under contention. ARM64 `compute` reproduced the known
reference exactly (`runs=363 builds=28 hits=5844 fallbacks=3
completed=377955238`), which validates the pipeline end to end.

| arch | workload | runs | builds | hits | fallbacks | completed |
|---|---|---:|---:|---:|---:|---:|
| arm64 | compute | 363 | 28 | 5,844 | 3 | 377,955,238 |
| arm64 | branch | 1,279 | 140 | 20,785 | 3 | 1,338,821,289 |
| arm64 | calls | 7,861 | 159,314 | 12,833,528 | 3,005 | 2,914,858,750 |
| arm64 | memory | 1,128 | 807 | 141,983 | 4 | 1,175,780,789 |
| arm64 | tlb | 25 | 26 | 55 | 25 | 180 |
| arm64 | malloc | 753 | 3,097 | 6,390 | 753 | 18,663 |
| arm64 | syscall | 500,001 | 12 | 500,027 | 2 | 9,500,012 |
| arm64 | file | 2 | 10 | 23 | 2 | 37 |
| arm64 | pipe | 3 | 21 | 45 | 3 | 93 |
| arm64 | signal | 2,200,007 | 4,400,042 | 9,900,091 | 3 | 12,100,180 |
| amd64 | compute | 6,413 | 221 | 13,059 | 1 | 420,027,117 |
| amd64 | branch | 31,131 | 236 | 58,368 | 1 | 2,039,817,854 |
| amd64 | calls | 58,963 | 293 | 110,949 | 1 | 3,863,692,750 |
| amd64 | memory | 9,113 | 232 | 10,998,898 | 0 | 30,992,937 |
| amd64 | tlb | 3,718 | 128 | 123,770 | 0 | 243,496,642 |
| amd64 | syscall | 11,000,005 | 226 | 11,000,250 | 1 | — |
| amd64 | mmap | 2 | 117 | 141 | 0 | — |
| amd64 | file | 880,019 | 290 | 881,355 | 1 | — |
| amd64 | pipe | 3,300,016 | 287 | 3,300,414 | 0 | — |
| amd64 | signal | 4,400,010 | 259 | 4,400,300 | 1 | — |

Four attributions fall straight out of the counters, and each points a lane at a
different mechanism than the JIT-arena work currently underway on `compute`.

**`malloc` (rank 1) barely executes natively at all.** `runs=753` against
`fallbacks=753` is a 100% fallback rate, and `completed=18,663` means fewer than
nineteen thousand guest instructions retired through the native path across the
entire phase — against 377 million for `compute`. It still pays for 3,097 builds
across 148 sites with `relocation_cold_targets=5,043` and
`a64_dirty_overflow=21`, so the engine is translating repeatedly and then
throwing the result away. This is the single largest prize on the board by two
orders of magnitude and nobody is currently working it.

**`signal` (rank 2) is a rebuild storm, and it is ARM64-only.** ARM64 records
2,200,007 runs against 4,400,042 builds — two full builds per run — with
`fills=1100007`, `ibtc_site_misses=1100007` and
`a64_branch_nonrelocatable=1100007`. Every signal delivery discards and
retranslates. The same phase on amd64 builds 259 times total, so this is a
property of the aarch64 lowering and its IBTC, not of signal handling in general.

**`tlb` (rank 3) never JITs at all.** `runs=25 builds=26 hits=55 fallbacks=25
completed=180` — a 100% fallback rate, split
`a64_fallback_guard_read=10 / a64_fallback_guard_write=15`, all classified
`a64_fallback_form_memory`. The 33x is not slow generated code; it is the
absence of generated code. The equivalent amd64 row translates normally
(`builds=128`, `fallbacks=0`), so again this is aarch64-specific.

`malloc` and `tlb` share a signature — a 100% fallback rate on the aarch64
memory-guard forms — and between them they are 168 of the 234 seconds lost across
the whole board. Whatever rejects those guard forms is the highest-value single
fix this map identifies.

**`calls` (rank 5) is IBTC thrash.** `builds=159,314` and `hits=12,833,528` with
`site_collisions=58,107` against only `sites=5`, plus `a64_guard_full=477,812,329`
and `a64_dirty_merged=238,827,480`. The indirect-branch cache is colliding on a
handful of sites and forcing continuous retranslation. `memory` shows the same
guard signature (`a64_guard_full=585,520,076`) without the build churn.

By contrast `compute` shows `builds=28` against 363 runs and only 3 fallbacks:
translation genuinely is not the cost there, which is consistent with the perf
finding that 97.37% of samples sit inside the JIT arena. That lane is looking in
the right place for `compute` — but `compute` is the smallest prize on the board.

### Not measured, and why

* **`malloc`, amd64 anything** — the gate's own 600-second internal deadline
  fires before the phase completes under the x86 interpreter
  (`testing: timed out after 600s`). Not obtainable at `--divisor 1`; it needs a
  divisor or a longer harness deadline.
* **`mmap`, arm64** — fails deterministically, and not because of load: at
  `host_load=4.14` the gate exits with `testing: native execution was requested
  but native diagnostics are missing`. The amd64 side of the same phase runs
  clean, so this is an ARM64 native-diagnostics gap rather than a benchmark
  problem. It should be treated as a harness bug to fix before this row can be
  mapped.
* **All amd64 timings** — not attempted. A single amd64 counter pass over five
  phases took about 35 minutes; the brief's own note that x86 run-to-run spread
  is ~25% means amd64 needs *more* repeats than arm64, not fewer, and the box was
  never quiet enough for any of it to be admissible. amd64 contributes counters
  only in this checkpoint.
* **`compute_cold`, `intdiv`, `float_simd`, `crypto`, `atomics`, `string`** — the
  17-phase guest defines these, but `testing benchmark gate --workload` accepts
  only `compute, branch, calls, memory, tlb, malloc, syscall, mmap, file, pipe,
  signal, full`. They are reachable through the guest's own `--phase` flag but
  have no gate row, so they carry no C baseline or verdict.
* **`sqlite`** — guarded by `#ifdef HL_BENCH_SQLITE` and not compiled into the
  benchmark binary at all.
* **`full`** — supported by the gate but not run; at `--divisor 1` it is the sum
  of all 17 phases per repeat, and `malloc` and `signal` alone are about 203
  seconds of rust time per repeat, so nine repeats could not finish inside the
  gate's 600-second deadline.

### Rerun note

Nothing in this table carries a clean verdict, so the absolute columns should be
re-taken on a quiet box before they are quoted as a baseline. The ordering and
the counter attributions do not depend on that rerun.

## 2026-08-05: periodic exact-tree checkpoint

Clean detached commit `a94efb2ff42cfb95ffb5258a8f179893d590166d`
was built in release mode and measured on Linux ARM64 CPU 17. The host exposed
18 logical CPUs; admission reported 21 GiB available RAM, 22 GiB free swap,
160 GiB free disk, and load 3.73. Five samples per cell used the same static
ARM64 guest and its internal warmup. The fast cadence uses `--divisor 1000` and
the historically comparable cadence uses `--divisor 20`; each phase is selected
separately. The matrix applied 60- and 120-second row deadlines respectively,
bounded output, fresh process groups, checksum admission, and content-bound
resumable ledgers. No row failed or timed out. Syscall checksums contain the
provider TID and are intentionally not compared.

Rust native selection was supplied only through the typed option set
`HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`, with the supervisor setting
`HL_COMPAT_ENGINE_OPTIONS` to that exact set. Before timing, a one-repeat compute
proof reported `native-verified`, seven native runs, 16 builds, 39,490 hits, two
fallbacks, and 341,397 completed instructions. Timed native rows were quiet only
after that content-bound proof. Interpreter rows supplied no native option.

Times are five-sample medians in microseconds. `C throughput` is
`C median / Rust median * 100`; `C latency gap` is
`(Rust median / C median - 1) * 100`.

| cadence | phase | host | C | Rust native | C throughput | C latency gap | Rust interpreter | C throughput | C latency gap |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fast | compute | 47 | 47 | 46,352 | 0.101% | +98,521.3% | 8,131 | 0.578% | +17,200.0% |
| fast | float SIMD | 124 | 125 | 207,207 | 0.060% | +165,665.6% | 296,800 | 0.042% | +237,340.0% |
| fast | memory | 173 | 169 | 30,908 | 0.547% | +18,188.8% | 114,268 | 0.148% | +67,514.2% |
| fast | syscall | 844 | 849 | 2,513 | 33.784% | +196.0% | 5,511 | 15.406% | +549.1% |
| fast | pipe | 339 | 332 | 5,447 | 6.095% | +1,540.7% | 10,278 | 3.230% | +2,995.8% |
| historical | compute | 2,383 | 2,339 | 1,456,966 | 0.161% | +62,190.1% | 303,323 | 0.771% | +12,868.1% |
| historical | float SIMD | 6,574 | 6,613 | 22,644,923 | 0.029% | +342,330.4% | 13,710,632 | 0.048% | +207,228.5% |
| historical | memory | 5,697 | 6,650 | 4,997,335 | 0.133% | +75,047.9% | 4,925,238 | 0.135% | +73,963.7% |
| historical | syscall | 45,043 | 44,163 | 167,557 | 26.357% | +279.4% | 237,210 | 18.618% | +437.1% |
| historical | pipe | 17,648 | 17,018 | 323,283 | 5.264% | +1,799.7% | 340,751 | 4.994% | +1,902.3% |

The fast native compute samples ranged from 8,696 to 61,459 microseconds, so
its surprising interpreter advantage is an exact observation but not a stable
optimization claim. Float SIMD and memory remain dominated by full guarded
accesses. Historical native proof recorded 87,859,588 full guards for float
SIMD and 28,991,955 for memory, with zero fast guards in both rows. Guest,
Rust engine, matrix runner, and retained-C runner SHA-256 values were
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`,
`15799909895b8b97a0f01c83013a53780fcb496fc47132f0c80942504e225e8c`,
`6d5f8ba128c981e81733473207d11188d1f50145d3bff2bc6c63e2bb68b6761a`,
and `0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`.

The retained audit covered `../engine/tools/bench_runner.c` (`native_build`,
`qemu_build`, `hl_build`, `run_once`, `cmd_run`, `umedian`, and `cmd_report`),
`../engine/tests/perf/combined_bench.c` (`timer_count`, `timer_freq`, each
selected phase, its warmup, and `main`), `../engine/src/core/dispatch.c`
(`run_block`, `block_return`, and `run_guest`), and AArch64
`stubs.c`/`translate.c` (`emit_prologue`, `emit_spill`,
`emit_chain_exit_from`, and `translate_block`). One invocation owns provider
arguments and setup until teardown; every repeat is a fresh child; phase clocks
exclude startup; medians admit only stable phase sets and checksums. CPU state
is engine-local for its lifetime, translated blocks borrow it synchronously,
and slow exits fully publish it before dispatch, signal selection, or teardown.
Direct chains retain live registers; publication and invalidation remain under
the translator lock. The same-ISA AArch64 path is the host branch measured here.

The Rust owners are `benchmark::{matrix,alternating,adapter}` for immutable
content identity, typed options, bounded child lifecycle, resume, parsing, and
median evidence; `src/native/exec` cache, AArch64 trace/stub/fault code for
translated-state lifetime and publication; and `hl-engine` for selected backend
composition. No implementation changed in this checkpoint. The repeatable fast
cadence is the README matrix command with five repeats, 60-second timeout,
`--divisor 1000`, and one selected phase; the historical checkpoint changes
only divisor to 20 and timeout to 120. Preserve the output directory and use
`--resume` only with the exact content identity already recorded there.

### Native versus interpreter attribution

The saved per-repeat CSVs distinguish guest execution from fixture and process
setup. At the fast cadence, native compute spent a 46,352-us median inside the
guest-timed phase and 65,515 us wall-clock, versus 8,131 and 34,212 us for the
interpreter. At the historical cadence the corresponding medians were
1,456,966/1,498,203 us and 303,323/352,166 us. Thus startup contributes to wall
spread but cannot explain the inversion: 71% and 97% of native wall time was
inside the selected compute phase. Native compute's fast samples
(8,696--61,459 us) are much noisier than host and C (both exactly 47 us), while
the longer historical samples narrow to 1,441,567--1,474,217 us. The short row
is too small for a stable regression threshold; the historical inversion is
repeatable.

Compute diagnostics identify the generic control-flow owner. Fast proof made
39,459 native branch transitions for 341,397 completed instructions; historical
proof made 11,962,238 transitions for 18,642,223 instructions, 287 public runs,
53 builds, and only three fallbacks. It had three full guards and no fast guard,
so memory admission cannot explain compute. Retained `dispatch.c::run_guest`
and AArch64 `stubs.c::emit_chain_exit_from` keep registers live across direct
edges and poll at cycle boundaries. Rust `trace.c` terminates certifiable ranges
at every control transfer, while `stub.c` and the executor's AArch64 run loop
own branch completion, budget accounting, interrupt polling, and cache lookup.
The attributable domain is bounded conditional trace chaining and cycle-edge
accounting, not opcode interpretation, setup, or the benchmark fixture.

Float SIMD and memory have a different owner. At divisor 20, float SIMD made
87,859,588 full guards, 29,213,509 dirty publications, 8,844 generated guard
fallbacks, and zero fast guards; memory made 28,991,955 full guards,
14,516,893 dirty publications, 3,628 generated guard fallbacks, and zero fast
guards. Float native was 65% slower than the interpreter; memory was within
1.5%. Their phase shares were 91% and 94% of native wall time, again excluding
startup as the cause. The retained same-ISA translator folds authenticated
memory accesses and preserves publication/fault order through the JIT-owned
mapping lifetime. Rust `guard.c`, `single.c`, `pair.c`, and projection leases
perform per-access range/permission/owner admission and exact dirty archival.
Those full guards and dirty publication are the generic measured owner. The
recorded evidence does not authorize weakening mapping identity, fault
atomicity, executable-write invalidation, or dirty ordering.

No broad rerun is scheduled from this attribution. The next periodic checkpoint
should follow an integrated conditional-trace or authenticated-memory-admission
change, or the normal scheduled cadence. Use the preserved content-bound
artifacts under `/var/tmp/husklet-benchmark-a94efb2ff-artifacts`; do not compare
opcode microtests to these end-to-end phase times.

## 2026-08-04: exact-head float-SIMD attribution

Clean detached commit `fbfe10df8dc21b1c4827580a624da54c67416585`
was built warning-strict in release mode and measured seven times on CPU 17 with
the same ARM64 guest (`--divisor 1000 --phase float_simd`). The guest SHA-256 was
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`;
the retained-C runner was
`0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`,
the Rust engine was
`8c7dd39e3cf3e776f6a22edfeb535b7a39e594f434007ed28b988de6493e8497`,
and the matrix runner was
`7e6a7f251a7358c6fa665e2353e63cac58499bcecc8397fea6d26aca4f8c2f2f`.
Host load at admission was 7.74/5.69/4.64 with 9.2 GiB available memory and
23.9 GiB free swap.

| provider | median us | range us | versus native | checksum |
|---|---:|---:|---:|---:|
| host native | 124 | 123--129 | 1.000x | 2,302,728,000 |
| retained C | 123 | 122--129 | 0.992x | 2,302,728,000 |
| Rust native-verified | 282,375 | 279,009--286,296 | 2,277.218x | 2,302,728,000 |

The retained oracle audit covered
`../engine/src/translator/guest/aarch64/translate.c` (`gpr_field_mask`,
`uses_x18`, ordinary same-ISA emission, optional I8MM/BF16 policy, memory
folding, and vector-dirty classification) and `stubs.c` (`emit_prologue`, full
and GPR-only spill, runtime vector currency, and block-return ownership).
Baseline vector-only arithmetic is emitted verbatim; cross-file GPR forms are
rewritten around stolen registers; optional host features are probed or lowered.
The CPU record owns all GPR/vector/FP state for the engine lifetime, translated
blocks borrow it synchronously, full exits republish it, and asynchronous faults
reconstruct pre-operation architectural state.

The corresponding Rust owners are `simd_float.c` and the other typed SIMD
families for opcode admission, `trace.c` for chained translation and provenance,
`single.c`/`pair.c` for transactional memory guards, and `stub.c`/`fault.c` for
CPU publication and fault reconstruction. Every Rust repeat reported identical
diagnostics: 12 runs, 21 builds, 59 hits, 12 fallbacks, 27 fallback exits, 9,133
completed instructions, **3,356 full guards, zero fast guards**, and 17 guard
fallbacks. Thus current float-SIMD time is no longer evidence that FMLA itself is
interpreted; it is dominated by guarded accesses to the benchmark's static
arrays. The previously rejected broad view/projection certificate cannot be
revived: it did not prove per-access mapping identity, permissions, invalidation,
or fault ordering. No production shortcut is justified until a bounded authority
mechanism preserves those contracts for every guarded address.

## 2026-08-04: current ARM64 compute boundary audit

> **Invalid retained-C comparison.** The command below configured
> `hl-engine-runner` as `--c-engine`. That executable expects
> `RUNNER ENGINE GUEST`; the benchmark supplied `RUNNER GUEST [args]`, so it
> executed the guest directly on the host. Its retained-C medians and ratios,
> and every later ledger with retained-engine SHA-256
> `0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`,
> are host-native measurements mislabeled as C-engine results. They remain
> below as historical evidence of the harness failure and must not be cited as
> engine performance evidence.
>
> A corrected CPU-17 run used the runner and retained engine as separate typed
> artifacts. Its content-bound ledger is
> `real-c-compute-historical/alternating.tsv`, SHA-256
> `38f0cbe79da26ce70ab02ca5b274415869ec920cfea786718b5201fe6e808ffe`.
> At divisor 20 and checksum `7097455804780747230`, five-sample medians were
> host-native 2,365 us, real retained C 2,545 us, and Rust 13,563 us. This
> corrects provider identity; it does not retroactively validate the ratios
> below or constitute a release-wide performance claim.

Exact detached Husklet commit `fbfe10df8dc21b1c4827580a624da54c67416585`
was built in release mode and measured five times on inherited CPU 17.  The
same content-bound guest ran host-native, through the retained C engine, and
through Rust with native execution and diagnostics required.  The command was:

```sh
taskset -c 17 target/release/testing benchmark matrix \
  --arch arm64 \
  --binary target/testing/a64-ab/combined_bench_aarch64 \
  --c-engine /Users/x/dd/engine/build/unit-audit/bin/hl-engine-runner \
  --rust-engine target/release/hl-engine \
  --out target/testing/compute-before --repeats 5 --timeout 60 \
  -- --divisor 1000 --phase compute
```

All three rows produced checksum `9349119015121845085`.  Host-native was
46 us median (44--53), retained C was 46 us (46--47), and Rust was 2,111 us
(2,059--2,591): Rust remains 45.891 times host-native on this fixed work.
Every Rust repeat was `native-verified` and reported the same counters:
seven native runs, nine builds, 24 hits, two fallbacks, seven branch exits,
five yields, and 337,288 completed guest instructions.  The engine, runner,
guest, and retained engine SHA-256 values were respectively
`f96f044a4189dff40fa3d03c40f35511ba791427da8514ae508c6bbd826d1727`,
`33bdc527d552d1ee009c1be2a1c925301ad896cf075c3eb7ada48e563c62d930`,
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`,
and `0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`.
Host load at admission was 5.93/5.48/4.61, so this is focused causal evidence,
not a release performance claim.

The retained-domain audit covered `../engine/src/core/dispatch.c`
(`run_block`, `block_return`, and `run_guest`) and
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`,
`emit_spill`, and `emit_chain_exit_from`) together with
`translate.c` (`stitch_cond`, `translate_block`, the IRQ-slim entry anchors,
and conditional stitching).  Retained C keeps architectural registers live
across direct edges, stitches bounded conditional fall-through into one
superblock, and polls interruption at backward/indirect cycle edges.  Slow
exits spill full state before returning to the dispatcher; invalidation and
publication occur under the JIT lock, and signals are selected after a fully
published boundary.  The AArch64 host branch is the relevant same-ISA path;
other hosts do not execute this transliterator.

The Rust owners are `src/native/exec/src/arch/aarch64/trace.c`
(`trace_build` and `hl_a64_conditional_chain`), `stub.c`
(`hl_a64_stub_budget_begin` and `hl_a64_stub_exit`), `entry.S`, cache
`relocation.c`, and `src/native/exec/src/executor.c::run_aarch64`, with the
safe call/lifetime owner in `src/containers/hl-engine/src/native/executor.rs`.
Rust relocation correctly targets `body_offset`, skipping the full register
reload on a chained edge.  It nevertheless terminates every conditional basic
block.  Each destination body republishes its executing-cache identity, polls
both interrupt sources, loads and checks budget, and loads/stores both budget
and completed count.  The benchmark loop has two hot conditional block edges,
so this fixed admission is paid repeatedly even though only seven public
native calls occur.

That makes bounded conditional trace stitching, not another opcode fast path,
the largest generic compute gap.  The dormant loop/certificate structures are
not authority to bypass these checks: the rejected run-scoped view certificate
does not safely cover mapping-owner transitions, and simply chaining past the
entry guard would weaken interrupt and checkpoint latency.  No production
optimization was made in this lane.  A valid implementation must port the
retained bounded conditional-stitch domain as a unit, preserve a polling edge
on every cycle, retain exact instruction accounting, and invalidate every
stitched edge with its source epochs.  Acceptance needs branch-shape tests
(taken, fall-through, nested conditions, backward cycles), mid-cycle interrupt
and budget exhaustion, mapping/instruction epoch invalidation, fork/cache
retirement, and same-base pinned before/after evidence.

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

### Dormant seccomp admission

The retained syscall boundary in
`../engine/src/linux_abi/syscall/dispatch.c` (`service`, `service_local`, and
`svc_done`) performs seccomp work only when a guest policy is active, while
retaining ptrace ordering, signal selection, restart, and errno publication at
the common boundary. The Rust owner is
`hl-runtime/src/seccomp/{control,syscalls}.rs`, called from the per-thread
router. It previously acquired the global seccomp-control mutex and searched
the thread-policy `BTreeMap` on every syscall even before any policy had ever
been activated.

The correction publishes a monotonic `ever_active` bit. A registered runtime
can return `Continue` without the global policy lock only while that bit has
never been set. Strict mode, filter commit, checkpoint restore, and even a
failed activation set it before policy mutation, permanently selecting the
exact locked evaluation path. Thus TSYNC, rollback, thread retirement,
checkpoint, missing-registration errors after activation, and active filter
decisions retain their existing ownership and ordering. The optimization is
process- and workload-independent.

On CPU 17, seven ARM64 native-verified syscall-phase repeats compared the
clean identity-routing engine (`d5c48a6351f96e5a793f42580c1e8c997442278f8f3f5c65b4ce7356fcd733c0`)
with the candidate engine
(`ea19ecc5f9b92a8b3cb436cfa6eba376b7830678b7132e92eb020e293df5ed11`).
The same guest and runner were used for both. Median in-guest time decreased
from 179,375 us (175,740--180,033) to 176,842 us
(175,471--178,572), a 1.41% reduction. Every repeat reported 24,999 syscall
exits, two fallbacks, zero yields, 475,012 completed instructions, and checksum
1,000,000. Native and retained-C medians in the candidate cell were 41,792 us
and 41,702 us respectively, so syscall dispatch remains about 4.24 times the
retained C path; the next owner is the outer router mutex and common boundary,
not dormant seccomp evaluation.

### Syscall and descriptor hot-path audit

The retained implementation audit for the syscall/descriptor lane covered
`../engine/src/linux_abi/syscall/dispatch.c` (`service`, `service_local`, and
`svc_done`), `syscall/proc.c` (`svc_proc`, including canonical `getpid` and
`gettid` identity), `syscall/io.c` (`svc_io`, scalar/vector I/O, descriptor
duplication, close, seek, pipe, splice, and synchronization), and
`syscall/helpers.c` (descriptor/OFD identity, close teardown, locking, and
restart/error completion). The dispatch boundary owns signal-entry ordering,
seccomp-before-service, ptrace routing, original-register preservation, errno
projection, restart, and descriptor publication. Descriptor aliases share OFD
state; descriptor-local flags remain per descriptor; close tears down emulated
state only after alias and lock bookkeeping; blocking and partial-result paths
retain Linux `EINTR` and progress semantics. AArch64 uses canonical syscall
numbers and advances past `svc`; x86 translates its historical number table and
must preserve the number in `rax` across restart.

The Rust mapping is `hl-linux/src/syscall/{table,frame,mod,ports}.rs`
for ABI admission, `hl-runtime/src/syscall_router.rs` for seccomp/ptrace/signal
ordering and family dispatch, `hl-runtime/src/process/{dispatch,syscalls}.rs`
for task identity and lifecycle, `hl-runtime/src/filesystem/` for descriptor
syscall composition, and `hl-descriptor` for table, descriptor, OFD, operation
lease, alias, and teardown ownership. Engine construction in
`hl-engine/src/ffi/linux/execution/routing/mod.rs` creates one router per Linux
thread. The measured identity loop exposed an avoidable generic divergence:
after seccomp admission Rust dynamically redispatched immutable `getpid` and
`gettid` through the mutable process port, whereas retained C reads identity
directly from stable process/CPU state. The candidate publishes the typed
`ProcessId`/`ThreadId` into the per-thread router and answers only those two
operations there. Seccomp, tracing, result publication, restart, and signal
boundaries remain on the common path; all mutable task operations continue
through the process port.

On exact base `0f3927e8b`, seven release-mode ARM64 native-verified repeats of
the combined benchmark's syscall phase (`--divisor 20 --phase syscall`) had a
241,851 us median and 239,284--249,135 us range. The candidate, built in the
same isolated worktree and measured with the same guest and command, had a
209,289 us median and 208,173--212,419 us range: 13.46% lower median time.
Every repeat on both trees reported 24,999 syscall exits, two fallbacks, zero
yields, and checksum/count 1,000,000. The candidate engine SHA-256 was
`850079ad9d29ffb89cb58732f4576c9522b929a886febdbbbc1fb18dbaeda94d`;
the testing runner was
`0a338fde24f3e88453a5cf0eab075adca4fa8bd8957c435016d5ee373b564ac0`
and the ARM64 guest was
`b5f1df4463a41c926a7ce2819edd0e0363a0fd9b69cb84182607770e06d5ef6e`.

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

One shared dirty-tree `--divisor 20 --phase float_simd` row completed
`native-verified` with the same checksum at 14,251,204 us, 8.2% below the clean
checkpoint median. That tree contained unrelated companion native changes, so
the delta cannot be attributed to SIMD admission. An exact clean detached run
of accepted combined commit `31cefa656` took 19,484,700 us under higher host
load, also with the same checksum and native proof. No end-to-end speed claim is
accepted from these samples. Vector memory guards and remaining fallback
boundaries are still the next measured owners.

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

The accepted bounded correction preserves that contract and removes an
unrelated scheduler asymmetry. The scheduler already grants its sole runnable
native task 65,536 element/instruction credits, but the x86 path discarded that
selected budget and hardcoded 4,096. It now receives the same selected budget
as AArch64. Shared-runnable execution remains at 4,096, while sole-runnable REP
falls from 256 to 16 yield exits per MiB. Sixteen exits are intentional: making
the complete MiB one accounting unit would again weaken the established
checkpoint and scheduling bound. A greater-than-1-MiB native regression checks
every 65,536-element partial state and resume, exact RCX/RSI/RDI, the final
syscall boundary, and byte equality. Existing focused string tests retain DF,
overlap, fault, dirty-range, executable-write, zero-count, and address-size
coverage.

Exact clean detached commit `5b0d27e85` confirms the bounded correction. Five
AMD64 memory repeats were all `native-verified`, held checksum 36,526, and
reported a 119,295-us median with a 118,386--138,524-us range. This is 2.54
times faster than the 303,537-us checkpoint Rust median, while remaining 18.27
times slower than the retained-C median of 6,531 us. Native diagnostics were
identical across all five repeats (`x86_public_exits=7131`, 238 builds, 397,962
hits, and five fallbacks). The exact `hl-engine` SHA-256 was
`f363cc376f433def6740fd941d6c292bc85e70b4f9b2e65bffac9a82f9ffe2ff`; the
exact `testing` SHA-256 was
`071120b66176b44e0f456a2df4207ad66c000ef8a5f417d82c3c39d98c013824`.

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
the retained-C median. Structural tests prove the scalar family removes those
fallbacks, while the timing only suggests their cost; packed and scalar-single
SSE remain the larger gap. This is shared dirty-tree evidence. The observed post-build `HEAD` was
`2114daca34f3790f0ce2c887dcaf3ad6551567eb`, and the candidate engine SHA-256
was `a9a544aadfb9b7ec69c8c4c59036f24eed6fb60d60d8240396b37e7e8b2dbf92`.

The exact clean detached `31cefa656` verification passed both warning-strict
native tests. Its five-repeat compute median was 846,456 us with a wide
655,170--1,420,562-us range and the same checksum. That is only 1.15 times below
the checkpoint Rust median and remains about 169 times retained C; the spread
is too large for an accepted timing claim. The exact engine SHA-256 was
`1baf1ab38e73bfe91c4514b2793d963671f18b368d805efb9f30456641c6c3e8` and
the exact testing binary was
`91e12bbb0dc3e4488d69d35dab5656696a2662b32cf14d83c94793f09a30b6c6`.

### Benchmark rootfs startup audit

The retained benchmark oracle was inspected at
`../engine/tools/matrix_runner.c`: `stage_rootfs`, `open_case_workspace`,
`run_case`, and `remove_rootfs`. It creates one private root per case, stages
the guest and optional loader/libc once, runs it, then removes exactly those
owned paths. It has no image-store snapshot, cross-process lease, or durable
publication contract comparable to `hl-images`.

The Rust path was inspected from `TestImage::materialize` through
`Images::rootfs`, `Roots::fork`, `Snapshots::prepare`, `Tree::copy_to`, and
`Draft::commit_with`. The image-store operation lock spans a full root fork.
The fork recursively traverses the immutable parent, temporarily makes each
directory traversable and each file readable, reflinks or copies regular
files, preserves hard-link identity, forks ownership/name sidecars, recursively
syncs the new tree, atomically renames it, syncs committed directories, and
publishes the snapshot before creating its lease. Release owns lease deletion
and private snapshot removal. These durability and isolation transitions must
not be weakened for benchmark speed.

Before this correction, provenance preparation called `materialize` only to
read the immutable manifest digest, then released that complete private root;
execution immediately repeated the same durable fork. One uncontended ARM64
syscall row at base `fbfe10df8` measured that discarded provenance root at
284,747 us plus 28,498 us release, while the execution root cost 122,545 us
plus 45,515 us release. The correction resolves and platform-validates the
same image metadata for provenance without creating a writable root. Execution
still performs exactly one isolated durable fork and release.
