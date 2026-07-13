# Runtime/tests deep audit — wave L (2026-07)

Documentation-only follow-up covering cfg-sensitive imports, test support, binary fixtures, hardcoded
paths, persisted fields, and public commands.

## Exact safe cuts

- The unused-import list in wave I is not hidden by cfg branches: none of the 24 affected
  `dd-tests/src/cases/ext/*.rs` files contains `#[cfg]` or `cfg!`. Their builder names are ordinary
  functions, not macro expansion inputs. Remove the listed imports and each blanket allowance together;
  `syscallcompat.rs` can drop its allowance directly.
- `dd-daemon/src/test_support/build.rs:406` defines `_touch(_p: &Path)` solely to make an imported `Path`
  look used. It has one occurrence and no test consumer. Delete `_touch`, its `#[allow(dead_code)]`, and
  narrow/remove the corresponding import.
- There are 49 transient “Owner: … agent”, “Edit ONLY this file”, or “Keep this module compiling” lines
  in ext-case documentation. No cfg/build behavior depends on comments; remove the coordination claims
  while retaining descriptions of what each group proves.

No other definition-only helper was found in daemon `test_support`, the Rust harness, or scenario support
by symbol occurrence. Similar helper bodies often look duplicated but encode different fixture state;
deduplicate only after a shared helper preserves failure context and does not add subprocess work.

## Checked-in executable/source pairs

The repository tracks 32 executable-bit guest files. Twenty are ELF GUI-matrix probes with same-stem C
sources; five x86 ELF fixtures (`ctest_x64`, `g_x64`, `gw`, `hello_x86`, `hx`) also have same-stem C;
`darwin/hello` has `hello.c`. The remaining executables include architecture/toolchain-only fixtures
(`go_*`) and scripts.

Do not delete all checked-in binaries: cross-compilers, guest libraries, and macOS build tools are not
available on every test host, and compiling every probe per run would reduce speed and reproducibility.
The safe deduplication is artifact governance:

1. Keep source as truth and checked-in binaries as cached fixtures where a runner consumes them directly.
2. Add a manifest recording compiler triple/flags and source digest, then a Rust test that rejects a stale
   source/binary pair without rebuilding it during normal tests.
3. Rebuild in one explicit maintainer/CI job; do not compile in each test invocation.
4. Delete a binary only when the runner already compiles that source into a cache and all supported hosts
   have the toolchain.

This preserves current test speed and coverage while eliminating silent source/binary drift. GUI binaries
are especially not independent “old copies”: `run_gui_matrix.sh` executes probe names and the Makefile
builds the source set.

## Hardcoded host paths

The developer path `/Users/x/dd/poc/images` is duplicated in the Rust scenario runner, harness provisioner,
eight scenario/tool scripts (`docker.sh`, `realsw.sh`, `compose.sh`, `docker-net.sh`, `docker-full.sh`,
`compose-multinet.sh`, `macos-container.sh`, `memwatch.sh`), and dynamic coverage rootfs lookup. Replace it
with one repository/state-derived default or require `DD_IMAGES`; keep `DD_IMAGES` as the override.

`dd-tests/src/scenario/daemon.rs` correctly requires a repo-visible path for the mac bridge, but its
comment unnecessarily embeds `/Users/...`; describe the shared-mount requirement without a username.
`/tmp` paths inside guests and uniquely tagged host test paths are intentional ephemeral fixtures, not
portability bugs.

Shell scenario files overlap the newer Rust scenario runner but remain wired by distinct Make targets.
They are not safe deletions until their Docker API/compose/network assertions are mapped to Rust cases.

## Persisted and serde compatibility

Daemon model structs under `model/wire` are persisted to state JSON. Their extensive `#[serde(default)]`
annotations are migration behavior: old container/network files predate newer fields. Runtime-only fields
explicitly marked `#[serde(skip)]` must remain skipped. The custom architecture serializer stores stable
guest target strings and is not replaceable with derived enum serialization without migration tests.

API structs under `dd-daemon/src/api` are Docker wire contracts rather than persistence. Apparently unused
fields and unusual renames (`ID`, `IPAM`, `RootFS`, `OOMKilled`, `timeNano`, `progressDetail`) are consumed
by strict Docker/bollard clients. Do not remove them based on Rust field reads; serialized golden tests are
the correct ownership evidence.

Client models and `dd-images` manifests likewise cross crate/disk boundaries. A true field cut requires:
fixture load of the oldest supported JSON, round-trip tests, and confirmation that Docker/OCI output keys
remain unchanged. No field met that standard for immediate removal.

## README, website, release and rebrand commands

The active declared binary is `ddcli`; the website consistently demonstrates `ddcli`. README still uses
`dd install`, `dd app`, and `dd doctor` at lines 169-191 and calls the product a “`dd` CLI” near line 54.
`release.yml` repeats `dd install` / `dd app`, while runtime lookup merely accepts `dd` as a fallback alias.
Keep aliases during the rebrand, but choose one canonical command and test/install it before publishing.
At present, `ddcli` is the executable guaranteed by Cargo and bundle flows.

Benchmark removal also requires deleting stale `make bench` reproduction claims from README and
`website/index.html` plus the blog method paragraph; those commands otherwise become broken documentation.
Do not remove historical performance prose solely because the harness is removed, but stop promising a
nonexistent reproduction target.

CLI and terminal help strings point users to `docs/ideas/CUDA_ON_METAL.md` and
`docs/ideas/RENDERING_PLAN.md`. These are repository-internal paths shown inside the shipped application,
not useful installed-product help, and the rendering work has moved to consolidated current docs. Replace
with stable user documentation/URL during rebrand; comments may link current developer docs separately.

`website/assets/SCREENCAST.md` says “dd app” while the active binary is `ddcli app`; update with the same
canonical-command decision rather than letting marketing assets drift independently.

## Ordered maintenance plan

1. Apply import/comment/`_touch` cuts and run Rust all-target checks.
2. Centralize image-root discovery across Rust and shell runners.
3. Add source-digest manifests for checked-in executable fixtures; preserve cached execution speed.
4. Retain all persisted/API serde fields until compatibility fixtures prove a migration.
5. Complete benchmark-doc cleanup and canonicalize commands as part of the retained rebrand goal.
