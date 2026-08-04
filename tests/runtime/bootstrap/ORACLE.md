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
