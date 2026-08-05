# Network data-path audit

## Retained C oracle

The retained implementation was read without modification at:

- `/Users/x/dd/engine/src/linux_abi/syscall/net.c`: the `sendmsg`/`recvmsg`
  cases gather and scatter guest vectors, preserve partial byte counts, translate
  flags and addresses, and deliver `EPIPE`/`SIGPIPE`, `EAGAIN`, and `EINTR` at
  the Linux boundary.
- `/Users/x/dd/engine/src/host/linux/host.c`:
  `hl_linux_network_send_message`, `hl_linux_network_receive_message`, and
  `hl_linux_network_readiness` issue one host `sendmsg`/`recvmsg` per operation
  and use `poll(..., 0)` only for an explicit readiness query.
- `/Users/x/dd/engine/src/host/linux/host.c`:
  `hl_linux_event_control`, `hl_linux_event_wait`, and
  `hl_linux_event_wake` own epoll registration, blocking, cancellation wakeup,
  and teardown. Socket I/O itself does not emit a wake syscall after every
  successful transfer.

The host descriptor table owns descriptor identity and lifetime. The syscall
layer owns guest-vector ordering, partial-result precedence, and errno/SIGPIPE
translation. The pollset owns readiness registration and wakeup ordering.

## Rust comparison

`native/io.rs::read` and `write` preserve one host transfer and return its exact
partial count. `SocketDescription` owns blocking retry and cancellation; the
native adapter keeps host sockets nonblocking. `reactor.rs::disarm` clears read
or write interest only after the corresponding kernel event, and the next
operation rearms it.

Before this change, `Native::arm_read` wrote to the reactor wake pipe after
every successful receive even if read interest was already armed. Repeated
`EAGAIN` writes did the same for write interest. Those writes add a host syscall
and force pollset reconstruction without changing state.

The optimization wakes only on the `false -> true` transition. A disarmed
interest still wakes immediately, while an already armed interest already
belongs to the current or next poll snapshot. Partial I/O, nonblocking errors,
blocking retry, cancellation, readiness delivery, and teardown are unchanged.
