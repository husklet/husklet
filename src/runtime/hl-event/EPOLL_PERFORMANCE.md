# Epoll readiness performance audit

The retained C engine under `../engine` was inspected read-only.

## Retained implementation and ownership

- `src/linux_abi/epoll.c`: `epoll_notify`, `epoll_unsubscribe`,
  `epoll_close`, `epoll_retire`, `epoll_clone`, `epoll_ready`,
  `epoll_subscribe`, `watch_index`, `hl_linux_epoll_control`,
  `epoll_sample`, and `hl_linux_epoll_wait`.
- `src/linux_abi/syscall/event.c`: `svc_epoll_wait_common`, temporary signal
  mask handling, kqueue change submission, ready-at-registration priming,
  cross-thread `EVFILT_USER` wakeup, membership publication, close/dup
  lifecycle, fork reconstruction, SCM transfer, and architecture-specific
  `epoll_event` encoding.
- Host event services provide create/control/wait/wake/close. The generic C
  object owns a mutex-protected watch array and a host wake object. A watch is
  keyed by descriptor generation plus open-file-description generation and
  owns exactly one host or callback subscription. Retirement wakes waiters;
  final close quiesces subscriptions. Wait loops sample before blocking,
  preserve absolute deadline, retry internal-only wakes, and expose
  interruption as `EINTR`.

The Rust `hl-event::Epoll` owns the corresponding watch lifecycle under one
state mutex, a FIFO ready-token queue, per-target subscriptions, readiness
sequence/revision validation, a condition variable, cancellation subscription,
and absolute timeout deadline. `hl-runtime` owns graph validation, descriptor
composition, guest copyout, temporary masks, and errno conversion.

## Finding and change

Every Rust readiness callback linearly searched all watches by token. Peeking a
ready set repeated that search for every queued token, and committing a batch
linearly searched watches twice per event then scanned the entire ready queue
once per selection. At the supported 4,096-watch limit, publication was
quadratic across a ready burst and batch polling/commit was quadratic again.

`EpollState` now owns a token-to-index hash index. Add, restore, delete,
retirement pruning, and final retirement maintain it under the same lock.
Batch commit validates through the index, updates each selected watch, removes
all consumed tokens in one ready-queue pass, then appends unchanged
level-triggered selections in their original order. Newer edges remain queued;
oneshot, edge, revision rollback, FIFO order, wake, cancellation, timeout, and
subscription lifetime semantics are unchanged.

## Exact A/B evidence

Base and candidate derive from `24a44d4e6` in isolated worktrees. The release
diagnostic registers the supported maximum of 4,096 eventfds, publishes all
ready, then samples and commits the complete set. Seven alternating pairs:

```text
publication base ns/event:      1366 1385 1385 1358 1357 1361 1361
publication candidate ns/event:  392  396  397  397  397  400  401
median:                         1361 -> 397 ns (-70.8%, 3.43x)

poll+commit base ns/event:      5215 5209 5238 5109 5161 5145 5170
poll+commit candidate ns/event:  100  117   73   88   93   88   75
median:                         5170 -> 88 ns (-98.3%, 58.8x)
```

The diagnostic isolates Rust readiness bookkeeping from host scheduling. The C
oracle's generic watch sampling is also array based, while its native host path
delegates the ready set to kqueue/epoll. This change removes the avoidable Rust
quadratic work; host/native end-to-end latency remains a separate benchmark.
