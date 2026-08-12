# Retained C migration manifest

This is the ownership ledger for the translation, native-execution, guest-memory,
and translation-cache parts of
`src/runtime/native/retained`. It records where each retained
responsibility lives in Husklet, which behavior is intentionally absent, and the
automated evidence required before the retained implementation stops being an
oracle.

`src/native/c` contains temporary C migration debt only. Permanent Rust runtime
packages remain in their existing `src/runtime/*` locations; replacing a C
module extends the corresponding runtime package instead of moving Rust code
under `src/native`. Once compatibility and performance parity are proved,
`src/native/c` and differential-only scaffolding are removed.

This document does **not** authorize a file move or a second production owner.
The retained tree remains the source oracle while Rust owns engine policy and
`src/runtime/native/exec` owns the narrow C/assembly execution kernel described in
[`exec/BOUNDARIES.md`](exec/BOUNDARIES.md).

That policy ownership does not mean every product entry point selects the Rust
executor. On Linux/AArch64, `dockerd` and ordinary `testing runtime` launches
currently default through `ProductionFactory` to the retained-C worker. The
packaged raw `hl-engine` family enters `Program` and uses `RustRuntimeFactory`;
the signed macOS Husklet GUI also uses Rust because C selection is Linux/AArch64
only. `testing product-ab` explicitly selects both arms and implies no CLI default.

## Status and evidence rules

The status column uses these terms:

- **Replaced**: Rust owns the responsibility. Copying the retained C into the
  native kernel would recreate an unwanted second policy owner.
- **Native**: a current C/assembly implementation exists under
  `src/runtime/native/exec`; it is not necessarily a line-for-line port.
- **Split**: Rust owns authority, lifetime, or fallback policy and the native
  kernel owns a bounded mechanism.
- **Omitted**: the retained optimization is deliberately absent. Semantic
  behavior must still be covered by the non-optimized path.
- **Open**: current automated evidence is not sufficient to retire the retained
  implementation for that row.

Only checked-in, automated assertions are permanent evidence. A source
comparison, a historical benchmark, or a statement in an audit document is
useful context but is not parity proof. Each completed semantic row needs:

1. a focused native or Rust test for the local contract;
2. a native-versus-Rust-interpreter differential when translated guest state is
   involved; and
3. an engine-level test for ownership, fallback, invalidation, or lifecycle
   behavior that crosses the FFI boundary.

The retained implementation is still the behavioral and performance oracle.
There is currently no general retained-C-versus-current-native differential in
the permanent gate, so rows marked **Open** must not be declared complete from
the existing component tests alone.

Path shorthand in the tables is deliberate: retained `translator/...` and
`core/...` paths are relative to
`src/runtime/native/retained/src`; `arch/...` is relative to
`src/runtime/native/exec/src`; `exec/test/...` is relative to `src/runtime/native`; and
`native/...` and `ffi/...` are relative to `src/containers/hl-engine/src`.

## Ownership boundary

| Concern | Policy and lifetime owner | Bounded native mechanism |
| --- | --- | --- |
| Process lifecycle, scheduling, fallback, signals | `src/containers/hl-engine/src/ffi/linux/execution` and the `hl-runtime`, `hl-task`, and `hl-execution` crates | Entry, translated execution, and classified exits only |
| Guest memory and executable identity | `hl-memory` projection/direct-authority leases and executable versions | Checked source/projection views, memory lowering, dirty ranges, and fault provenance |
| Translation admission and cache identity | `ffi/linux/execution/scheduler/{native,pool}.rs` and `native/executor.rs` | Per-executor translation cache, relocation, chaining, and IBTC mechanics |
| Architectural state | Generated schema in `src/native/cpu` and Rust CPU state in `hl-execution` | ABI-compatible state load/store around native entry |

## Translation manifest

