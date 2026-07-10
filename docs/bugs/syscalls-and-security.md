# Syscall and Security Gaps

This file covers syscall fidelity, fake-success paths, host-authority leaks, and syscall coverage holes.

## `pidfd` Host Pid Authority Leak

Priority: P0
Impact: guest-visible security/namespace escape risk
Confidence: High

Evidence:

- `pidfd_open` accepts any positive pid that either equals `container_pid()` or passes host `kill(pid, 0)`: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:165`.
- `pidfd_send_signal` resolves the stored pid and calls host `kill(pid, sig_l2m(sig))`: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:180`.

Why this is bad:

A guest pid value can name a host process visible to the dd host user. If `pidfd_open(<host-pid>, 0)` succeeds, `pidfd_send_signal` can signal that host pid. A real container should not let a process address arbitrary same-user host pids through pidfd APIs.

Verification:

Create a guest probe that calls `pidfd_open` on a known same-user host pid outside the container, then calls `pidfd_send_signal(fd, 0, NULL, 0)`. Expected container behavior is failure; current source suggests dd can report success.

Status (2026-07-10): SKIPPED — architectural. This mirrors dd's deliberate 1:1 host-pid model for `kill(2)` (guest processes ARE host processes; `sched_pid_live` and `kill` case 129 intentionally allow cross-guest-process signalling via `kill(pid,0)`/`kill(pid,sig)`). Closing pidfd alone while `kill(2)` stays open does not close the leak, and restricting to a per-process registry would break legitimate cross-guest-process pidfd signalling (rare.c case 424 explicitly supports it). A real fix needs a container-wide guest-pid namespace (map guest pids to a private table shared across engine processes) so `pidfd_open`/`kill` reject any pid not in the container — a large cross-cutting change tracked separately. Invalid-flags fidelity was fixed independently (see the pidfd flags fix below).

## Deliberate But High-Impact: `seccomp` No-Op

Priority: architectural risk
Impact: guest self-sandboxing is not enforced
Confidence: High

Evidence:

- `seccomp(op, flags, args)` accepts strict/filter installs but does not enforce BPF filters: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:77`.

Why this matters:

The source comment is explicit that this is deliberate. It should remain visible because it fails open: a guest can believe syscalls are blocked, trapped, or killed while dd continues servicing them.

Verification:

Install a deny filter for a harmless syscall such as `getpid` and then call it. Linux should block according to filter action; dd is expected to allow it.

## Fixed (2026-07-10, branch bugfix/completeness-env-v2)

Each fix is minimal at the cited source and guarded by a scoped completeness guest that oracle-diffs dd vs native (aarch64) / qemu (x86_64), passing on both engines.

- `unshare`/`setns` blanket fake-success — `unshare(unknown flags)` now returns `EINVAL`, `setns(-1, …)` returns `EBADF` (`syscall/proc.c` cases 97/268). Test: `comp-sys-proc/unshare-setns` (`sys_unshare_setns.c`).
- `close_range` unknown flags — bits outside `CLOSE_RANGE_UNSHARE|CLOSE_RANGE_CLOEXEC` now return `EINVAL` before any fd is touched (`syscall/rare.c` case 436). Test: `comp-sys-misc/close-range-flags` (`sys_close_range_flags.c`).
- pidfd flags fidelity — `pidfd_open` rejects flags outside `PIDFD_NONBLOCK`; `pidfd_send_signal` rejects bits outside `PIDFD_SIGNAL_{THREAD,THREAD_GROUP,PROCESS_GROUP}` (kernel ≥6.9) with `EINVAL`, validated before delivery (`syscall/rare.c` cases 434/424). Test: `comp-sys-signal/pidfd-flags` (`sys_pidfd_flags.c`). (Host-pid authority remains open — see above.)
- pidfd registry capacity — `pidfd_register` now signals table-full and `pidfd_make` fails the open with `EMFILE` (closing the minted fd) instead of returning a real fd that later can't be resolved (`syscall/dispatch.c`). Test: `comp-sys-signal/pidfd-cap` (`sys_pidfd_cap.c`) — every successful `pidfd_open` stays resolvable; overflow fails cleanly.
- `SO_PASSCRED`/`SO_PEERCRED` fd/type validation — both now validate the fd via `SO_TYPE` first, so a closed fd is `EBADF` and a non-socket is `ENOTSOCK` (`syscall/net.c` cases 208/209). Test: `comp-sys-misc/passcred-badfd` (`sys_passcred_badfd.c`).
- `getresuid`/`getresgid` NULL output pointers — a NULL/unwritable pointer now faults `EFAULT`, writing none (`syscall/proc.c` cases 148/150). Test: `comp-sys-cred/getresuid-null` (`sys_getresuid_null.c`).
