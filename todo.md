# Husklet production migration

This is the authoritative checklist for the C-primary production migration. Checked items have current repository evidence. Unchecked items remain required; “in progress” means an active lane owns the work but completion is not yet proven on the merged tip.

## Repository recovery and ownership

- [x] Preserve the prior Rust repository in `../engine_rust`, excluding build artifacts. The recovery tree exists and is independent of production builds.
- [x] Keep `../engine` read-only and remove build/runtime dependency on both sibling repositories. Current `cargo metadata`, normal/build dependency trees, and repository-escape scan contain no sibling dependency.
- [x] Make Husklet the active repository and retain its container, workspace, terminal, GUI, daemon, logging, image, and lifecycle surfaces.
- [x] Remove obsolete Rust runtime crates replaced by the C engine. Current Cargo metadata and the filesystem contain only `src/runtime/hl-native` under `src/runtime`; deleted runtime package names are absent from `Cargo.lock`.
- [x] Delete obsolete duplicate native executor, differential production selector, retained directory, fixtures, manifests, provenance hashes, and source bookkeeping.
- [x] Remove `tests/runtime/legacy` and its stale/generated compatibility corpus.
- [x] Remove all tracked Python tooling and Python build dependencies. Python remains only as a guest workload to test compatibility.
- [x] Remove Make and CMake build frontends. Cargo is primary; Nix pins the toolchain and system dependencies.
- [x] Keep only intentional Markdown: `AGENTS.md`, `README.md`, this checklist, and the positive/negative lint examples; fold the former pipeline proposal into this checklist.
- [x] Restore and update root `AGENTS.md` for the Cargo-owned C architecture.
- [x] Restore `lint/examples/positive.md` and `lint/examples/negative.md` as executable design documentation.
- [x] Run a final unused-file, unused-package, reachability, and generated-artifact audit on the merged tip. Current evidence covers all 18 Cargo packages and 74 targets, reaches all 310 C files across the four modeled host closures, finds no unused direct dependencies, and finds no tracked build artifacts, ignored files, empty files, or broken links.

## `hl-native` package and C boundary

- [x] Move the production C engine into `src/runtime/hl-native/src/native`.
- [x] Make `src/runtime/hl-native` a normal Cargo package with `Cargo.toml`, `build.rs`, `src`, and package tests only.
- [x] Compile one private shared C engine through `build.rs` into Cargo `OUT_DIR`; do not mutate or commit `bin/` during ordinary builds.
- [x] Keep all C and headers inside `hl-native/src/native`; keep Rust bindings/wrapper under `hl-native/src`.
- [x] Expose a slim opaque Rust API around the C engine and isolate/document every unsafe ABI boundary.
- [x] Route `hl-engine` lifecycle through `hl-native` and remove dependencies on deleted Rust runtime packages.
- [x] Give `hl-native` a configured zero-local-dependency budget in `lint.toml`.
- [x] Install the Cargo-built private shared library beside packaged products with relocatable Linux/macOS loader paths and portable artifact naming.
- [x] Prove installed-product execution on Linux from a fresh copied prefix, including sibling-library selection, relocatable RUNPATH, backend receipts, and artifact hashes.
- [x] Prove authoritative packaging/install behavior on macOS ARM64 artifacts.
  The native `aarch64-darwin` `installed-product` and `host-darwin-aarch64-native`
  flake checks passed together on commit `ef4ad33f0`: the Cargo-built dylib and
  all three launchers are exact ARM64 Mach-O artifacts, copied-prefix loader and
  deterministic hash-bound receipt contracts pass, removing the sibling dylib
  fails closed, and strict C/C++ consumers compile, link, and execute against it.
