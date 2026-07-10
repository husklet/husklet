# Syscall Compatibility and Completeness Gaps

This file keeps syscall findings framed around Linux compatibility, fake-success behavior, data loss, probe correctness, hangs, and workload breakage.

## `pidfd` Fixed Registry Capacity Cliff

Priority: P2
Impact: pidfd allocation cliff
Confidence: High

(The invalid-flags half of this finding is FIXED: `pidfd_open` now rejects unknown flag bits with
`EINVAL` in `rare.c`. Only the capacity cliff below remains.)

Evidence:

- The pidfd table is fixed-size (`PIDFD_MAX 64`) in syscall dispatch state: `dd-jit-darwin/src/runtime/os/linux/syscall/dispatch.c:209`.

Why this is bad:

Linux can allocate far more than 64 pidfds. dd hits a capacity cliff once the table fills, which makes
pidfd-heavy tests or runtimes fail differently from Linux (`many_ok=0` where Linux prints `many_ok=1`).

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

## Sentry `ppoll` Masks Stale Fds Instead Of `POLLNVAL`

Priority: P1
Impact: event loops can sleep through fd invalidation
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-slot-G`.

Evidence:

- Sentry `ppoll` translates unmapped virtual fds to `-1`: `dd-jit-darwin/src/runtime/os/linux/sentry.c:1095`.

Why this is bad:

Linux reports `POLLNVAL` for stale or closed fds in a poll set. Mapping them to `-1` asks the host kernel to ignore them, so event loops miss invalidation.

Isolated proof:

The `audit_slot_g_sentry_fd.c` probe was run natively and under dd; the filtered JIT run failed with the expected oracle mismatch.

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

## Sentry `pselect6` Masks Invalid Virtual Fd Bits

Priority: P2
Impact: invalid fd readiness is hidden instead of `EBADF`
Confidence: Medium

Evidence:

- Sentry `pselect6` rebuilds fd sets from virtual fds and silently skips unmapped/unrepresentable fds: `dd-jit-darwin/src/runtime/os/linux/sentry.c:1118`.

Why this is bad:

Linux `select`/`pselect` should fail `EBADF` for invalid positive fd bits. Skipping them can make event loops block instead of noticing closed fds.

Verification:

Run an untrusted-mode `pselect6` probe with an invalid positive fd bit and compare against native `EBADF`.

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

## Signal Ucontext Stack Metadata Is Zero

Priority: P2
Impact: signal handlers see corrupted `ucontext_t.uc_stack`
Confidence: High

Verification status: Proven on aarch64 in isolated worktree `/Users/x/dd/dd-worker-V-jit-runtime-20260710`.

Evidence:

- aarch64 signal frames are zeroed and then selected fields are filled: `dd-jit-darwin/src/runtime/translate/aarch64/sigframe.c:27`.
- aarch64 mask/register setup does not populate `uc_stack`: `dd-jit-darwin/src/runtime/translate/aarch64/sigframe.c:46`.
- x86_64 signal frame construction has the same zeroed-frame pattern: `dd-jit-darwin/src/runtime/translate/x86_64/sigframe.c:40`.

Why this is bad:

Runtimes and crash handlers inspect `ucontext_t` to understand the active stack and altstack state. dd delivers handlers on the altstack but exposes zero stack metadata.

Isolated proof:

```sh
mac bash -lc 'DDJIT_DIR=$OUT $OUT/ddjit-linux_aarch64 scratch-worker-V/sig_uc_stack.aarch64'
```

Native printed `seen=1 on_alt=1 uc_stack=1`; dd printed `seen=1 on_alt=1 uc_stack=0`.

## `signalfd` Update Keeps Stale Signals (ORs Masks Instead Of Replacing)

Priority: P1
Impact: signal event loops can receive masked-out signals
Confidence: High

(The short-read half of this finding is FIXED: a signalfd `read` with `count < 128` now returns `EINVAL`
without consuming the pending signal. Only the mask-update half below remains.)

Evidence:

- `signalfd` update ORs new masks into the shared mask instead of replacing them: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:793`.

Why this is bad:

Linux updates an existing signalfd mask to exactly the new set. dd's OR keeps previously-enabled signals
active, so an event loop that narrowed its mask can still receive signals it meant to drop. (This is also
entangled with the single shared-signalfd model -- see the multiple-signalfd-independence finding.)

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

## `dup(epoll_fd)` Loses Pending Interest Registration

