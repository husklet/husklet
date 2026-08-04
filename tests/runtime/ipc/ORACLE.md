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

## `SHM_LOCK` / `SHM_UNLOCK` oracle audit

The complete retained SysV implementation was read again at
`../engine/src/linux_abi/syscall/sysv.c` before this family was ported. The
entry points and helpers studied were `ipc_init`, `hl_ipc_ctrl`,
`hl_ipc_lock`/`hl_ipc_unlock`, `hl_ipc_access`, `hl_ipc_owner`,
`hl_perm_init`, `hl_ipc_id`, `shm_by_id`, `shm_idx_of`, `shm_free`,
`shm_stat_to_guest`, `sem_by_id`, `sem_free`, `msg_by_id`, `msg_free`,
`shm_count`, `sem_count`, `msg_count`, `sysv_after_fork`,
`sysv_after_exec`, `sysv_on_exit`, and every `svc_sysv` case for shared
memory, semaphores, and message queues (lines 1-1992 in the retained file).

The retained control block is a namespace-scoped, shared mapping containing
fixed slot tables. Each live object owns permission and lifecycle metadata;
the slot's sequence is preserved across destruction and incremented before
reuse, and a public identifier encodes sequence plus slot. `shm_by_id` rejects
negative, vacant, removed, and sequence-stale identifiers. Shared-memory
removal first hides the key and marks the entry removed; destruction and
sequence advancement are deferred until the authoritative attachment count is
zero. The control-block robust spinlock serializes lookup, permission checking,
metadata mutation, waiter state, and destruction. Blocking semaphore and
message operations release it before polling; segment mapping also releases it
before `shm_open`/`mmap`. `shmctl` holds it continuously across identifier
resolution and the `SHM_LOCK`/`SHM_UNLOCK` owner check.

For both commands the retained implementation ignores the buffer argument,
resolves the live generation first, then calls `hl_ipc_owner`: effective uid 0,
the current owner uid, and the creator uid succeed; an unrelated effective uid
gets `EPERM`. A missing, removed, or stale identifier gets `EINVAL` before the
command switch. The two cases deliberately perform no wired-page operation and
do not alter the key, owner, mode, creator, process ids, timestamps, attachment
count, removal state, backing bytes, or sequence. There is no blocking,
partial result, cancellation, signal, or host-call path for either command.
The operation is identical for AArch64 and x86-64 guests and across hosts; only
the surrounding `semid64_ds` wire layout has a guest-architecture branch, which
is outside this family.

Fork clones attachment identity and increments attachment counts under the
shared lock, clears child `SEM_UNDO`, and does not transfer namespace-creator
ownership. Exec detaches all inherited segment mappings and clears `SEM_UNDO`.
Exit detaches remaining mappings, applies generation-qualified undo records,
and performs namespace-creator cleanup. None of these transitions creates or
persists lock/unlock state, confirming that the commands remain authorization-
only no-ops across fork, exec, exit, and checkpoint.

Rust maps slot/generation identity, ownership, removal, attachments, and the
namespace mutex to `hl-ipc::SharedMemoryNamespace`. Its typed
`SharedMemoryLockIntent` and `authorize_lock` operation own the live-generation
and root/owner/creator authorization invariant without changing the pointer-free
snapshot. `hl-linux::SysvAbi::shmctl` owns command decoding and preserves the
ignored buffer and typed unlock flag. `hl-runtime` maps both intents to that IPC
operation and projects absent, removed, and stale objects to `EINVAL`, unrelated
actors to `EPERM`, and success to zero. The unit matrix covers root, current
owner, creator, unrelated actor, both intents, removed and reused generations,
and exact snapshot immutability. The public syscall matrix covers both guest
ISAs, both commands, ignored invalid buffers, state immutability, invalid ids,
and unrelated actors. `sysv_shm_lock.c` is the retained/native differential
fixture for owner success, both intents, ignored buffers, unchanged metadata,
removed/stale identifiers, and cleanup.
