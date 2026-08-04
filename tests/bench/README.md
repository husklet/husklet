# End-to-end engine benchmarks

Every direct benchmark folder owns its C guest and an unversioned `test.yaml`.
Definitions select the image, cross compiler, build flags, repetitions, timeout,
arguments, and observable exit/output contract. Build products are written only
under `target/testing/bench/<benchmark>/<isa>/`; generated guests do not belong
in this source tree.

Run every benchmark for both guest ISAs:

```sh
cargo run -p testing -- bench
```

Select a folder and ISA with `testing bench combined --isa arm64`. Each successful
line reports cold, percentile, phase, and provenance measurements. Guest output is
validated by a stable marker because timing values are intentionally variable.

The runner always compiles the folder-owned `main.c`; it never consumes a checked-in
guest binary. Generated guests and ordinary run evidence stay below the ignored
`target/testing/bench/` tree. Export benchmark output to a reviewed path only when
the resulting evidence is intentionally being published.

For a controlled comparison of one already-built guest through the host, retained
C engine, and Rust native engine, pin the runner to one CPU:

```sh
taskset -c 17 target/release/testing benchmark matrix \
  --arch arm64 \
  --binary /path/to/combined-bench-aarch64 \
  --c-engine /path/to/c-engine \
  --rust-engine target/release/hl-engine \
  --out target/testing/benchmark-matrix \
  --repeats 3 -- --divisor 1000 --phase compute
```

The matrix hashes the guest, each engine, the runner, options, and inherited CPU
affinity. `PHASE` microseconds are measured inside the guest and exclude provider
startup; `wall_us` deliberately includes process and provider startup.
