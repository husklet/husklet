# Initial environment oracle audit

## Workload and retained C call graph

The retired seed was `tests/runtime/legacy/source/environment.c`. At `_start` it
reads the kernel initial stack directly as `[argc][argv pointers][NULL][envp
pointers][NULL]`, requires exactly two ordered records (including an empty
value), and exits through the architecture syscall ABI. The old first value
ended in a non-UTF-8 byte. Husklet's public container `Process` intentionally
owns UTF-8 `String` names and values today, so this migrated public-path case
retains deterministic public-container ordering, termination, and empty-value
coverage with `TZ=UTC`; arbitrary
byte environment transport remains an explicit frontend gap rather than being
silently represented as different UTF-8 bytes.

The read-only retained implementation was followed through:

- `../engine/src/core/engine.c`: `hl_engine_environment_valid`, engine creation,
  and teardown own/copy the newline-record launch configuration with bounded
  names and values. The engine instance owns the copied string until destroy;
  no guest pointer aliases it.
- `../engine/src/core/options.c` and `environment.c`: option import is the sole
  ambient-host boundary. Explicit engine options win over ambient defaults and
  are instance-scoped before launch.
- `../engine/src/core/launch.c`, `lifecycle.c`, and `activation.c`: the launch
  payload bounds and copies configuration/argv, transfers it to the activation
  child, constructs the engine, and releases payload/engine ownership after the
  run. Activation descriptors and relay state are closed during teardown.
- `../engine/src/linux_abi/elf.c::build_stack`: AArch64 resolves launch records,
  optionally decodes exec escaping, keeps container records first, fills only
  missing default keys, captures `/proc/self/environ`, places strings in Linux
  ascending address order, then writes argc/argv/envp/auxv at a 16-byte-aligned
  stack pointer above a protected guard.
- `../engine/src/linux_abi/x86.c::build_stack`: the x86-64 owner performs the
  same record selection, default-key suppression, NUL termination, pointer
  table, auxv, alignment, stack guard, and `/proc` capture. Its historical
  string-placement direction differs and is preserved as an ISA branch.
- `../engine/src/linux_abi/syscall/proc.c::exec_forward_env` plus the execve and
  execveat arms: guest exec environment is authoritative, `NULL` means empty,
  newline/backslash records are escaped before old image teardown and decoded
  while building the replacement stack. Bounds are checked before copies;
  failure leaves the existing image active until the exec transaction commits.

There is no hot-path lock or blocking operation in initial-stack planning. The
retained C implementation uses process-global loader/options state, while Rust
owns the candidate image and stack bytes transactionally per loader instance.
Host branches concern mapping/guard mechanisms and activation transport; the
guest stack remains the same Linux ABI. Windows uses its activation transport,
macOS/Linux use POSIX process transport, and guest ISA selects the initial SP
handoff and string placement policy.

## Rust capability matrix

| Retained capability | Rust owner | Status |
|---|---|---|
| Bounded argv/environment capture from execve | `hl-linux::process::ProcessAbi` | Implemented |
| Bounded strings/counts and NUL rejection | `hl-loader::stack::StackPlanner` | Implemented |
| argc/argv/envp/auxv pointer table and alignment | `hl-loader::stack::StackPlanner::write_table` | Implemented |
| Per-ISA string ordering and platform/auxv data | `hl-loader::stack::StackStringOrder` and auxiliary planner | Implemented, C divergence documented |
| Guarded stack mapping and transactional publication | `hl-loader::transaction::Loader` | Implemented |
| Exec candidate ownership and environment forwarding | `hl-runtime::loader::exec` | Implemented |
| Typed launch environment into engine plan | `hl-container::engine::Spec` / `hl-engine::launch_plan::RuntimePlan` | Implemented for UTF-8 container values |
| Empty values and deterministic public-container record order at `_start` | This runtime workload | Implemented; public `BTreeMap` canonicalizes key order |
| Non-UTF-8 public container environment values | No `Process` representation | Remaining frontend gap; runtime/loader byte owners already support bytes |

The case is intentionally freestanding so libc cannot reorder, normalize, or
inject records before the direct initial-stack observation.