| Retained source | Retained responsibility | Current owner | Status | Permanent evidence and retirement condition |
| --- | --- | --- | --- | --- |
| `translator/host/aarch64/asm.{c,h}`, `translator/emit.h` | AArch64 instruction encoding and emission primitives | `src/runtime/native/exec/src/arch/aarch64/assembler.{c,h}` plus family-specific emitters | **Native** | `exec/test/aarch64_assembler.c` and the family tests named `aarch64_*.c` run through `src/native/tests/exec_c.rs`. Retirement also requires every encoding used by a migrated family to have an assertion; the retained file's existence is not coverage. |
| `translator/guest/aarch64/translate.c` | Decode, lower, terminate blocks, and select fallback | `arch/aarch64/frontend.c`, `block.c`, `terminator.c`, `trace.c`, and the instruction-family files; unsupported work returns to the Rust interpreter | **Split, Open** | Focused `aarch64_frontend.c`, `aarch64_block.c`, `aarch64_terminator.c`, `aarch64_trace.c`, and family C tests plus AArch64 cases in `native/executor_differential.rs`. Before retirement, inventory every retained decoder family as native-equivalent or intentional Rust fallback and add a retained/current differential for uncovered families. |
| `translator/guest_fetch.{c,h}` | Bounded guest instruction fetch across source windows | `arch/aarch64/source.{c,h}`, the source spans in `native/executor.rs`, and `hl-memory` projection leases | **Split** | `exec/test/aarch64_source.c`, `aarch64_frontend.c`, `aarch64_stale_site.c`, and source-boundary differential tests in `native/executor_differential.rs`. Retirement requires truncation, overflow, cross-view, and stale-source cases to remain asserted. |
| `translator/guest/aarch64/stubs.c`, `dispatch.h` | Entry stubs, dispatcher transitions, chaining, and public exits | `arch/aarch64/entry.S`, `stub.c`, `indirect.c`, `terminator.c`, `src/runtime/native/exec/src/executor.c`, and `src/runtime/native/exec/src/dispatch/exit.c`; Rust scheduler owns whether execution is re-entered | **Split** | `exec/test/{run,exit,integration,lifecycle}.c`, `aarch64_{entry,stub,indirect,stitch,cycle,counter}.c`, `control_metadata.c`, and `indirect_metadata.c`. Engine re-entry/fallback assertions live in `native/executor_differential.rs` and `ffi/linux/execution/test.rs`. |
| `translator/guest/aarch64/{abi,cpu}.h` | Guest CPU layout and the translator/dispatcher ABI | `src/native/cpu/{layout.tsv,generate.rs,rust/layout.rs}`, `src/runtime/native/cpu/include/layout.h`, `src/runtime/native/exec/include/executor.h`, and `hl-execution` AArch64 CPU state | **Split, Open** | `src/runtime/native/cpu/test/layout.c`, C `_Static_assert`s in the public header, `exec/test/state_tally.c`, and Rust executor state round-trip/differential tests are the evidence targets. Before retirement, wire the standalone layout check or an equivalent generated-output check into the permanent gate so changing only one language fails. |
| No retained x86-64 translator exists in this copied tree | x86-64 guest translation on an AArch64 host | `src/runtime/native/exec/src/arch/x86_64` with Rust interpreter fallback | **Native, independently derived** | `exec/test/x86_*.c` and the x86 cases in `native/executor_differential.rs`. AArch64 parity cannot be used as x86 evidence; each scheduler change must compare both ISA arms as required by `AGENTS.md`. |

## Execution and signal manifest

| Retained source | Retained responsibility | Current owner | Status | Permanent evidence and retirement condition |
| --- | --- | --- | --- | --- |
| `core/{engine,dispatch,lifecycle}.c` | Retained engine lifecycle, process start/wait/finish, and dispatch policy | Rust owns selection and worker supervision in `runtime/{api,execution}.rs`; retained C remains the selected core for eligible default Linux/AArch64 product launches | **Split, Open** | Workspace Rust tests cover selection and Rust execution; retained worker/lifecycle tests cover the current C default. Retirement requires compatibility evidence for start, exit, signal, fork, exec, and fallback through both backends. |
| `translator/guest/aarch64/signal.{c,h}` | Linux guest signal frame delivery/restoration | `hl-linux/src/signal`, `hl-runtime/src/signal`, and `ffi/linux/execution/{signal_frame,routing/signal,threads/signal}.rs` | **Replaced** | `hl-linux/src/signal/{frame_test,test}.rs`, `hl-runtime/src/signal/{frame_test,queue_test,wait_test}.rs`, task signal tests, and the workspace library/bin gate. Native code must not reacquire signal-queue or signal-frame policy. |
| The fault-reconstruction half of `translator/guest/aarch64/signal.c` and provenance in `translator/cache.c` | Turn a host fault in translated code into a guest-visible classified exit | `src/runtime/native/exec/src/fault`, `arch/aarch64/fault*.c`, `native/signal.rs`, and the Rust `HostFaultOwner` integration | **Split** | `exec/test/{fault_thread,fault_coordinator}.c`, `aarch64_fault.c`, `aarch64_provenance.c`, and Rust host-fault owner tests. Retirement requires cold and warm translated faults, wrong-thread faults, and teardown/fork gaps to remain covered. |

