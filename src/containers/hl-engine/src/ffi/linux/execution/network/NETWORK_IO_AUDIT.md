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

## Pinned loopback evidence

On 2026-08-04, the typed ignored benchmark was run in seven alternating pairs
on Linux AArch64 pinned to CPU 17. It performs 20,000 64-byte write/read cycles
after socket, token, and reactor setup for both `SOCK_STREAM` and `SOCK_DGRAM`.
Every row produced checksum `1800000`. The test binaries were invoked directly:

```text
taskset -c 17 <test-binary> --exact --ignored --nocapture \
  ffi::linux::execution::network::native::kind_cache_test::loopback_stream_and_datagram_data_path
```

The instrumented baseline was commit `ed55aa0b80c0b44876b7ae95bb04ef2aa0649c2c`,
binary SHA-256 `71148694abb0b2939d5c910a46310102b3febfcd5900d878d8976254f4ce8531`.
The candidate was commit `852cfbf167b0b4863f2705a9f221cb7c6c8dbbde`, binary SHA-256
`96917b36283cca42f428ba6c8dca32b8f78bfca4f0c6d33d04ea222b0347ef43`.

Raw rows, in pair order:

```text
baseline stream ns: 32890875 30303166 30668583 30384333 29428334 31362000 30792416
baseline stream wakes: 20000 20000 20000 20000 20000 20000 20000
baseline stream pollsets: 11692 10976 11759 10913 10989 11712 11184
candidate stream ns: 27285959 25924541 25573208 25592209 26387042 25478000 26161917
candidate stream wakes: 4522 4432 4436 4531 4379 4458 4694
candidate stream pollsets: 9044 8864 8872 9062 8757 8915 9387
baseline datagram ns: 30855750 30295125 31097959 31272958 31780500 31740375 32236833
baseline datagram wakes: 20000 20000 20000 20000 20000 20000 20000
baseline datagram pollsets: 11569 10861 11585 11624 12241 11999 11644
candidate datagram ns: 26730791 26669750 27848500 25867334 26890709 27786834 26587458
candidate datagram wakes: 4890 4916 5118 4531 4644 5102 4834
candidate datagram pollsets: 9780 9832 10236 9062 9288 10203 9668
```

Stream median fell from 30.669 ms to 25.925 ms (`-15.47%`), wake writes
from 20,000 to 4,458 (`-77.71%`), and pollset builds from 11,184 to 8,915
(`-20.29%`). Datagram median fell from 31.273 ms to 26.731 ms (`-14.52%`),
wake writes from 20,000 to 4,890 (`-75.55%`), and pollset builds from 11,624
to 9,780 (`-15.86%`). Candidate timing ranges did not overlap baseline.

Warning-strict verification at the optimization commit passed all 27 focused
network tests and the complete `hl-engine` library suite: 481 passed, zero
failed, two ignored.