- [ ] Prove authoritative packaging/install behavior on Windows AMD64 artifacts.
  Local Nix checks fully compile/link Linux ARM64 and AMD64. They also compile the
  Windows GNU Rust surface and host bridge and link a PE/COFF ABI fixture DLL plus
  import library. This is not MSVC ABI, Windows SDK, complete engine-DLL, loader,
  or runtime evidence. Darwin cross-compilation remains unavailable on Linux until
  an Apple SDK/framework-stub closure can be packaged lawfully and reproducibly;
  native macOS ARM64 CI is authoritative meanwhile.
  Native Windows CI additionally compiles the public API as strict C and C++ and
  pins Win64 layouts, function signatures, and C linkage. Its executed DLL is a
  fixture only; it does not claim that the complete engine DLL links or runs.
  Linux cross checks also link strict C and C++ LP64 consumers against each
  target's actual shared engine without executing foreign binaries. Native
  macOS CI links and runs the same consumers against its Cargo-built dylib;
  this proves public ABI loading, not Linux-guest compatibility behavior.
  Each Linux ISA cross-check also requires the same exact thirteen-symbol
  dynamic export set and SONAME, preventing ARM64/AMD64 ABI drift without
  claiming that a foreign-target library was executed.
  The Windows GNU cross-check pins the host bridge and both public-header
  consumers to x86-64 COFF, so a 32-bit object cannot satisfy the AMD64 lane;
  it also compiles every Windows host-service translation unit and combines
  them into one exact x86-64 COFF object, and compiles the POSIX compatibility
  implementation with the same forced prelude the Cargo build uses. It remains
  compile/link evidence rather than a complete engine DLL or Windows runtime evidence.
  That lane cross-checks both `hl-native` and its `hl-engine` Rust consumer so
  Windows-only type or composition drift cannot hide behind a leaf-crate build.
  Native macOS CI requires the installed dylib and all three launchers to be
  ARM64-only Mach-O artifacts before exercising their loader and receipt
  contracts; a wrong-architecture or accidental universal artifact fails.
  The dedicated native-Darwin host gate compiles, links, and executes strict C
  and C++ public-header consumers against the packaged ARM64 dylib; it is not
  an alias for the broader workspace verification derivation.
  Linux installed-product launchers carry `$ORIGIN/../lib` first and permit
  only immutable Nix-store library directories afterward; appended relative,
  host, or writable search directories fail even when the sibling comes first.

## Host architecture

- [x] Split Linux’s 5,000-line host implementation into capability-owned files.
- [x] Organize Linux under `context`, `handles`, `fs`, `process`, `io`, `memory`, `network`, `time`, `sync`, and logging owners.
- [x] Keep Linux `host.c` limited to registration/assembly.
- [x] Integrate the macOS capability hierarchy onto the current native layout and verify its source/build selection on the merged tip.
- [x] Obtain authoritative macOS ARM64 compilation and runtime evidence from a native macOS host.
- [x] Restore the Windows AMD64 source backend, isolate its Cargo source closure, and wire its DLL/import-library boundary into the current native layout.
- [ ] Replace the Linux-specific bridge lifecycle and descriptor imports with Windows host-service handles before enabling Windows support.
- [ ] Prove Windows runtime behavior; previous oracle evidence was incomplete and is not production evidence. (Windows implementation is only priority if you are on windows and have powershell and tools.)
- [x] Eliminate the five single-file-directory findings without adding ceremonial siblings.
- [x] Make the generic catch-all rule recognize contextually owned `memory/shared.c` while still rejecting ambiguous shallow or sibling-less catch-all sources.

## Supported platform and execution matrix

- [x] Model Linux ARM64, Linux AMD64, macOS ARM64, and Windows AMD64 host targets in `hl-native`.
- [x] Model both ARM64 and AMD64 Linux guests.
- [x] Correctly report the optional same-ISA transliterator as Linux AMD64-only.
- [x] Merge the host-capability matrix and host-specific source selection onto the shared-library/Windows tip.
- [ ] Verify every supported target through authoritative platform CI.
- [ ] Compare ARM64 and AMD64 scheduler paths whenever either is changed; neither architecture may silently lag.

## Generic ELF and compatibility

- [x] Move generic ELF inspection and executable-authority planning into `hl-native`; remove dependence on deleted Rust loader/runtime crates.
- [x] Remove executable provenance/hash pinning and application-specific interpreter bookkeeping tests.
- [x] Validate ELF metadata, program-table bounds, load spans/alignment, interpreter uniqueness, and executable entry authority before unsafe C loading.
- [x] Audit production native sources and confirm ELF behavior is generic rather than selected by Go, V8, Node, Python, or executable names.
- [ ] Finish generic ET_EXEC/non-PIE address placement and translation without Go-, V8-, Node-, Python-, sqlite-, or executable-name detection.
- [ ] Prove PIE, static PIE, non-PIE, Go, V8/Node, Python, sqlite, self-modifying code, signals, threads, writes, atomics, and both guest ISAs.
- [ ] Diagnose and repair the deterministic AMD64 static ET_EXEC/thread/TLS compatibility cluster.
- [ ] Run the full compatibility corpus on the final C-backed product and classify every remaining failure.

