# Process observation audit

## Retained C oracle

This lane studied the read-only retained implementation in:

- `../engine/src/linux_abi/syscall/proc.c`: `syscall_proc`, especially the
  credential mutation cases around `setuid`/`setgid` and the direct
  `getppid`/`getuid`/`geteuid`/`getgid`/`getegid` cases.
- `../engine/src/linux_abi/thread.c`: process/thread identity construction and
  teardown, including generated guest thread IDs.
- `../engine/src/linux_abi/syscall/dispatch.c`: syscall dispatch into
  `syscall_proc`.

The retained engine owns real/effective credential and parent identity in the
current process state (`g_ruid`, `cred_euid`, `g_rgid`, `cred_egid`, and
`g_self_gppid`). Simple identity syscalls read those fields directly. Credential
mutation validates the requested transition before replacing the corresponding
process-owned values. These reads neither enumerate unrelated processes nor
construct a global process image.

Process identity is established during task creation and inherited across fork;
exec preserves the process identity while replacing the executable image.
Teardown retires the task identity. Architecture-specific syscall numbers and
register decoding are handled before `syscall_proc`; the process observation
semantics are shared. The simple getters do not block, allocate, partially
complete, or return an errno after successful dispatch. Mutation cases preserve
their explicit permission and validity errors. The retained implementation uses
process-global current-task state and therefore needs no registry lookup lock.

## Rust ownership and mapping

Rust owns process state in `hl_task::TaskRegistry`. A `ProcessId` contains a slot
and generation; registry lookup checks both while holding the registry mutex, so
a retired slot reused by a later process cannot satisfy an observation for the
old identity. `process_snapshot` now clones exactly one process while that lock is
held and releases the lock before the caller performs guest-memory writes or any
other host work. It uses the same field constructor as the full registry
snapshot, preserving credentials, parent, lifecycle, signal, namespace, limit,
and control state without scanning or cloning unrelated process, thread,
session, or process-group slots.

`RuntimeProcessSyscalls::snapshot` is the shared observation boundary for the
current-process identity, credential/capability, namespace, and `prctl` query
families. It now returns `ProcessObservation`, which contains only the parent,
credentials, and simple control fields consumed by that family. It does not
clone children, threads, arguments, limits, signal actions, pending signal
queues, namespace topology, or process-group/session state. A missing or stale
process continues to map to `ESRCH`. Rich one-process and full registry snapshots remain at
call sites that genuinely need global topology or enumeration, including
process counts, pidfd resolution, signal fanout, retirement, procfs, and
checkpoint capture.

## Evidence

The focused registry test uses 65,536 process and thread slots, proves the direct
snapshot is exactly equal to the corresponding entry in a full snapshot, and
proves a mismatched generation is rejected.

On ARM64 pinned to CPU 17, nine repeats of 499 guest `getuid` syscalls produced
the same checksum (`5,010,000`) and native diagnostics before and after the
change. The Rust-engine syscall phase fell from 138,703 us to 6,544 us
(-95.28%); wall time fell from 167,522 us to 22,715 us (-86.44%). The retained C
oracle measured 837-880 us and host-native measured 912-961 us in the paired
runs. This removes the global-registry scan but does not claim native parity;
the remaining 7.18x guest-phase gap belongs to later syscall-boundary and
single-process observation work.

A second exact-tree comparison split that simple observation from the rich
`ProcessSnapshot`. Against the preceding one-process-snapshot commit, the same
nine-repeat workload fell from 6,722 us to 4,334 us (-35.52%) with checksum and
native diagnostics unchanged. Host-native measured 850-896 us and the retained
C engine measured 841-918 us. The resulting Rust guest phase is 5.10x native;
wall measurements (23,615 us versus 24,020 us) were dominated by startup noise
and do not establish a wall-time improvement.
