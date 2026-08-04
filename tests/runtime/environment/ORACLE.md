# Initial environment oracle audit

## Workload and retained C call graph

The retired seed was `../engine_rust/src/tests/compat/source/environment.c`. At `_start` it
reads the kernel initial stack directly as `[argc][argv pointers][NULL][envp
pointers][NULL]`, requires exactly six ordered records (`TZ=UTC\xff`, `EMPTY=`,
then the retained engine defaults `PATH`, `HOME`, `TERM`, and `LANG`), rejects a
seventh record, and exits through the architecture syscall ABI. Declaring all
six makes native Linux and the retained engine's initial-launch default filling
converge while preserving the raw-byte, empty-value, and ordering contract.

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
| Typed launch environment into engine plan | `hl-container::Environment` / `hl-container::engine::Spec` / `hl-engine::launch_plan::RuntimePlan` | Implemented; legacy OCI text maps and exact ordered byte records are distinct representations |
| Empty values and exact public-container record order at `_start` | This runtime workload on AArch64 and x86-64 | Native QEMU oracle and the Rust production runtime pass both ISAs |
| Non-UTF-8 public container environment values | `hl-container::Environment::Exact` and `Process::env_bytes` | Production runtime passes on both ISAs; NUL, empty-name, `=`-in-name, count, and aggregate-size validation remains at the process-spec boundary |
| Health-command environment overlay | `hl-container::Environment::overlay` | Implemented; command values replace matching names, duplicate base names collapse, and unrelated exact records retain their order |
| OCI image commit representation | `hl-container::containers::image::commit_runtime` | Exact records are accepted only when names and values are UTF-8 and names are unique; unrepresentable OCI metadata is rejected rather than changed |

The case is intentionally freestanding so libc cannot reorder, normalize, or
inject records before the direct initial-stack observation. The YAML oracle
launcher must likewise preserve raw declaration order; a key-mapped process
environment is not evidence for that property. No AArch64 or x86-64 runtime
verdict is claimed until the ordered launcher and this case execute together.

## Ordered native-oracle launch

The YAML oracle previously used `std::process::Command::env_clear` plus repeated
`env` calls. That API stores changes by name, so it cannot prove declaration
order or preserve duplicate names. `hl-process` now owns the generic host
boundary: on Unix its exact-environment path builds a bounded vector of
NUL-terminated `name=value` records and passes that pointer vector directly to
`posix_spawn` after resolving a bare command through the parent's `PATH`. It
does not construct a map. Record storage remains owned
through the spawn call and is released only after the kernel copies it.
Existing process-group ownership, timeout/cancellation, capture bounds, and
descendant teardown supervise the oracle unchanged.

The retained POSIX path in
`../engine/src/core/activation.c::activation_start` likewise builds owned
`child_env`, removes inherited `HL_ACTIVATION_FD`, appends the launch-owned
record, and passes the explicit vector to `execve` or `posix_spawn`.
`environment.c::hl_environment_take_activation_descriptor` consumes that
record once. `launch.c::hl_read_config_file` separately copies the validated
guest environment option into launch-owned storage. The older matrix/process
runners accept only text manifest environment and use `execvp`, so they cannot
prove duplicate or non-UTF-8 records.

The launcher resolves a non-absolute oracle command through the parent's host
`PATH` before calling `posix_spawn`; the explicit vector is solely the child's
environment. The byte-exact unit test re-executes the repository's
own test binary by absolute path. Its selected child test walks raw `environ`
and asserts the complete pointer sequence for ordered duplicates, an empty
value, and `0xff`. Empty, over-count, aggregate-byte overflow, timeout,
cancellation, and output-limit behavior are independently covered.

Windows remains an explicit gap. Its native environment block is UTF-16 and
the current `hl-process` adapter rejects exact byte-valued environments with
`Unsupported`; it does not normalize bytes, order, or duplicates silently.

Linux creates capture pipes atomically with `O_CLOEXEC`. Darwin lacks that
portable primitive in this implementation, so `hl-process` serializes every
package-owned spawn across pipe creation, CLOEXEC installation, and
`posix_spawn`. An unrelated external library that spawns a process without
participating in this lock could still observe Darwin's narrow descriptor
window; eliminating that remaining embedder-wide race requires a host primitive
or a process-wide spawn authority shared by all launchers.
