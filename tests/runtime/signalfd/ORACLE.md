# Signalfd oracle audit

Retained C was studied read-only in `../engine/src/linux_abi/signal.c`
(`sigq_push`, `sfd_alloc`, `sfd_deliver`, `sfd_routed`, `host_sig_pend`,
`raise_guest_signal_info`), `../engine/src/linux_abi/syscall/io.c` (descriptor
duplication/relocation and signalfd `read`),
`../engine/src/linux_abi/syscall/event.c` (`signalfd4` and epoll dispatch), and
`../engine/src/linux_abi/syscall/dispatch.c` (descriptor publication). The
architecture syscall-number translation was checked in
`../engine/src/translator/guest/x86_64/legacy.c`; pointer admission for both
guest architectures was checked in `../engine/src/linux_abi/syscall/nonpie_args.h`.

The C engine owns each signalfd as an independently masked, refcounted
open-file-description backed by a private wake pipe. Standard pending signals
coalesce while real-time signals queue; queued metadata retains sender PID,
UID, code, and value. Delivery marks the task pending and wakes every matching
signalfd. Reads require at least one 128-byte record, preserve priority order,
return `EAGAIN` when a nonblocking queue is empty, and leave a pending record
available after an `EFAULT`. Descriptor aliases share the OFD and teardown is
last-close. Epoll observes the wake/readiness transition and becomes unready
after the queue drains. Fork shares descriptor/OFD state while the child's
pending-signal queue is process-local; close and exec sweeps preserve private
wake ends and honor `CLOEXEC`.

| C capability | Rust owner | Acceptance case |
|---|---|---|
| `signalfd4` mask/flag validation and descriptor publication | `hl-linux::event::abi`, `hl-runtime::event::syscalls`, `hl-descriptor` | `edges` |
| 128-byte records, nonblocking empty read, sender metadata, directed and realtime signals | `hl-event::SignalFd`, `hl-runtime::signal::queue` | `edges` |
| failed guest copy does not consume the pending record | Linux memory-copy boundary plus `hl-event::PreparedSignalSelection` | `edges` |
| `CLOEXEC` is descriptor-local | `hl-descriptor`, `hl-runtime::event::syscalls` | `edges` |
| level readiness and drain transition through epoll | `hl-event::SignalFd`, `hl-event::Epoll`, `hl-runtime::epoll` | `epoll` |
| inherited signalfd OFD with process-local pending queues and last-close teardown | `hl-runtime::fork::event`, descriptor fork, task signal queue | `fork` |

The three cases cover the retained fixture's complete observable widths and
lifecycle paths: one and two 128-byte reads, ordinary and realtime signals,
process- and thread-directed metadata, empty/fault/success results, epoll
ready/drained states, fork isolation, close, nonblocking, and close-on-exec
flags. Checkpoint restoration, descriptor passing, mask replacement, blocking
cancellation, multiple independent signalfds, and duplicate-descriptor
last-close are implemented and unit-tested Rust capabilities but are outside
this legacy three-case cohort; they are not claimed by this migration.
