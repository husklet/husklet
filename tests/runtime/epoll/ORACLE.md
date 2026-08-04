# Epoll lifecycle oracle

This workload was migrated from `tests/runtime/legacy/source/epoll.c`. Its checked
output remains the exact `epoll-ok\n` byte sequence from the legacy golden file.

## Retained C engine audit

Read-only oracle files studied:

- `../engine/src/linux_abi/epoll.c`: `hl_linux_epoll_create`,
  `hl_linux_epoll_control`, `epoll_subscribe`, `epoll_sample`,
  `hl_linux_epoll_wait`, `epoll_clone`, `epoll_retire`, and `epoll_close`.
- `../engine/src/linux_abi/epoll.h`: operation, interest, event-layout, and public
  entry-point contracts.
- `../engine/src/linux_abi/eventfd.c`: `eventfd_read`, `eventfd_write`,
  `eventfd_readiness`, subscription lifecycle, clone/close, and create/install.

The C epoll object owns a mutex, wake handle, dynamically sized watch table, and
monotonic nonzero watch tokens. A watch records both descriptor and OFD identities
with generations, its interest/data state, previous readiness for edge detection,
oneshot disablement, and subscription ownership. Control pins the epoll OFD,
snapshots the target identity, rejects self/nested registration and invalid
ADD/MOD/DEL state, updates the watch under the object lock, subscribes outside the
lock, and wakes blocked waiters. Wait samples readiness before sleeping, releases
the object lock during the host wait, then relocks and checks retirement; timeout,
interruption, stale target, edge transitions, and oneshot disable/rearm are explicit.
Close unsubscribes every live watch before closing the wake handle and freeing the
table and lock. Clone duplicates the wake object and watch state but resets active
subscriptions so the child owns fresh registrations.

The eventfd object owns a host counter, synchronization handle, and bounded
subscription array. Reads require at least eight bytes and consume exactly eight;
writes require exactly eight and reject `UINT64_MAX`. Subscription mutation is
locked, clone duplicates the counter with an empty subscription table, and final
close unsubscribes before closing the counter and lock. The workload relies on the
shared OFD surviving `dup` plus one descriptor close, and on final alias close
retiring the epoll watch without stale delivery.

There are no guest-architecture branches in these retained domain owners. The
test source carries the Linux syscall-number and packed `epoll_event` ABI
difference for AArch64 versus x86-64. Host-specific readiness is behind the C host
event/counter service boundary.

## Rust ownership mapping

| C capability | Rust owner | Status exercised here |
|---|---|---|
| epoll entity, watch identity, edge/oneshot state | `hl-event/src/epoll.rs` | level, edge, oneshot, MOD, final-target close |
| eventfd counter and readiness | `hl-event` eventfd implementation | read/write readiness and lifecycle |
| guest epoll struct and syscall ABI | `hl-linux/src/event/abi.rs` | AArch64 and packed x86-64 layouts, bad pointer |
| syscall composition and blocking/cancellation | `hl-runtime/src/event/{syscalls.rs,epoll.rs}` | create, control, zero-time wait |
| descriptor/OFD alias lifetime | `hl-descriptor` plus `hl-runtime` descriptor integration | `dup`, one close, final close |

This one workload does not prove blocking, cancellation, fork, checkpoint,
cross-thread wakeups, nested epoll, or the complete errno surface. Those remain
separate compatibility cohorts rather than inferred passes.

## Syscall transaction boundary

The 2026-08-04 boundary audit additionally followed the legacy syscall path in
`../engine/src/linux_abi/syscall/event.c`: `svc_event` cases 20--22 and 441
(`epoll_create1`, `epoll_ctl`, `epoll_pwait`, and `epoll_pwait2`) and
`svc_epoll_wait_common`. The control path reads an ADD/MOD event before
descriptor admission, then validates the epoll descriptor, self-registration,
the target descriptor and pollability, the operation, and membership. DEL does
not require a readable event pointer. The wait path recomputes a monotonic
remaining timeout after internal wakeups, releases its event lock while
blocking, restores a temporary signal mask, and copies exactly the events it
delivered. In particular, lines 1023--1037 perform no guest output access when
the delivered count is zero.

The Rust event and descriptor objects already preserve watch identity,
retirement, blocking, interruption, edge, and oneshot state. The audited gap was
confined to `hl-linux/src/event/abi.rs` and
`hl-runtime/src/event/epoll.rs`: wait planning eagerly required writable space
for `maxevents`, and control decoded the operation before reproducing the Linux
descriptor-error order. Boundary tests therefore cover both guest layouts,
compound-invalid control calls, a zero-event wait with an invalid output
address, and a one-event copy fault followed by successful redelivery. The last
case is load-bearing: readiness is committed only after the delivered bytes are
copied, so an `EFAULT` cannot consume an EPOLLONESHOT event.
