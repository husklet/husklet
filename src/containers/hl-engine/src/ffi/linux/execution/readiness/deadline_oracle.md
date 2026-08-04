# Deadline publication oracle

The 2026-08-04 timer-wake audit inspected the retained C engine read-only:

- `../engine/src/linux_abi/syscall/time.c`: `gtimer_arm`, `gtimer_loop`,
  `gtimer_disarm`, `gtimer_take_overrun`, and syscall cases 107--111;
- `../engine/src/linux_abi/syscall/signal.c`: pending-signal delivery at the
  dispatcher boundary;
- `../engine/src/host/linux/host.c`: `hl_linux_event_wait`,
  `hl_linux_event_arm_timer`, and `hl_linux_event_wake`;
- `../engine/src/host/macos/host.c`: `hl_macos_event_wait`,
  `hl_macos_event_arm_timer`, and `hl_macos_event_wake`.

The host owns timer handles and publishes expiry records through a process-owned
pollset. The single retained timer worker consumes an expiry, takes the timer
table lock, advances or disarms the timer, publishes the guest signal and its
metadata, and only then calls `sfd_deliver` to wake guest-visible readiness.
Timer deletion and disarming hold the same lock, so an observed readiness wake
cannot precede the state transition it represents. Linux uses timerfd/epoll;
macOS uses kqueue timers. Guest ISA affects later signal-frame construction but
not this publication order. Fork discards inherited POSIX timers and rebuilds
the process-owned timer table lazily.

The Rust deadline queue had the opposite order: it wrote its eventfd before
invoking the scheduled callback. Under parallel load, a poller could consume
that readiness while timer state was still unpublished. The queue now completes
the callback before making its eventfd readable. A gated concurrency test holds
the callback in progress and proves that readiness remains unobservable until
the callback is released.
