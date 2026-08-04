# Syscall core oracle audit

This cohort preserves the retained engine's `core/syscall` acceptance boundary.
The retained tree at `../engine` was read only. The fixtures alone were not used
as an implementation specification.

## Retained implementation studied

The audit followed syscall entry through `src/linux_abi/syscall/dispatch.c` and
its family dispatch into:

- `helpers.c`: `svc_done`, descriptor/OFD lookup, shared cursor and file-lock
  machinery;
- `guest_copy.c`: `guest_span`, `guest_copy_from`, `guest_copy_to`,
  `guest_iov_import`, accessible-prefix handling, and scalar/vector descriptor
  transfer;
- `io.c`: `sparse_seek_fallback`, `io_guest_vector_gather`,
  `io_guest_vector_scatter`, and `svc_io`;
- `event.c`: provider/object watch registries, `svc_epoll_wait_common`, and
  `svc_event`;
- `fs.c`: `guest_fill_linux_stat`, `guest_statfs_magic`, and `svc_fs`;
- `signal.c`: `svc_poll_retry` and `svc_signal`;
- `time.c`: dynamic-clock lookup and `svc_time`;
- `mem.c`: `guest_bad_ptr`, `svc_vm_iov_copy`, and `svc_mem`;
- `net.c`: socket-address/message bounce handling and `svc_net`;
- `container/netns.c`: `cmsg_l2m`, `cmsg_m2l`,
  `cmsg_tmpfds_close`, `cmsg_inflight_hold`, `cmsg_inflight_finish`, and
  `cmsg_import_ofd_trailer` for SCM_RIGHTS framing and lifetime;
- `proc.c`: `svc_proc`.

The call boundary in `src/core/dispatch.c::run_guest` and architecture syscall
number/layout selection were also checked for return and signal-delivery
semantics. This syscall domain has no standalone assembly implementation; the
architecture entry machinery supplies register state, while the files above own
the behavior exercised here.

## State, lifetime, and locking

- Descriptor numbers own table entries; open-file descriptions own shared
  cursor/status state. Duplication and fork retain the OFD, while last close
  releases locks and watches. Descriptor generation prevents a reused number
  from reviving a stale epoll registration.
- SCM_RIGHTS sends retain the transferred open-file description independently
  of the sender's descriptor. A successful receive installs one newly owned
  descriptor atomically; truncation or conversion failure closes temporary
  descriptors instead of leaking them. The canonical send-then-close sequence
  therefore keeps the file alive and preserves its shared offset until the
  receiver closes its installed descriptor. On macOS, explicit in-flight
  holds bridge XNU's different Unix-rights garbage-collection behavior; Linux
  uses the kernel's native retention.
- Guest-copy projections are bounded and pinned for the operation. They preserve
  an accessible prefix, so a later invalid page yields a partial result instead
  of discarding completed I/O. Table locks are not held across host I/O.
- Epoll registrations retain object identity independently of the descriptor
  number. Mutation invalidates stale observations, and teardown waits for
  in-flight callbacks before destroying their state.
- Paths are resolved through the retained jailed VFS ownership boundary.
  Directory cursors, file offsets, temporary-file visibility, rename atomicity,
  and procfs task/descriptor identities therefore outlive individual syscall
  frames.
- Pending signals are task/process state. Signalfd consumes only masked matching
  signals, while ordinary delivery occurs at the dispatcher safe boundary.
- Mappings own guest address/protection state; protection changes serialize with
  guest projections. Clock deadlines and timer state use monotonic ownership and
  are cancelled by signal/task teardown.

## Ordering and Linux results

The retained implementation validates descriptor and iovec shape before host
work, bounds counts before allocation, preserves zero-length operations, and
commits shared offsets only for transferred bytes. `sendfile` and `splice`
preserve explicit-offset versus OFD-offset behavior and return partial progress
at EOF or interruption. File locks use stable OFD/process identity and release
on the Linux-defined close path.

Polling retries internal wakeups, but a deliverable guest signal produces
`EINTR`. Signal queueing precedes wakeup; `SIGPIPE` is queued only after the
write-side failure is established. Absolute clock sleeps retain their deadline
across internal wakeups. `svc_done` is the common family-tail conversion from
host result to exact Linux negative errno, including `EFAULT`, `EAGAIN`,
`EWOULDBLOCK`, and partial-result precedence.

## Architecture and host branches

AArch64 and x86-64 select different syscall numbers and guest structure layouts,
but converge on the same family functions. The retained code accounts for split
offset/word widths and guest alignment before invoking those functions. Host
branches normalize errno and filesystem behavior, provide sparse-seek fallback
when `SEEK_DATA`/`SEEK_HOLE` is absent, emulate Linux-only temporary-file and
statfs details, and select the host polling mechanism. The retained macOS lane
explicitly excludes the `mprotect` case even though both Linux QEMU oracles can
execute it; this status is preserved rather than converted into a false pass.

## Rust ownership comparison

| Retained capability | Rust owner | Audit state |
| --- | --- | --- |
| syscall number/layout and Linux result marshalling | `hl-linux`; `hl-engine/src/ffi/linux/execution` | implemented, still acceptance-gated by this cohort |
| descriptor identity, OFD sharing, close/dup/fork | `hl-descriptor`; `hl-runtime`; engine routing | implemented |
| scalar/vector I/O, sendfile and splice transactions | `hl-descriptor`; `hl-ipc`; engine `execution/path` | implemented, cross-domain composition remains in engine adapters |
| paths, directory cursors, metadata, rename and times | `hl-vfs`; engine `execution/path` | implemented |
| epoll, poll/select, signalfd and wake ordering | `hl-event`; `hl-runtime` | implemented |
| pipe packet/size and wake semantics | `hl-ipc` | implemented |
| abstract sockets and message flags | `hl-network`; engine `execution/network` | implemented |
| SCM_RIGHTS record queue, OFD retention and atomic descriptor installation | `hl-network/src/ancillary`, `hl-descriptor/src/transfer.rs`, `hl-runtime/src/network/message.rs`, engine native message adapter | implemented and directly exercised by `scmrights` |
| signals, queues and process/task identity | `hl-task`; engine `execution/task` | implemented |
| clocks and deadlines | `hl-time`; engine `execution/task` | implemented |
| mappings and protection | `hl-memory`; engine virtual-memory adapter | divergent on the retained macOS `mprotect` acceptance row |
| procfs and descriptor/task projection | `hl-vfs`; `hl-task`; engine path composition | implemented |

“Implemented” records an identified Rust owner and oracle-compatible surface; it
does not claim the whole syscall domain complete. Product-engine parity must be
established separately by running these rows through the Rust engine.
