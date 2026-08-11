# Signals and signalfd oracle audit

## Workload boundary

The canonical signals folder owns both the former bootstrap workload
`tests/runtime/legacy/source/signal.c` and the complete former libc
`tests/compat/signals` cohort. The freestanding seed proves a process-wide
`SIGUSR1` action, a per-thread blocked mask,
pending observation, alternate-stack selection, thread-directed `tkill` and
`tgkill`, nested handler replacement, and architecture-specific rt-sigframe PC
restoration. Successful completion now prints `signal-ok` so the self-contained
folder has an explicit golden result.

## Retained C implementation studied

The read-only oracle was followed through these entry points and owners:

- `../engine/src/linux_abi/syscall/signal.c`: `syscall_should_restart` and the
  signal syscall arms for `rt_sigaction`, `rt_sigprocmask`, `rt_sigpending`,
  `sigaltstack`, `tkill`, and `tgkill`. It validates Linux's eight-byte sigset,
  stages old state before mutation, rejects invalid targets, wakes a target out
  of an interruptible host call, and distinguishes transparent `SA_RESTART`
  from guest-visible `EINTR`.
- `../engine/src/linux_abi/signal.c`: `host_sigh`, `host_sigh_si`,
  `maybe_deliver_signal`, `sigreturn_frame`, `deliver_guest_fault`, and the
  `g_sigact`, `g_pending`, per-signal FIFO, sender/fault metadata, disposition,
  handler-depth, and host-signal translation state. Actions and process pending
  state are process-wide; masks, directed pending state, altstack, deferred
  masks, and frame depth are CPU/thread-owned. Standard pending signals
  coalesce, realtime instances queue FIFO, ignored signals are flushed, and
  delivery atomically consumes one eligible instance before publishing a frame.
- `../engine/src/core/dispatch.c`: `run_guest` registers each live CPU in the
  thread and stop-the-world registries, polls process and directed pending bits
  only at spilled block boundaries, recognizes the sigreturn sentinel, restores
  one frame/deferred-mask level, drains another eligible signal, and unregisters
  before CPU teardown. No table lock is held while guest code runs.
- `../engine/src/translator/guest/aarch64/signal.c`:
  `hl_aarch64_signal_build`, `hl_aarch64_signal_restore`, capture, and resume.
  It selects and marks `SA_ONSTACK`, publishes `siginfo_t`/`ucontext_t`, all GPR,
  vector, FP status, mask, and altstack state, sets x0/x1/x2 and x30 to the
  sigreturn sentinel, and reconstructs faults from AArch64 host context.
- `../engine/src/translator/guest/x86_64/signal.c`:
  `hl_x86_signal_build`, `hl_x86_signal_restore`, capture, and resume. It owns
  the x86-64 rt-sigframe offsets, red-zone/alignment rules, RIP/RSP/RFLAGS and
  GPR/vector/x87-compatible image, restorer return slot, altstack publication,
  and translated-cache fault reconstruction.
- `../engine/src/core/target/aarch64.c` and
  `../engine/src/core/target/x86_64.c`: `build_signal_frame`, `do_sigreturn`,
  backend fault capture, exact guest-PC refinement, and return-to-dispatch glue.
  AArch64-on-AArch64 native execution captures host `ucontext` and repairs
  folded scratch registers; interpreter/cross-ISA paths use their per-ISA
  capture implementation. X86 similarly refines the exact faulting guest RIP.
- `../engine/src/linux_abi/host_signal.h`: Linux/macOS signal-number mapping,
  host handler installation, engine-private interrupt signals, and the host
  mask/altstack lifecycle. Windows replaces POSIX fault delivery with VEH and
  cannot provide ordinary cross-process POSIX signalling; this workload's guest
  behavior remains owned above the host adapter boundary.

Cancellation ordering is: a host interruption marks pending state and wakes the
blocked CPU; the syscall boundary returns toward the dispatcher; a deliverable
handler runs before either `EINTR` is exposed or the saved syscall is retried.
Checkpoint/exec cancellation suppresses restart. Teardown first removes the CPU
from signal targeting, then releases its host altstack. The retained C engine's
Go-image `SIGURG` suppression is an application-specific divergence and is not a
capability to reproduce; Rust must preserve the generic safe-boundary invariant.