Priority: P1
Impact: duplicated epoll fds can miss readiness
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BA2-fd-event-20260710`.

Evidence:

- Deferred changelists are stored per numeric epoll fd: `dd-jit-darwin/src/runtime/os/linux/syscall/helpers.c:924`.
- `epoll_ctl` queues changes under the original epfd: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:348`.
- `epoll_wait` submits only the waiting fd's changelist: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:471`.
- `dup` does not copy epoll instance metadata: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:900`.

Why this is bad:

Linux treats a duplicated epoll fd as the same epoll instance. If `epoll_ctl` registers interest on the original epfd, `epoll_wait` on the duplicate must see the ready event.

Isolated proof:

```sh
mac bash -lc 'timeout 5 ddjit-linux_aarch64 scratch-BA2/epoll_dup_instance.aarch64'
```

Linux observed `wait_dup=1 ... data=0xabcddcba`; dd observed `wait_dup=0 errno=0`.

## Fork Children Lose Inherited Epoll/Timerfd State

Priority: P1
Impact: child processes lose inherited event sources and timers
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AH-jit-runtime-20260710`.

Evidence:

- Fork child calls kqueue rebuild: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:300`.
- Rebuild creates empty kqueues for epoll/timerfd/inotify: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:151`.
- Timerfd state uses kqueue: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:811`.
- Timerfd read drains kqueue state: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:438`.

Why this is bad:

Linux children inherit epoll and timerfd objects. Replacing them with empty kqueues drops interest lists and armed timers, so child event loops can time out or hang.

Isolated proof:

```sh
DD_PCACHE=0 cargo run -q -p dd-jit --example ah_run_guest -- target/ah-probes/epoll_fork_childwait.aarch64
timeout 5 env DD_PCACHE=0 cargo run -q -p dd-jit --example ah_run_guest -- target/ah-probes/timerfd_fork.aarch64
```

Linux/qemu epoll child observed `wait=1 data=61626364 child_ok=1`; dd observed `wait=0 data=0 child_ok=0`. Linux/qemu timerfd child read one expiration; dd timed out.

## Forked Child Loses Inherited Inotify Watch And Can Hang

Priority: P1
Impact: fork children can lose inherited file-watch state and block
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BA2-fd-event-20260710`.

Evidence:

- Fork rebuilds epoll/timerfd/inotify kqueues as fresh empty kqueues: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:151`.
- Inotify creates a kqueue and applies `IN_NONBLOCK`: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:567`.
- Watches are registered on the original kqueue: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:589`.
- Read can block if rebuilt fd loses `O_NONBLOCK`: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:382`.

Why this is bad:

Linux children inherit inotify instances and watches. dd drops the inherited watch and can also lose nonblocking status, so the child blocks in `read()` instead of receiving the event.

Isolated proof:

```sh
mac bash -lc 'timeout 5 ddjit-linux_aarch64 scratch-BA2/inotify_fork_watch.aarch64'
```

Linux observed `child_read=32 errno=0 mask=0x100`; dd produced no child result and was killed by timeout.

## `dup(signalfd)` Loses Signalfd Semantics

Priority: P1
Impact: duplicated signalfds return raw pipe bytes and reject valid updates
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AT-jit-fd-event-20260710`.

Evidence:

- Signalfd reads only use the virtual path when `rfd == g_sigfd_read`: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:340`.
- Updates reject duplicated signalfds because they are not the original numeric fd: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:775`.
- `dup` does not carry signalfd metadata: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:900`.

Why this is bad:

Linux duplicated signalfds refer to the same signalfd object. dd turns the duplicate into a raw pipe-like descriptor and rejects valid mask updates.

Isolated proof:

```sh
./scratch-t186/ddjit-aarch64 ./scratch-AT/.signalfd_dup.bin
```

Linux observed `read_dup=128` and a successful update; dd observed `read_dup=1` and `update_errno=22`.

## `dup(inotify_fd)` Loses Inotify Read Semantics

Priority: P2
Impact: duplicated inotify descriptors cannot read events
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AT-jit-fd-event-20260710`.

Evidence:

- Inotify instances are stamped only on the original fd: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:567`.
- Inotify reads require `g_inotify[rfd]`: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:367`.
- `dup` does not carry that state: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:900`.

Why this is bad:

Wrappers often duplicate descriptors before passing them through event loops. dd loses the virtual inotify metadata, so events are unavailable through the duplicate.

Isolated proof:

```sh
./scratch-t186/ddjit-aarch64 ./scratch-AT/.inotify_dup.bin
```

Linux observed `read_dup=32 read_dup_errno=0 first_mask=0x100`; dd observed `read_dup=-1 read_dup_errno=6`.

## `inotify_init1(0)` And `timerfd_create(..., 0)` Set Close-On-Exec

Priority: P1
Impact: event fds vanish across exec even when close-on-exec was not requested
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BJ2-fd-event-20260710`.

