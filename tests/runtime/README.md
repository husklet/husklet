# Runtime compatibility corpus

This directory has one source of truth for Rust and retained-C compatibility
runs. `cases.yaml` is the readable definition for the checked smoke cases;
`manifest.tsv` remains the pinned build projection consumed by the current CMake
runner during the YAML-driver migration.


- `manifest.tsv`, `recipe.tsv`, `source/`, `golden/`, and `prebuilt/` are the
  checked freestanding smoke corpus. Both guest ISAs are always available even
  when a host has no cross compiler.
- `prebuilt/manifest.tsv` pins every smoke artifact's SHA-256, byte size,
  compiler identity, source digest, ABI-header digest, and recipe digest.
- `corpus.py import ../engine` refreshes the read-only retained source snapshot,
  builds `build-plan.tsv`, and preserves excluded dispositions and notes.
- `corpus.py build` cross-compiles every row runnable on at least one supported
  host (`active`, `excluded-macos`, and future `excluded-windows`) to
  `artifacts/full/{suite}/{isa}/{case}` and writes `artifacts/manifest.tsv`.
- Corpus builds default to one job, resume from existing pins, and accept
  `--batch-size N` for bounded persistent batches. `--rebuild` is the explicit
  whole-corpus replacement mode.
- `corpus.py verify` rejects artifact, source, recipe, or size drift.
- `corpus.py audit-cmake ../engine/build/unit-audit` joins each persistent pin
  to the generated CMake/Ninja fixture edge by retained source, ISA, and suite,
  then requires byte-identical artifacts. This is the cross-project fidelity
  gate: matching source and nominal GCC version is insufficient when compiler
  wrappers, sysroots, static libc, linker defaults, or build flags differ.
- `corpus.py import-cmake ../engine/build/unit-audit --unique` transactionally
  replaces persistent guests with the uniquely mapped successful CMake outputs.
  It stages and synchronizes every byte before publication, preserves executable
  modes, and pins the SHA-256, size, and exact generated Ninja command digest.
  A stable exclusive lock rejects concurrent importers. A small synchronized
  `planned -> prepared -> committed` journal records every staged, target, and
  backup path; the next invocation rolls an interrupted pre-commit publication
  back or completes post-commit cleanup before reading pins. Thus SIGKILL and
  power loss recover to an exact old-or-new artifact/manifest pair. Without
  `--unique`, any missing or ambiguous output rejects the entire selection;
  source-missing, independently prebuilt, source-different, and command-missing
  rows are refused by the same preflight. With `--unique`, every refusal class
  is explicitly counted and left untouched while valid rows proceed. Repeating
  the command is safe. `--case`, `--suite`, and `--isa` provide bounded imports.
- `inventory.tsv` is the normalized execution contract owned by the shared
  runner. Rust and retained C must consume it rather than rediscovering cases.
- `priority.py` joins active retained-C passes to explicit `SYS_*` and `__NR_*`
  source evidence and the audited production syscall inventory. It writes the
  deterministic `priority.tsv` and `COMPAT_PRIORITY.md` planning views without
  inferring hidden libc calls or consuming Rust runner failures.

Refresh and verify the prioritization views with:

```text
python3 tests/runtime/priority.py
python3 tests/runtime/priority.py --check
PYTHONPATH=tests/runtime python3 -m unittest tests/runtime/priority_test.py
```

The current imported build plan contains 3,131 rows: every one of the live C
engine's 3,101 case/ISA legs plus 30 explicit Rust-local rows. The C corpus has
2,954 macOS-active legs and 3,073 Linux-active legs. All 3,103 rows runnable on
at least one supported host are pinned; only 28 global `excluded-known-bug`
rows remain unbuilt. The execution inventory is the 3,113-row cross-host union:
3,103 corpus artifacts plus ten bootstrap rows. Host disposition selects 2,996
rows on macOS and all 3,113 on Linux. `report/C_INVENTORY_GAP.md`, regenerated
from the live C manifests, is the authoritative reconciliation and reports zero
missing C legs. Artifact and fixture generation reject a missing, orphaned, or
duplicate key, so the execution denominator cannot silently shrink.

The blocking smoke integration lives at `src/app/hl-engine/tests/compat.rs`.
An app-owned Rust supervisor starts one worker process per row, captures its
standard streams, enforces a timeout, and contains crashes. The worker stages
the persistent C guest and a relative side input through the public typed
`runtime::Builder`, calls start/wait/destroy, and proves workspace cleanup.
Repeated lifecycle coverage checks that state does not survive into a new run.

Run the full API inventory, or a bounded slice, with:

```text
HL_COMPAT_JOBS=1 cargo test --offline --locked -p hl-engine --test inventory inventory_matrix -- --ignored --nocapture
HL_COMPAT_ISA=aarch64 HL_COMPAT_SUITE=bootstrap cargo test --offline --locked -p hl-engine --test inventory inventory_matrix -- --ignored --nocapture
```