## C source structure cleanup

- [x] Split checkpoint implementation into capability fragments below the file-size threshold.
- [x] Split filesystem syscall implementation and make that subtree lint-clean.
- [x] Split event syscall implementation and make that subtree lint-clean.
- [x] Split IO syscall implementation into capability files below 1,500 lines.
- [x] Split network namespace implementation below 1,500 lines.
- [x] Begin sentry decomposition with isolated control operations.
- [x] Decompose `svc_fcntl`, `svc_read`, and `svc_write` below the configured function and nesting limits.
- [x] Decompose checkpoint image capture, descriptor restore, and resource restore functions below the configured limits.
- [x] Decompose the rare syscall dispatcher into capability handlers below the configured limits.
- [x] Finish netns ancillary/service function decomposition below all configured C structure limits.
- [x] Split `container/vfs.c` into capability-owned unity fragments below the file threshold and decompose its synthetic-stat/proc-content hotspots.
- [x] Split `syscall/binding.c` into capability fragments and reduce `bound_route` to an ordered family router below all limits.
- [x] Split sentry service, lifecycle, marshalling, copy-back, and worker routing below all configured C structure limits.
- [x] Split memory, network, process, rare, signal, SysV, and time syscall domains below all configured C structure limits.
- [x] Split Linux ABI ELF and thread support below all configured C structure limits.
- [x] Split Linux ABI context, fork monitoring, socket ABI vocabulary, and number translation below the configured limits.
- [ ] Split ARM64 interpreter/translator units and oversized functions.
- [ ] Split AMD64 AVX, interpreter, translator, legacy, crypto, move, and shift units/functions.
- [ ] Refactor remaining oversized test C functions rather than suppressing them.
- [ ] Reach zero `c-file-length`, `c-function-length`, and `c-maximum-nesting` findings.

## Generic reusable linter

- [x] Organize linter rules into Rust, C, repository, and support domains.
- [x] Remove Husklet package/business literals from reusable dependency analysis.
- [x] Rename root policy to `lint.toml` and retain the declarative layer dependency matrix.
- [x] Remove the stale explicit per-package dependency-edge ledger.
- [x] Enforce package/runtime/container/workspace/application dependency direction from configuration.
- [x] Embed tree-sitter C; do not shell out to parsers or depend on Python.
- [x] Add generic C file-length, function-length, and nesting rules.
- [x] Add strict generic `// hl-lint: allow(rule-id) -- reason` validation for exceptional C findings.
- [x] Add generic catch-all source-path, empty-directory, single-file-directory, and redundant-parent-filename rules.
- [x] Add a generic documentation inventory/example contract.
- [x] Add a generic detached-constructor rule: free constructors returning `T`, `Option<T>`, or `Result<T, E>` belong on `T` when ownership is proven.
- [x] Move detached constructors reported by the live scan onto their owned result types, including `Schedule` and `Sample`.
- [x] Fix the remaining Rust nesting finding; merged-tip self-lint reports zero `maximum-nesting` errors.
- [x] Repair stale linter self-test expectations and make `cargo test -p hl-design-lint --lib` fully green.
- [ ] Run the full linter to zero findings on the final merged repository.

## Testing ownership and fixtures

- [x] Move native fixtures and native tests into the testing application or root compatibility suites as appropriate.
- [x] Move syscall audit ownership into `src/apps/testing`; delete the runtime audit package.
- [x] Remove interpreter hash/license/include-string bookkeeping tests.
- [x] Remove Python ERI benchmark adapters and obsolete Python tests.
- [x] Replace unexplained zero-byte golden files with typed empty-output assertions.
- [x] Add leak-sanitizer build support and testing leak integration.
- [x] Restore testing compilation against only the new `hl-native`/`hl-engine` public APIs and remove obsolete deleted-Rust-engine adapters.
- [x] Expose and execute the native C leak non-vacuity probe through the slim `hl-native::Native` boundary.
- [x] Add an independent Linux Valgrind Memcheck gate for bounded non-JIT executable-authority lifecycle tests, with a deliberate 4,096-byte native leak proving non-vacuity.
- [ ] Audit every package/root test for correct unit, package integration, compatibility, or testing-app ownership.

## Quality gates

