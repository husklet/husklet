# x86-64 performance baseline

This is the first checked-in execution-mode-proven x86-64 comparison between
the retained C engine and the Rust native adapter. It is a regression baseline,
not an acceptance result: every Rust slowdown below is an open performance
defect.

The same persistent static-PIE `combined-bench` x86-64 guest ran each selected
phase five times with `--divisor 20`. Runs were sequential and inherited the
same Linux CPU affinity, logical CPUs 2--5. Guest medians are in `us`; `wall_us`
is the median complete engine-process duration. The runner required nonzero
native diagnostics and stamped every Rust row `native-verified`.

| Phase | C guest | Rust guest | Guest ratio | Wall ratio |
|---|---:|---:|---:|---:|
| compute | 5.408 ms | 942.653 ms | 174.307x | 46.223x |
| integer division | 16.219 ms | 166.796 ms | 10.284x | 6.214x |
| memory | 7.381 ms | 46,327.241 ms | 6,276.553x | 2,359.498x |
| pipe | 71.928 ms | 954.969 ms | 13.277x | 11.500x |
| syscall | 22.638 ms | 524.571 ms | 23.172x | 13.766x |

Compute, integer division, memory, and pipe checksums match. The syscall
checksum diverges (`142216500000` in C and `1000000` in Rust), so its row is
both compatibility and performance evidence rather than a like-for-like speed
acceptance result.

## Provenance

- Date: 2026-08-03
- Host: Linux AArch64 under OrbStack, 18 logical CPUs
- Affinity: logical CPUs 2--5 through `taskset -c 2-5`
- C source: `7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`
- C executable SHA-256: `8225510cd7f9b0c0024bac909918e65e5a3306a14128fdde80c61c8f3ba40a94`
- Rust source: `176be34f2d987ae7ce34b5818b8de13a19b21307`
- Rust executable SHA-256: `5ffb035c32021c03f5d8f2210c42d7abe6beaedc8205980253fc291395b83cc7`
- Benchmark runner SHA-256: `324bd97679e28c1876ef922d84f2a45883afad1fbaa19095714c939842c93469`
- Guest SHA-256: `a97de512853d3a35fa985a4d771c155f65a70f44eb396877e87df242d0b1beb7`

The Rust memory row deterministically reported 114,068 native runs, 198 builds,
933,197,970 translation hits, five instruction fallbacks, 3,780 operand
callbacks, 466,596,805 operand-cache hits, and 466,600,586 fallback boundaries
per repeat. This boundary volume is the highest-priority measured x86-64
performance defect.

Resource use remained bounded. The memory engine process was sampled at
116,640 KiB RSS and one fully occupied CPU; the complete clean Rust target used
181 MiB and the retained C build used 78 MiB. At completion the host had 24 GiB
available memory, 27 GiB free swap, 221 GiB free disk, and no zombie processes.

## Commands

The retained C executable was built from its clean detached tree with:

```sh
cmake -S . -B /tmp/hl-x86-bench.jbmpeM/c-build-make -G 'Unix Makefiles'
cmake --build /tmp/hl-x86-bench.jbmpeM/c-build-make \
  --target hl-engine-linux-x86_64 -j 12
```

Rust binaries were built from their clean detached tree with:

```sh
cargo build --offline --locked --release -p hl-engine -p hl-benchmark
```

Each `PHASE` in `compute intdiv memory pipe syscall` then ran sequentially:

```sh
taskset -c 2-5 target/release/hl-benchmark run \
  --provider c-engine --arch amd64 \
  --engine /tmp/hl-x86-bench.jbmpeM/c-build-make/linux-production/hl-engine-linux-x86_64 \
  --binary tests/bench/prebuilt/x86_64/combined-bench \
  --repeats 5 --out target/benchmark/x86_64-baseline/c-${PHASE}.csv \
  -- --phase ${PHASE} --divisor 20

taskset -c 2-5 target/release/hl-benchmark run \
  --provider rust-engine --arch amd64 --engine target/release/hl-engine \
  --binary tests/bench/prebuilt/x86_64/combined-bench \
  --repeats 5 --out target/benchmark/x86_64-baseline/rust-${PHASE}.csv \
  --engine-option HL_NATIVE_EXECUTION=1 -- --phase ${PHASE} --divisor 20
```

