# Husklet production migration

This is the authoritative checklist for the C-primary production migration. Checked items have current repository evidence. Unchecked items remain required; “in progress” means an active lane owns the work but completion is not yet proven on the merged tip.

## `hl-native` package and C boundary

- [ ] Prove authoritative packaging/install behavior on Windows AMD64 artifacts.
  Local Nix checks fully compile/link Linux ARM64 and AMD64. They also compile the
  Windows GNU Rust surface and complete C engine into an x86-64 PE/COFF DLL plus
  import library, with the exact thirteen-symbol public export set. This is not
  MSVC ABI, Windows SDK, loader, packaging, or runtime evidence. Darwin
  cross-compilation remains unavailable on Linux until
  an Apple SDK/framework-stub closure can be packaged lawfully and reproducibly;
  native macOS ARM64 CI is authoritative meanwhile.
  Native Windows CI additionally compiles the public API as strict C and C++ and
  pins Win64 layouts, function signatures, and C linkage. It does not yet load or
  execute the complete engine DLL.
  Linux cross checks also link strict C and C++ LP64 consumers against each
  target's actual shared engine without executing foreign binaries. Native
  macOS CI links and runs the same consumers against its Cargo-built dylib;
  this proves public ABI loading, not Linux-guest compatibility behavior.
  Each Linux ISA cross-check also requires the same exact thirteen-symbol
  dynamic export set and SONAME, preventing ARM64/AMD64 ABI drift without
  claiming that a foreign-target library was executed.
  The Windows GNU cross-check pins the host bridge and both public-header
  consumers to x86-64 COFF, so a 32-bit object cannot satisfy the AMD64 lane;
  it also compiles every Windows host-service translation unit, compiles the
  POSIX compatibility implementation with the same forced prelude the Cargo
  build uses, and links those archives into the complete GNU Windows engine DLL.
  It remains compile/link evidence rather than Windows loader, packaging, or
  runtime evidence.
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

- [ ] Replace the Linux-specific bridge lifecycle and descriptor imports with Windows host-service handles before enabling Windows support.
- [ ] Prove Windows runtime behavior; previous oracle evidence was incomplete and is not production evidence. (Windows implementation is only priority if you are on windows and have powershell and tools.)

## Supported platform and execution matrix

- [ ] Verify every supported target through authoritative platform CI.

## Generic ELF and compatibility

- [ ] Finish generic ET_EXEC/non-PIE address placement and translation without Go-, V8-, Node-, Python-, sqlite-, or executable-name detection.
- [ ] Prove PIE, static PIE, non-PIE, Go, V8/Node, Python, sqlite, self-modifying code, signals, threads, writes, atomics, and both guest ISAs.
- [ ] Make metadata-only VFS traversal authority-safe and complete: retain exact
  overlay objects through `PATH_ONLY` handles without read permission or
  pathname reopening; enforce ancestor search, noexec, `AT_EMPTY_PATH`, real vs
  effective credentials/capabilities, and invalid `faccessat2` flags.
  Merged `e7daf35ef`..`0d68e34cc` supplies object-pinned metadata traversal,
  absolute/chained guest symlinks, correct named-path DAC/credential/noexec
  handling, and inspectable `O_PATH|O_NOFOLLOW` symlink descriptors. The
  focused matrix passes both ISAs, `hl-native --all-targets` and C lint are
  green. Merged `c06be85f8` retired the unreachable opaque tree-provider
  transport instead of extending its incomplete UID/GID wire, while preserving
  the exported bridge ABI as a fail-closed compatibility tombstone. Merged
  `03a53261e` now enforces virtual search on every traversed ancestor, follows a
  trailing-slash final symlink even under `AT_SYMLINK_NOFOLLOW`, and explicitly
  authorizes the namespace-correct synthetic `/proc/<pid>` ancestor before
  terminalizing `/proc/*/exe`. Exact merged-tip debug artifacts (runner
  `713e1c99…`, library `73ca91b2…`) pass the expanded `faccessat-flags` case on
  ARM64 and AMD64, and `hl-native --all-targets` is green. Remaining work is
  `AT_EMPTY_PATH` noexec provenance stored on the OFD and propagated through
  dup/fork/SCM/checkpoint.
- [ ] Diagnose and repair the deterministic displaced ET_EXEC/thread/TLS compatibility clusters on both guest ISAs.
  Merged-tip `0b5404c69` closes the ARM64 `pairatomics-nonpie` soft-exclusive gap by folding low ET_EXEC addresses before bus/soft guards; the exact case and adjacent non-PIE/signal cases pass, removing the fold reproduces signal 11, and `hl-native --all-targets` passes with 74 active tests and 4 intentional ignores.
