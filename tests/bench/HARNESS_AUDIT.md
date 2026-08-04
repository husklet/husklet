# Benchmark harness provenance audit

## Scope and retained oracle

This audit compares Husklet commit `a38d60628fd6339940148b222f6e7947b19b2d27`
with the retained C engine at `../engine` without modifying the retained tree.

The retained implementation studied was:

- `../engine/tools/bench_runner.c`: `cmd_run`, `run_once`, `native_build`,
  `qemu_build`, `hl_build`, `docker_build`, `umedian`, and `cmd_report`;
- `../engine/tools/bench/README.md`: the provider matrix, same-binary rule,
  guest-internal timing contract, checksum policy, and host/ISA limitations;
- `../engine/tests/perf/combined_bench.c`: the `PHASE <name> us=<n>
  ok=<checksum>` producer and architecture-specific monotonic clock selection.

`cmd_run` resolves one provider, performs setup once, creates a fresh process
for every repeat through `run_once`, requires a stable phase set and checksum,
and writes the median/minimum/maximum with the environment and architecture.
Provider state is local to one runner invocation. The benchmark guest owns the
phase clock, so provider startup is excluded from phase time. The C runner does
not claim that its CSV is immutable provenance: its documented reproduction
procedure binds the explicitly selected binary, engine, provider, architecture,
and result file at invocation time.

The Rust implementation studied was:

- `src/apps/testing/src/bench.rs`: `run`, `plan`, `execute_work`, and
  `fingerprint`;
- `src/apps/testing/src/bench/definition.rs`: `Benchmark::load` and
  `Benchmark::build`;
- `src/apps/testing/src/bench/execution.rs`: `execute`, `execute_with_image`,
  `run_case`, `Invocation::new`, `Invocation::execute`, `Invocation::wait`,
  `Measurements::record`, `Measurements::finish`, and `parse_phases`;
- `tests/bench/combined/main.c` and `tests/bench/combined/test.yaml`.

The Rust runner retains the important timing mechanics: every repetition uses
a fresh process/container, phases are timed inside the guest, measured samples
exclude the declared warmups, output is bounded, the phase set must remain
stable, and non-syscall checksums must agree. The folder-owned combined source
matches the retained guest source.

## Provenance defect

`bench::run` opens the resumable ledger before building or materializing any
row. `fingerprint` currently hashes only the work key, `test.yaml`, guest source,
and expected-output marker. A prior `pass` row therefore remains eligible for
resume after any of these performance-relevant inputs changes:

| Input | Current identity in resume stamp |
|---|---|
| Rust runner and linked Rust engine | missing |
| selected execution backend/configuration | only indirectly represented by YAML text |
| compiler executable and version | missing |
| compiled guest artifact | missing |
| materialized image/rootfs | missing |
| host/ISA mechanism and native diagnostic proof | not an immutable identity |

This makes a resumed report non-authoritative as exact-tree performance
evidence. It can print an old passing measurement after the Rust engine changed,
after a compiler produced different code, or after an image tag resolved to a
different rootfs. The issue affects evidence identity rather than phase timing;
the measurements produced by a non-resumed, controlled run are not invalidated
by this finding.

## Required ownership boundary

Do not patch this by adding only the current `testing` executable to the existing
hash. That would close one input while leaving the report apparently content
bound when the guest artifact and rootfs are still mutable.

Introduce a cohesive `BenchmarkProvenance` value after preparation and before
ledger admission. It must own content identities for:

1. the runner/linked engine executable and exact committed tree;
2. the selected target, execution mode, and native-diagnostic contract;
3. the resolved compiler/toolchain and the bytes of the compiled guest artifact;
4. the immutable image manifest plus materialized rootfs identity;
5. the benchmark definition, source, arguments, expectations, warmups, samples,
   timeout, and relevant host scheduling/pinning policy.

The build and image layers should supply their public content identities; the
benchmark harness should compose them rather than reconstructing cache layout or
executing host hashing commands. Encode every field with length-delimited typed
serialization, then derive the ledger stamp from that representation. Resume may
admit a row only when its full provenance identity matches. Printed evidence must
include that identity so a result can be traced independently of the mutable
ledger path.

Until that boundary is implemented and tested by independently changing each
input above, `--resume` is a convenience for exploratory runs, not an
authoritative retained-C/Rust performance comparison.
