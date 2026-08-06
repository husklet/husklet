# Bootstrap syscall oracle audit

These three byte-preserved freestanding seeds are the smallest complete engine
launch contract: terminate with status 42, write all thirteen bytes to stdout,
and obtain a positive process ID before writing eleven bytes.

## Retained C implementation studied

- `../engine/src/linux_abi/syscall/fs.c`, the `write` syscall entry and adjacent
  guest-buffer validation/partial-result paths.
- `../engine/src/linux_abi/syscall/proc.c`, the `getpid` and `exit` entries,
  including per-thread retirement and process-result publication.
- `../engine/src/linux_abi/syscall/dispatch.c::run_guest` and
  `../engine/src/core/lifecycle.c::hl_production_finish_process`, which own CPU
  teardown, code-versus-signal result identity, host wait ordering, and final
  low-eight-bit status delivery.
- `../engine/src/host/process.h` plus the POSIX and Windows process wait/result
  adapters, which own child identity and waitable-handle lifetime.

The descriptor table owns stdout's open file description; `write` validates the
descriptor/access mode and guest range, may return a positive partial count, and
does not retain the guest pointer. The dedicated write fixture requires the
whole message and turns a partial/error result into a nonzero exit. The syscall
fixture requires byte-exact output through its golden but does not separately
assert the write return value. `getpid` observes the stable guest process
identity without blocking. `exit` retires the calling thread and publishes its
status only after the preceding syscall completes. No fixture
allocates, locks, handles a signal, or owns a cancellation path. Final engine
teardown releases the CPU, descriptors, mappings, and host process object after
the result is consumed.

AArch64 uses syscall numbers write=64, exit=93, getpid=172; x86-64 uses 1, 60,
and 39. Host wait mechanisms differ (POSIX wait status versus Windows process
handle), but the Linux-visible code and bytes do not.

## C-to-Rust capability matrix

| Capability | Rust owner | Coverage |
|---|---|---|
| per-ISA syscall admission | `hl-linux::syscall::table` | all three calls, both ISAs |
| stdout descriptor/OFD write and exact result | `hl-descriptor` + `hl-runtime::filesystem` | thirteen and eleven bytes |
| stable guest process identity | `hl-task` registry + runtime process adapter | positive getpid |
| per-thread exit and process result publication | `hl-runtime::process` + `hl-task::ExitStatus` | status 42 and zero |
| engine/host lifecycle composition | `hl-engine` launch and container wait adapters | public container path |

These seeds establish launch plumbing only; they do not claim broad descriptor,
process, signal, fork, exec, or partial-I/O domain completeness.

## System launch publication audit

The retained launch paths in `../engine/src/core/target/aarch64.c` and
`../engine/src/core/target/x86_64.c` parse `HL_MEM_MAX` into the process-global
`g_mem_max` before `container_populate_machine_id`. The latter entry in
`../engine/src/linux_abi/container/vfs.c` derives the machine identity from
`proc_reg_key`; `boot_id_bytes` derives the matching stable boot identity from
the same `HL_NETNS`, hostname, or session key. Resource readers in
`container/vfs.c` combine `g_mem_max`, atomic memory/fork counters, and host
snapshots, while `../engine/src/linux_abi/syscall/dispatch.c` independently
assembles the corresponding `sysinfo` view through `hl_host_system_read`.

Those retained values have process lifetime, process-global identity, and no
single publication lock: initialization precedes guest execution, dynamic
memory/fork counters are atomic, and host sampling is per read. There is no
blocking, cancellation, partial-result, signal, or errno behavior in the
initial publication itself. AArch64 and x86-64 differ only in their target
entry files; the container VFS and syscall publication code is shared. Host
branches are confined to `hl_host_system_read` and are completed before the
Rust launch transaction is prepared.

The target entry functions studied were `main` in both target files. Each
parses launch options, initializes container globals, calls
`container_populate_machine_id`, and reaches guest execution only after that
initialization. In `container/vfs.c`, `proc_reg_key`,
`container_populate_machine_id`, `boot_id_bytes`, `proc_meminfo_text`, and the
cgroup resource renderers own the identity and resource projections. In
`syscall/dispatch.c`, `service_local` dispatches `sysinfo` through
`hl_host_system_read`. These globals live until process teardown; dynamic
memory and fork counters use atomic updates while identity initialization is
single-threaded. No retained lock is held across a host call.

Rust maps the retained container key and resource projection to the
instance-owned `hl_runtime::SystemAuthority`; `hl-engine` owns launch-option
capture and host sampling. `SystemLaunchUpdate` validates and copies those
values after task-slot admission, reserves the next generation, then replaces
boot identity, resource snapshot, random sequence, and generation infallibly
under the authority's one state lock. `SystemAuthority::view` is the public
combined observation contract and returns either the complete old or complete
new tuple. Separate `boot_identity` and `snapshot` calls are independent
observations and deliberately make no cross-call coherence promise. Resource
free-memory observations made during construction use an explicit
generation-qualified handle. Its route state and pending resource tuple live
under the same authority mutex as the published tuple. Commit publishes the
tuple and promotes every permit-owned route in that single critical section;
abort retires every route under the same lock, so late construction objects
cannot publish failed-launch limits. External uptime, fork, and memory
observations remain live and are merged into the pending tuple in mutex order.
Other conflicting launch mutations fail immediately with `LaunchBusy`. Drop
releases construction state without discarding successful external observations.

This is atomic publication of the system tuple, not a transaction over every
domain touched by route composition. Task-slot exhaustion is admitted before
the system permit and is returned as launch failure. Later task, seccomp,
mapping, ptrace, host-path, and descriptor effects still rely on their existing
domain cleanup and are not collectively rolled back by `SystemLaunchUpdate`.
Cross-domain rollback for those post-permit effects remains a separate launch
composition gap; this slice does not claim launch-wide atomicity.

`SystemAuthority` is not yet a checkpoint participant. A prepared launch update
is therefore not checkpoint-portable state and must finish or drop before a
runnable route is published. Adding system identity, resource observations,
and random-sequence state to the checkpoint format remains a separate
cross-domain migration; this launch slice does not claim that capability.

## Consolidated freestanding variants

The category also owns the retained `runtime/legacy-exit/status-42` and
`runtime/legacy-write/stdout` identities. Their former folders duplicated
`source/exit.c`, `source/write.c`, `source/abi.h`, and the same expected bytes.
The manifest keeps their original non-PIE compiler and linker flags as distinct
builds, so consolidating ownership does not weaken their ET_EXEC launch and
low-address syscall-pointer coverage.

For the exit variant, `../engine/src/linux_abi/number.c` maps x86-64 syscall 60
to canonical exit 93 while AArch64 issues 93 directly; `svc_proc` records the
thread exit and code. For the write variant, `service_local` applies non-PIE
argument rebasing before `svc_io` validates stdout and the guest range,
preserves partial and error results, and raises SIGPIPE on EPIPE. The fixture
requires the complete `compat-write\n` byte count before exiting zero. Neither
variant retains a guest pointer or introduces an additional lifetime owner.
