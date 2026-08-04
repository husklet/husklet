# Status process-exit oracle audit

The migrated workload is the former bootstrap `status` seed. It writes the exact
seven bytes `status\n` to descriptor 1, then invokes the per-thread Linux
`exit(2)` syscall with status 17. It has no allocation, blocking operation,
cancellation point, signal handler, lock, child, shared state, or teardown
resource of its own.

## Retained C implementation studied

- `tests/runtime/legacy/source/status.c`, `_start`, and
  `tests/runtime/legacy/source/abi.h`, `guest_call`, `guest_write`, and
  `guest_exit`: workload entry, raw syscall ABI, write ordering, and terminal
  status. These were byte-preserved into a self-contained source file before the
  legacy artifacts were removed.
- `../engine/src/linux_abi/syscall/proc.c`, the complete process-syscall switch,
  with the `exit` (AArch64 93) and `exit_group` (AArch64 94) entries studied in
  their surrounding credential, namespace, futex, clone, wait, and teardown
  paths. `exit` marks only the current CPU/thread exited and retains the full
  integer on the CPU until lifecycle publication; `exit_group` performs robust
  futex, Unix rendezvous, accounting, proc registration, descriptor identity,
  provider descriptor, advisory-lock, and SysV `SEM_UNDO` teardown before
  publishing and `_exit`.
- `../engine/src/linux_abi/syscall/dispatch.c`, `run_guest`: owns the emulated CPU
  lifetime and returns a thread `exit` result to its caller; the fork-server
  `g_noexit` path turns group exit into a normal run-loop unwind.
- `../engine/src/core/lifecycle.c`, `hl_production_finish_process`: validates the
  published child result against the host wait result, distinguishes code from
  signal, normalizes macOS translated-fault `SIGBUS` to Linux `SIGSEGV`, rejects
  corrupt/out-of-range publication, and returns the guest code.
- `../engine/src/host/process.h`, the process-service ownership contract, plus
  `../engine/src/host/macos/host.c`, process wait/result functions, and
  `../engine/src/host/windows/process.c`, process exit/wait functions: the host
  process object owns the OS child identity and waitable lifetime; POSIX retries
  interrupted waits and decodes `waitpid`, while Windows masks exit codes to the
  Linux low eight bits and uses process handles. Handles/registrations are
  released by their process owner after result consumption.

## Rust ownership and capability matrix

| Capability | Retained C behavior | Rust owner | State |
|---|---|---|---|
| syscall numbers | arm64 `write=64`, `exit=93`; amd64 `write=1`, `exit=60` | `hl-linux` syscall tables and ISA decoder | implemented |
| write ordering/result | descriptor 1 write completes before exit; negative/partial result is observable | `hl-runtime` filesystem dispatch + `hl-descriptor` OFD | implemented; fixture rejects partial write with 111 |
| per-thread exit | retire caller; group remains while peers live | `hl-runtime/src/process/dispatch.rs`, `retire.rs` | implemented |
| group exit | retire all threads, publish one terminal process result | `hl-runtime/src/process/control.rs`, `retire.rs`, `hl-task` registry | implemented |
| exit status | Linux-visible low eight bits; code distinct from signal | `hl-task::ExitStatus`, engine execution result | implemented |
| teardown | robust futex, task/process and cross-domain owned resources retire in lifecycle order | `hl-runtime` process retirement adapters plus owning domains | implemented; broad teardown parity remains covered by dedicated process cohorts |
| errno | successful `write` returns byte count; this valid `exit` does not return | Linux personality boundary | implemented |

The fixture is architecture-specific only at raw syscall entry. Process semantics
are shared. Host differences remain behind the engine process adapter: POSIX uses
wait status and signals; Windows uses a process handle and explicit exit code.
The case exercises neither a blocking nor cancellable path, so `EINTR` and restart
semantics are intentionally outside this seed and remain owned by the broader
process/wait cohorts.
