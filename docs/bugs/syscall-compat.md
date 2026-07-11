# Syscall Compatibility and Completeness Gaps

This file keeps syscall findings framed around Linux compatibility, fake-success behavior, data loss, probe correctness, hangs, and workload breakage.

## Periodic `timerfd` Ignores Earlier First Deadline

Priority: P1
Impact: delayed wakeups and latency cliffs
Confidence: High

Verification status: Proven across aarch64 and x86_64 in isolated worktree `/Users/x/dd/dd-workerA-syscall-audit-20260710`.

Evidence:

- The code computes `first_delay` from `it_value`: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:919`.
- Periodic timers are then armed with `interval_ns` instead of the first delay: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:926`.

Why this is bad:

Linux timerfd supports a first expiration at `it_value` followed by periodic expirations at `it_interval`. dd delays the first fire until the interval when the interval is larger than the initial deadline.

Isolated proof:

```sh
DDJIT_DIR="$PWD/target-workerA-syscall-audit/release/build/dd-jit-darwin-16122afd27b6bb64/out" \
  cargo run -q -p dd-tests -- -e aarch64 timerfd-first-interval
```

Observed: native fires within 20ms (`ready=1 n8=1 exp=1`); dd does not fire within 150ms (`ready=0 n8=0 exp=0`). x86_64 showed the same mismatch.

## Sentry Close-On-Exec Does Not Clean Virtual Fds

Priority: P1
Impact: fd leaks across guest exec and wrong pipe/resource lifetime
Confidence: High

Evidence:

- In sentry mode, `execve` stays local in the worker process: `dd-jit-darwin/src/runtime/os/linux/sentry.c:1387`.
- The sentry fd table tracks real/borrowed fds but has no exec cleanup path at that redirect point.

Why this is bad:

Guest close-on-exec semantics should close marked fds in the post-exec process. Leaving sentry-owned virtual fds alive can keep pipes open, prevent EOF, or leak resources.

Verification:

Run an untrusted-mode probe that sets `FD_CLOEXEC` on a pipe, execs, and verifies the peer observes EOF.

## aarch64 `AT_PAGESZ` Exposes Host Page Size

Priority: P2
Impact: Linux ABI and page-size probes see host 16K instead of expected 4K
Confidence: High

Evidence:

- Runtime explicitly sets `AT_PAGESZ` to host mmap granularity: `dd-jit-darwin/src/runtime/os/linux/elf.c:918`.
- Existing completeness xfail remains registered in `dd-tests/src/cases/ext/completeness/sys_misc.rs:10`.

Why this is bad:

Linux aarch64 workloads commonly expect 4K page size unless running on a different configured kernel. Exposing Apple Silicon 16K host granularity can break allocator sizing, page math, ABI probes, and tests that compare auxv against Linux defaults.

Verification:

Run the existing `AT_PAGESZ` completeness probe under aarch64 and compare with native Linux/qemu oracle.

## `F_SETLEASE` Lease-Break Signal Not Delivered (residual)

Priority: P3
Impact: a lease holder is not notified when another guest process opens the leased file
Confidence: High

Status: PARTIAL — `F_NOTIFY` (dnotify) is now fully emulated; `F_SETLEASE` argument
validation + state tracking is now Linux-consistent; only the cross-process lease *break*
signal remains unimplemented.

What is now implemented (`dd-jit-darwin/src/runtime/os/linux/syscall/io.c`):

- `F_NOTIFY` is backed by a real kqueue `EVFILT_VNODE` watch on the directory fd, drained on
  a lazily-spawned thread that raises the requested signal (an `F_SETSIG` signal, else the
  `SIGIO` default) in the guest via the same async path POSIX timers/timerfd use. Oracle-diffed
  vs native aarch64: `arm=0 got_sigio=1` on both. `DN_MULTISHOT` re-arms; a zero mask removes
  the watch; the watch is torn down on `close()`.
- `F_SETSIG` / `F_GETSIG` now round-trip a per-fd signal (they were previously forwarded to the
  macOS `fcntl`, whose command numbering diverges).