## Rust capability matrix

| C capability | Rust owner | Status |
|---|---|---|
| Actions, eight-byte masks, pending query, altstack ABI | `hl-linux::signal::SignalAbi`; `hl-runtime::signal::state` | Implemented |
| Process and thread pending queues, standard coalescing, realtime FIFO, ignore flush | `hl-task::signal`; `hl-task::registry::signal::delivery` | Implemented |
| `tkill`/`tgkill` target validation and directed enqueue | `hl-runtime::signal::send`; `hl-task::TaskRegistry` | Implemented |
| Safe-boundary selection, action/deferred-mask transition | `hl-task::registry::signal::plan`; `hl-runtime::signal::boundary` | Implemented |
| AArch64 and x86-64 rt-sigframe construction/restoration | `hl-linux::signal::{aarch64,x86,frame}`; `hl-runtime::signal::frame` | Implemented |
| Transactional guest-memory/frame publication and CPU replacement | `hl-engine::ffi::linux::execution::signal_frame::Port` | Implemented |
| Slow-syscall cancellation and `SA_RESTART` decision | `hl-task::registry::signal::delivery`; `hl-runtime::signal::boundary` | Implemented |
| Host fault intake and native-context source | `hl-engine::ffi::linux::execution::{signal_source,routing::signal}` | Implemented on supported host adapters |
| C application-specific `SIGURG` suppression | No Rust owner | Intentionally omitted; generic delivery must be fixed instead |
| Windows ordinary cross-process POSIX signal forwarding | No native host mechanism | Remaining host limitation; guest queue/frame semantics are host-neutral |

These workloads exercise the implemented rows through the public engine path on
both guest ISAs. The libc cohort adds 71 stable cases covering dispositions and
flags, process- and thread-directed delivery, pending and realtime ordering,
`SIGCHLD`, synchronous faults, alternate stacks, interruption and restart,
synchronous waits, timers, pipes, and signalfd. Its original case IDs, target
sets, compiler flags, exit values, and golden bytes are preserved in the one
canonical `test.yaml`. Source and golden leaf renames are content-only
relocations matched by the stable case IDs in the former `build-plan.tsv` and
this manifest.

The retained C study for this broader cohort followed the complete signal path,
not individual failing fixtures. In addition to the entry points above, it
covered `raise_guest_signal_info`, `sigq_push`, `host_sig_pend`, default-signal
termination, and child notification in `../engine/src/linux_abi/signal.c`; the
signal syscall admission, target lookup, restart decision, and wait interruption
paths in `../engine/src/linux_abi/syscall/signal.c`; timer enqueue in
`../engine/src/linux_abi/syscall/time.c`; descriptor reads and duplication in
`../engine/src/linux_abi/syscall/io.c`; and signalfd creation/update plus epoll
readiness in `../engine/src/linux_abi/syscall/event.c`. Process action and pending
tables outlive individual CPUs, while masks, directed pending state, altstack,
and nested frame state remain thread-owned. Queue mutation and wake publication
are ordered under the signal lock, but descriptor locks are not held across host
blocking, guest copies, or handler execution. Standard signals coalesce,
realtime instances retain FIFO identity per signal, synchronous faults bypass
ordinary blocking, and teardown removes task targeting before releasing host
signal storage.

Architecture-specific frame layout and fault-PC reconstruction remain in the
AArch64 and x86-64 translator files listed above. POSIX host adapters translate
host signal numbers and contexts; Windows VEH provides fault intake but cannot
provide ordinary host POSIX process signalling. The retained Go-image `SIGURG`
suppression is recorded as an oracle divergence, not mapped into Rust product
policy.