Evidence:

- Inotify creation sets `FD_CLOEXEC` only when requested but does not clear macOS kqueue's default: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:567`.
- Timerfd creation has the same shape: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:826`.
- Epoll creation documents the kqueue default and explicitly clears it: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:258`.
- Exec closes fds with close-on-exec metadata: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:207`, `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1703`.

Why this is bad:

Linux preserves `inotify_init1(0)` and `timerfd_create(..., 0)` descriptors across exec. dd closes them, so event loops and supervisors can silently lose watches and timers after re-exec.

Observed proof:

```text
Linux: parent_inotify=1 parent_timerfd=1 child_inotify=1 child_timerfd=1
dd:    parent_inotify=2 parent_timerfd=2 child_inotify=-9 child_timerfd=-9
```

## `timerfd` CLOCK_REALTIME Absolute Deadlines Are Treated As Monotonic

Priority: P1
Impact: realtime absolute timers can be scheduled decades in the future
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BQ2-fd-event-20260710`.

Evidence:

- `timerfd_create` accepts `CLOCK_REALTIME`: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:811`.
- `timerfd_settime(TFD_TIMER_ABSTIME)` subtracts `CLOCK_MONOTONIC` from the absolute value regardless of the timer clock: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:913`.

Why this is bad:

Linux `CLOCK_REALTIME` absolute timers use realtime clock values. Treating that value as monotonic makes a near-future realtime deadline look like an enormous monotonic deadline, so poll/read never fire on time.

Observed proof:

```text
Linux: rem_sec=0 ... poll=1 ... elapsed_ms=85 read=8 ... count=1
dd:    rem_sec=1782499723 ... poll=0 ... elapsed_ms=401 read=-1 read_errno=11 count=0
```

## `wait4` Misses `WCONTINUED` And Corrupts Final Status

Priority: P1
Impact: child continuation and exit status reporting is wrong
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-wait-audit-20260710`.

Evidence:

- Wait options are passed to host `wait4`: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1813`.
- Status is translated afterward: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1862`.

Why this is bad:

Linux `WCONTINUED` is `0x8`, while macOS uses a different bit. Passing Linux bits directly to host wait misses continued events and can corrupt the following final exit status.

Observed proof:

```text
qemu: wait_continued stop=1 r2=95844 errno2=0 cont=1 r3=95844 errno3=0 exit=1 raw2=0xffff raw3=0x700
dd:   wait_continued stop=1 r2=0 errno2=0 cont=0 r3=38816 errno3=0 exit=0 raw2=0x0 raw3=0x9300
```

## `SA_NOCLDWAIT` Does Not Suppress Zombies

Priority: P1
Impact: children remain waitable despite no-zombie signal policy
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-wait-audit-20260710`.

Evidence:

- `rt_sigaction` stores flags but host handler setup uses only `SA_SIGINFO`: `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:394`, `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:420`.

Why this is bad:

Linux `SA_NOCLDWAIT` prevents child zombies. dd still leaves a reapable child, so supervisors can observe impossible wait behavior.

Observed proof:

```text
Linux: sa_nocldwait sigs=1 wait=-1 errno=10 no_zombie=1 raw=0x0
dd:    sa_nocldwait sigs=1 wait=38818 errno=0 no_zombie=0 raw=0x1700
```

## `clone` Ignores Parent And Child TID Stores

Priority: P2
Impact: process clone TID synchronization fields are not written
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-wait-audit-20260710`.

Evidence:

- Process clone handling ignores `CLONE_PARENT_SETTID` and `CLONE_CHILD_SETTID`: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1465`.
- Clone3 likely has the same gap: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1972`.

Why this is bad:

Linux stores the child pid into the parent and child TID pointers when requested. dd leaves the parent store unchanged and the child store invisible.

Observed proof:

```text
qemu: clone_tid_parent child=95848 ptid=95848 parent_ok=1 child_ok=1 raw=0x0
dd:   clone_tid_parent child=38820 ptid=-1 parent_ok=0 child_ok=0 raw=0x2a00
```

## `SA_NOCLDSTOP` Still Delivers Stop SIGCHLD

Priority: P2
Impact: signal handlers see child-stop notifications that should be suppressed
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-wait-audit-20260710`.

Evidence:

