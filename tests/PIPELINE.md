# Testing pipeline

This document specifies the target repository-test pipeline. It replaces the
current coupling between compilation, checked-in output files, and execution.
Its a proposal to be implemented

## Goals

The test runner must:

- compile once and reuse immutable artifacts for focused or full runs;
- execute the same artifact through Husklet and independent Linux providers;
- generate reference observations rather than silently rewrite source files;
- compare artifacts, behavior, and steady-state execution time;
- run locally and in CI through the same typed command surface;
- preserve complete provenance while minimizing disk use; and
- run deterministically, concurrently, resumably, and offline after inputs have
  been acquired.

The logical pipeline is:

```text
compile -> golden -> run -> compare
```

Every stage consumes immutable records and produces immutable records. A later
stage refers to earlier records by digest rather than copying their contents.

## Providers

A provider is the environment that executes a stage. It is not synonymous with
an oracle. Each provider declares whether it can compile, execute, provide an
image, and measure guest work.

Initial providers are:

| provider | compile | execute | image model |
|---|---:|---:|---|
| `host` | yes | native ISA only | none |
| `docker` | yes | yes | OCI container |
| `qemu` | when its rootfs has a compiler | yes | Linux rootfs or prepared VM |
| `engine` | when its image has a compiler | yes | Husklet container |

Compilation and execution providers are independent. For example, an AMD64
artifact may be compiled in Docker, observed under QEMU, and tested under
Husklet. Unsupported provider/stage combinations fail explicitly.

Provider provenance records the effective stack. AMD64 Docker on an ARM64 host
may actually be `docker+qemu` or `container+rosetta`; it must not be counted as
an independent oracle from the translation layer it uses. QEMU user mode is an
ISA and Linux-ABI reference that uses the surrounding Linux kernel, not an
independent kernel reference.

Oracle authority is a separate YAML policy. Docker, QEMU, or another provider
can be authoritative for a case, and multiple providers can be required to
agree.

### Provider composition boundary

`testing` is an end-to-end composition application. It approaches Husklet from
the same supported surfaces available to product clients; it must not reproduce
image, container, engine, process, archive, or Docker protocol implementation.

Each provider has one typed configuration containing all provider-specific
settings required by every stage. Environment variables and CLI flags are
parsed once at the application boundary and converted into this model:

```text
ProviderConfig
  host: compiler/toolchain and native execution policy
  docker: endpoint, API policy, platform, image and resource policy
  qemu: mode, binary, machine/CPU, rootfs or VM, transport and resource policy
  engine: engine configuration, image, execution backend and resource policy
```

Stage-specific code does not read ambient environment variables, reconstruct
endpoints, or append provider-specific command fragments. CLI and CI profiles
select or override a `ProviderConfig`; the exact resolved configuration is
serialized into provenance after secrets have been removed.

Providers expose the same typed action vocabulary:

```text
prepare_image
prepare_session
compile
install_artifact
spawn
wait
capture
inspect
reset
stop
```

Break this into traits and each engine should have implement some of the portation or all of them.

Actions use existing repository packages and their public APIs. In particular:

- OCI acquisition, selection, verification, unpacking, leases, and garbage
  collection use `hl-images`;
- Husklet container creation and execution use the public container/engine
  surfaces selected by the production composition root;
- Docker-compatible execution uses the typed `hl-client` API rather than
  constructing HTTP requests or invoking the Docker CLI;
- archives, bounded process capture, logging, identities, and cache publication
  use their existing owning packages; and
- provider diagnostics use `hl-log` with case, stage, provider, ISA, artifact,
  image, and run identities attached once at the boundary.

The testing application owns orchestration, selection, comparison, timing, and
reporting only. Reusable provider mechanics belong to the package that owns the
corresponding public capability; test-only adapters remain inside `testing` and
must not become production dependencies.

Direct host process execution is not the default integration mechanism. It is
permitted only in a narrow typed adapter when the dependency is genuinely an
external executable with no repository API, initially a compiler/linker or
QEMU. That adapter supplies an exact environment, bounded input/output,
timeouts, process-group ownership, descendant teardown, executable identity,
and structured diagnostics through `hl-process`. Shell scripts, shell command
strings, Docker CLI calls, and scattered `std::process::Command` use are not
provider implementations.

