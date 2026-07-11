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

Status (2026-07-10): FIXED — a container-wide guest-pid namespace now bounds every pid-taking signal syscall. The per-container process registry (`proc_reg_*`, keyed by `DD_NETNS`/`DD_HOSTNAME` so every engine process of ONE container agrees and two containers never collide) is the namespace boundary: a host pid belongs to this container iff it published a `<dir>/<hostpid>` record. `container_host_member()` / `container_gpid_member()` (vfs.c) resolve a guest target to a host pid and require membership; the signal paths reject a non-member with `-ESRCH`:

- `pidfd_open` (rare.c case 434) resolves the guest pid to a container-local host pid and requires membership (else ESRCH); it now STORES the resolved host pid, so a pidfd for guest pid 1 targets the init's host process, not host pid 1 (launchd).
- `pidfd_send_signal` (rare.c case 424) re-validates membership before delivery (a pidfd to a departed/out-of-container process → ESRCH).
- `kill(2)` (signal.c case 129) requires cross-process targets to be members; a guest can no longer signal an arbitrary same-user host pid, a sibling engine (another container), or the launcher.
- `sched_pid_live` (proc.c) — the sched_* family's liveness gate — now uses membership instead of a raw host `kill(pid,0)` probe, closing the same host-pid existence/authority leak for `sched_{get,set}affinity`/`sched_{get,set}param`/`sched_{get,set}scheduler`.

Legitimate cross-guest-process signalling (a real container peer IS a registry member) is preserved. Membership is race-free across the real-fork tree: every fork/clone/clone3 publishes the child's marker in the PARENT (`proc_reg_mark_child`) synchronously before it can return, and reap (`wait4`/`waitid`) drops the marker (`proc_reg_reap`) so a recycled host pid can't inherit stale membership. Gated on container mode (`g_init_hostpid`); bare (non-container) mode keeps the historical host model. Invalid-flags fidelity was fixed independently (see the pidfd flags fix below).

Residual (documented, low blast-radius): if an in-container process is reaped by a NON-container reaper (e.g. a host launchd adopting an orphan) so `proc_reg_reap` never runs, AND the host later recycles that exact pid to a same-user process the guest targets by that number, membership can false-positive until the stale marker is pruned. This is orders of magnitude narrower than the former "any same-user host pid" surface (a genuine reject already needs the marker AND a live process), and normal parent-reaped trees never hit it.

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