- [ ] Run the full compatibility corpus on the final C-backed product and classify every remaining failure.
  The immutable `096bfe182` release corpus recorded 3,297 rows: 3,226 active rows passed, 58 failed, and 13 were explicitly `NOT_RUN`. The unique ledger is `target/testing/runtime/results-096bfe182-20260814T2055.tsv` (SHA-256 `159455059f5e1bd004031849790e69d94d550d82afb7e7bea7a426920b35e4a8`); its runner was `d59f2da56d98f3030a3263a3fe2a3a5ecf06ff0af9278860bbce4e17ee3ced86`, its private library was `d2da2d3bb94d7ee37067709c98cb9e09aa103ae1a8b1bef6437436bf6cce0c1a`, and the staged artifact passed `hl-native-artifact-smoke-v1` before and retained both hashes after execution. Mutable images, builds, workers, state, and 58 retained failure overlays live under `/var/tmp/husklet-runtime/corpus-096bfe182-20260814T2055`, eliminating the prior host `EMFILE`/loader cascade. This is valid compatibility evidence, not final-tip closure: 2,590 rows ran above the declared host-load threshold, so load-sensitive failures and timeouts require isolated reruns before classification, and every real failure still requires repair and a new immutable full-tip sweep.
  Current-tip targeted reruns additionally prove 19 of the baseline's 22 AMD64
  soft-span-signature timeouts closed by `b4b5c65bb`, including 14 cases rerun
  together with immutable runner/library hashes. `abi-corpus-x_vex_sse2int` and
  Current-tip immutable reruns close the historical AMD64
  `abi-corpus-x_vex_sse2int` timeout: the exact case passes 3/3 in
  5.268--6.248 seconds and four adjacent VEX/AVX cases pass, while the immutable
  historical `64606792f` artifact still reproduces its 30-second timeout under
  quiet locked load. They also close the stale `publication/shared-writeback`
  classification: both ISAs pass 3/3
  in 4.990--5.659 seconds, adjacent `publication/multi-view` passes both ISAs
  in 5.169--5.641 seconds, and no processes or mounts survive cleanup. Together
  with the six filesystem-policy and six `O_PATH`/
  sentry-fchdir closures, 31 of the original 142 failures now have focused
  closure evidence, leaving 111 baseline failures before a new full-tip corpus.
  The exhaustive classification is preserved at `/var/tmp/failure-clusters.tsv`.
  Merged commit `49acf42ef` also activates five isolated scratch-rootfs rows;
  all five pass the integrated engine and QEMU oracle with non-vacuity evidence.
  Merged commits `8842362c6` and `7741fc3bd` close both-ISA signalfd
  copyout-edge failures and the AMD64 real-time signal ordering failure: a
  failed signalfd destination no longer drains its queued signal, and a normal
  handler return is no longer mistaken for siglongjmp before sigreturn restores
  deferred delivery state. Exact focused runs pass both ISAs.
  The current merged tip also has focused, mutation-backed closure evidence for
  epoll hangup reporting, recognized `unshare` refusal, fileless ELF segments,
  bound-descriptor execution, overlay lock identity, cursorless jailed `fchdir`,
  pending timer-signal interrupts, interruptible SysV waits, filesystem-neutral
  memfd controls, IPv6 UDP source-family reconstruction, collision-free POSIX
  queue fixtures, displaced ARM64 DC ZVA stores, and bounded fork-child
  diagnostic publication, plus eventfd vector routing and zero-length iovec
  address validation. Last-thread `_exit` now also retires process-scoped
  descriptor, accounting, registry, locking, and IPC state; the exact
  fork-reclamation fixture passes both ISAs and the pre-fix path exhausts the
  descriptor-visibility arena near fork 160. Both ISAs also match native/QEMU
  error precedence for final-dot `rmdir`, trailing-slash `rename`, and directory
  `truncate`; staged guards independently close each prior mismatch. These
  Exact tip `7021bde3f` now has a fresh immutable release artifact (runner
  `91098bde83f6cd4d2ccfe221b03399c4357b57c1aa15978c0b5f57a655114676`,
  library `32fb1fc4839296df9e564ef805e9e3533d37d0b7f2bae1048fa40ce91d87f350`)
  whose first correctness wave passes exit, real guest fork/wait, job-control
  lifecycle, malloc, lower-file authority, and Python JSON on both ISAs plus
  sqlite on ARM64. A second both-ISA wave passes default signal exit codes,
  exit teardown, nested fork, fork during blocked I/O, canonical PTY, PTY job
  signals, allocator reclamation, and Python standard-library behavior. Across
  both waves: 29 unique ledgers, zero failures. Merged `c198f5da9` closes the
  AMD64 sqlite gap generically by giving each guest compiler its matching Nix
  sqlite headers/static archive and enabling the existing fixture on both ISAs;
  QEMU oracles, exact integrated runs, deliberate golden mutation, and 49
  focused tests pass. There is still no runtime scenario exercising actual
  engine/container checkpoint capture-and-restore; `ltp_checkpoint` is only
  futex synchronization.
  Merged `b4e4572e0` fixes a proven adapter omission: every configured
  checkpoint transport now projects `HL_CHECKPOINT=1`, while restore startup
  additionally projects `HL_RESTORE=1`; removing the flag makes the focused
  plan test red. This is necessary wiring, not runtime closure. A real
  container-rootfs three-process fixture reaches READY, but default SentryOnly
  explicitly refuses the P3 sentry/untrusted split and Sandbox::Disabled still
  ends capture with `WaitFailed`; direct `hl-engine` round trips remain green.
  Merged `66bc85352` proves a plain Engine followed by a checkpoint Engine
  captures and publishes a manifest on both ISAs, excluding process-global
  initialization as the production timeout cause; diagnosis is now comparing
  the real box-backed spawn/capture path with the green direct spawn path.
  Merged `99acd042b`..`fb1d89be0` makes failed checkpoint publication
  transactional: image staging and engine claims are aborted, transaction
  ownership is token-scoped and fenced across providers, cleanup deadlines are
  cooperative, and recovery completion cannot race admitted mutations. On the
  replayed current-main integration tip, `hl-engine` passed 89/89,
  `hl-container` passed 218/218, and Husklet runtime checkpoint tests passed
  7/7. This closes failure cleanup and cross-provider ownership; it does not
  close the still-red native capture/restore compatibility path.
  A historical-failure rerun on the same artifact passes directory lifecycle,
  PID/procfs identity, sentry fork under both sandbox and untrusted policies,
  and background TTY signaling on both ISAs (10 unique ledgers). Merged
  `e7daf35ef`..`0d68e34cc` closes absolute-root and chained symlink traversal on
  both ISAs while preserving lower-file and Python JSON behavior.
  These
  closures do not replace the required immutable
  full-tip corpus rerun.
  Merged VFS traversal now preserves `ELOOP` for deep/cyclic symlinks and keeps
  plain `O_NOFOLLOW` distinct from `O_PATH|O_NOFOLLOW`; both ISAs, independent
  mutations, the native Linux oracle, and the full `hl-native` gate are green.
  Displaced AMD64 ET_EXEC self-modifying code now invalidates stale translations
  even when an executable-page transition precedes both exact cache indexes;
  the original MMX/XMM SIGILL fixture, adjacent cases, mutation, and native gate
  prove closure. Profile the conservative miss fallback after compatibility is stable.
  The staged-store transition stress is now finite and non-vacuous: exactly
  1,000 remaps must overlap at least 100 writer iterations, passing three times
  per ISA in 1.35--1.81 seconds instead of coupling termination to stalled
  writer progress.

