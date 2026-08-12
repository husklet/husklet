# Process compatibility oracle audit

> **Historical ownership:** The old `ffi/linux/execution` path below is deleted.
> Process cases now exercise the selected C production closure.

Retained C was studied read-only in `../engine/src/linux_abi/syscall/proc.c`, `../engine/src/linux_abi/fork.c`, `../engine/src/linux_abi/signal.c`, `../engine/src/linux_abi/syscall/net.c`, `../engine/src/linux_abi/container/netns.c`, `../engine/src/linux_abi/host_socket.h`, `../engine/src/linux_abi/seccomp.c`, `../engine/src/linux_abi/seccomp_vm.c`, and `../engine/src/linux_abi/number.c`. Process identity, credentials, UTS namespace and prctl state, socket/OFD identity, explicit host/isolation policy, per-thread stacked filters, verifier/action precedence, fork/exec inheritance, signal/wait ordering, cancellation, and teardown were followed. Host-specific socket mechanics stay behind the host adapter; guest syscall numbers differ by ISA while Linux-visible state does not.

Rust ownership maps to `hl-task`, `hl-network`, `hl-linux`, `hl-descriptor`, and `hl-runtime` process/network/seccomp composition. The eight cases preserve the complete local boundary cohort: namespace/uname, explicit INET modes, credential mutation, parent-death/subreaper lifecycle, and seccomp filter/matrix behavior.

The appended byte-exact `job_control.c` seed additionally follows retained
`proc.c` entries for `setpgid`, `getpgid`, `getsid`, and `setsid` through the
process registry, fork, wait, exit, and final reap call graph. Group/session
identity remains registry-owned until reap; negative identifiers validate
before lookup, group leaders receive `EPERM` from `setsid`, and the pipe release
orders child exit without sleeps. Rust ownership is `hl-task` process/group/
session state plus `hl-runtime` lifecycle composition and `hl-descriptor` pipe
OFDs. Only syscall admission differs by guest ISA.

## Full retained process compatibility category

The complete retained process category was audited against the read-only C
implementation before migration. The files and entry points studied were
`../engine/src/linux_abi/syscall/proc.c` (`svc_proc`, `exec_forward_env`,
`exec_close_cloexec`, `fork_child_hooks`, `bound_fork_prepare`,
`bound_fork_complete`, `vfork_release_parent`),
`../engine/src/linux_abi/fork.c` (`hl_server_main`, `hl_client_main`,
`hl_forkserver_runner`, `fsrv_restore_pristine`),
`../engine/src/linux_abi/thread.c` (`futex_key`, `futex_wake_bucket`,
`futex_wake_op_apply`, `futex_op`, `futex_robust_exit`, `thread_after_fork`),
`../engine/src/linux_abi/signal.c` (`raise_guest_signal_info`,
`maybe_deliver_signal`, `guest_group_fatal`, `ptrace_intercept_signal`),
`../engine/src/linux_abi/syscall/rare.c` (`svc_rare`), and
`../engine/src/linux_abi/host_wait.h` (`wait4`, `waitid`).

The C engine owns process identity in a process registry and thread identity in
per-thread CPU/task state. Fork first prepares descriptor, mapping, watch,
sequence, and private-host-service plans; the parent either completes or
cancels every plan, while the child resets inherited locks, caches, translated
state, signal stacks, futex state, accounting, mappings, and registry identity.
Vfork adds a private rendezvous whose writer is released only by child exec or
exit. Exec forwards the guest environment as authoritative state, closes
descriptor-local `CLOEXEC` ownership without touching engine descriptors,
resets caught signal dispositions, and replaces the image. Wait consumes the
registry/reap record only after producing status, siginfo, and resource usage;
`WNOWAIT` observes without consuming. Fatal default signals terminate the whole
thread group, and robust-list/clear-tid cleanup precedes the final waiter wake.

Futex buckets distinguish process-private virtual-address identity from stable
shared-mapping identity. Wait publication and signal interruption are ordered
to avoid lost wakeups; wake, wake-op, PI, robust-owner death, and clear-tid all
use bounded bucket ownership. Ptrace interception occurs before ordinary signal
delivery and keeps attach/stop/continue/exec/reap ordering in the process
registry. Capability, credential, prctl, scheduler, process-group, session,
pidfd, rlimit, and accounting operations validate Linux arguments before host
translation and return Linux errno at the personality boundary. AArch64 and
x86-64 differ in syscall numbers, clone argument normalization, register
layouts, and ELF machine validation; macOS has explicit host adapters for wait,
subreaping, pidfd/ptrace limitations, and fork-time signal/JIT repair.

Rust ownership maps process, thread-group, job-control, wait, signal, and robust
state to `hl-task`; Linux ABI values and ptrace contracts to `hl-linux`; futex
queues and PI ownership to `hl-sync`; process composition to
`hl-runtime/src/process/{fork,exec,wait,control,pidfd,prctl,syscalls}.rs`; and
native launch/fork/exec adapter wiring to `hl-engine/src/ffi/linux/execution`.
Descriptor inheritance and `CLOEXEC` remain `hl-descriptor` ownership, while
memory inheritance and image replacement remain `hl-memory`/loader ownership.
Typed broken entries record remaining clone shared-VM, bound-fd teardown,
blocked ptrace attach, and concurrent-spawn divergences. Typed unsupported
entries preserve macOS-only host gaps and the two-program forkserver protocol
that the current canonical runner cannot yet express.

### Fork rollback transaction

The rollback expectation was checked against the retained process-fork domain
in `../engine/src/linux_abi/syscall/proc.c` (`bound_fork_prepare`,
`bound_fork_complete`, clone case 220, and clone3 case 435) and
`../engine/src/linux_abi/linux_abi.c` (`hl_linux_abi_fork_prepare`,
`hl_linux_abi_fork_parent`, `hl_linux_abi_fork_child`,
`hl_linux_fork_unpin`, and `hl_linux_fork_discard_children`). Preparation pins
each live OFD, clones resources outside the table lock, and then verifies the
generation and handle topology before arming the fork. A partial clone failure
closes every child clone already produced and releases every snapshot pin.
Failure after the host fork kills and reaps the child in the parent; child-side
repair failure exits the child. The original `errno` or the boundary's typed
`EAGAIN`, `ENOMEM`, or `EIO` mapping is returned without publishing a child.

The corresponding Rust owners are `hl-runtime/src/fork/control.rs` for ordered
prepare, reverse commit, and reverse rollback; `fork/task.rs` and
`hl-task::TaskRegistry` for staged process identity; and the descriptor,
memory, provider, execution, network, event, and IPC fork participants for
their resources. A rolled-back Rust task reservation must disappear and its
generation must become stale. It need not be the next slot allocated: the Rust
registry deliberately prefers a never-used PID slot while one remains, matching
Linux's delayed PID-reuse behavior. Tests that specifically prove generation
advance therefore use a capacity with no alternate free process slot.
