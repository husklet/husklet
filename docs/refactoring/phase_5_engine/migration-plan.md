# Migration plan

The migration is ordered to preserve a working macOS product at every merge. Each wave is small enough to revert and
has a behavioral gate. File motion follows proven dependency direction; it does not precede it.

## Wave 0 — freeze evidence and surface

- Record current symbols, config bytes, engine paths, Mach-O segments, linked frameworks, cold/warm performance and
  both Linux guest matrices.
- Generate unity include graphs and Clang warnings for all three targets. Record address-taken callbacks and emitted
  code references so splitting `static` functions does not create false dead-code decisions.
- Add a C consumer of `ddjit_api.h` that launches a tiny guest and validates error/config skew behavior.
- Classify all current `dd-jit`/`dd-jit-darwin` consumers and the native-Darwin guest compatibility requirement.

Exit: reproducible baseline artifacts and distributions exist; required macOS prerequisites fail preflight.

## Wave 1 — establish standalone `engine/` build

- Copy/move C sources, entitlement and public headers into `engine/` without semantic edits.
- Make CMake (or a small portable Make layer) compile the same unity runners first. Cargo invokes this build and no
  longer owns source lists/compiler command construction.
- Emit an artifact manifest containing host, host CPU, guest ISA, engine ABI, paths, hashes and signing requirements.
- Preserve current macOS frameworks/codesigning only in the macOS build target.

Exit: old and new runners are byte-behavior equivalent on the C/Rust suite; packaging uses the manifest and never a
stale baked path.

## Wave 2 — turn unity seams into headers and object libraries

- Introduce private headers for engine state, translator hooks, Linux ABI state and target composition.
- Replace cross-file implicit declarations and include-order globals with declarations and explicit owner modules.
- Compile one source file at a time with `-Werror=implicit-function-declaration`, missing prototypes, conversion and
  shadow warnings appropriate to the baseline.
- Link object/static libraries but retain the same target entrypoints and direct ARM lowering.

Exit: no `.c` file includes another `.c`; both Linux targets and the Darwin compatibility target link; exported
symbols match the allowlist; performance stays inside the defined budget.

## Wave 3 — instance state and engine lifecycle

- Introduce opaque `hl_engine`, translator state, Linux process state and host context.
- Migrate globals by ownership clusters, starting with configuration and cold state. Hot translation tables move only
  after generated assembly/performance comparison.
- Replace environment reads with parsed config/diagnostic options at creation. Every code-changing option participates
  in cache identity until removed.
- Link `libhl-engine.a` into `hl-engine-runner`; keep execution out of the Rust process.

Exit: repeated create/run/destroy under leak/sanitizer tests; two isolated runner instances cannot cross-contaminate
fds, pids, caches, signals or configuration.

## Wave 4 — define and inject host-macos services

- Land `host_services.h`, validator and deterministic fake backend.
- Move one semantic group at a time: clocks/random, file I/O, VM/JIT, events, process/thread, network, identity and
  optional GPU.
- Keep Linux errno/flags/struct conversion on the Linux side. Move only native calls and native object ownership.
- For event/process/fork groups, run Chrome/Go/JVM/database stress after each move; these areas have historical
  cross-process and lost-wakeup failures.

Exit: no macOS header/token outside `src/host/macos`, runner/build/sign files and the quarantined Darwin guest.

## Wave 5 — stabilize Linux ABI models

- Consolidate guest fd/OFD, process/thread/pid, VMA and event ownership into explicit instance-owned modules.
- Generate the canonical cross-architecture syscall ownership table, while preserving raw-number visibility for
  seccomp/ptrace and final `ENOSYS` behavior.
- Move `/proc`/`/sys` generation to model queries rather than host probes.
- Make guest fork work through an abstract child-state operation; retain fast host fork as optional acceleration.

Exit: Linux ABI tests run against host-macos and fake host services with identical guest-visible results.

## Wave 6 — introduce translator IR incrementally

- Define private IR version/validator and first lower control exits, memory/fault operations and safepoints.
- Port instruction families with native/QEMU state differential tests and code-size/throughput gates.
- Split guest frontend and host CPU backend selection. Preserve direct ARM adapter until both frontends are migrated.
- Version/segregate pcache formats; never load an arena built by a different IR/codegen/config identity.

Exit: aarch64 and x86_64 guest frontends use the IR for the accepted scope; no OS call exists in translator; direct
adapter deletion requires full scope and no regression.

## Wave 7 — add host-linux

- Implement every required service group and link the common host-contract suite against it.
- Do not shortcut the Linux ABI with arbitrary syscall passthrough. Use host facilities only behind semantic services.
- First qualify Linux/arm64 to reuse the existing host-CPU backend; then add/qualify x86_64 host lowering.
- Compare guest-visible fixtures across macOS and Linux byte-for-byte where deterministic.

Exit: the same unmodified Linux application suite passes on macOS and Linux; platform-specific skips are documented
capabilities, not silent success.

## Wave 8 — add host-windows

- Implement process cloning through state serialization/spawn, IOCP-backed host events, Windows file/identity and VM
  services without exposing them to Linux ABI.
- Qualify cancellation, path/rename/delete semantics, sparse files, case behavior, signals, Unix sockets and resource
  cleanup explicitly.
- Add ARM64/x86_64 host backends as required by the supported Windows matrix.

Exit: common host contract and Linux compatibility suite pass; no macOS/Linux-only assumption is present above the
backend.

## Wave 9 — Rust cutover and repository extraction

- Switch higher Rust runtime/daemon/CLI consumers to `hl-engine` and remove direct `dd-jit-darwin` use.
- Preserve temporary source compatibility only where Phase 3 declares it; remove old build.rs artifact production.
- Verify `engine/` builds/tests from a clean checkout using only documented C toolchain prerequisites.
- Extract repository history or subtree only after all engine-owned tests, fixtures, licenses, build files and release
  metadata are self-contained.

Exit: deleting the old C runtime directory and `dd-jit-darwin` introduces no missing source/test/package input.

## Rollback rule

Every wave retains the previous runner as an A/B oracle until the new path passes. Rollback is artifact selection,
not two indefinitely maintained semantic implementations. Once a wave is accepted, delete the old path and update
this document; do not accumulate fallback flags or duplicate caches.