- `F_SETLEASE` validates exactly like Linux: bad arg → `EINVAL`; non-regular file → `EINVAL`;
  `F_RDLCK` on a writable fd → `EAGAIN`. The lease type is tracked per fd and round-trips
  through `F_GETLEASE`. Oracle-diffed vs native aarch64 (byte-identical):
  `set_wr=0 get1=1 set_un=0 get2=2 set_rd=-1 get3=2 bad_einval=1 pipe_einval=1`.

Residual:

The lease *break* — Linux sends the lease holder a signal (default `SIGIO`) when another process
opens the file in a conflicting mode, then blocks that opener until the lease is downgraded — is
NOT delivered. It needs a hook that fires on *another opener* of the same file; macOS offers no
rootless vfs open-intercept across the emulated guest's process tree. The `F_WRLCK` "sole opener"
precondition is likewise not enforced (dd cannot enumerate other openers). Single-holder lease
state is fully consistent; only conflicting-open notification is missing.

## `mlockall` Best-Effort Under `RLIMIT_MEMLOCK` (residual)

Priority: P3
Impact: a range the host refuses to wire stays pageable while the call still reports success
Confidence: High

Status: FIXED (with residual) — `mlockall` now wires pages for real. `MCL_CURRENT` host-`mlock`s
every currently-mapped guest range; `MCL_FUTURE` arms a flag so each subsequent `mmap` (mem.c
case 222) is wired on creation; `munlockall` unwires them all. macOS has `mlock(2)` (already used
by `mlock`/case 228), so residency is genuine, not just `/proc` state. Oracle-diffed vs native
aarch64 (byte-identical): `rc=0 lck_after_pos=1 b_ok=1 ml=0 mu=0 un=0 lck_end=0 data_ok=1`.
See `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c` (230/231),
`dd-jit-darwin/src/runtime/os/linux/syscall/mem.c` (case 222 MCL_FUTURE),
`dd-jit-darwin/src/runtime/os/linux/container/vfs/gmap.c` (`mlk_wire_current`/`mlk_unwire_all`).

Residual:

Wiring is best-effort: a range the host `mlock` declines (e.g. `RLIMIT_MEMLOCK` exhausted) is left
pageable and the call still returns success, where Linux would fail the whole `mlockall` with
`ENOMEM`. The wired ranges are real and `/proc` `VmLck:`/`Locked:` reporting is honest; only the
all-or-nothing failure mode differs.

## aarch64 4K Subpage `munmap` Returns `EINVAL`

Priority: P2
Impact: valid Linux aarch64 4K-page unmaps fail
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-aarch64-runtime-20260710`.

Evidence:

- Runtime guest page-size handling uses the host-sized value: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:143`.
- `munmap` validation rejects ranges not aligned to that value: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:191`.

Why this is bad:

Linux aarch64 commonly uses 4K pages. dd exposes/uses 16K alignment, so valid 4K-aligned `munmap` calls fail with `EINVAL`.

Observed proof:

```text
Linux: pagesz=4096 ok=1
dd:    pagesz=16384 ok=0 einval=1 errno=22
```

## Aligned `mprotect` On Unmapped Range Succeeds

Priority: P2
Impact: memory mapping probes see false success instead of `ENOMEM`
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-aarch64-runtime-20260710`.

Evidence:

- `mprotect` handler updates bookkeeping and returns success without verifying mapped pages: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:652`.

Why this is bad:

Linux returns `ENOMEM` for page-aligned `mprotect` ranges that are not mapped. dd reports success, hiding bad mappings and feature-probe failures.

Observed proof:

```text
Linux: mprotect_unmapped enomem=1 success=0 errno=12
dd:    enomem=0 success=1 errno=0
```

## `kill(0, sig)` Only Signals The Caller

Priority: P1
Impact: process-group signal delivery silently misses sibling processes
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-pgrp-signal-20260710`.

Evidence:

- `kill(pid=0)` routes through a caller-only signal path: `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:92`.

Why this is bad:

Linux `kill(0, sig)` sends to every process in the caller's process group. dd signals only the caller, so job-control shells, process supervisors, and group-shutdown logic can leave child processes running.

