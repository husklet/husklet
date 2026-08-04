# Event syscall integration

`RuntimeEventSyscalls` dispatches `eventfd`, `eventfd2`, `epoll_create1`,
`epoll_ctl`, `epoll_wait`, `epoll_pwait`, `epoll_pwait2`, `timerfd_create`,
`timerfd_settime`, `timerfd_gettime`, `signalfd4`, `inotify_init1`,
`inotify_add_watch`, and `inotify_rm_watch`.

Epoll delivery is transactional. `hl-event` peeks a bounded batch without
changing edge-triggered or one-shot state. The runtime writes that exact batch
to guest memory and commits it afterward. A guest copyout fault abandons the
batch without consuming readiness. A concurrent `MOD`, `DEL`, or final close
invalidates the batch; the runtime retries without consuming the replacement
registration. A readiness transition that occurs while a batch is outstanding
has a newer sequence and remains queued after the older batch commits.

The stateful event families use composition-owned ports injected into
`RuntimeEventSyscalls`:

- `signalfd4` needs the calling task/thread identity and its `SignalQueue`.
- `timerfd_*` needs the selected `TimerClockSource`.
- `inotify_*` needs the workspace VFS `WatchSource`.

Absent capabilities return `ENOSYS`; the event domain does not read ambient
process state or construct host-global substitutes. Timer, signal, and watch
objects are registered against open-description identities, so descriptor
aliases share state while final close retires the operation registry and event
catalog entries. Event-shaped reads use prepared transactions: guest copyout
must complete before timer expirations, pending signals, or inotify records are
consumed.