Provider capabilities are discovered from the resolved typed configuration and
surface handshake, not inferred from executable names. A stage asks the
provider for a capability and receives a typed unavailable verdict when the
surface cannot provide it.

## Cache

The ignored repository-local cache is:

```text
.cache/testing/
  store/
    oci/
      blobs/sha256/
      manifests/
    rootfs/<platform>/<chain_digest>/
    artifacts/sha256/
    outputs/sha256/
  providers/
    host/
      artifacts/
      golden/
      results/
      roots/
    docker/
      artifacts/
      golden/
      results/
      roots/
    qemu/
      artifacts/
      golden/
      results/
      roots/
    engine/
      artifacts/
      golden/
      results/
      roots/
  locks/
  temporary/
```

Large bytes live once in the global content-addressed store. Provider folders
contain small immutable manifests or receipts that refer to those objects. This
keeps provider ownership visible without storing the same Alpine layer, rootfs,
artifact, or output several times.

OCI layers remain compressed and separate. Images derived from Alpine naturally
share their base layers, so they must not be squashed. One read-only rootfs may
be materialized per platform and ordered layer-chain digest. Runs receive an
ephemeral writable overlay. Docker or Apple Containers owns its external image
store; Husklet records an import receipt rather than copying that store into the
testing cache.

QEMU may use a prepared base disk with ephemeral overlays or a prepared
long-lived provider session. Provider realization receipts include daemon, VM,
or provider identity so a reset invalidates stale receipts.

All writes use temporary staging followed by atomic publication. Active jobs
hold leases. Garbage collection marks provider records, explicit pins, retained
results, and active leases, then sweeps unreachable objects. Rebuildable rootfs
snapshots are evicted before compressed OCI layers. Old results and unused
artifacts are governed by separate age and size budgets.

## Identities

A build identity includes:

- source and every declared input byte;
- complete build arguments and declared environment;
- target ISA and ABI;
- provider and effective provider implementation identity;
- compiler, linker, sysroot, and auxiliary toolchain identities; and
- build-image manifest digest when compilation occurs in an image.

A golden or result identity includes:

- artifact digest;
- run arguments and exact environment;
- provider and effective implementation identity;
- target ISA and relevant kernel identity;
- image manifest and ordered layer-chain digest;
- runtime options;
- output normalization rules; and
- measurement protocol identity.

An image, golden, or execution provider is not a source input unless it actually
participates in compilation. Conversely, compiler identity must never be reduced
to its executable name.

## Commands

All commands use the same selector grammar and common options:

```text
testing compile [category[/case]] --provider <provider>
testing golden  [category[/case]] --provider <provider>
testing run     [category[/case]] --provider <provider>
testing compare [category[/case]] --kind artifact|behavior|timing
testing pipeline [category[/case]] --profile <profile>
testing cache status|verify|prune
```

Providers are repeatable. `golden` and `run` accept an independently selected
artifact provider. Examples:

```text
testing compile runtime/abi --provider docker --provider qemu --isa amd64
testing golden runtime/abi --provider qemu --artifact-provider docker
testing run runtime/abi --provider engine --artifact-provider docker
testing compare runtime/abi --kind artifact --left docker --right qemu --mode elf
testing compare runtime/abi --kind behavior --left engine --right docker
testing compare bench/syscall --kind timing --baseline docker --candidate engine
```

Common selection includes `--isa`, `--class`, `--jobs`, `--shard`, `--offline`,
and `--refresh`. Selection is stable before sharding. Each stage is independently
resumable.

Running `testing` without arguments executes the checked-in `default` profile.
It validates definitions, compiles the default test set, obtains required
reference observations, runs Husklet, compares behavior, and writes one
machine-readable summary.

## Golden observations

Reproducible provider output is not checked into every test directory. `golden`
captures an immutable observation containing:

- stdout and stderr digests;
- exit status or signal;
- bounded filesystem-state observations when requested;
- artifact, image, provider, kernel, ISA, arguments, and environment provenance;
- raw execution samples; and
- the normalizer used for comparison.

YAML selects the authority. Examples:

```yaml
expect:
  providers: [docker, qemu]
  agreement: all
```

```yaml
expect:
  provider: docker
  reason: requires native Linux namespace behavior
```

