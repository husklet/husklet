# Engine benchmarks

`cases.yaml` is the readable benchmark catalog. `manifest.tsv` additionally pins
the produced binaries and is therefore generated evidence rather than a second
workload definition.

This directory retains the working C engine's benchmark workloads as migration
oracles. The sources under `src/` are byte-for-byte copies from C engine commit
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. They must remain application-neutral
and must not be changed to make the Rust engine look faster.

`prebuilt/<isa>/` contains persistent static-PIE Linux guests for both supported
ISAs. `manifest.tsv` records their hashes, sizes, source owners, and original
flags. The dedicated guests cover combined self-timing, event, file, IPC latency,
IPC throughput, mapping, pipe, lifecycle resources, syscall, and translation.
The process-level startup, busy-loop, fork-stress, and warm-cache gates reuse the
persistent compatibility corpus rather than duplicating those binaries here.

The repository-only driver is `src/packages/hl-benchmark`. It runs the same guest
under native Linux, QEMU, the retained C engine, or the Rust engine; drains bounded
stdout and stderr concurrently; terminates the complete process group on timeout;
checks phase checksums; and emits median guest time plus median host wall time.

Example bounded comparison:

```sh
cargo run -p hl-benchmark --release -- run \
  --provider rust-engine --arch arm64 \
  --engine target/release/hl-engine \
  --binary tests/bench/prebuilt/aarch64/combined-bench \
  --repeats 5 --out target/benchmark/rust-arm64.csv \
  --engine-option HL_NATIVE_EXECUTION=1 -- --phase compute --divisor 20
```

Engine options are typed launch configuration and are intentionally distinct
from `--env`, which only changes the host process environment. When native
execution is requested the runner enables native diagnostics automatically,
requires evidence of at least one native run, and stamps the CSV row as
`native-verified`; a missing or inactive native adapter makes the run fail.

Use the same `--binary`, repeats, guest arguments, CPU load, and host for every
provider. Compare with:

```sh
cargo run -p hl-benchmark --release -- report --baseline c-engine \
  target/benchmark/c-arm64.csv target/benchmark/rust-arm64.csv
```

The combined guest normally exposes 17 phases (18 when built with SQLite): eight
CPU/ALU phases including cold and warm compute, four allocator/memory phases, and
five OS/kernel phases. The retained process-level gate adds 13 timed cases per ISA
plus one resource-lifecycle gate. Self-timed AArch64 rows are invalid if the guest
virtual counter does not advance; host `wall_us` remains useful for diagnosing
that engine defect but is not a replacement for the retained guest-time contract.
