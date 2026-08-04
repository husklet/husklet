# SysV IPC compatibility oracle audit

Retained C was studied read-only in `../engine/src/linux_abi/syscall/sysv.c`, including the complete shared-memory, semaphore, and message entry points, object tables, wait queues, fork inheritance, `SEM_UNDO`, removal, interruption, timeout, and teardown paths. Objects are identity-bearing table entries; locks protect state and wait queues, host calls are not made while table locks are held, and removal wakes blocked operations with Linux errno semantics. Rust ownership maps to `hl-ipc` entities and `hl-runtime` fork/syscall composition.

`sysv-fork` covers attachment identity, peer visibility, undo, and deferred removal. `sysv-blocking` covers wake, removal, timeout, and signal interruption. Both guest ISAs retain the exact sources and stdout contracts.

The appended byte-exact `socket_stop.c` seed was studied with the retained Unix
`socketpair` and blocking read paths in `../engine/src/linux_abi/syscall/net.c`
and stop/cancellation handling in `../engine/src/core/dispatch.c`. Each endpoint
has descriptor/OFD ownership; an idle read sleeps on the socket wait queue until
external engine stop cancels it, then teardown unregisters the wait, closes both
endpoints, and reaps the process. Rust maps this to `hl-network` stream state,
`hl-descriptor`, and `hl-runtime` cancellation/retirement. The unified runner
does not yet express expected external stop, so the case is registered broken
with evidence rather than converted into an ordinary timeout contract.

## Complete retained IPC domain audit

The compatibility category was checked against the retained implementation,
not merely its fixture manifest. The read-only files studied were
`../engine/src/linux_abi/epoll.c` (`hl_linux_epoll_create`,
`hl_linux_epoll_control`, `hl_linux_epoll_wait`, subscription and teardown),
`eventfd.c` (`hl_linux_eventfd_create`, send/receive, read/write/readiness,
clone and close), `pipe.c` (endpoint read/write/status/readiness, clone and
close), `syscall/sysv.c` (`ipc_init`, `svc_sysv`, fork/exec/exit hooks and the
complete shm/sem/msg tables), `syscall/net.c` (`svc_net`, sockaddr and message
bounce paths), and `syscall/binding.c` (poll/ppoll/select, SCM_RIGHTS and the
event/timer/signal descriptor bindings).

The retained epoll object owns a mutex, a generation-qualified OFD watch table,
subscriptions and one wake handle. Control preserves `EEXIST`, `ENOENT` and
`EINVAL`; wait snapshots under the lock, samples without retaining that lock
across object or host calls, records edge history, disables oneshot watches, and
relocks after interruptible waits. Clone copies identity-bearing watches but
forces subscription reconstruction; close unsubscribes before freeing storage.
Eventfd owns its shared counter and bounded subscriptions, preserving asymmetric
Linux read/write size rules, overflow/EAGAIN behavior, OFD flags and SCM transfer
identity. Pipes own two endpoints over shared bounded state with partial
transfer, EOF/SIGPIPE, nonblocking readiness, capacity and close wakeup semantics.
SysV objects are namespace table entries with sequence-qualified IDs,
permissions, per-family locks and wait state, plus explicit fork, exec,
`SEM_UNDO`, removal, interruption and process-exit transitions. Unix and INET
sockets preserve datagram boundaries, credentials, truncation flags, SCM_RIGHTS
ownership transfer, descriptor-local flags and peer-close wakeups. Timerfd and
signalfd are OFD-scoped event objects whose readiness and masks survive dup,
fork and descriptor transfer.

The Rust ownership mapping is `hl-event` for epoll, eventfd, timerfd, signalfd
and inotify state machines; `hl-ipc` for pipes, POSIX queues and SysV shared
memory/semaphore/message objects; `hl-network` for Unix/INET sockets and queued
ancillary messages; `hl-descriptor` for generation-safe OFDs and atomic
SCM_RIGHTS installs; `hl-task` for signal/process identity; and `hl-runtime` for
cross-domain syscall, fork/exec, epoll-target and signal-delivery adapters. The
126 canonical registrations exercise the full retained category. Seven
host-policy divergences remain typed `broken` with their retained Linux evidence;
both legacy untrusted registrations remain distinct generic-engine cases so
registration coverage is not erased.

The broad AArch64 QEMU check additionally exposed one typed oracle-provider
divergence: `scm-rights-trunc` produced `ctrunc=0 nfds=2 readable=2 no_leak=1`,
while the retained checked Linux contract is
`ctrunc=1 nfds=2 readable=2 no_leak=1`. The case remains registered as broken;
its source, expected bytes and engine acceptance contract are unchanged.
`scm-epoll` is also typed broken for the oracle provider: AArch64 QEMU exits 1
after producing `n=0 data=0`, while the retained contract is `n=1 data=5151`.