Execution defaults to one worker. Each worker starts in its own POSIX process
group, is given its row's wall-clock deadline, and is terminated as a group with
TERM followed by KILL. Linux stall detection samples both descendant CPU ticks
and capture growth; `HL_COMPAT_STALL_MS` can lower the default 60-second stall
budget for diagnosis. A stall budget at least as large as the row deadline is
inactive. Hosts without a trustworthy CPU source retain the wall-clock guard.
Stdout has a 1 MiB termination threshold and stderr has an independent 64 KiB
threshold, so a broken guest cannot keep logging for the full row deadline.

Long runs can be split into persistent, fingerprinted batches:

```text
HL_COMPAT_RESUME=1 HL_COMPAT_BATCH=100 HL_COMPAT_JOBS=1 \
  cargo test --offline --locked -p hl-engine --test inventory inventory_matrix -- --ignored --nocapture
```

Repeat the same command until the canonical report is written. Completed rows
are flushed and synchronized to the report's `.partial.tsv` sibling after every
case, and an exclusive host lock rejects a concurrent owner. Resume drops only
a torn final non-newline record. It rejects interior corruption or changed
inventory, binaries, guest/golden/resource bytes, host architecture, settings,
filters, and duplicate rows instead of combining unlike runs. Use an absolute
`HL_COMPAT_REPORT` path when launching Cargo from an uncertain working
directory.

The unfiltered execution result is `report/api-results.tsv`. Filtered runs write
deterministic `api-results--<filters>.tsv` siblings unless `HL_COMPAT_REPORT`
selects an explicit path, preserving the canonical combined report. The current
full measurement is regenerated from the authoritative `fixture-schema.tsv`.
Executable, in-engine multiprocess, side-file, directory-tree, entry-symlink,
and the four concrete rootfs categories execute through typed setup. A
`multi-process-service` row launches one C guest in one engine instance; fork,
clone, pthread, wait, and exec descendants are engine-owned guest tasks, just as
they are in the retained C runner. Device and network fixtures remain explicit
skips until their typed setup exists. This
expectation-based result
corrects the earlier “no-fault” classification, which did not establish exit or
golden-output compatibility.

The archived reports are historical checkpoints, not a current combined
measurement. The newest complete typed AArch64 checkpoint is 182 pass / 568
fail / 377 explicit fixture skips across 1,127 rows. The newest documented
complete x86-64 checkpoint is 195 pass / 588 fail / 376 skips across 1,159
rows. A new bounded complete run is required before quoting one current total
for the 2,996-row execution inventory.

Seven scratch-rootfs rows use an owned empty root containing `/guest`, with guest
argv identity distinct from its confined host staging path. The other 21 rows
now execute too: six mapping-data rows stage a private `/data`, thirteen Alpine
rows stage a bounded pseudo-filesystem directory skeleton, and two dynamic rows
stage the ISA loader and libc. Every row reached guest execution and reported
workspace cleanup; none failed in worker setup. The resulting failures are now
engine evidence: AArch64 exposes filesystem/process semantic gaps and one
unsupported instruction, while x86-64 mostly reaches decode faults. The
The AArch64 dynamic guest now loads pinned libc and passes. Its bounded trace
validated rooted `openat`, private file-backed fixed mappings, and Linux's
ignored `MAP_DENYWRITE` compatibility flag without loader-specific behavior.
The x86-64 guest next reaches the unsupported `0f 31` instruction. The
4,423,312-byte dynamic closure is pinned under
`artifacts/runtime`, with SHA-256, size, mode, origin, and license verification
by `corpus.py verify`. Test execution does not consult an installed cross
sysroot.

## CMake

```text
cmake -S tests/runtime -B build/runtime -DHL_CORPUS_MODE=PREBUILT
cmake --build build/runtime --target compat-smoke
```

`PREBUILT` verifies and consumes the checked binaries. `REBUILD` requires both
Linux cross compilers, rebuilds every smoke fixture from the checked recipe, and
fails unless each result is byte-identical to its prebuilt. The
`compat-full-verify` target verifies the imported full artifact corpus when it is
present.

## Migration path

The importer copies manifests, sources, goldens, setup files, and exclusion
notes without renaming retained paths. New retained suites become rows through
their manifest rather than through new runner code. Nonstandard schemas remain
listed in `schema-report.tsv` until the normalizer gives them an explicit
adapter. Arguments, environment, dependencies, rootfs needs, and dispositions
belong in the normalized inventory so setup failures cannot be mislabeled as
engine incompatibilities.

Persistent compatibility binaries must be imported from the successful CMake
fixture build and pinned by SHA-256; they must not be independently rebuilt by
an ambient distro cross compiler. A reproducible rebuild is acceptable only
when it replays the exact Nix compiler wrapper and static-library closure from
the CMake command and proves byte equality before replacing a pin. Rows absent
from the current CMake graph are Rust-local additions or a stale C build and
remain separately classified until CMake owns them. Multiple C outputs from
one source require an explicit output identity; source-name guessing is not an
import contract. `import-cmake` also refuses source-byte drift between the CMake
graph and the retained Rust-side oracle copy. The existing `corpus.py import`
remains source/manifest normalization only, while `audit-cmake` prevents its
independently generated artifacts from being mistaken for the C oracle.
