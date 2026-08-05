# Runtime corpus migration audit

This audit records the ownership boundary between the self-contained runtime
categories and the retired centralized corpus. It was made from the working
tree on 2026-08-04. The retained `../engine` tree was read only.

## Current inventory

The runtime runner discovers only direct children of `tests/runtime` containing
`test.yaml`. There are currently 36 such category manifests containing 1,608
case definitions: 1,429 unconditionally active, 20 active except on macOS, 119
explicitly broken, and 40 unsupported. The 20 host-conditional cases account
for 40 ISA rows and no longer lose Linux and Windows coverage. Every manifest
loaded successfully through `testing runtime` and all 1,608 declared stdout
paths exist inside the category that names them.

This does not yet make `tests/runtime/legacy` removable. Three other direct
children are not ordinary runtime categories:

| Directory | Current owner | Remaining work |
|---|---|---|
| `legacy` | retired Python/CMake/TSV corpus, reports, generated-artifact manifests | remove its active Rust and flake consumers before deleting it |
| `nested` | `testing nested` and `chains.yaml` | move to a top-level nested suite or deliberately extend the common category schema; it is not discovered by `testing runtime` |
| `terminal` | oracle audit only | add a manifest-backed process/PTY contract or move the audit beside the package tests that own the behavior |

The working tree removes 841 of 1,386 tracked legacy files and has copied the
large C corpus into category folders. The remaining tracked legacy tree contains
545 files, 507 of them historical reports. Generated-artifact
manifests remain, but the binaries and dynamic-loader files they describe do
not exist. A manifest is provenance, not an executable fixture.

The detached CMake smoke entry point has been removed. Nothing in the repository
included it, and all three paths it required (`prebuilt/manifest.tsv`,
`manifest.tsv`, and `source/abi.h`) were already absent. Its Python replacement
remains in the flake because the YAML-native inventory test does not yet reproduce
the corpus verifier's artifact/source/recipe hashing, fixture classification, or
production-priority analysis. The warning-strict workspace test does load every
direct-child YAML manifest, validate its inputs and goldens, expand every case/ISA
row, and prove that the planned active set exactly partitions the declared set;
that is manifest integrity, not mechanical legacy parity. `LEGACY_PARITY.md`
records the remaining parity gaps explicitly.

## Active legacy consumers

These consumers must be removed or migrated before the current legacy
deletions can be accepted. Checked-in prebuilt binaries must not be restored or
relocated.

| Consumer | Missing input | Category coverage | Decision |
|---|---|---|---|
| `src/containers/hl-engine/src/runtime/machine_test.rs::execution_installs_checkpoint_roles` | `legacy/prebuilt/aarch64/exit` | `runtime/bootstrap/exit` proves exit execution and the unit test proves role composition, but neither captures a live assembly | add a repository YAML checkpoint case with an explicit guest rendezvous; do not claim the synchronous exit fixture is live-capture evidence |
| `src/containers/hl-engine/src/ffi/linux/execution/test.rs::bootstrap_instructions_execute` | deleted `exit` and `write` prebuilts | `runtime/bootstrap/{exit,write}` is an exact source and exit/stdout mapping for both ISAs | remove the detached package integration test after the manifest rows are part of the required gate |
| `src/containers/hl-engine/src/ffi/linux/execution/test.rs::clone_teardown` | absent `legacy/prebuilt/{aarch64,x86_64}/clone` | `runtime/clone/robust-clear-tid` covers clone, `CLONE_CHILD_CLEARTID`, futex wake, and robust owner-death teardown on both ISAs | compare the unavailable binary's provenance before declaring exact equivalence; no prebuilt manifest row exists for it |
| `src/apps/testing/src/bin/compat_worker.rs` | four absent loader/libc files declared by `legacy/artifacts/runtime/manifest.tsv` | `runtime/process/nonpie-dladdr` owns its source, golden, dynamic build, and Alpine image closure | migrated boundary: the retained inventory injects `dynamic-rootfs=<selected-corpus>/artifacts/runtime`; a generated corpus must still materialize the four declared files |
| `src/apps/testing/tests/projection_linux.rs` | missing `legacy/artifacts/full/...` guests and missing dynamic loaders | `runtime/process/{uname-boundary,nonpie-dladdr}` overlaps guest behavior, but does not cover `ProcessAuthority` projected-root mechanics | migrate projected read, directory, write, uname, and dynamic-loader cases as explicit repository process categories before deleting these tests |
| `src/apps/testing/tests/{inventory,compat}.rs` | entire centralized legacy inventory | the 36 YAML categories are the intended replacement | delete only after a mechanical case/ISA/disposition comparison reports no lost row |
| `flake.nix` | `legacy/{corpus,fixture_schema,priority}.py` and their tests | no current YAML-native equivalent is called by the flake | replace with the typed manifest validation and full case-ID/ISA/disposition inventory gate before removing the Python tools |

