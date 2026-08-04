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
line reports all process wall-time samples and their integer average. Guest output
is validated by a stable marker because timing values are intentionally variable.

The retained `prebuilt/` and `results/` trees are historical evidence consumed by
the separate comparison tooling; the YAML-driven runner neither discovers nor
writes them.
