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

## Seccomp Filter Install Reports Success But Does Not Enforce

Priority: P1
Impact: feature probes and self-sandboxed software run under false syscall policy
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verify-4b`.

Evidence:

- `seccomp()` accepts filter installs as a no-op: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:77`.
- `PR_SET_SECCOMP` also accepts strict/filter modes: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1379`.

Why this is bad:

Software can install a filter, receive success, and then continue executing syscalls that the filter would block on Linux. This is a compatibility and probe-truth problem even when security is not the focus.

Isolated proof:

PoC `seccomp_filter_getpid.c` was added in `/Users/x/dd/dd-verify-4b`. Native Linux aarch64 blocks `getpid` with `EPERM`; dd reports install success and still allows `getpid`.

Observed:

```text
jit: blocked=0 pid=67906 errno=0
native: blocked=1 pid=-1 errno=1
```

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

## `F_SETLEASE` / `F_NOTIFY` Return Success Without Arming Anything

Priority: P2
Impact: file coordination and invalidation probes receive fake support
Confidence: High

Evidence:

- `F_SETLEASE` / `F_NOTIFY` return success while comments state they arm nothing: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:1185`.

Why this is bad:

Software using leases or dnotify for coordination can believe it has exclusive notifications or invalidation when no such mechanism is active.

Verification:

Add a lease/dnotify probe that arms the feature, mutates the file from a second process, and asserts Linux-visible behavior rather than just syscall return value.

## `mlockall` Tracks State But Does Not Wire Pages

Priority: P3
Impact: realtime and memory-residency probes can overestimate guarantees
Confidence: High

Evidence:

- Runtime comments state `mlockall` cannot wire process pages and only tracks state for `/proc`: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:510`.

Why this is bad:

Programs using `mlockall` for latency control can receive success-like state while pages are still pageable by the host. This is mainly a documented model gap, but it should stay visible because it affects performance-sensitive workloads.

## Multiple `signalfd` Descriptors Are Not Independent

Priority: P1
Impact: event loops can consume signals from the wrong descriptor
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-V-jit-runtime-20260710`.

Evidence:

- Signal delivery uses shared signalfd state: `dd-jit-darwin/src/runtime/os/linux/signal.c:229`.
- `signalfd(-1, ...)` reuses global pipe/read state and ORs masks: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:772`.
- Reads consume from the shared fd path: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:340`.

Why this is bad:

Linux creates independent signalfd descriptors. A `SIGUSR1` fd and a `SIGUSR2` fd should not alias each other or broaden each other's masks.

Isolated proof:

```sh
mac bash -lc 'DDJIT_DIR=$OUT $OUT/ddjit-linux_aarch64 scratch-worker-V/signalfd_multi.aarch64'
```

Native printed `distinct=1 s2_eagain=1 s1_usr1=1`; dd printed `distinct=0 s2_eagain=0 s1_usr1=0`.

## Epoll Loses Readiness When Watched Fd Closes But Dup Remains

Priority: P1
Impact: epoll interest lifetime does not follow open-file-description semantics
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AH-jit-runtime-20260710`.

Evidence:

- `epoll_ctl` registers kqueue filters by fd number: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:348`.
- `dup` carries some metadata but no epoll ownership/lifetime metadata: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:900`.
- `close` resets fd-number emulation state: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2334`.

Why this is bad:

Linux epoll interest is tied to the underlying open file description. Closing the watched numeric fd should not remove readiness while a duplicate descriptor still keeps the same pipe/socket object alive.

Isolated proof:

```sh
DD_PCACHE=0 cargo run -q -p dd-jit --example ah_run_guest -- target/ah-probes/epoll_dup_lifetime.aarch64
```

Linux/qemu observed `wait=1 ... read=1 char=Q`; dd observed `wait=0 ... read=1 char=Q`.

## Fork Children Lose Inherited Epoll Interest

Priority: P1
Impact: child processes lose inherited epoll interest lists
Confidence: High

(The timerfd half is FIXED: the fork child's kqueue rebuild now re-arms an inherited timerfd from its
tracked deadline/interval, so the child sees the expiration. Only the epoll interest-list half remains.)

Evidence:

- Rebuild creates empty kqueues for epoll on fork: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c` (`kqueue_rebuild_after_fork`).

Why this is bad:

Linux children inherit the epoll interest list. dd rebuilds an empty kqueue and has no persistent record of
each registration's `epoll_event.data` (udata) to re-register with, so a child that epoll_waits WITHOUT
re-registering (rather than re-initialising its event loop, as most runtimes do) sees no events. A full fix
needs a per-instance registration table (fd + events + udata) written on every epoll_ctl -- a hot-path
change deferred to avoid destabilising the epoll fast path; most real runtimes re-register post-fork so this
is rarely hit.

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

## `renameat2(RENAME_WHITEOUT)` Silently Becomes Plain Rename

Priority: P2
Impact: overlay-style whiteout operations lose the source whiteout marker
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-runtime-fs-syscalls-BR-20260710`.

Evidence:

- `RENAME_WHITEOUT` is accepted as a valid flag: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1203`.
- Only `NOREPLACE` and `EXCHANGE` are translated into host rename flags: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1230`.

Why this is bad:

Linux `RENAME_WHITEOUT` creates a source whiteout while renaming. dd reports success but performs a plain rename, removing the source and losing overlayfs semantics.

Observed proof:

```text
Linux/qemu: rejected=0 src_exists=1 src_chr=1 dst_exists=1 errno=0
dd:         rejected=0 src_exists=0 src_chr=0 dst_exists=1 errno=0
```