Provider disagreement is `ORACLE_DIVERGENCE`; the runner does not choose one
silently. Cached observations are accepted only when every identity input
matches. CI may restore a digest-verified observation from durable cache or
regenerate it.

Checked-in expected bytes remain only for irreproducible or externally authored
contracts. They live beside the test under `expected/` and require a reason:

```yaml
expect:
  authored: expected/protocol.bin
  reason: externally specified wire-format fixture
```

Migration must classify the existing checked-in goldens mechanically. Until a
case is classified and backed by provider evidence, its old golden remains a
legacy expectation rather than silently becoming authoritative.

## Comparisons

Behavior comparison covers exit status or signal, stdout, stderr, and requested
state observations after normalization explicitly declared by the test.

Artifact comparison has two modes:

- `exact` compares complete bytes and is appropriate for pinned reproducible
  toolchains;
- `elf` compares architecture, ABI, interpreter, dependencies, loadable
  sections, symbols, and relocations after excluding declared nondeterministic
  metadata.

Both modes report raw artifact digests. A normalized match never implies that
the files were byte-identical.

## Timing

Every provider reports three separate quantities:

- `setup`: provider, image, container, or VM preparation;
- `execution`: invocation through an already-ready provider session; and
- `payload`: monotonic time measured inside the guest workload when supported.

Steady-state comparisons establish readiness, perform configured warmups, run
multiple samples without restarting the provider, and retain every raw sample.
Reports include minimum, median, p90, p99, coefficient of variation, and setup
separately. Docker container creation, QEMU boot, image materialization, and
Husklet startup must not be included in steady-state execution time.

The repository scenario runner records `setup_us`, `execution_us`,
`payload_us`, and `teardown_us` as separate durable columns. `payload_us` is
explicitly `unavailable` until a workload supplies an in-guest monotonic phase;
the runner never substitutes host wall time. `--warm-provider` retains only the
provider service. Every case still gets a fresh image view, container, process
tree, and writable state, and force-removal completes before that service can be
reused. Any failed, timed-out, or expected-failure row evicts the provider
conservatively so uncertain state cannot leak into the next case. Without the
flag, every row receives an independent provider service.

## Execution and isolation

Runtime compatibility tests use warmed providers without placing the complete
suite in one mutable failure domain. The default execution topology is:

```text
coordinator
  |-- engine/arm64 worker: warmed provider and shared read-only rootfs
  |-- engine/amd64 worker: warmed provider and shared read-only rootfs
  |-- qemu/arm64 worker
  `-- qemu/amd64 worker