- [x] Register C lint and structure checks in the standard Rust linter.
- [x] Preserve strict C compiler warnings and Cargo-owned source discovery.
- [x] Integrate clang-format verification, clang-tidy, and cppcheck without Make/CMake. Cargo owns C compilation and the embedded tree-sitter rules; Nix supplies Bear, clang-format, clang-tidy, and cppcheck, and the normal verification derivation runs them through `hl-design-lint` with Cargo's real compilation database and file-level diagnostics.
- [x] Classify and fix the 17 path-sensitive reports found by `scan-build` 21.1.8. Current ARM64 and AMD64 analyses both report zero findings using target-matched Nix compiler/sysroot closures.
- [x] Make zero-finding ARM64 and AMD64 `scan-build` analysis a bounded Nix hard gate using target-matched compiler/sysroot closures and disposable analyzer outputs.
- [ ] Make the complete `hl-design-lint` suite green.
- [x] Run `cargo check --workspace --all-targets` successfully on the integrated C-backed workspace; rerun on the final delivery tip.
- [x] Run `cargo test --workspace --lib --bins` successfully after restoring the C-backed engine/testing applications; rerun again on the final delivery tip.
- [ ] Run `cargo test -p hl-native --all-targets` and all C executable/compatibility tests on the final merged tip.
- [ ] Run feature-gated application Clippy checks and Nix flake checks on the final merged tip.
- [ ] Obtain macOS CI evidence for platform-specific application/native code.
- [ ] Obtain Windows CI compile/link/ABI smoke evidence and runtime evidence.

## Performance and production readiness

- [x] Restore a typed Rust benchmark harness with hashed and smoke-tested artifacts, exact-output checks, balanced scheduling, qualified nulls, unique resumable ledgers, bounded waits, two guest layouts, complete phases, and per-row host load.
- [ ] Freeze reproducible original standalone-C and integrated-C baseline binaries; record hashes and smoke-run each copied binary for the acceptance campaign.
- [ ] Run balanced-order, unique-ledger, box-locked benchmarks on at least two guests and report every phase.
- [ ] Benchmark Python, sqlite, and malloc against the faster original/retained C baseline.
- [ ] Meet the final requirement: Husklet no more than 10% slower than the faster C baseline.
- [ ] Profile only after compatibility/build stability; validate profile hypotheses with mutations, not self-time alone.
- [ ] Preserve lifecycle/engine observability without hot-path logging overhead.
- [ ] Verify no dependency on `../engine` or `../engine_rust` at build or runtime.
- [ ] Run final disk/artifact cleanup while preserving `../engine_rust` recovery evidence.
- [ ] Complete a requirement-by-requirement production audit and mark the migration complete only when every item above has authoritative evidence.

## Testing pipeline redesign — low priority, do last

Do not begin this section until C-primary production readiness, compatibility, cleanup, and performance acceptance are complete. These tasks preserve the former `tests/PIPELINE.md` proposal as actionable work.

### Typed providers and orchestration

- [ ] Implement one resolved `ProviderConfig` for host, Docker, QEMU, and engine providers; parse CLI/environment input once and serialize secret-free configuration into provenance.
- [ ] Define capability discovery and typed unavailable verdicts for image preparation, sessions, compilation, artifact installation, spawn, wait, capture, inspect, reset, and stop.
- [ ] Keep `testing` responsible only for E2E selection, orchestration, comparison, timing, and reporting; reusable provider mechanics remain with their owning packages.
- [ ] Permit host execution only through a bounded `hl-process` adapter for external compilers, linkers, and QEMU; prohibit shell/Docker CLI provider implementations.
- [ ] Select compilation and execution providers independently and record effective translation stacks so dependent implementations are not counted as independent oracles.
- [ ] Add separate oracle-authority policy, explicit multi-provider agreement, and `ORACLE_DIVERGENCE` results.

### Immutable cache and identities

- [ ] Implement a digest-addressed `.cache/testing` store for OCI data, root filesystems, artifacts, outputs, provider receipts, locks, and staged publication.
- [ ] Store large bytes once, preserve/share OCI layers and read-only roots, and give runs ephemeral writable overlays.
- [ ] Publish atomically, protect active jobs with leases, and implement reachability garbage collection with separate result/artifact age and size budgets.
- [ ] Define complete build identities covering inputs, arguments, environment, ISA/ABI, provider implementation, toolchain/sysroot, and build-image digest.
- [ ] Define complete golden/result identities covering artifact, run configuration, provider/kernel/ISA/image/runtime identities, normalization, and measurement protocol.

