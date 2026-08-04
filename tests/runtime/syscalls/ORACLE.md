# Linux syscall oracle audit

This folder migrates the complete legacy `compat/syscall` registration as one
semantic category. The 88 registered cases preserve their guest bytes, compiler
flags, two-ISA scope, expected exits, and stdout contracts. The two deterministic
adapters remain the executable sources: `epoll_fin.c` removes only the unstable
success timing observation, and `pidfd_signal.c` waits for SIGTERM settlement
before cleanup. Their unmodified source witnesses are retained as
`epoll_finraw.c` and `pidfd_raw.c`.

## Retained C implementation studied

The read-only oracle was audited at `../engine/src/linux_abi/syscall/`: `dispatch.c`
(`service`, canonical-number dispatch and errno return), `nonpie_args.h`
(`nonpie_rebase_args`, pointer-position translation), `guest_copy.c` (bounded
pin/copy and partial-result handling), `binding.c` (typed descriptor/path/poll
routing), `aio.c` (`svc_aio`), `event.c` (`svc_event`), `inotify.c`, `io.c`
(`svc_io`), `fs.c` (`svc_fs`), `proc.c` (`svc_proc`), `signal.c` (`svc_signal`),
`time.c` (`svc_time`), `mem.c` (`svc_mem`), `rare.c` (`svc_rare`), `misc.c`, and
the shared ownership/lifecycle operations in `helpers.c`.

The C engine owns per-process descriptor visibility plus typed provider state;
open-file-description aliases survive dup/fork, descriptor-local close-on-exec
state is retired during exec, and child repair rebuilds locks, watches, mappings,
timers, AIO, and event registrations. Table locks protect identity changes but are
not held across blocking host calls. Blocking poll/epoll paths distinguish pending
guest signals, Linux never-restarted waits, timeout expiry, and `EINTR`; I/O paths
validate vectors and guest spans before host work and preserve partial results.
Close, dup, range-close, epoll membership, inotify queues, eventfd counters,
timerfd expirations, AIO completions, and pidfd/process state all have explicit
teardown hooks. Architecture differences are confined to canonical syscall
numbering, structure layouts, and pointer translation; macOS uses typed provider
adapters where host Linux syscalls are unavailable.

## Rust ownership map

- syscall admission, canonical frames, and errno conversion: `hl-linux`;
- descriptor/OFD identity and flags: `hl-descriptor` plus `hl-runtime` adapters;
- AIO, epoll, signalfd, timers, task/fork/exec, VFS, memory, and process joins:
  their sibling runtime crates and `hl-runtime`;
- host calls and platform-specific provider composition: `hl-engine` Linux/macOS
  FFI adapters.

The seven cases marked `!unsupported` are retained and visibly enumerated because
their legacy contract documents a macOS-provider divergence. They remain valid
Linux/QEMU compatibility contracts; the status does not weaken or discard their
expected bytes.

## QEMU differential evidence

On 2026-08-03 all 88 sources cross-compiled in parallel for both ISAs (176 static
ELFs; 88 AArch64 and 88 x86-64). Bounded QEMU runs matched 72/88 AArch64 and
71/88 x86-64 contracts byte-for-byte. The typed `!broken` cases preserve these
observed divergences: AIO setup/opcode/persistence fails with exit 1 on both
ISAs; `prctl-ltp` exits 1 on both; epoll-pwait2, finite epoll reblock, fanotify,
getrandom length, high-fd emulation, iov bounds, pidfd signal, pipe2 bad flags,
process-vm, and pwritev2 return the expected exit but different bytes on both;
`modern-procfd` differs only on x86-64. The already-unsupported `fcntl-cmds` and
`seccomp-probe` also differ on both and remain classified by their stronger
documented platform constraint. No QEMU row timed out under the eight-second
per-process bound.

### Retained shared-fixture provenance

The former `tests/compat/fixtures` probes now owned here were audited against
`../engine/src/linux_abi/epoll.c` (`epoll_subscribe`, `epoll_unsubscribe`,
`epoll_sample`), `eventfd.c`, and `syscall/{event,time,inotify,fs,mem}.c`.
Those owners preserve descriptor/OFD lifetime, edge/oneshot rearming, fork
inheritance, blocking wakeup and errno ordering. Both ISA dispatch paths enter
the same Linux-ABI owners. Exact duplicates already owned by `epoll_inf.c` were
not registered twice. Rust ownership maps to `hl-event`, `hl-descriptor`,
`hl-memory`, `hl-vfs`, and their `hl-runtime` cross-domain adapters.
