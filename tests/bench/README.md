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

Use `testing product-ab` or `make bench-direct-ab` for controlled comparisons of
already-built product artifacts. Both commands require explicit artifact and
results paths so a comparison cannot silently select a sibling checkout or reuse
an existing ledger.
