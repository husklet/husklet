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

Coverage gap:

Existing pidfd tests cover self/child happy paths, not host-pid rejection, invalid flags, or capacity stress.

## `unshare` and `setns` Blanket Fake-Success

Priority: P1
Impact: namespace isolation and feature probes can silently lie
Confidence: High

Evidence:

- `unshare` and `setns` return success unconditionally: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:634`.

Why this is bad:

`unshare(CLONE_NEWNET)` and `setns(fd, nstype)` can report success without changing namespace state. Invalid calls like `setns(-1, CLONE_NEWNET)` should fail with `EBADF`; unknown flag combinations should fail with `EINVAL`. Fake success is worse than honest `ENOSYS` or `EPERM` because setup code may continue under false isolation assumptions.

Verification:

Probe `setns(-1, CLONE_NEWNET)` and `unshare(0xdeadbeef)`. Linux returns errors; current dd source returns `0`.

Coverage gap:

The current `unshare-files` test only checks a benign `CLONE_FILES` path. It does not exercise namespace creation, invalid flags, or `setns` error ordering.

## `close_range` Accepts Unknown Flags

Priority: P2
Impact: fd sanitizers and probes can get fake success
Confidence: High

Evidence:

- `close_range(first, last, flags)` validates only `first > last`: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:135`.
- Unknown flag bits are ignored; `flags & 4` selects `CLOSE_RANGE_CLOEXEC`, everything else closes: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:151`.

Why this is bad:

Linux rejects unknown `close_range` flag bits with `EINVAL`. dd reports success and may close fds or set cloexec under an invalid contract.

Verification:

Raw syscall `close_range(fd, fd, 0x80000000)` should return `-1/EINVAL`.

Coverage gap:

Existing probes cover `flags=0`, not unknown bits or `CLOSE_RANGE_UNSHARE` fidelity.

## `pidfd` Validation and Capacity Gaps

Priority: P2
Impact: wrong errno and silently unusable pidfds
Confidence: High

Evidence:

- `pidfd_open(pid, flags)` ignores `flags`: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:165`.
- `pidfd_send_signal(pidfd, sig, siginfo, flags)` ignores `flags`: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:180`.
- The pidfd registry has a fixed table in `dd-jit-darwin/src/runtime/os/linux/syscall/dispatch.c` and no obvious failure path when full.

Why this is bad:

Linux rejects unknown pidfd flags with `EINVAL`. A fixed table can return a real fd that later fails pidfd lookup once more than the internal capacity is opened.

Verification:

Probe `pidfd_open(getpid(), 0x40000000)`, `pidfd_send_signal(fd, 0, NULL, 1)`, and 65 simultaneous pidfds.

## `SO_PASSCRED` and `SO_PEERCRED` Skip Fd/Type Validation

Priority: P2
Impact: invalid fds and non-sockets can receive synthetic success
Confidence: High

Evidence:

- `setsockopt(SO_PASSCRED)` records a per-fd flag and returns `0` without validating the fd first: `dd-jit-darwin/src/runtime/os/linux/syscall/net.c:1074`.
- `getsockopt(SO_PASSCRED)` returns recorded state without validating socket type: `dd-jit-darwin/src/runtime/os/linux/syscall/net.c:1137`.
- `getsockopt(SO_PEERCRED)` falls back to synthetic credentials after failed `LOCAL_PEERPID`: `dd-jit-darwin/src/runtime/os/linux/syscall/net.c:1152`.

Why this is bad:

Linux should return `EBADF` for closed fds and `ENOTSOCK` for regular files. Returning synthetic credentials on a non-socket is fake-success behavior.

Verification:

Probe `setsockopt(-1, SOL_SOCKET, SO_PASSCRED, ...)`, `getsockopt(-1, SOL_SOCKET, SO_PASSCRED, ...)`, and `getsockopt(open("/dev/null"), SOL_SOCKET, SO_PEERCRED, ...)`.

## `getresuid` / `getresgid` Accept Null Output Pointers

Priority: P3
Impact: wrong errno and weaker fault-path fidelity
Confidence: High

Evidence:

- `getresuid` writes each output only if non-null and then returns success: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:999`.
- `getresgid` has the same shape: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1008`.

Why this is bad:

Linux treats invalid output pointers as `EFAULT`. Optional output pointers can mask caller bugs and diverge from native oracle behavior.

Verification:

Probe `syscall(SYS_getresuid, NULL, &euid, &suid)` and the gid equivalent.

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
