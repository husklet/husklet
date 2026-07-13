# Commentary and historical prose audit — wave T (2026-07)

Documentation-only audit of comments and narrative text across runtime, frontends, tests, README and
website. A broad historical-marker scan returned 422 matches; most are legitimate regression, ABI, safety,
or performance rationale. The safe cleanup set below is deliberately narrow.

## Exact safe removal/update set

### Transient coordination prose — 49 lines

Across `dd-tests/src/cases/ext`, 49 lines contain “Owner: … agent”, “Edit ONLY this file”, or “Keep this
module compiling”. These describe temporary parallel work allocation, not durable ownership or a test
contract. Delete those clauses/lines while preserving each module's coverage and oracle explanation.
This includes modules beyond the 24 blanket-import files (`forkx`, completeness, `execfaultx`, `scratchx`,
and GPU render IR). Cargo registration and CODEOWNERS/AGENTS—not source comments—must own edit policy.

### Obsolete terminal architecture — 2 lines

- `dd-term-core/Cargo.toml:15-17` says `dd-term` is a winit+wgpu GPU shell.
- `dd-term-core/src/lib.rs:6` repeats “window + wgpu draw”.

The actual declared binary is GTK4/GSK/VTE. Replace these with one current statement in the crate docs and
remove the duplicated manifest narrative. This is misleading future architecture, not historical context.

### Refactor archaeology — 5 module-header lines

Remove “moved verbatim/former single-file/behavior unchanged” narration from:

- `dd-daemon/src/build/prune.rs:2-3`;
- `dd-daemon/src/build/steps.rs:3`;
- `dd-daemon/src/containers/exec/mod.rs:2-3`;
- `dd-daemon/src/containers/lifecycle/mod.rs:2-3`;
- `dd-daemon/src/containers/lifecycle/run.rs:2-3`.

Keep what each module owns. Git records the old file layout; “verbatim” becomes false as soon as the new
module evolves and explains no invariant.

### Product/help contradictions — 8 focused updates

- `dd-cli/src/cli.rs:79-87`: CUDA says “presence only, not compute” and both CUDA/GUI options point at
  `docs/ideas/*`; reconcile with current GPU integration and link stable user documentation.
- `dd-gui/src/bin/term.rs:1038,1076,1607,1685,1106`: shipped UI/help and comments point at the same internal
  idea documents and call framework acceleration “work in progress”. Replace user-visible strings with a
  current capability statement/URL; retain a developer link only if the target doc remains canonical.
- README/release/screencast `dd` commands conflict with declared `ddcli`; update together under rebrand.
- README/site `make bench` reproduction text becomes false after benchmark removal; delete the command
  promise rather than preserving an unusable historical instruction.

Count here is by logical text locations, not wrapped physical lines; the implementation diff may touch more
lines due to formatting.

### Stale route history — 1 comment block

`dd-daemon/src/system.rs:312-314` says system prune was “Previously unrouted”. Keep its present Docker
semantics, remove the historical sentence. Registration and tests now prove reachability.

## Comments that must remain

Do not bulk-delete “pre-fix”, “formerly”, or “used to” text. In regression cases these phrases identify the
failure mode that makes a verdict meaningful. Examples worth retaining include:

- x86 syscall-number collision and opcode fallback explanations in `dd-tests/src/cases/syscall.rs` and
  `regress.rs`;
- overlay/statfs/procfs expected failures in ext filesystem cases;
- forkserver warm-runner, pcache, ELF collision and exec-fault crash-shape explanations;
- archive streaming's former temp-file collision rationale;
- terminal UTF-8 parser and daemon health grace-window regression rationale.

Also preserve comments explaining Docker key spelling, serde/default compatibility, PTY/fork safety,
clean environment construction, memory ordering, lock scope, streaming instead of buffering, cache keys,
and performance scheduling. These explain why apparently simpler code is incorrect.

The scan's apparent TODO/XXX results are false positives: `XXXXXX` is the required `mkstemp` template and
`XXX`/`YYY` are test payload sentinels. There is no actionable TODO/FIXME/HACK inventory in this scope that
should be mechanically removed.

## Duplicated explanations to condense

- Image-store defaults and daemon setup are repeated across Rust and eight shell suites. Generate script
  help/default text from one sourced test-support contract, or keep a single canonical paragraph and link
  it; do not copy `/Users/x/...` examples.
- GUI terminal launch's clean-environment/fork-safety rationale appears near command construction and helper
  construction. Keep the full safety explanation at the helper and a short call-site reference.
- Extended case modules repeat builder syntax and “keep compiling” boilerplate. Put builder conventions in
  `dd-tests/src/cases/ext/mod.rs` module docs; individual files should describe only unique coverage.
- README, website, release body and screencast instructions duplicate install commands. Define canonical
  release/user commands in one maintained source or at minimum add a command-consistency test.

## Documents/artifacts suited to generation

Generate or validate data-heavy inventories, not design judgment:

- rebrand token/env tables from repository search plus a small annotation file;
- fixture source/binary hash manifests and website media manifests (wave Q);
- route/method inventory from `routes.rs` and client public methods;
- dd-tests guest/reference classification TSV from tracked files and explicit annotations;
- website repeated navigation/footer markup from a tiny static template only if deployment continues to
  commit the rendered HTML, preserving zero-build GitHub Pages behavior.

Do not generate `engine.md`, Chrome fix plans, rendering goals, ABI/safety rationale, or audit conclusions.
Those require human judgment and should instead be condensed into canonical current documents with stale
rows removed.

## Quantified outcome

The immediately safe text set is **57 physical/logical lines plus 8 focused help locations**: 49 transient
coordination lines, 2 obsolete architecture lines, 5 refactor-archaeology header lines, and 1 stale route
history block. The focused help updates are migration-coupled and should not be deleted without replacement.
The remaining historical matches are retained unless an individual correctness contract is migrated.