Observed proof:

```text
Linux/qemu: kill_zero ready=1 kill_ok=1 child_got=1
dd:         kill_zero ready=1 kill_ok=1 child_got=0
```

Status (2026-07-10): FIXED — `kill(0, sig)` (signal.c case 129) now delivers to the caller's whole process group WITHIN the container, not just the caller. dd forwards `setpgid` to the host so the host process group mirrors the guest's, but the engine shares its host session/process-group with the launcher and sibling engines, so a raw `kill(-getpgrp())` would escape the container. Instead `container_group_kill()` (vfs.c) enumerates this container's process registry (the same per-container `proc_reg_*` boundary the guest-pid namespace uses — see docs/bugs/syscalls-and-security.md "pidfd Host Pid Authority Leak") and signals each MEMBER whose host process-group matches the caller's, plus the caller itself via the in-process self path. `sig == 0` stays a group existence/permission probe (returns success). Regression test: `sc-kill-zero-group` (guests/syscallbug/kill_zero_group.c) — run in container mode and oracle-diffed vs native aarch64, expecting `child_got=1`.

## Guest PROT_NONE Mappings Remain Directly Readable

Priority: P1
Impact: memory protection faults are bypassed for translated guest loads
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-aarch64-atomics-perms-20260710`.

Evidence:

- Guest `PROT_NONE` anonymous mappings are physically host read/write and only recorded in `g_gna`: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:631`.
- `g_gna` is consulted by syscall pointer validation, not translated guest data access: `dd-jit-darwin/src/runtime/os/linux/thread.c:334`.
- `mprotect` is a physical no-op: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:652`.

Why this is bad:

Linux faults direct guest loads from `mmap(PROT_NONE)` and from a `MAP_FIXED|PROT_NONE` replacement, delivering `SIGSEGV` with the accessed page as `si_addr`. dd lets translated guest loads read the page successfully, hiding guard-page and memory-safety bugs from guest software.

Observed proof:

```text
Linux: prot_none direct=1 sig=11 addr=1 ... fixed_direct=1 fixed_sig=11 fixed_addr=1
dd:    prot_none direct=0 sig=0 addr=0 val=0 ... fixed_direct=0 fixed_sig=0 fixed_addr=0 fixed_val=0
```

## Writes To `mprotect(PROT_READ)` Pages Do Not Fault

Priority: P1
Impact: guest read-only page protections are bypassed by translated stores
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-jit-perm-fault-20260710`.

Evidence:

- Anonymous mappings are forced host read/write: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:447`.
- `mprotect` is a physical no-op except for guest bookkeeping: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:652`.
- x86 translated stores emit raw host stores: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:246`, `dd-jit-darwin/src/runtime/translate/x86_64/emit.c:87`.

Why this is bad:

Linux faults writes to pages protected with `mprotect(PROT_READ)`. dd lets the write succeed and changes memory, breaking guard pages, copy-on-write checks, and memory safety probes.

Observed proof:

```text
Linux/qemu: write_ro outcome=fault sig=11 code=2 addr_delta=0 value=11
dd:         write_ro outcome=write_ok value=22
```

## Execute Permission Is Not Enforced For Guest Fetch

Priority: P1
Impact: non-executable guest pages can run as code
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-jit-perm-fault-20260710`.

Evidence:

- Dispatch translates any guest PC without an execute-permission check: `dd-jit-darwin/src/runtime/engine/dispatch.c:169`.
- x86 decode reads guest instruction bytes directly: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:1091`, `dd-jit-darwin/src/runtime/translate/x86_64/decode.c:126`.
- Memory protection code treats guest `PROT_EXEC` as meaningless to the host DBT: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:451`.

Why this is bad:

Linux faults instruction fetch from non-executable mappings. dd translates and executes the bytes anyway, so W^X and JIT permission sequencing bugs are invisible inside the guest.

Observed proof:

```text
Linux/qemu: exec_noexec outcome=fault sig=11 code=2 addr_delta=0
dd:         exec_noexec outcome=exec_ok ret=42
```