## C source structure cleanup
- [ ] Split ARM64 interpreter/translator units and oversized functions.
  Merged tip `d7703fe46` extracts the byte-identical 166-line AdvSIMD structure decoder into `interp/structure.c`, reducing `vector.c` from 687 to 522 lines; forced-interpreter structure/cross-view cases, C lint/format, and all 74 active `hl-native` tests pass.
- [x] Remove the unreachable macOS Mach/CRASHDBG subsystem and inert duplicate
  native includes; the 478-line cleanup preserves the live POSIX fault path and
  passes macOS ARM64 compilation plus focused native tests.
- [x] Remove the unreachable standalone forkserver/client protocol while retaining
  real guest-fork, checkpoint descriptor transport, and persistent-cache fork
  lifecycle. Merged as `7021bde3f`: 1,452 lines and the unsupported equivalence
  fixture are gone; native all-targets (74 active), workspace lib/bin (including
  265 testing cases), and the full Rust/repository/C design lint pass with zero
  failures. Exact merged tip also passes Linux ARM64 and AMD64 unity/shared-library
  plus strict public C/C++ ABI gates and the Windows AMD64 GNU DLL/import-library,
  export, host-adapter, and ABI smoke gate.
- [x] Extract the 208-line Windows scalar/positioned/append/vector transfer
  subsystem into `host/windows/io.c`; the moved implementation is byte-identical,
  passes strict Windows cross-compilation, and leaves the service ABI unchanged.

## Testing ownership and fixtures

- [x] Restore typed lower-file and merged-directory authority without pathname reopening. Merged as `7a3ea6080`/`31f9f6d0a`; current-tip `hl-native --all-targets` passes, and immutable exact-tip artifacts pass lower-file union/offset, Python JSON, and symlink `ELOOP` cases on both ISAs. Removing merged-directory tagging makes the union regression fail with the lower entry absent.
- [ ] Audit every package/root test for correct unit, package integration, compatibility, or testing-app ownership.