The report was reproduced with:

```sh
target/release/hl-benchmark report --baseline c-engine \
  tests/bench/results/x86_64/c.csv \
  tests/bench/results/x86_64/rust.csv
```

## Optimized REP-string checkpoint

The original files above remain the immutable pre-optimization baseline. After
the generic native x86-64 REP MOVS/STOS bulk path was merged through
`c2d6e4bd732d125ea77bded0733e1428bd5aaedc`, the same memory workload produced
a confirmed three-repeat median of 316.117 ms with checksum `36526`. That is
146.551x faster than the original 46,327.241 ms Rust result. It remains 42.828x
slower than the retained C engine's 7.381 ms result, so this checkpoint closes
the byte-at-a-time REP defect but is not performance acceptance.

The boundary change identifies what was removed:

| Counter per repeat | Original Rust | REP optimized |
|---|---:|---:|
| translation hits | 933,197,970 | 94,193 |
| branch boundaries | 466,597,231 | 44,882 |
| fallback boundaries | 466,600,586 | 49,158 |
| operand-cache hits | 466,596,805 | 45,369 |
| operand callbacks | 3,780 | 3,788 |

The persisted raw row in `optimized-memory.csv` is a hash-capture rerun from
`9de5e37785899a8395cf1f4fd27177e2dcd7bdc5`, whose only commit after the REP
merge is the unrelated AArch64 affine-loop change. It measured a 319.142 ms
median (317.690--324.885 ms), 377.581 ms median wall time, and the same checksum
and boundary counters. This rerun is 145.162x faster than the original Rust
baseline and 43.238x slower than C; the small difference from the confirmed
316.117 ms checkpoint is ordinary host timing variation.

### Optimized provenance

- Date: 2026-08-03
- Host and affinity: the baseline Linux AArch64 OrbStack host, logical CPUs
  2--5 through `taskset -c 2-5`
- REP merge: `c2d6e4bd732d125ea77bded0733e1428bd5aaedc`
- Hash-capture source: `9de5e37785899a8395cf1f4fd27177e2dcd7bdc5`
- Retained C source: `7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`
- Rust executable SHA-256:
  `12d6793a8c786cbf8f4b57229d2385da88bd7e407f65fcc5d5e5603e57b16d7d`
- Benchmark runner SHA-256:
  `324bd97679e28c1876ef922d84f2a45883afad1fbaa19095714c939842c93469`
- Guest SHA-256:
  `a97de512853d3a35fa985a4d771c155f65a70f44eb396877e87df242d0b1beb7`

The optimized measurement was produced with:

```sh
cargo build --release -p hl-engine -p hl-benchmark

taskset -c 2-5 target/release/hl-benchmark run \
  --provider rust-engine --arch amd64 --engine target/release/hl-engine \
  --binary tests/bench/prebuilt/x86_64/combined-bench \
  --repeats 3 --out /tmp/x86-checkpoint.csv \
  --engine-option HL_NATIVE_EXECUTION=1 -- \
  --phase memory --divisor 20
```

The direct native REP contract was rebuilt with warnings as errors and then
executed directly. This covers all MOVS/STOS widths, overlap and DF semantics,
budget and zero-count accounting, partial protection boundaries, executable
writes, dirty-journal capacity, and conservative prefix admission:

```sh
cc -std=c11 -Wall -Wextra -Werror -O2 -pthread \
  -Isrc/native/execution/include -Isrc/native/execution/src \
  -Isrc/schema/cpu/include \
  $(find src/native/execution/src src/native/execution/cache -name '*.c' -print) \
  src/native/execution/src/arch/aarch64/entry.S \
  src/native/execution/src/arch/x86_64/entry.S \
  src/native/execution/test/x86_rep.c \
  -o /tmp/x86-rep-checkpoint-test
/tmp/x86-rep-checkpoint-test
```
