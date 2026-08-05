# Nested engine oracle audit

The retained implementation was studied read-only in `../engine`.

| C capability | Oracle owner and entry | State, ordering, and teardown | Rust test owner | Status |
|---|---|---|---|---|
| Validate a chain before launch | `tools/nested_engine_gate.c:main` | Checks every executable in outer-to-inner order; a missing cross-tree artifact returns 77 and explicitly says the gate did not run | `src/apps/testing/src/nested.rs` manifest validation and `unavailable` | Implemented; unsupported is a non-green result |
| Construct recursive execution | `cmake/Phase3Gates.cmake:hl_nested_case` | Engine ELF architecture must equal its parent's guest ISA; final element is the leaf guest | Typed `Layer { artifact, guest_isa, options }` flattened in manifest order | Implemented; artifact production remains external and truthful |
| Per-layer engine configuration | C production engines consume their own command line; no ambient option is forwarded through a parent guest | Each layer owns its argv and lifetime; native diagnostics require native execution | `EngineOptions::append` immediately after its owning layer | Implemented |
| Exact result gate | `tools/nested_engine_gate.c:main`, `read_file` | Waits for the chain, requires ordinary exit and byte-exact stdout; stderr remains a diagnostic channel | `execute` | Implemented; native-mode rows additionally require propagated `hl-native-detail` evidence |
| Bounded supervision | `tools/process.c:hl_process_run`, `child_exec`, `read_output` | Fork/exec ownership, EINTR-safe reads and wait; the C capture grows without a bound and has no timeout/process-group teardown | `capture`, `drain`, `terminate_group` | Rust intentionally strengthens the oracle with byte bounds, timeout, TERM/KILL, and reap |
| Foreign static nested artifacts | `cmake/Phase2Production.cmake:hl_linux_production`, `cmake/Phase3Gates.cmake:nested-foreign-engines` | Second static cross-toolchain; deliberately absent from default build | Typed Cargo recipe plus `testing nested prepare`; the recipe and complete Rust source tree determine the cache key, while a SHA-256 receipt verifies reused bytes | Implemented; target toolchains remain an explicit host prerequisite |

Architecture branches are encoded by the manifest rather than host-conditionals in
the runner. The retained matrix covers same-ISA, cross-ISA, three-layer inverse,
and `aarch64 -> x86_64 -> aarch64`. `testing nested prepare` builds the declared
Linux targets with locked, offline Cargo, installs verified artifacts atomically,
and reuses only a content-bound cache entry. `testing nested run` prepares first,
so an unavailable compiler or target is a build failure rather than a false test
result.

The artifact recipe uses the pinned GNU Rust targets with `+crt-static` applied
only to the final `hl-engine` binary through `cargo rustc`; applying that feature
workspace-wide prevents proc-macro construction, while the MUSL libc bindings do
not expose Linux `statx` and socket ABI types used by the host adapter. The flake
provides a target-specific linker for each GNU triple. Commit `07dcff6be` removed
the former `runtime/core` fixture; the nested leaf is now the active
`runtime/abi-core/hello` artifact, whose own manifest declares ARM64, exit 42, and
the byte-exact `hi\n` output used by this gate.

The native executor compilation owner is `src/containers/hl-engine/build.rs`,
which deliberately compiles the common C translation unit on both Linux host
architectures and selects assembly by target. The AArch64 fallback accounting,
IBTC site fill, and fatal translated-run exit routines in
`src/native/exec/src/executor.c` are owned exclusively by the AArch64 run path;
their definitions are therefore target-guarded along with their call sites so
the warning-strict AMD64 foreign build does not create dead host code. The
retained engine has no corresponding portable nested artifact builder: its
relevant oracle remains `cmake/Phase2Production.cmake:hl_linux_production` and
`cmake/Phase3Gates.cmake:nested-foreign-engines`, which build each host-native
engine using a separate static cross toolchain.

## Runtime construction and image-launch audit

The retained launch domain was studied directly in:

- `../engine/src/core/launch.c`: `hl_read_config_file` and
  `hl_run_config_file_with` own bounded wire ingestion, launch-scoped option and
  argv storage, private-descriptor registration, and teardown after the selected
  runner returns;
- `../engine/src/core/target/aarch64.c`: `container_init`,
  `engine_global_init`, `load_program`, `run_loaded`, and
  `hl_run_linux_guest`;