## Linux runtime ownership map

The retained backend temporarily brings a complete Linux personality with the fast translator. The
permanent Rust owner already exists for almost every policy domain, but crossing from translated C into
Rust on every syscall would put an IPC/FFI round trip in sqlite's hot path. Elimination therefore follows
ownership *and* frequency rather than replacing similarly named files in arbitrary order.

| Retained C modules | Existing permanent owner | Current call edge | Migration rank and required evidence |
| --- | --- | --- | --- |
| Deleted `core/{cli,config,launch}.c`, `core/target/run.c` | `hl-engine/src/{cli.rs,launcher/{wire,plan}.rs,c_execution/{wire,process,worker}.rs}` | The retired standalone edge was `hl_engine_entry -> hl_cli_route_parse / hl_run_config_file_with -> hl_standalone_run -> hl_native_engine_run`; the product worker enters `CGuestExecutor -> hl_c_backend_create/hl_c_backend_run -> hl_engine_create_with_borrowed_options/hl_engine_run` | **Retired.** `c_standalone_retirement.rs` asserts physical absence, manifest absence, and linked-symbol absence; reintroducing a source file makes the test fail. |
| `core/options.c` | `hl-engine/src/options.rs` and the validated `RuntimePlan` worker wire | `CGuestExecutor::create_with_streams -> hl_c_backend_create -> hl_options_init_records -> hl_engine_create_with_borrowed_options`; retained consumers read the lifetime-stable launch store, while guest-exec environment mutation uses process-private overlay state | **2: keep the C read view, eliminate remaining C parsing/mutation policy.** Continue separating runtime-only state from launch inputs; differential tests must cover absent versus empty, overwrite, integer bounds, environment exclusion, fork/exec inheritance, and all option consumers. |
| `linux_abi/container/vfs*.c`, `fdcache.c`, `open_plan.c`, filesystem syscall arms | `hl-vfs`, `hl-fs`, and `hl-runtime` filesystem/descriptor ports | `hl_engine_run -> hl_production_start_process -> hl_run_linux_guest* -> service_local -> syscall/dispatch.c -> fs.c/vfs.c` | **3, cold policy first; path lookup remains performance-sensitive.** Move namespace construction and mount validation before lookup/mutation. Require the filesystem, overlay, exact-bind, descriptor, exec-image, and two-guest sqlite benchmarks before redirecting any lookup call. |
| `linux_abi/container/{pidmap,state}.c`, `thread.c`, process/signal/wait syscall arms | `hl-task`, `hl-runtime/src/{process,thread,signal}`, `hl-execution` | Translator exits to `service_local`; dispatch calls process/thread globals and returns register results to `run_guest` | **4: retain while C owns guest execution scheduling.** Replacement needs fork/exec/wait/signal/job-control differentials and must not add one host IPC per syscall. |
| `linux_abi/{logical_vma,image,elf}.c`, memory syscall arms, `translator/guest_memory.c` | `hl-memory`, `hl-loader`, `hl-runtime` memory ports; narrow mechanism in `src/runtime/native/exec` | Loader and `mmap` update the logical-VMA ledger; translator fetch/data resolution and cache provenance read it on every translated block/operand | **Last policy split, hot mechanism stays native.** Do not route these accesses through the worker socket. Prove aliases, W^X, partial windows, dirty publication, self-modifying code, fork gaps, and both guest layouts. |
| `linux_abi/syscall/{dispatch,io,event,time,net,aio,sysv,ptrace,...}.c` | `hl-runtime` syscall router plus `hl-{descriptor,event,time,network,aio,ipc,sync,task}` | `run_guest -> service_local -> syscall/dispatch.c`; many arms call retained host services directly | **Domain-by-domain after an in-worker ABI exists.** Counts are not costs: measure each candidate's crossing before replacing it. Preserve a direct in-process fast route for frequent fd/memory/time calls. |
| `core/provider/*`, `host/*` | `hl-provider` and `hl-engine/src/native/{authority,host}` | VFS/network/provider operations demultiplex into host file/process/memory services | **Cold authority operations may move early; data-plane operations stay local.** Require SCM_RIGHTS, projected tree/file, readiness, failure/reconnect, and syscall-phase benchmarks. |
| `translator/*`, `core/dispatch.c`, target entry/stubs | `src/runtime/native/exec`, `hl-execution`, and Rust native scheduler | `hl_run_linux_guest* -> run_guest -> cache lookup/translate_block -> emitted native code -> dispatcher exit` | **Keep C now.** This is the proven fast core. Eliminate only by native-vs-retained instruction families and balanced malloc/mmap/sqlite evidence, never by source similarity. |