The broad migration evidence is intentionally domain-level. All 143 declared
case/ISA combinations cross-compiled successfully with the exact retained flags
(75 logical cases: 72 ARM64 and 71 AMD64 builds). A parallel ARM64 QEMU oracle
run passed the canonical seed and first 19 libc cases, then reported the existing
`sigurg-go-preempt` divergence. A parallel AMD64 QEMU oracle run passed the seed
and first 28 libc cases, then reported the existing `sa-nocldstop` golden
divergence. A parallel ARM64 production run emitted native diagnostics and then
aborted with stack-smashing inside the wider active cohort. These provider and
runtime observations are recorded without an application-specific workaround.
The manifest represents `rt-signal-order` as typed broken for the known x86-64
ordering defect. The explicit retained-C and Rust `sigurg-go-preempt` arms now
assert the same image-independent delivery contract as `sigurg-preempt`: inert
Go linker bytes cannot suppress SIGURG, and delivery across translated compute
boundaries must resume with an exact checksum. The Rust arm retains the native
execution counter floor; the C arm cannot emit that Rust-only record. The focused Rust test
build was attempted but the concurrent
shared tree lacked unrelated `hl-task` set-id and terminal-transition APIs needed
by `hl-runtime`; that compile blocker is outside this fixture-only migration.
This audit does not claim the Windows host limitation complete.

## Signalfd retained-C audit and Rust mapping

The retained signalfd domain was studied read-only in
`../engine/src/linux_abi/signal.c` (`sigq_push`, `sfd_alloc`, `sfd_deliver`,
`sfd_routed`, `host_sig_pend`, `raise_guest_signal_info`),
`../engine/src/linux_abi/syscall/io.c` (signalfd `read`, descriptor duplication
and relocation), `../engine/src/linux_abi/syscall/event.c` (`signalfd4` and
epoll dispatch), and `../engine/src/linux_abi/syscall/dispatch.c` (descriptor
publication). Architecture admission was checked in
`../engine/src/translator/guest/x86_64/legacy.c` and
`../engine/src/linux_abi/syscall/nonpie_args.h` for both guest ISAs.

Each C signalfd is an independently masked, refcounted open-file-description
with a private wake pipe. Standard signals coalesce and realtime instances queue
FIFO with PID, UID, code, and value. Enqueue marks task state pending and wakes
matching signalfds. Reads require at least one 128-byte record, preserve signal
priority, return `EAGAIN` for an empty nonblocking queue, and do not consume a
record when guest copy returns `EFAULT`. Aliased descriptors share the OFD;
teardown occurs on last close. Epoll is level-ready until the queue drains. Fork
shares descriptor/OFD state while the child's pending queue remains process
local; exec closes only `CLOEXEC` descriptors. The signal queue lock protects
selection/removal and wake publication; descriptor table locks are not retained
across host reads, guest copies, or waits. A blocking read is interrupted with
`EINTR` according to the same cancellation and restart ordering documented
above. Host-specific signal-number translation remains behind the POSIX adapter;
the 128-byte Linux record and guest mask semantics are identical on AArch64 and
x86-64.

| Retained C capability | Rust owner | Case/status |
|---|---|---|
| `signalfd4` mask and flag validation, descriptor publication | `hl-linux::event::abi`; `hl-runtime::event::syscalls`; `hl-descriptor` | `runtime/signalfd/edges`, active |
| 128-byte records, metadata, ordinary/realtime and directed delivery | `hl-event::SignalFd`; `hl-runtime::signal::queue` | `runtime/signalfd/edges`, active |
| Failed guest copy retains selected record | Linux copy boundary; `hl-event::PreparedSignalSelection` | `runtime/signalfd/edges`, active |
| Level readiness and drain transition | `hl-event::SignalFd`; `hl-event::Epoll`; `hl-runtime::epoll` | `runtime/signalfd/epoll`, active |
| Fork-shared OFD, process-local pending queue, last-close teardown | `hl-runtime::fork::event`; descriptor fork; task signal queue | `runtime/signalfd/fork`, active |

The three signalfd cases cover ordinary and realtime delivery, one- and
two-record reads, sender metadata, process- and thread-directed signals,
empty/fault/success results, epoll ready/drained transitions, fork isolation,
nonblocking and close-on-exec flags. Checkpoint restore, descriptor passing,
mask replacement, duplicate-descriptor last-close, multiple signalfds, and
blocking cancellation remain Rust unit-test capabilities outside this migrated
three-case acceptance cohort and are not claimed complete here.