- Signal action flag handling stores flags but does not apply `SA_NOCLDSTOP` to host delivery: `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:394`, `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:420`.

Why this is bad:

With `SA_NOCLDSTOP`, Linux suppresses `SIGCHLD` for child stops and reports only termination. dd delivers a stop notification anyway.

Observed proof:

```text
Linux: sa_nocldstop before=0 after=1 stop_ok=1 suppressed=1 raw=0x9
dd:    sa_nocldstop before=1 after=2 stop_ok=1 suppressed=0 raw=0x137f
```

## aarch64 Signal Ucontext Omits FPSIMD Context Record

Priority: P1
Impact: signal handlers and crash reporters cannot discover SIMD state
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-aarch64-runtime-20260710`.

Evidence:

- aarch64 sigframe construction writes signal context directly: `dd-jit-darwin/src/runtime/translate/aarch64/sigframe.c:4`.
- It copies raw NEON bytes into the reserved area without an `_aarch64_ctx` / `fpsimd_context` header: `dd-jit-darwin/src/runtime/translate/aarch64/sigframe.c:58`.

Why this is bad:

Linux aarch64 signal frames expose `FPSIMD_MAGIC` in `uc_mcontext.__reserved`. dd omits the context record, so handlers and unwind/crash tooling cannot locate SIMD state.

Observed proof:

```text
Linux: fpsimd_ctx found=1 sane_size=1
dd:    fpsimd_ctx found=0 sane_size=0
```

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

## Raw `waitid(..., rusage)` Leaves Buffer Untouched

Priority: P1
Impact: raw waitid callers lose child CPU/RSS accounting
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-proc-lifecycle-20260710`.

Evidence:

- `waitid` handles pid/status but never reads or writes arg 5 `struct rusage *`: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:642`.

Why this is bad:

Linux raw `waitid` fills a non-null rusage buffer. dd leaves the guest buffer unchanged, so CPU and RSS accounting can contain sentinel garbage.

Observed proof:

```text
Linux/qemu: waitid_rusage cpu_pos=1 maxrss=192
dd:         waitid_rusage cpu_pos=0 maxrss_reasonable=0 maxrss=-6510615555426900571
```

## Default Core Status Contradicts `RLIMIT_CORE=0`

Priority: P1
Impact: wait status reports core dumps despite zero core limit
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-proc-lifecycle-20260710`.

Evidence:

- `getrlimit(RLIMIT_CORE)` reports soft limit `0`: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:190`.
- Signal/core-limit helper defaults to `RLIM_INFINITY`: `dd-jit-darwin/src/runtime/os/linux/signal.c:136`.
- `wait4` and `waitid` consume that contradictory state: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1901`, `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:719`.

Why this is bad:

With soft core limit `0`, Linux reports a terminating signal but not a core dump. dd reports core-dumped status even while `getrlimit` says core files are disabled.

Observed proof:

```text
Linux: wait4_default_core soft0=1 core=0; waitid_default_core code=2 dumped=0
dd:    wait4_default_core soft0=1 core=1; waitid_default_core code=3 dumped=1
```

## `wait4` Writes Host Rusage Units Into Guest Layout

Priority: P2
Impact: child resource accounting reports byte-scale Darwin values
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-proc-lifecycle-20260710`.

Evidence:

- `wait4` passes the guest `struct rusage *` directly to host `wait4`: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1862`.
- `getrusage` has an explicit Linux conversion path, showing the needed pattern: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1177`.

Why this is bad:

Linux `ru_maxrss` is in kilobytes. dd exposes Darwin byte-scale values in the Linux guest layout.

Observed proof:

```text
Linux/qemu: wait4_rusage maxrss=192 / 8612
dd:         wait4_rusage maxrss=4898816 / 4325376
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

## Proc Stat Reports Wrong Process Group And Session

Priority: P2
Impact: process discovery sees false job-control identity for forked children
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-pgrp-signal-20260710`.

Evidence:

- Self stat prints process group and session as `pid,pid`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1635`.
- Peer stat prints session from process group data: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2212`.

Why this is bad:

For a normal forked child, `/proc/<pid>/stat` fields 5 and 6 should match `getpgrp()` and `getsid()`. dd reports child-local values, so supervisors and job-control tools reconstruct the wrong process tree.

Observed proof:

```text
Linux: child self stat_ok=1 pgrp_match=1 sid_match=1; parent peer stat_ok=1 sid_match=1
dd:    child self stat_ok=1 pgrp_match=0 sid_match=0; parent peer stat_ok=1 sid_match=0
```

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
