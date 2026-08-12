# C-primary completion checklist

This checklist defines the evidence required before calling the C-primary
restoration complete. Architecture and routing are described in
[`src/runtime/native/README.md`](src/runtime/native/README.md) and
[`src/containers/hl-engine/RUST_EXECUTION_REACHABILITY.md`](src/containers/hl-engine/RUST_EXECUTION_REACHABILITY.md).

## Final source state

- One production guest executor exists: `src/runtime/native/retained`.
- Production selection is fail-closed C-only for the product, containers,
  direct workers, tests, and GUI; replacement candidates are unselected.
- No compile, link, Cargo, CMake, runtime, or packaging dependency reads a
  sibling checkout.
- The final integration branch is committed, merged into the primary checkout,
  and clean. Evidence records the exact commit and tree IDs.

## Archive

- After the final integration commit, copy the complete repository and Git
  history to `../engine_rust` without `target`, caches, generated build trees,
  benchmark scratch output, object files, or linked binaries.
- Verify the archive is a standalone repository: `.git` is a directory, it has
  no object alternates or worktree indirection, and its `HEAD` and tree match
  the final source checkout.
- Record the archive path, commit, tree, size, exclusion scan, and clean
  `git status` in the final evidence report. Re-run this check after every final
  source or documentation commit.

## Build and quality evidence

- Run `make gate` on the exact clean final commit. Preserve its exit status and
  environment fingerprint.
- Run the C source inventory, strict-warning compile groups, clang-format,
  clang-tidy, cppcheck, deterministic policies, and per-file diagnostics through
  `make lint-c`.
- Run both guest-ISA C ABI tests and the product receipt/fail-closed routing
  tests. Build, then copy binaries in a separate command; hash and smoke each
  copied binary before using it as evidence.
- Record application/GUI feature gates and the macOS build, signing,
  notarization, and packaging CI result. A Linux type-check is not a macOS run.
- Run the complete compatibility/corpus inventory, including suites normally
  ignored by the quick gate, and account explicitly for every expected failure.

## Performance acceptance

- Use the E/R/I procedure in [`tests/bench/ERI_MATRIX.md`](tests/bench/ERI_MATRIX.md):
  selected embedded C, pinned standalone retained/original C, and integrated
  product C.
- Use unique resumable ledgers, exact-output validation, crossed and balanced
  arm order, a same-binary null arm, an unaffected control, both relevant guest
  layouts, and the box lock/quiet protocol in `AGENTS.md`.
- Preserve hashes and smoke receipts for every measured binary and report every
  phase, not only Python, sqlite, and malloc.
- Acceptance requires Python, sqlite, and malloc each to be no more than 1.10x
  the faster valid C baseline. Historical Rust-versus-C measurements are context,
  not acceptance evidence for the final product.

## Final report

The final report links immutable raw ledgers and gate logs, states the exact
commit tested, lists disk use before and after cleanup, records any expected
failure, and distinguishes completed evidence from deferred work. A passing
commit message or an older branch result is not final evidence.