## Quality gates
- [ ] Run `cargo test -p hl-native --all-targets` and all C executable/compatibility tests on the final merged tip.
  Exact clean tip `0d68e34cc808` passes workspace lib/bin tests (including
  `testing` 267/0 and `hl-container` 213/0), all 74 active `hl-native` targets
  with four intentional external/release-only ignores, and
  `cargo check --workspace --all-targets`; zero failures. The final-tip C
  executable/compatibility repetition remains open.
- [ ] Run feature-gated application Clippy checks and Nix flake checks on the final merged tip.
  Exact merged tip `0b5404c69` passes both pinned-shell application Clippy gates: `--features runtime` and `--features gui`; final-tip Nix flake and later-tip repetition remain open.
  Merged tip `03a53261e` passes `cargo test --workspace --lib --bins` in the pinned shell after the checkpoint responsibility and VFS traversal fixes: 1,511 passed, 0 failed, and 4 ignored; the later-tip application Clippy and Nix flake gates remain open.
  The current shared GUI/terminal tree also passes both pinned-shell `husklet --all-targets` Clippy gates with `-D warnings` for `runtime` and `gui`; repeat after its pending handoff is committed before closing this item.
- [x] Keep native ASan, LSan, and Valgrind gates non-vacuous. On `0b5404c69`, all clean runs pass; deliberate UAF/leak probes exit 97 and report the expected heap-use-after-free and exact 4,096-byte leak.
  Exact tip `7021bde3f` repeats the LSan evidence: lifecycle, fork/wait,
  self-exec, malloc, and AMD64 fork/wait workloads are clean; the deliberate
  probe exits 97 with one exact 4,096-byte leak. Sqlite is explicitly skipped
  because this shell lacks the ARM64 cross sqlite development headers.
  Merged `77f24fdc4` now executes that exact non-vacuity probe before workloads
  and fails immediately with the loader-safe instrumented `cargo run` command
  when the testing binary was built without LSan.
- [ ] Obtain macOS CI evidence for platform-specific application/native code.
  Native macOS ARM64 evidence on `fdeed8469` covers dylib compile/link, C/C++ ABI execution, architecture, install name, exports, rpath, receipts, and sibling-library isolation; signed application/release CI remains open.
- [ ] Obtain Windows CI compile/link/ABI smoke evidence and runtime evidence.
  GNU Windows AMD64 cross evidence on `fdeed8469` covers Rust/C compilation, EXE/DLL/import-library linking, exact exports, host translation units, and C/C++ ABI contracts. MSVC and Windows-kernel runtime evidence remain CI-only.
- [x] Remove or cfg-narrow the four Windows-only Rust warnings in `hl-engine` (`execution.rs` mutability/deadline and `composition.rs` terminal/port state). Merged as `d853c67d2`; Windows GNU check and Clippy with warnings denied, PE/DLL/import-library/ABI smoke, Linux workspace check, and path-scoped design lint pass without warning allowances.
- [x] Clear the checkpoint unsafe-boundary and testing async-blocking design-lint findings without suppressions. Merged as `93670bb56` and `6863c8aa3`; focused ownership/cleanup tests and package gates pass.
- [ ] Preserve a zero-finding repository design-lint run through the pending GUI/terminal handoff. Merged `4690ffdc0`, `2a5935349`, and `5a492e5c2` coherently split the oversized checkpoint directory, storage, broker, transaction, and request responsibilities; merged-tip `hl-container --lib` passes 218/218 and `hl-engine --lib` passes 89/89. The current shared tree now reports zero findings across every repository, Rust, C, dependency, safety, and structure rule after extracting terminal output forwarding and scrollback parsing; `husklet --features runtime` checks and `hl-ws-term` passes 103/103. Keep this item open until the concurrently owned GUI/terminal diff is committed and the same zero-finding result is repeated on that merged tip.

## Performance and production readiness

- [ ] Freeze reproducible original standalone-C and integrated-C baseline binaries; record hashes and smoke-run each copied binary for the acceptance campaign.
- [ ] Run balanced-order, unique-ledger, box-locked benchmarks on at least two guests and report every phase.
- [ ] Benchmark Python, sqlite, and malloc against the faster original/retained C baseline.
- [ ] Meet the final requirement: Husklet no more than 10% slower than the faster C baseline.
- [ ] Profile only after compatibility/build stability; validate profile hypotheses with mutations, not self-time alone.
- [ ] Preserve lifecycle/engine observability without hot-path logging overhead.
- [x] Verify no dependency on `../engine` or `../engine_rust` at build or runtime.
  Cargo metadata resolves all 47 local path dependencies inside this repository;
  the lockfile and production/build source paths contain no sibling-engine source,
  manifest, symlink, or runtime dependency.
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