The projected-root source files remain centralized at
`legacy/projected_{read,directory,write}.c`. They cannot be moved safely yet:
the current runtime schema stages one executable into a container image but
does not describe the host-side projected tree, writable projection, symlinks,
dynamic loader set, or post-run host-file assertions used by
`projection_linux.rs`.

The detached `hl-engine::environment_stack` test was removed after its only
inputs, `legacy/prebuilt/{aarch64,x86_64}/environment`, were deleted. Restoring
checked-in executables would have preserved an unowned fixture. Its replacement,
`runtime/environment/initial-stack`, owns the freestanding source and build for
both ISAs and expresses the original ordered byte contract directly: `TZ=UTC`
followed by `0xff`, `EMPTY=`, and the four explicit defaults. The workload reads
the kernel initial stack at `_start`, so this remains production execution
evidence rather than a planner-only unit test.

## Undeclared category inputs and rows

The manifest loader has a typed `build.inputs` field. Runtime categories now
declare their category-local headers and included sources, including the ABI,
compatibility, DBT, memory-RSS, socket, procfs, epoll, and fork inputs. Their
content is therefore part of each build fingerprint and remains owned by the
category that consumes it.

`process/source/fork_probe.c` is a second executable used by the retained
forkserver integration shape, not a header. The current one-source build schema
cannot compile and stage that auxiliary program for a case, so it remains an
honest schema gap rather than an undeclared `inputs` entry.

The following source/golden pairs are deliberately present but not registered
as cases: `completeness/x86_64/lddqu`,
`syscalls/{epoll_finraw,pidfd_raw}`, and their goldens. The category oracle
documents must either justify preserving each as evidence or the manifests
must give each a typed disposition. Image-specific goldens under
`isolation/golden/images` and `procfs/golden/images` are also not referenced by
the runtime manifests and should move with the image scenarios that consume
them or be deleted after a source-backed parity check.

## Retained C oracle audit

### Dynamic non-PIE and rootfs resources

The dynamic-rootfs lane was audited against
`../engine/tests/compat/process/nonpie_dladdr.c`,
`../engine/cmake/Phase3Compat.cmake` (`hl_guest_named` for
`nonpie_dladdr`), `../engine/cmake/GuestFixtures.cmake` (the dynamic guest
compiler selection), `../engine/src/linux_abi/elf.c` (`elf_interp`,
`load_elf`, and `build_stack`), and the corresponding loader path in
`../engine/src/linux_abi/x86.c`. The retained engine reads `PT_INTERP`, maps
the main image before its interpreter, owns both mappings through process
teardown, applies per-segment W^X, and constructs the initial stack with the
interpreter base. For a dynamic non-PIE main executable, guest-visible
`AT_ENTRY` and `AT_PHDR` remain at their low Linux addresses even when host
storage is biased high; glibc uses those values to build the `link_map` ranges
used by `dladdr` and `dlsym(RTLD_NEXT)`.