### Commands, goldens, and comparisons

- [ ] Implement typed `testing compile`, `golden`, `run`, `compare`, `pipeline`, and `cache status|verify|prune` commands with one selector grammar.
- [ ] Support stable selection before sharding, ISA/class/jobs/shard/offline/refresh controls, independent artifact providers, and independently resumable stages.
- [ ] Make argument-free `testing` run the checked-in default profile and emit one machine-readable summary with stable exit meanings.
- [ ] Capture immutable golden observations with bounded output/state, exit/signal, provenance, raw samples, and normalizer identity without rewriting sources.
- [ ] Classify every checked-in golden mechanically as generated, externally authored with reason, or temporarily legacy.
- [ ] Implement behavior comparison plus exact and normalized-ELF artifact comparison; always report raw artifact digests.

### Timing, isolation, and evidence

- [ ] Record setup, steady-state execution, and in-guest payload separately; retain raw samples and report minimum, median, p90, p99, coefficient of variation, and setup.
- [ ] Bind results/resume to engine profile and runner SHA-256; reject mismatched profiles and rebuilt-engine rows.
- [ ] Warm provider services while preserving a fresh image view, container, process tree, and writable state per compatibility case; evict uncertain state after failure.
- [ ] Implement persistent framed benchmarks without allowing persistent benchmark state to become compatibility evidence.
- [ ] Prove native execution with a separate diagnostics-on probe, time with diagnostics off, reject `HL_NATIVE_*` as guest environment, and record diagnostics plus phase context.
- [ ] Implement warmed provider/ISA workers, parallel sweep, and automatic isolated reruns of failures, crashes, timeouts, and unexecuted rows while preserving both attempts.
- [ ] Record all planned and inactive cases as bounded results/`NOT_RUN`; never infer results for work not executed.
- [ ] Support process, container, engine, provider, and exclusive isolation with bounded setup, execution, teardown, and descendant reaping; never weaken requested isolation.
- [ ] Use private guest build directories, record normalized host load, bound diagnostics, report first differing byte context, and avoid per-case full root/image copies.

### Profiles, migration, and remaining scenarios

- [ ] Implement default, pull-request, main, nightly, and release profiles through the same binary, including compatibility, oracle refresh, performance, cross-host/ISA, nested, scenario, and packaging matrices.
- [ ] Implement typed `testing nested prepare|run` with locked offline Cargo, complete identity, per-key publication locking, SHA-256 receipts, and mandatory current-run execution.
- [ ] Migrate in order: provider records; compile/run split; generated goldens; behavior/ELF comparisons; warmed timing; profiles/sharding/resume/cache maintenance; golden classification.
- [ ] Remove legacy golden-update and unconditional-recompile paths only after inventory and focused parity prove every case remains represented.
- [x] Move Docker build/network/compose/multi-network orchestration into a real Cargo target and remove the duplicated detached PTY workflow.
- [x] Preserve typed Docker system, image, runtime, attach/exec, update, event, commit, export, volume, network, metadata, prune, and root-filesystem-import contracts.
- [ ] Add typed interactive terminal input, live address routing, build/cache/concurrency/multistage/run-mount/result integration, and true daemon/client/CLI workflow actions.
- [ ] Give E2E actions bounded workspaces/streams, isolated daemon/socket lifecycle, typed client selection, substitutions, named resources, unconditional cleanup, readiness, diagnostics, and ordering.
- [ ] Delete detached workflow domains only after mapping each behavior and proving its committed replacement.

### Coverage-guided dead-code audit

- [ ] Merge LLVM source coverage from representative production workloads into per-file, function, region, and line reports, including subprocess profiles.
- [ ] Collect link-time discarded-symbol/section evidence and distinguish code absent from production binaries from compiled-but-unexecuted code.
- [ ] Audit Cargo metadata, features, targets, packages, and dependencies for unused production ownership.
- [ ] Cover both ISAs, interpreter/native, lifecycle/process/signals/faults, filesystem/network/terminal/Docker, checkpoint/restore, failure cleanup, nested execution, and supported hosts.
- [ ] Produce a reviewed deletion-candidate report; never delete code solely because one workload cohort did not execute it.