The standalone CLI/config-file launch chain has been physically deleted from the retained tree. Its
behavioral oracle remains available in read-only `../engine`; Husklet has only the Rust worker/wire/process
owner. `c_standalone_retirement.rs` prevents either the files or their linked symbols from returning.

## Memory manifest

| Retained source | Retained responsibility | Current owner | Status | Permanent evidence and retirement condition |
| --- | --- | --- | --- | --- |
| `translator/guest_memory.{c,h}` | Resolve guest operands, translate guest addresses, and authorize host access | `hl-memory/src/mapping/{projection,host_access,projection_publish}.rs`, `ffi/linux/mapping*.rs`, `native/executor.rs`, and `arch/aarch64/{memory,projection,direct}.c` | **Split, Open** | `exec/test/aarch64_{memory,projection,direct,dirty_bound,view_hygiene}.c`, `direct.c`, x86 projection/view/write tests, `ffi/linux/mapping_test.rs`, and memory differentials. Retirement requires an automated retained/current comparison for permission failures, aliases, partial windows, dirty publication, and self-modifying code. |
| Deleted `translator/window.{c,h}` | Overflow-safe interval/window construction | Checked ranges are owned by `hl-isa`/`hl-memory`; the retained persistent-cache consumer keeps a private bounds check | **Retired** | `c_window_retirement.rs` asserts physical, manifest, and linked-symbol absence. Source/projection boundary C tests and Rust `hl-memory` tests cover malformed bounds without restoring an ambient C API. |
| `translator/arena.{c,h}` | Executable allocation and writable-to-executable publication | `src/runtime/native/exec/src/arena.{c,h}` with memory allocation callbacks owned by `native/executor.rs` | **Split, Open** | `exec/test/allocation.c`, cache/run tests, and `exec/test/memory_lifecycle.sh`. The script is not currently part of `exec_c.rs`; it must become a normal gate before this row is closed. W^X, partial failure, repair, rotation, and destroy-after-fork are required cases. |

## Cache manifest