```

Each worker reuses its provider session and immutable image realization. Each
ordinary case receives a fresh guest process and resettable writable overlay.
After execution, the worker captures the result, terminates every descendant,
discards test-visible mutable state, and proceeds without rematerializing the
image or recompiling the artifact.

Running the entire suite inside one long-lived mutable container is not valid
authoritative evidence. A case can leak descriptors, tasks, mappings, signals,
environment, filesystem changes, or corrupted engine state into every later
case. Conversely, restarting the complete provider for every ordinary syscall
case wastes most execution time on setup.

The runner therefore has two complementary modes:

- `sweep` runs independently reset cases through warmed parallel workers for
  fast feedback;
- `isolated` gives each selected case an independent engine failure boundary.

An authoritative run performs a parallel sweep, then automatically reruns every
failure, crash, timeout, and unexecuted row in isolated mode. If a worker dies,
rows it did not complete are `NOT_RUN`, never inferred failures or passes. The
merged verdict uses the isolated result where one exists and preserves both
attempts' provenance.

`NOT_RUN` covers every planned case the sweep did not execute, so the result
file always represents the whole selection. Deliberately inactive cases
(`!broken`, `!unsupported`, `!host-excluded`) are recorded as `NOT_RUN` with
their reason and evidence, and a selection containing only inactive cases is a
successful run rather than an error. When the sweep aborts, its unreached rows
are recorded as `NOT_RUN` naming the abort before the error propagates. A row's
diagnostic is bounded and truncated with an explicit marker; an oversized or
unreadable diagnostic never removes a row from the corpus. Byte differences are
reported as the first differing offset with escaped context from both sides,
never as a raw byte-array dump.

YAML declares the smallest valid failure boundary:

```yaml
isolation: process
```

Supported levels are:

- `process`: a new guest process; the ordinary runtime-test default;
- `container`: new container state and writable overlay;
- `engine`: a supervised engine subprocess for signal, fork, checkpoint,
  corruption, and crash cases;
- `provider`: a new QEMU, Docker, or other provider instance for provider
  lifecycle tests; and
- `exclusive`: no concurrent cases when a bounded host-global resource makes
  isolation impossible.

A category may set a default and individual cases may request a stronger level.
The runner may strengthen isolation but must never weaken the declared level.
Every level has a bounded setup, execution, teardown, and descendant-reaping
deadline.

One runtime category normally shares its image and toolchain definition while
keeping separate source files and executables for independently meaningful
cases. Compilation produces independently schedulable artifacts. A single giant
test binary is appropriate only when the sources exercise one inseparable
mechanism; it must not be used merely to reduce process startup.

Hundreds of Alpine-derived cases reuse one compressed OCI layer chain and one
materialized read-only rootfs. Per-case writable overlays are ephemeral. The
runner must not create one full image or copied rootfs per case.

Benchmarks use a stricter persistent protocol. After provider readiness and
warmup, a benchmark may retain its container and guest process and accept framed
repeat requests over a pipe or socket. This measures repeated payload execution;
provider, container, process, and image startup remain separately reported. A
benchmark's persistent state must never be reused as compatibility evidence.

The harness never silently changes what it measures. `HL_NATIVE_DIAGNOSTICS=1`
enables per-boundary capture, which dominates runtime and can invert phase
rankings, so it is never added to a timed run. A native row proves itself with
one separate diagnostics-on probe and then takes its samples with diagnostics
off; a run that cannot prove nativeness fails instead of reporting. `HL_NATIVE_*`
settings are honoured only as `--engine-option`, and passing one as `--env` is
rejected rather than ignored.

Every row states what it measured. `diagnostics` is `on` or `off`, and
`phase_context` is `isolated` for a phase that ran alone in its process or
`sequence-of-N` for one measured after N-1 others. The two are not comparable: a
phase run mid-sequence inherits translations, suppression state and IBTC
contents, so isolated numbers can understate cost. Compare only rows whose
`diagnostics` and `phase_context` agree.

Rebuilding the native engine for a benchmark needs the release profile.
`cargo clean -p hl-native` only removes debug artifacts and rebuilds nothing the
benchmark uses; use `cargo clean --release -p hl-native`.

## CI profiles

Local and CI execution use the same binary:

```text
testing
testing pipeline --profile pull_request
testing pipeline --profile main
testing pipeline --profile nightly
testing pipeline --profile release
```

- `default` and `pull_request` run validation, affected tests, and a bounded
  cross-ISA smoke/compatibility set.
- `main` runs all active engine compatibility cases.
- `nightly` refreshes required Docker/QEMU observations and runs performance
  comparisons.
- `release` runs the complete supported host, ISA, provider, nested-engine,
  scenario, and packaging matrix.

CI may cache images, compiled artifacts, and verified oracle observations. It
must execute Husklet from the current committed tree; a cached engine result is
never a current verdict.

Nested-engine artifacts use the same rule through a typed surface:

```text
testing nested prepare
testing nested run
```

The nested manifest names a Cargo package, target triple, profile, and binary;
it cannot embed a shell command. Preparation uses locked, offline Cargo. Its
cache key frames that recipe, workspace manifests and lockfile, Cargo
configuration, effective Cargo/rustc and target-library identity, relevant
Rust flags and target-linker environment, toolchain pin, and every file below
`src/`. A cross-process per-key lock serializes admission and publication. The
cached executable carries a key-bound SHA-256 receipt which is checked before
reuse, then is hard-linked or copied to the declared chain path. A source,
compiler, configuration, or recipe change therefore prepares a new object,
while an identical chain reuses the exact bytes. The cache is build evidence only:
`nested run` still executes every selected chain from the current invocation.

Stable exit meanings are required: success, test or comparison failure, invalid
definition or selection, unavailable required provider, and interrupted or
incomplete execution. Interrupted work retains a resumable record.

## Migration order

1. Add provider capabilities, identities, cache paths, and immutable records.
2. Split compilation out of runtime execution and reuse artifact records.
3. Make `golden` capture provider observations without editing source files.
4. Add behavior and normalized ELF comparison.
5. Add warmed provider sessions and the timing protocol.
6. Add default and CI profiles, sharding, resume, and cache maintenance.
7. Classify existing goldens as generated, authored, or temporarily legacy.
8. Remove the legacy direct-golden-update and unconditional-recompile paths only
   after inventory and focused parity gates prove every case is represented.

## Scenario workflow migration closure

The former Rust workflow registry under `tests/scenarios/workflows/` was not
invoked by scenario discovery. Behavior was moved to package public-contract
tests or direct child YAML scenarios before detached modules were removed.

| Former workflow | Durable owner or remaining typed pipeline requirement |
|---|---|
| smoke | `tests/scenarios/smoke-realimage/`, preserving image, ISA, command, marker, timeout, and output rows |
| software | `tests/scenarios/{databases,languages}/`, with local goldens and oracle mappings |
| terminal | `tests/scenarios/terminal/`; attached timed input still requires a typed interactive action |
| network | `hl-container/tests/networks.rs` plus daemon live name routing; live address routing remains a typed integration requirement |
| compose | package label/volume/endpoint/alias/topology contracts; live two-network routing remains repository E2E work |
| Docker sweep | typed `hl-client`, `hl-daemon`, and `hl-container` contracts listed below |
| build | image parsing/model and daemon builder/API tests; full execution, cache reuse, concurrency, multistage copy, run mounts, and result execution remain integration work |

The redundant Docker sweep is covered by typed system ping/version/info/disk
tests; image archive load/list/inspect/history/tag/save/remove/reload tests;
headless and daemon runtime stream/exit tests; attach and exec tests; update and
restart policy; event replay; changes and commit; archive export; volume and
network CRUD; compatibility metadata; and resource-prune tests. Successful
root-filesystem import with an explicitly requested repository tag is owned by
`hl-client/tests/daemon/image.rs`.

| Former Docker workflow behavior | Exact owning public-contract evidence |
|---|---|
| ping, version, info, disk usage | `hl-client/tests/daemon/system.rs::system_contract_is_platform_derived_and_unsupported_routes_are_explicit` and `hl-daemon/tests/system_disk.rs::wire_client` |
| image load, list, inspect, history, tag, save, remove, reload | `hl-client/tests/daemon/image.rs::image_archive_round_trip_uses_shared_wire_contracts`, `image_archive_tag_save_remove_and_prune_share_wire_contracts`, and `hl-daemon/tests/api/image_archive.rs` |
| foreground exit, stdout/stderr logs | `hl-daemon/tests/api/headless_runtime.rs` and `hl-daemon/tests/api/daemon_runtime.rs` |
| attach stdin/stdout/stderr and exec exit/output | `hl-daemon/tests/api/daemon_runtime.rs` |
| update and restart policy | `hl-client/tests/daemon/update.rs::container_update_persists_effective_settings_and_rejects_unknown_fields` |
| create/start/die/destroy event replay | `hl-client/tests/daemon/event.rs::typed_event_stream_replays_create_and_destroy_from_real_handlers` |
| container changes and commit | `hl-client/tests/daemon/metadata.rs::container_changes_compare_owned_rootfs_with_immutable_image_baseline` and `hl-client/tests/daemon/filesystem.rs::image_list_shared_size_accounts_executed_child_layers` |
| container export | `hl-client/tests/daemon/observability.rs::container_archive_round_trip_streams_through_typed_client` and `hl-daemon/tests/container_export.rs::wire_contract` |
| volume create/list/remove | `hl-client/tests/daemon/volume.rs::volume_crud_is_shared_with_headless_ownership_and_protects_references` |
| network create/list/remove | `hl-client/tests/daemon/network.rs::network_client_and_server_share_headless_topology` and `forced_network_removal_uses_the_docker_delete_contract` |
| plugins, authentication, search | `hl-client/tests/daemon/metadata.rs::compatibility_metadata_surfaces_are_typed_and_truthful` |
| resource prune verbs | `hl-client/tests/system.rs::system_prune_reclaims_unused_resources_and_respects_volume_selection`, plus daemon image/network prune tests |

The final root-filesystem import closure is
`hl-client/tests/daemon/image.rs::rootfs_import_publishes_the_requested_repository_tag`.
It exports a real container root filesystem, imports it through
`POST /images/create?fromSrc=-` with an explicit repository and tag, and
discovers that requested tag through image inspection.

The remaining E2E action adapters must provide a bounded host workspace and
shell executor, isolated daemon/socket lifecycle, typed client selection and
environment substitution, named resources with unconditional cleanup, bounded
stream capture, per-action diagnostics, readiness, and ordered multi-action
execution. A generic API string switch would recreate the opaque retired
runner; package behavior stays in crate tests and only true daemon/client/CLI
workflows gain typed YAML actions.

The deletion gate is: map one coherent behavior domain, verify its replacement
from the exact committed tree, remove only its detached module and dispatch
entry, and remove registry scaffolding only when no named behavior remains.



# Dead code eliminations

We also need to understand profiles during execution, ideally if we can merge profiles during testing of engine.
Once full tests runs ideally we gather all sourcecode that was executed and all files.
We take src tree and compare againts what was executed. Result should be code that was never probed or executed.
This should give us decent list of files that should be saved together with lines where code never went and we should
investigate if that code is needed or not.


For this Rust engine, I’d use four signals:

  1. Compiler coverage instrumentation
     Build with LLVM source coverage:

  RUSTFLAGS="-C instrument-coverage" \
  LLVM_PROFILE_FILE=".cache/coverage/engine-%p-%m.profraw" \
  cargo build -p hl-engine

  Run the actual engine workloads normally, then merge profiles and report with llvm-profdata and llvm-cov. The report gives execution counts by
  source file, function, region, and line. Files with zero covered regions are the direct answer to “which compiled files were unused?”

  cargo-llvm-cov makes this substantially easier and can include tests, subprocesses, and separate workload runs in one accumulated report.

  2. Link-time dead-code evidence
     Coverage only describes compiled code. Enable linker map or symbol reporting to identify functions and sections that were not even retained
     in the final executable. Release builds with section garbage collection can expose which symbols were discarded. This catches statically
     unreachable code before any engine run.

  3. Cargo/package reachability
     Use Cargo metadata plus tools such as cargo-udeps to identify unused dependencies and packages. Also inspect which features and targets
     actually participate in the production hl-engine and dockerd compositions. A source file belonging only to an obsolete binary or feature may
     never enter the executable, so runtime coverage alone will not mention it meaningfully.

  4. A representative workload matrix
     Accumulate coverage across distinct cohorts:

  - ARM64 and AMD64 guests
  - interpreter and native execution
  - startup, exec, fork, signals, faults and exit
  - filesystem, networking, terminal and Docker workflows
  - checkpoint and restore
  - failed construction and cleanup
  - nested engine execution
  - macOS, Linux and Windows host-specific builds

  The useful output would be something like:

  coverage/
    compiled_and_executed.txt
    compiled_but_unexecuted.txt
    excluded_from_production_build.txt
    unused_dependencies.txt
    deletion_candidates.yaml

  Each candidate should carry evidence:

  path: src/runtime/hl_example/src/legacy.rs
  production_build: true
  covered_regions: 0
  referenced_symbols: 0
  host_targets_checked: [linux_aarch64, linux_x86_64, macos_aarch64]
  workloads_checked: [runtime, docker, checkpoint, nested]
  classification: deletion_candidate

  The key distinction is:

  - “Not compiled into the product” is strong evidence of obsolete composition.
  - “Compiled but linker-discarded” is strong evidence of static dead code.
  - “Compiled and retained but never executed” is only a review candidate.
  - “Executed” proves the code is used by that workload.

  So your proposed subtraction is viable:

  all source files
  − files executed by accumulated production workloads
  = unobserved files

  But I would then split the unobserved set into platform-specific, test-only, error/recovery-only, statically unreachable, and genuinely
  obsolete. Only the last two groups should be deleted aggressively.

  The cleanest implementation would be a testing coverage provider/stage that launches the real instrumented hl-engine, accumulates profiles
  across YAML-selected workloads, and generates a machine-readable deletion-candidate inventory. That would make dead-code reduction repeatable in
  CI rather than a one-time cleanup.
