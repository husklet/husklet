# Testing pipeline implementation gap audit

## Scope and exact tree

This audit compares `tests/PIPELINE.md` with Husklet
`6a377e255` without changing `tests/runtime` or `tests/scenarios` content.
It follows the proposal's migration order: shared provider capabilities and
immutable cache records must precede new `compile`, `golden`, `run`, `compare`,
and `pipeline` commands. Existing runtime, scenario, benchmark, and nested
commands remain production paths during that migration.

## Capability matrix

| Proposed capability | Current Rust owner and entry point | Status | Required coherent next owner |
|---|---|---|---|
| One resolved `ProviderConfig` | No shared model. `benchmark::Provider`, `runtime::manifest::OracleProvider`, scenario's engine provider, and nested Cargo settings are independent models. | Missing | `testing` application provider module; provider-specific configuration parsed once at the CLI/profile boundary. |
| Capability discovery (`compile`, `execute`, `image`, `measure`) | Benchmark validates combinations locally; runtime/scenario infer their fixed provider from the command path. | Divergent | Provider capability enum/set returned from resolved configuration and surface handshake. |
| Typed provider action vocabulary | Scenario uses public `hl-container` APIs. Runtime oracle uses `hl-process`. Benchmark has a bounded `adapter::Process`. There is no shared prepare/install/spawn/capture/reset contract. | Partial | Consumer-owned traits in `testing`; adapters delegate to `hl-images`, `hl-container`, `hl-client`, and `hl-process`. |
| External-process confinement | Runtime oracle and benchmark adapter use `hl-process`; runtime and benchmark compilation still construct `std::process::Command` or `tokio::process::Command` directly. | Partial | One typed compiler/QEMU adapter using `hl-process`, exact environment, timeout, capture bounds, and descendant teardown. |
| Provider-independent compile records | `Runtime::build` recompiles into `target/testing/runtime`; `Benchmark::build` recompiles before publishing only artifact bytes; nested preparation has a stronger recipe key and receipt. | Missing | Immutable compile record keyed by source, arguments, environment, ISA/ABI, provider, toolchain, sysroot, and image identity. |
| Provider-scoped `.cache/testing` layout | Nested uses `.cache/testing/nested/{artifacts,locks}`. Runtime, scenario, and benchmark use separate `target/testing/**` paths; benchmark places bytes under `target/testing/bench/cache/artifacts/sha256`. | Divergent | A cache owner under `testing` exposing validated paths for the global store, provider receipts, locks, and temporary staging. |
| Atomic content publication | Benchmark uses `NamedTempFile::persist_noclobber`; nested locks, verifies a key-bound digest receipt, and materializes by link/copy. No shared record format exists. | Partial | Reuse the nested admission/publication invariants in a generic immutable object/record store, without moving provider policy into `hl-fs`. |
| OCI/rootfs reuse | Runtime image preparation and scenarios use `hl-images`/`hl-container`; caches are not represented by common provider receipts. | Partial | Provider receipt refers to `hl-images` manifest, layer-chain, snapshot, and lease identities rather than duplicating OCI bytes. |
| `testing compile` | No CLI command. Compilation is embedded in runtime and benchmark execution/preparation. | Missing | Pipeline stage consuming definitions and publishing compile records only. |
| `testing golden` | `testing oracle` builds and executes one configured command, then can rewrite checked-in output. | Divergent | Immutable observation record; never edit source expectations; authority remains separate policy. |
| `testing run` | Runtime, scenarios, benchmark, and nested each have dedicated execution paths and result ledgers. | Partial | Pipeline stage selects artifact and execution providers independently and records identity-linked observations. |
| `testing compare` | Benchmark report compares CSV timing. Runtime compares captured bytes with legacy files. Normalized ELF and provider-to-provider behavior comparison are absent. | Missing | Typed artifact/behavior/timing comparison over immutable records. |
| `testing pipeline` and profiles | No command; running without arguments is a Clap usage error. | Missing | Profile resolver composes stages through their typed APIs and produces one machine-readable summary. |
| Golden authority and divergence | Runtime YAML has typed oracle provider/status evidence, but there is no multi-provider agreement record or `ORACLE_DIVERGENCE` result. | Partial | Authority policy selects immutable observations after capture, independent from provider configuration. |
| Resumable immutable results | Runtime, scenario, and benchmark have separate TSV ledgers; scenario fingerprints warm-provider policy and preserves phase columns. | Partial | Content-addressed stage records with explicit incomplete/interrupted outcomes and stable exit classification. |
| Setup/execution/payload timing separation | Scenario ledger has `setup_us`, `execution_us`, `payload_us`, and `teardown_us`; benchmark report retains raw samples. | Implemented locally | Preserve these contracts when result records are generalized; do not infer payload time. |
| Warm provider with fresh case state | Scenario `--warm-provider` retains only passing provider state and recreates case-visible state. Runtime has warmed workers. | Implemented locally | Lift the lifecycle into provider session actions after immutable artifacts exist. |

## Largest architectural blockers

1. There is no common immutable record identity. Adding stage commands before
   this would create another set of ad hoc paths and make cache hits
   unauditable.
2. Provider is currently an execution-mode enum in several unrelated runners,
   not a resolved stack with typed capabilities and provenance. A shared trait
   over today's enums would erase provider-specific configuration instead of
   modeling it.
3. Compilation is not a stage. Runtime always builds in `Runtime::build`, and
   benchmark preparation builds before its artifact-byte cache lookup. The
   nested runner is the strongest existing source for locked recipe identity,
   atomic publication, receipt verification, and link/copy materialization.
4. Cache roots conflict with the proposal. Moving paths mechanically would
   invalidate existing resume evidence without providing record migration or
   verification.

## Recommended first implementation slice

Add a `testing`-owned cache/record module with validated `.cache/testing`
paths, framed identities, immutable atomic publication, digest verification,
and provider receipt namespaces. Port nested artifact publication to that API
first while preserving its current cache keys and receipts. Only then add
provider configuration/capability records and split runtime compilation from
execution. This order supplies an auditable storage boundary without inventing
an omnibus provider trait or changing compatibility definitions.

No implementation was attempted in this lane because the required focused
build/test gate was unavailable during the repository-wide quiet measurement
window. This document therefore makes no buildability or runtime claim.