| Retained source | Retained responsibility | Current owner | Status | Permanent evidence and retirement condition |
| --- | --- | --- | --- | --- |
| `translator/cache.c`, `cache_abi.h` | Translation lookup/publication, source provenance, invalidation, generations, chaining, and indirect-target caching | `src/runtime/native/exec/cache/cache.{c,h}`, `src/runtime/native/exec/src/{translation,executor}.c`, architecture trace/indirect code, and Rust `NativePool` epoch/source tables | **Split, Open** | `exec/test/{cache,provenance,ibtc_rollover,control_metadata,indirect_metadata}.c`, `aarch64_{read_cache,store_cache,stale_site,stitch}.c`, x86 chain tests, and Rust mapping/executable-version tests. `exec/test/translation.c` is currently allowlisted as a known failure, so it is not parity evidence until fixed and removed from `KNOWN_FAILING`. |
| `translator/reloc.{c,h}` | Record and resolve translation relocations | `src/runtime/native/exec/cache/relocation.c` and architecture block/stitch emitters | **Native, Open** | `exec/test/cache.c`, `translation.c`, `aarch64_stitch.c`, `pcrel_materialization.c`, and integration tests. Closure is blocked while `translation.c` remains known-failing. |
| `translator/guest/aarch64/cache.c` | AArch64 persistent-cache image validation, load/save, and architecture relocation | In-memory architecture metadata and relocation live in the current cache; persistent images do not | **Split, Open** | In-memory behavior is covered by cache/stitch/stale-site tests. Before retiring the oracle, classify each retained field as represented, recomputed, or intentionally discarded and assert that a cold rebuild produces the same guest-visible result. Do not claim persistent-format parity. |
| `translator/{digest,identity,persist}.{c,h}` | Content identity and persistent translation-cache storage | No current production equivalent | **Omitted** | This is a performance omission, not a semantic fallback. Permanent evidence is an engine test proving that cache absence, invalid data, and a fresh process all rebuild safely without stale execution. A benchmark may justify adding persistence later but cannot close semantic parity. |
| Retained stop-the-world, fork, and self-modifying-code cache coordination | Prevent execution or reuse while mappings/code identities change | Rust mapping checkpoints, projection/direct-authority leases, executable versions, scheduler pool invalidation, and native per-executor cache generations | **Split, Open** | `exec/test/{lifecycle,fault_thread,fault_coordinator,provenance}.c`, `x86_fork_gap.c`, executable-version tests in `hl-memory`, and engine mapping tests. `memory_lifecycle.c` is skipped by `exec_c.rs`, so lifecycle closure requires integrating its archive-backed runner or equivalent assertions into the permanent gate. |

## Permanent gate

The minimum migration gate is the project gate, run on the exact tip being
evaluated:

```sh
cargo test --workspace --lib --bins
cargo test -p hl-native --test exec_c
```

Use `make gate` for the complete pinned-toolchain gate. The direct Cargo commands
above must be run in the pinned development shell as described by `AGENTS.md`.
The second command discovers standalone C programs in `src/runtime/native/exec/test` and
fails on build failures and unexpected passes. Two exclusions matter to this
manifest:

- `translation.c` is in `KNOWN_FAILING`; its expected failure is anti-rot, not a
  passing cache/relocation assertion.
- `memory_lifecycle.c` is skipped because it needs the malloc-interposing archive
  built by `memory_lifecycle.sh`; the shell script is useful targeted evidence,
  but it is not yet part of the permanent gate.

`src/containers/hl-engine/src/native/executor_differential.rs` is the permanent
native-versus-Rust semantic oracle for translated instructions and boundary
state. Keep it in the workspace gate. Tests under `src/runtime/native/exec/test` prove
the C kernel's local contracts; they do not by themselves prove Rust ownership,
scheduler behavior, or retained-backend compatibility.

The audit documents [`exec/HOT_PATH.md`](exec/HOT_PATH.md),
[`exec/FALLBACK_AUDIT.md`](exec/FALLBACK_AUDIT.md), and
[`exec/WRITE_PUBLICATION.md`](exec/WRITE_PUBLICATION.md) explain design choices
and measurements. They are supporting rationale, not permanent parity evidence.

## Retirement order

Retire responsibilities, not filenames. For each row:

1. enumerate the retained entry points and every caller that depends on their
   semantics;
2. classify the current owner as Rust, native, split, or intentionally omitted;
3. add the missing focused, differential, and engine-level assertions;
4. mutate the current owner and confirm the named assertion fails;
5. run the permanent gate on the final tip; and
6. only then remove or archive the retained implementation in a separate change.

The safe dependency order is CPU ABI and checked bounds, then source/projection
and publication, then translation families and dispatcher exits, then cache
invalidation/fork coordination, and finally lifecycle integration. Persistent
cache files are not on the semantic critical path and must not hold up removal
once cold-rebuild safety is permanently asserted.
