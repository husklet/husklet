# Current test inventory (2026-07-13)

This inventory is based on the current workspace manifests, Rust `#[test]`/`#[cfg(test)]` modules,
integration-test directories, `dd-tests` registries, C guest fixtures and scenario runner. Counts describe
files, not executed cases; phase implementation must generate executable case counts from each owner.

## Aggregate test package

`dd-tests` currently owns:

- one generic engine harness and one aggregate engine runner;
- 57 Rust case/registry modules under `src/cases`;
- 51 scenario catalog modules under `src/scenarios` plus daemon boot/drive infrastructure;
- 689 guest/fixture files under `guests` at inventory time;
- nine top-level Rust integration-test files;
- LTP compatibility scripts/config/baseline;
- daemon-oriented shell scenarios;
- a benchmark runner and 16 C benchmark fixtures slated for removal.

Large guest groups include 149 `completeness`, 62 `ext_libc`, 54 `ext_abi`, 53 Darwin, 51 `ext_ipc`,
51 `ext_posix`, 43 `ext_linuxsys`, 29 `ext_net`, 27 `ext_threads`, and 69 `gui_matrix` files. These are
domain corpora, not generic helper data.

## Current top-level integration files

| File | Behavioral owner |
|---|---|
| `dd-tests/tests/suite.rs` | `dd-jit-darwin` engine matrix |
| `forkserver.rs` | `dd-jit-darwin` |
| `pcache.rs` | `dd-jit-darwin` |
| `overlay.rs` | `dd-jit-darwin` Linux VFS/runtime |
| `nonpie_dladdr.rs` | `dd-jit-darwin` loader/runtime |
| `rendering_ir.rs` | `dd-gpu` |
| `rendering_backends.rs` | split between `dd-gpu` and `dd-shim-common` |
| `gate_invariants.rs` | split across owning runner and root CI; benchmark assertions removed |

The current `rendering_ir.rs` tests are already behavioral and valuable; their defect is ownership, not
test style. The recently added backend test likewise calls real APIs and should be split without weakening
its assertions.

## Crate-local coverage already present

- `dd-daemon` has broad in-module tests across build, archive, lifecycle, inspect, exec, networks, volumes,
  images, state and API utilities, plus reusable `src/test_support`. This makes it the natural destination
  for the external daemon scenario catalog.
- `dd-images` has in-module coverage for registry, archive, manifest, discovery, cache, Dockerfile and
  layer handling. Registry/archive-only journeys should land here; container launch remains daemon-owned.
- `dd-jit` tests its host-neutral builder, environment, user, handle and pump APIs. It should not absorb
  host-engine instruction/syscall fixtures.
- `dd-jit-darwin` currently tests guest selection, spawn configuration and launch wire, but its large C
  engine corpus is still externally owned by `dd-tests`.
- `dd-gpu`, `dd-gpu-wgpu`, all shim crates, `dd-display`, and `dd-compositor` already have crate-local
  Rust integration suites. The global rendering corpus should join these rather than create a new owner.
- `dd-term-core` has extensive in-module tests plus `tests/probe.rs`; `dd-gui` has only small model/widget
  tests and needs controller/launch behavior planned separately from native visual smoke.
- `dd-client` tests view-model conversions but lacks a crate-local fake Unix-socket server integration
  gate; add one before relying solely on daemon end-to-end tests.

## Test-value problems to resolve while moving

The phase-1 [test-value audit](../../phase_1_audit/research/deep-test-value-wave-ab-2026-07.md) identifies:

- exact source-substring tests in `dd-gpu/tests/capability_handshake.rs` to remove;
- archive flag-presence tests in `dd-images` that should become metadata round trips;
- false-green skips in overlay, pcache, forkserver, non-PIE, translator parity, GL pixel parity and
  Metal/golden families;
- ABI/manifest inventories that should remain as build evidence while behavioral API calls prove runtime
  correctness.

Do this remediation at the destination. Do not faithfully copy a weak test and declare migration parity.

## Current invocation problem

`cargo test` over default members includes `dd-tests`, so unrelated product behavior is presented as one
package result while excluded macOS crates require separate gates. The target state exposes one command per
owner and a root orchestrator that reports them individually. This makes a daemon/PostgreSQL failure a
`dd-daemon` failure, a PTX translation failure a CUDA/GPU failure, and a guest syscall failure a
`dd-jit-darwin` failure.
