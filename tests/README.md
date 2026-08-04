# Repository tests

The repository-level suites are grouped by the behavior they prove:

- `runtime/` owns C guest sources and their YAML execution definitions. It is
  the compatibility and differential corpus shared by the Rust engine and the
  read-only C oracle.
- `scenarios/` owns daemon, container, and interactive workflow definitions.
  The daemon harness is being moved here without changing its public behavior.
- `bench/` owns C benchmark sources and YAML workload definitions. Persistent
  binaries remain pinned by the detailed artifact manifest.

`cases.yaml` is the readable source of suite intent. Detailed generated build,
artifact, and execution inventories may remain TSV where a tabular join is the
actual contract; those files must be checked against the YAML definitions and
must not become a second hand-maintained case catalog.

[`PIPELINE.md`](PIPELINE.md) specifies the target compile, oracle observation,
execution, comparison, cache, timing, and CI architecture.