- `../engine/src/core/target/x86_64.c`: the corresponding five entry points;
- `../engine/src/linux_abi/elf.c:load_elf` and
  `../engine/src/linux_abi/x86.c:load_elf`, which own ISA-specific ELF mapping,
  interpreter loading, segment protection, entry/base publication, and failure
  diagnostics;
- `../engine/src/core/dispatch.c:run_guest`, which owns the synchronous guest
  execution lifetime after image and CPU publication.

The C order is config validation -> per-launch options/argv -> host-service and
Linux-state binding -> container/root identity -> process-global engine/cache
initialization -> main/interpreter mapping -> heap/stack/CPU publication ->
`run_guest` -> thread/sentry join and launch storage teardown. Short reads retry
on `EINTR`; malformed/truncated configuration returns 78. Construction failures
return 70 or a phase-specific nonzero result, and ELF mapping failures emit the
failing image/host status before execution. AArch64 and AMD64 have separate
loader and CPU entry paths; macOS and Windows branches differ in mapping, fault,
and cold-process launch mechanisms, while the Linux launch order remains the
same. Process-global initialization is idempotent; per-launch image, stack,
thread, and option ownership is retired only after guest execution and peer
thread/sentry teardown.

| C capability | Rust owner | Status |
|---|---|---|
| bounded config/options/argv ingestion | `launch_plan`, `options`, `Program` | implemented with typed errors before composition |
| host services and runtime-domain assembly | `runtime/api.rs`, `runtime/machine.rs`, `ffi/linux/execution/mod.rs` | implemented, but construction errors are flattened |
| ISA inspection and main/interpreter load | `hl-loader`, `ffi/linux/execution/mod.rs` | implemented; `EngineError::Load` preserves loader errors |
| memory, IPC, task, descriptor, exec/fork/clone registration | `ffi/linux/execution/mod.rs` and `routing/*` | implemented; dozens of distinct failures collapse to `LaunchFailed` |
| CPU publication and bounded worker scheduling | `threads`, `waiter`, `GuestExecutor` | implemented; start failures collapse to `LaunchFailed` |
| ordered cancellation, peer join, and teardown | `ThreadSet`, waiter pool, `EngineBackend` | implemented |
| phase-specific construction evidence | retained return codes and loader stderr | divergent: Rust preserves only loader errors; the remaining construction graph loses its owner/error at the API boundary |

After correcting the manifest ISA edges, structured nested reports advance from
`WrongArchitecture` to the inner AArch64 engine's `Engine(LaunchFailed)`. That
result does not identify which construction capability diverged. A behavior fix
is therefore not source-justified until `GuestExecutor::run` and `routing::create`
return a typed construction phase/error instead of mapping every domain failure
to `EngineError::LaunchFailed`.

### Typed construction-error boundary

A mechanical inventory found 124 `LaunchFailed` construction sites across the
engine composition path: 33 in `ffi/linux/execution/mod.rs`, 29 in
`routing/composition.rs`, 18 in `routing/image.rs`, 10 each in
`routing/mod.rs` and `native/launcher.rs`, six each in `runtime/api.rs` and
`execution/transfer.rs`, five in `execution/scheduler.rs`, four in
`runtime/machine.rs`, two in `execution/service.rs`, and one in
`composition.rs`. The public `EngineError` appears at 714 source locations and
448 variant construction/match sites. Adding domain payloads directly to that
public `Copy` enum would widen a stable cross-adapter API while still leaving
the construction traits unable to carry the original error.

The coherent owner is therefore an internal, bounded `ConstructionError` in the
runtime-construction boundary, with exhaustive phase/domain variants for
configuration, assembly, task, memory, loader image publication, routing,
descriptor, IPC, exec, clone, fork, thread, checkpoint, transfer, waiter, and
scheduler construction. `RuntimeFactory::construct`, `GuestMachine::start`, and
the Linux machine adapter must preserve it until one explicit engine-boundary
projection emits the bounded structured phase/domain and converts to the
existing public `EngineError::LaunchFailed`. Representative failure injection
must cover memory, task, IPC, descriptor, and start owners. Changing only
`GuestExecutor::run`, or only the currently observed nested path, would leave
the other admission paths silently flattened and would not be an exhaustive
typed contract.

That refactor spans every concrete `GuestMachine`/`RuntimeFactory` adapter and
their public-contract tests. It is larger than this bounded continuation and no
partial taxonomy was committed as if it covered the domain.
