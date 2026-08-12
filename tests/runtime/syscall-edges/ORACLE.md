# Syscall-edge oracle audit

> **Historical ownership:** The `ffi/linux/execution` path below records the
> deleted Rust executor. These syscall-edge contracts now gate selected C.

## Scope and retained implementation studied

This category is the complete 52-case `syscall_edges` manifest, not only the
original 19 `syscallbug` guests. The read-only retained implementation was
audited in `../engine/src/linux_abi/syscall/dispatch.c` (`service`,
`service_local`), `io.c` (`svc_io`, `guest_fd_*`, eventfd and descriptor-alias
state), `guest_copy.c` (`guest_span`, `guest_copy_from`, `guest_copy_to`,
`guest_iov_import`, `guest_fd_rejects`, `guest_read_would_not_copy`), `event.c`
(`svc_event`, `svc_epoll_wait_common`, epoll interest/ready bookkeeping),
`inotify.c` (bound-provider add/modify/remove/drain/close), `time.c`
(`svc_time`, timer reservation/arm/disarm), `signal.c` (`svc_signal`, restart and
poll retry), `proc.c` (`svc_proc`), `mem.c` (`svc_mem`), and the x86 syscall
number translation in `../engine/src/linux_abi/number.c`. Host-specific event,
timer, and watch adapters were inspected in
`../engine/src/host/linux/host.c`, `../engine/src/host/windows/event.c`, and
`../engine/src/host/native_compat.h`.

The first 19 cases are byte-preserved guests from the legacy `syscallbug`
corpus; the remaining registrations extend that same syscall-edge domain. All
52 logical registrations retain both production ISAs, exact Linux errno and
verdict bytes, rootfs/environment requirements, exit status, and build flags.
There are 51 source/golden pairs because `sc-sentry-cloexec-exec` and
`sc-procfd-exec` intentionally share the same source and expected output while
retaining distinct environment, dependency, and engine-path contracts.

## State, lifetime, locking, and teardown

The retained descriptor table owns descriptor-local flags while aliases share
the underlying open-file state and offset. Duplication reserves a target before
vacating/rebinding it and carries virtual provider identity. Eventfd uses a
shared counter slot, semaphore/nonblocking metadata, alias reference counts,
and a readiness pipe; counter/readiness mutation is serialized by
`g_eventfd_lock`, which is repaired after fork. Epoll owns interest and ready
state keyed by epoll generation plus target descriptor identity, batches host
changes, retires endpoint watches on close, rebuilds kqueue state after fork,
and rehomes aliases. Bound inotify providers own watch tokens and queued events,
are registered for notification, clone across fork, and retire watches and host
resources on final close. Timer state owns reservation, deadline, interval,
overrun count, and wake thread; fork repairs its synchronization and teardown
disarms before releasing a slot.

The Rust ownership mapping is: descriptor identity, generations, aliases,
descriptor flags, and operation leases in `src/runtime/hl-descriptor`; eventfd,
epoll, inotify, timerfd counters/readiness/subscriptions/retirement in
`src/runtime/hl-event`; joined epoll, event, filesystem, process, signal, memory,
and descriptor operations in `src/runtime/hl-runtime`; VFS file/splice behavior
in `src/runtime/hl-vfs`; guest ABI routing and coarse native boundary in
`src/containers/hl-engine/src/ffi/linux/execution`.

## Ordering, partial results, blocking, and errno

The oracle validates descriptor/access mode before touching guest buffers where
Linux requires `EBADF`; an EOF or empty nonblocking source returns zero or
`EAGAIN` without copyout, while data pending across a failed copyout remains
pending. Vector admission bounds count and address arithmetic before allocation,
preserves zero-count results, validates offsets/flags, and returns partial I/O
when a prefix completed. Eventfd short transfers are `EINVAL`, bad pointers are
`EFAULT`, maximum-counter overflow is `EAGAIN`, zero writes do not change state,
and semaphore reads decrement by one. Timer, poll, signal-set, scheduler, rlimit,
path, open, ioctl, splice, wait, and memory arguments are rejected with their
Linux errno before state mutation. Epoll preserves add/modify/delete
`EEXIST`/`ENOENT`/`EINVAL`, level readiness until drain, oneshot rearm, and
timeout/signal interruption ordering. Inotify removal queues `IN_IGNORED` and
does not consume unrelated descriptors; undersized reads do not consume queued
events. Close/exec/fork preserve shared OFD state while applying descriptor-local
`FD_CLOEXEC`.

## Architecture and host matrix

Both AArch64 and x86-64 guests are registered for every case. AArch64 dispatches
native Linux syscall numbers; x86-64 is normalized through `number.c`, including
legacy dup2-versus-dup3 distinctions. Linux uses epoll/eventfd/timerfd/inotify
host mechanisms. macOS uses kqueue and emulated provider state, including delayed
epoll submission and explicit fork rebuild. Windows uses its event adapter and
cannot assume a native eventfd-shaped object. Guest-visible errno, flags,
descriptor identity, and readiness remain Linux contracts across hosts. The
`efault-syscalls` page-straddle case is explicitly unsupported for the retained
macOS oracle because that engine maps guarded guest pages host-readable; it is
kept visible rather than silently omitted.

## Capability comparison

| Capability | Rust owner | State |
|---|---|---|
| guest copy, vector bounds, zero/partial-copy ordering | `hl-runtime` filesystem/memory plus Linux marshaling | implemented; corpus acceptance pending |
| dup/fcntl descriptor-local flags and shared OFD state | `hl-descriptor`, `hl-runtime` filesystem | implemented; corpus acceptance pending |
| eventfd counter, semaphore, blocking, readiness, aliases | `hl-event`, `hl-runtime` event | implemented; corpus acceptance pending |
| epoll identity, level/edge/oneshot, wake and retirement | `hl-event` epoll, `hl-runtime` epoll | implemented; corpus acceptance pending |
| inotify watch identity, queue/non-consumption, retirement | `hl-event` inotify, `hl-runtime` event/filesystem | implemented; corpus acceptance pending |
| timerfd validation, interval, queued expirations | `hl-event` timerfd, `hl-runtime` event | implemented; corpus acceptance pending |
| signal validation, masks, routing, interruption | `hl-task`, `hl-runtime` signal | implemented; corpus acceptance pending |
| rlimit, scheduler, wait, identity validation | `hl-runtime` process | implemented; corpus acceptance pending |
| path/open/append/positional/splice semantics | `hl-vfs`, `hl-runtime` filesystem | implemented; corpus acceptance pending |
| macOS `PROT_NONE` straddle enforcement | native/memory host adapter | divergent; explicit unsupported evidence |

No application, runtime, executable-name, or vendor-specific branch is required
by this category. Failures must be assigned to the generic ownership rows above.