Rust maps those capabilities to `hl-loader`: image planning and interpreter
loading own the two mappings, `DynamicLoaderHandoff` owns their entry/base
handoff, and stack construction owns `AT_ENTRY`, `AT_PHDR`, and `AT_BASE`.
The compatibility worker supplies only the filesystem closure. It no longer
derives that closure from `CARGO_MANIFEST_DIR`: the inventory runner converts
the `dynamic-rootfs` schema token to
`dynamic-rootfs=<selected-corpus>/artifacts/runtime`, and its existing run
fingerprint hashes that selected resource tree.

The selected legacy corpus must contain these exact manifest-owned inputs:

- `aarch64/lib/ld-linux-aarch64.so.1`;
- `aarch64/lib/aarch64-linux-gnu/libc.so.6`;
- `x86_64/lib64/ld-linux-x86-64.so.2`;
- `x86_64/lib/x86_64-linux-gnu/libc.so.6`.

All four are absent from the current checkout; `manifest.tsv`, `NOTICE.md`,
and `COPYING.LIB` are provenance rather than executable resources. The durable
replacement, `tests/runtime/process/test.yaml` case
`runtime/process/nonpie-dladdr`, builds the same retained source for both ISAs
and runs it inside its declared `alpine:3.20` image, which owns the loader and
libc closure. This covers the non-PIE loader behavior, not the host projected
root mechanism. `src/apps/testing/tests/projection_linux.rs` still needs typed
projected-tree inputs, writable projection and post-run host-file assertions
before its dynamic-loader case can leave the legacy corpus.

The following retained implementation was studied directly:

- `../engine/cmake/Phase3Compat.cmake`: `hl_guest_binary`,
  `hl_guest_suite`, and `hl_compat_suite` registrations;
- `../engine/tools/matrix_runner.c`: `load_manifest`, `isa_servable`,
  `engine_format_of`, case supervision, result comparison, and resource-leak
  checks;
- `../engine/tools/compat_runner.c`: `run_one` and `main`;
- `../engine/tools/process.c`: `make_pipe`, `child_exec`, `read_output`, and
  `hl_process_run`;
- `../engine/tools/nested_engine_gate.c`: `main` and its chain validation;
- the source and expected-output trees under `../engine/tests/compat` and
  `../engine/tests/soak` that correspond to the migrated category names.

The C build graph owns one output per source and ISA, with explicit linkage,
flags, libraries, architecture exclusions, and special auxiliary fixtures.
The matrix runner owns case identity and disposition, rejects a suite when an
ISA engine is absent, determines host-specific exclusions from the engine
object format, launches each case in a separate process, compares ordinary
exit and exact stdout, and checks child/descriptor/thread cleanup. It preserves
`EINTR` handling around reads and waits. Category-level resource locks protect
network namespaces, System V keys, process groups, and shared scratch state;
soak is deliberately serial. The simple compatibility runner applies a
five-second deadline, kills and reaps timed-out children, and continues through
all requested executables.

The Rust YAML runner maps source, flags, targets, disposition, environment,
timeout, exact stdout, and exit status into each category. Its worker-process
boundary and bounded captures strengthen isolation. Remaining parity gaps are
the auxiliary/multi-program fixture model, explicit category resource locks,
projected-root setup and postconditions, non-UTF-8 argv/environment bytes,
dynamic-rootfs resources, and a mechanical full inventory comparison. Until
those are closed, successful YAML parsing proves manifest integrity but not
complete retirement of the C corpus or the legacy harness.

## Acceptance order

1. Add typed multi-artifact and projected-root setup where the retained C lane
   requires auxiliary executables or filesystem state.
2. Migrate the active consumers above and remove their detached package tests
   only after exact YAML equivalents pass on both ISAs.
3. Compare every retained C manifest row to a YAML case with matching ISA,
   flags/linkage, disposition, exit, stdout, environment, and special setup.
4. Replace the flake's Python legacy checks with the typed inventory gate, then
   delete `tests/runtime/legacy` as one final boundary.
