# Testing pipeline

This document specifies the target repository-test pipeline. It replaces the
current coupling between compilation, checked-in output files, and execution.

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

