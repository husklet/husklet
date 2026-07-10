# Syscall Compatibility and Completeness Gaps

This file keeps syscall findings framed around Linux compatibility, fake-success behavior, data loss, probe correctness, hangs, and workload breakage.

## `close_range` Unknown Flags Close File Descriptors

Priority: P1
Impact: fd sanitizer data loss and wrong feature-probe result
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verify-agent1-src2-20260710-112023`.

Evidence:

- `close_range(first, last, flags)` validates only `first > last`: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:137`.
- Unknown flags are not rejected before the close loop: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:152`.

Why this is bad:

Linux rejects unknown `close_range` flag bits with `EINVAL` and leaves the requested fd range unchanged. dd can report success and close the fd anyway, which turns a feature probe or defensive sanitizer into silent fd loss.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-verify-agent1/release/build/dd-jit-darwin-16122afd27b6bb64/out \
  CARGO_TARGET_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-linux-harness \
  cargo run -q -p dd-tests -- -e aarch64 close_range_flags
```

Observed: dd prints `einval=0 unchanged=0`; Linux oracle prints `einval=1 unchanged=1`.

## `SO_PASSCRED` and `SO_PEERCRED` Bad Fds Fake Success

Priority: P1
Impact: socket capability probes and credential checks receive synthetic results
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verify-agent1-src2-20260710-112023`.

Evidence:

- `setsockopt(SO_PASSCRED)` stores a per-fd flag without first validating the fd/socket: `dd-jit-darwin/src/runtime/os/linux/syscall/net.c:1080`.
- `getsockopt(SO_PASSCRED)` can answer from synthetic state: `dd-jit-darwin/src/runtime/os/linux/syscall/net.c:1144`.
- `getsockopt(SO_PEERCRED)` falls back to synthetic credentials: `dd-jit-darwin/src/runtime/os/linux/syscall/net.c:1152`.

Why this is bad:

Linux returns `EBADF` for bad fds and `ENOTSOCK` for regular files. dd returns success for cases that should reject, so callers can believe they have valid socket credentials or credential passing on an invalid target.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-verify-agent1/release/build/dd-jit-darwin-16122afd27b6bb64/out \
  CARGO_TARGET_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-linux-harness \
  cargo run -q -p dd-tests -- -e aarch64 net_sockcred_badfd
```

Observed: dd prints all five bad-fd/non-socket checks as `0`; Linux prints all five as `1`.

## `pidfd` Invalid Flags and Fixed Registry Capacity

Priority: P2
Impact: wrong errno and pidfd allocation cliff
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verify-agent1-src2-20260710-112023`.

Evidence:

- `pidfd_open(pid, flags)` does not reject unknown flags: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:166`.
- The pidfd table is fixed-size in syscall dispatch state: `dd-jit-darwin/src/runtime/os/linux/syscall/dispatch.c:209`.

Why this is bad:

Linux rejects invalid pidfd flags and can allocate more than the current fixed table size. dd accepts bad flags and then hits a capacity cliff, which makes pidfd-heavy tests or runtimes fail differently from Linux.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-verify-agent1/release/build/dd-jit-darwin-16122afd27b6bb64/out \
  CARGO_TARGET_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-linux-harness \
  cargo run -q -p dd-tests -- -e aarch64 pidfd_flags_capacity
```

Observed: dd prints `bad_open=0 bad_send=0 many_ok=0`; Linux prints `bad_open=1 bad_send=0 many_ok=1`.

## `unshare` and `setns` Invalid Inputs Fake Success

Priority: P1
Impact: namespace feature probes continue under false assumptions
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verify-agent1-src2-20260710-112023`.

Evidence:

- `unshare` and `setns` return success unconditionally: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:634`.

Why this is bad:

Invalid inputs such as `setns(-1, ...)` and unknown `unshare` flags should fail. Returning success lets setup code believe namespace state changed when it did not.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-verify-agent1/release/build/dd-jit-darwin-16122afd27b6bb64/out \
  CARGO_TARGET_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-linux-harness \
  cargo run -q -p dd-tests -- -e aarch64 ns_invalid
```

Observed: dd prints all invalid-input checks as `0`; Linux prints all as `1`.

## `getresuid` and `getresgid` Accept Null Outputs

Priority: P3
Impact: wrong errno and missed caller bugs
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verify-agent1-src2-20260710-112023`.

Evidence:

- `getresuid` skips null outputs and returns success: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:999`.
- `getresgid` has the same shape: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1008`.

Why this is bad:

Linux returns `EFAULT` for invalid output pointers. dd masks caller bugs and gives a false success path.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-verify-agent1/release/build/dd-jit-darwin-16122afd27b6bb64/out \
  CARGO_TARGET_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-linux-harness \
  cargo run -q -p dd-tests -- -e aarch64 sys_getresid_null
```

Observed: dd prints `uid_efault=0 gid_efault=0`; Linux prints `uid_efault=1 gid_efault=1`.

## `sched_setscheduler(SCHED_FIFO)` Reports Success Without Applying Policy

Priority: P2
Impact: real-time scheduling probes silently lie
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verify-agent1-src2-20260710-112023`.

Evidence:

- Scheduler policy emulation returns synthetic success for accepted policy shapes: `dd-jit-darwin/src/runtime/os/linux/syscall/rare.c:493`.

Why this is bad:

Unprivileged Linux normally rejects real-time `SCHED_FIFO` with `EPERM`. dd returns success without applying RT scheduling, so programs can believe latency-sensitive scheduling was installed when it was not.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-verify-agent1/release/build/dd-jit-darwin-16122afd27b6bb64/out \
  CARGO_TARGET_DIR=/Users/x/dd/dd-verify-agent1-src2-20260710-112023/target-linux-harness \
  cargo run -q -p dd-tests -- -e aarch64 sched_policy_guard
```

Observed: dd prints `fifo_eperm=0 badpol_einval=1 getbad_einval=1`; Linux prints `fifo_eperm=1 badpol_einval=1 getbad_einval=1`.

## `epoll_wait` / `epoll_pwait` Accept `maxevents <= 0`

Priority: P2
Impact: wrong errno and input-validation compatibility
Confidence: High

Verification status: Proven across aarch64 and x86_64 in isolated worktree `/Users/x/dd/dd-workerA-syscall-audit-20260710`.

Evidence:

- `epoll_pwait` clamps negative `maxevents` to zero instead of rejecting zero/negative values: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:405`.

Why this is bad:

Linux returns `EINVAL` when `maxevents <= 0`. dd returns success-like behavior, so bad caller input and feature probes get the wrong verdict.

Isolated proof:

```sh
DDJIT_DIR="$PWD/target-workerA-syscall-audit/release/build/dd-jit-darwin-16122afd27b6bb64/out" \
  cargo run -q -p dd-tests -- -e aarch64 epoll-wait-badmax
```

Observed: native `zero=1 neg=1`; dd `zero=0 neg=0`. x86_64 showed the same mismatch.

## `inotify_init1` Accepts Unknown Flag Bits

Priority: P2
Impact: feature probes can believe unsupported behavior exists
Confidence: High

Verification status: Proven across aarch64 and x86_64 in isolated worktree `/Users/x/dd/dd-workerA-syscall-audit-20260710`.

Evidence:

- `inotify_init1(flags)` creates a kqueue and applies known bits without rejecting unknown flags: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:567`.

Why this is bad:

Linux rejects unknown `inotify_init1` flags with `EINVAL`. Accepting them creates false support signals.

Isolated proof:

```sh
DDJIT_DIR="$PWD/target-workerA-syscall-audit/release/build/dd-jit-darwin-16122afd27b6bb64/out" \
  cargo run -q -p dd-tests -- -e aarch64 inotify-init-flags
```

Observed: native `badflag=1 valid=1`; dd `badflag=0 valid=1`. x86_64 showed the same mismatch.

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

## `mprotect` Unaligned Address Succeeds

Priority: P2
Impact: errno/order compatibility and hidden allocator misuse
Confidence: High

Verification status: Proven across aarch64 and x86_64 in isolated worktree `/Users/x/dd/dd-workerA-syscall-audit-20260710`.

Evidence:

- `mprotect` is modeled as a no-op/intent tracker and does not reject unaligned addresses before returning success: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:652`.

Why this is bad:

Linux requires the address to be page-aligned and returns `EINVAL` otherwise. dd can hide bad allocator/runtime calls by reporting success.

Isolated proof:

```sh
DDJIT_DIR="$PWD/target-workerA-syscall-audit/release/build/dd-jit-darwin-16122afd27b6bb64/out" \
  cargo run -q -p dd-tests -- -e aarch64 mprotect-invalid
```

Observed: native `unaligned=1 valid=1`; dd `unaligned=0 valid=1`. x86_64 showed the same mismatch.

## `ppoll` Accepts Invalid `tv_nsec`

Priority: P2
Impact: invalid timeout probes silently succeed or become immediate timeouts
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-slot-G`.

Evidence:

- `ppoll` validates pointer accessibility but does not reject `tv_nsec < 0` or `tv_nsec >= 1000000000`: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:690`.

Why this is bad:

Linux rejects invalid timespec values with `EINVAL`. dd can treat them as normal timeouts, hiding caller bugs.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-slot-G/target-slot-G/release/build/dd-jit-darwin-16122afd27b6bb64/out make test FILTER=audit-slot-g
```

Result: filtered run had `0 passed, 2 failed`; one failing oracle probe covers invalid `ppoll` timeout handling.

## `madvise` Fake-Succeeds Invalid Advice And Unaligned Ranges

Priority: P2
Impact: false feature probes and missed memory API bugs
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-slot-G`.

Evidence:

- `madvise` is treated as best-effort and only selected advice values are forwarded: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:794`.
- Host `madvise` errors are ignored before returning success: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:868`.

Why this is bad:

Linux rejects unknown advice and unaligned invalid ranges. dd can return success, making software believe a memory policy was accepted when nothing happened.

Isolated proof:

```sh
DDJIT_DIR=/Users/x/dd/dd-slot-G/target-slot-G/release/build/dd-jit-darwin-16122afd27b6bb64/out make test FILTER=audit-slot-g
```

Result: filtered run had `0 passed, 2 failed`; one failing oracle probe covers invalid `madvise` behavior.

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

## Unknown Futex Ops/Flags Can Report Success

Priority: P2
Impact: futex capability probes misdetect unsupported behavior
Confidence: Medium-high

Evidence:

- The futex syscall masks op bits with `& 0x7f`: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:638`.
- Unmodelled futex ops fall through to success: `dd-jit-darwin/src/runtime/os/linux/thread.c:840`.

Why this is bad:

Native Linux returns errors for unknown futex op/flag cases. dd can report success and make libraries choose an unsupported synchronization path.

Verification:

Promote the native oracle checked in `/Users/x/dd/dd-slot-G` into a dd-tests probe for unknown op and flag combinations.

## Raw `pselect6` Accepts Invalid `tv_nsec`

Priority: P2
Impact: invalid timeout probes silently succeed or sleep
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-slotN`.

Evidence:

- The raw `pselect6` path copies the timeout and normalizes nanoseconds instead of rejecting invalid values: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:640`.

Why this is bad:

Linux rejects `tv_nsec < 0` or `tv_nsec >= 1e9` with `EINVAL`. dd accepts both and returns success-like results.

Isolated proof:

```sh
cargo run -p dd-tests -- audit-pselect-bad-timeout
```

Result: failed both Linux engines. dd `hi=0 neg=0`; native `hi=1 neg=1`.

## `prlimit64` Accepts Invalid Pid And Resource

Priority: P2
Impact: resource-limit probes see unsupported inputs as valid
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-slotN`.

Evidence:

- `prlimit64` fills and sets limits without validating pid or resource first: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1939`.
- It returns success unconditionally at the end: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1955`.

Why this is bad:

Linux returns errors for invalid resources and invalid target pids. dd can make runtimes believe unsupported limits or dead pids are valid.

Isolated proof:

```sh
cargo run -p dd-tests -- audit-prlimit-invalid
```

Result: failed both Linux engines. dd `resource=0 pid=0`; native `resource=1 pid=1`.

## x86_64 `mincore(..., vec=NULL)` Succeeds

Priority: P2
Impact: invalid copyout pointer is masked
Confidence: High

Verification status: Proven for x86_64 in isolated worktree `/Users/x/dd/dd-audit-slotN`.

Evidence:

- The x86 guest-page slow path uses a scratch host vector: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:773`.
- It validates/copies the guest vector only when `a2` is non-null: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:774`.

Why this is bad:

Linux returns `EFAULT` for a null output vector. dd can return success, hiding caller bugs.

Isolated proof:

```sh
cargo run -p dd-tests -- audit-mincore-nullvec
```

Result: aarch64 passed, x86_64 failed. dd `efault=0`; native `efault=1`.

## Plain `dup()` Drops Proc-Text Read-Only Metadata

Priority: P2
Impact: duplicated synthetic proc fds can diverge from source behavior
Confidence: Medium

Evidence:

- `dup()` copies path/description/pagemap/socket metadata but omits `g_proc_text_ro`: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:905`.
- `dup3` and `F_DUPFD` copy that metadata: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:965`, `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:1214`.

Why this is suspicious:

Plain `dup()` should preserve the same open file description semantics as the other dup paths. Missing proc-text read-only metadata can make the duplicate behave differently.

Verification:

Open the affected proc text synthetic fd, `dup()` it, then verify write/read-only behavior matches the source and `dup3`/`F_DUPFD`.

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

## `SIGKILL`/`SIGSTOP` Can Enter The Guest Signal Mask

Priority: P1
Impact: unmaskable signals can become pending instead of fatal/stopping
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-P-jit-runtime-20260710`.

Evidence:

- `rt_sigprocmask` applies guest masks without clearing unmaskable signals: `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:453`.
- `raise_guest_signal` checks the guest mask before fatal/default handling, so blocked `SIGKILL` becomes pending: `dd-jit-darwin/src/runtime/os/linux/signal.c:415`.

Why this is bad:

Linux does not allow `SIGKILL` or `SIGSTOP` to be blocked. dd can let a process survive a blocked `SIGKILL`.

Isolated proof:

```sh
cargo run -q -p dd-tests -- -e aarch64 slotp-sigmask-unmaskable
cargo run -q -p dd-tests -- -e x86_64 slotp-sigmask-unmaskable
```

Result: failed both engines. dd `killed=0`; native `killed=1`.

## `sigaltstack` Accepts Invalid Stack Configs

Priority: P2
Impact: later `SA_ONSTACK` delivery can use invalid or tiny stacks
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-P-jit-runtime-20260710`.

Evidence:

- `sigaltstack` copies old/new state directly and returns success: `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:175`.

Why this is bad:

Linux validates invalid flags, minimum stack size, active-stack changes, and bad pointers. dd accepts invalid configurations that can corrupt later signal delivery.

Isolated proof:

```sh
cargo run -q -p dd-tests -- -e aarch64 slotp-sigaltstack-validate
cargo run -q -p dd-tests -- -e x86_64 slotp-sigaltstack-validate
```

Result: failed both engines; dd accepts invalid inputs that native rejects.

## `signalfd4` Ignores `sizemask`

Priority: P2
Impact: raw syscall validation mismatch and potential bad-pointer behavior
Confidence: High on aarch64; medium cross-arch

Verification status: Proven on aarch64 in isolated worktree `/Users/x/dd/dd-worker-P-jit-runtime-20260710`.

Evidence:

- `signalfd4(fd, mask, sizemask, flags)` validates flags and fd but then dereferences the mask without validating `sizemask`: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:764`.

Why this is bad:

Linux rejects invalid `sizemask` values such as zero with `EINVAL`. Bad mask pointers should return `EFAULT`, not fault the engine.

Isolated proof:

```sh
cargo run -q -p dd-tests -- -e aarch64 slotp-signalfd-size
```

Result: failed on aarch64. dd `einval=0`; native `einval=1`. The exact PoC passed on x86_64.

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

## Raw Signal Validation Is Too Permissive

Priority: P2
Impact: invalid signal setup reports success
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-U`.

Evidence:

- `sigaltstack` accepts stack state without full Linux validation: `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:175`.
- `rt_sigprocmask` treats invalid `how` as set-mask behavior and ignores `sigsetsize`: `dd-jit-darwin/src/runtime/os/linux/syscall/signal.c:453`.

Why this is bad:

Signal setup APIs are frequently used as feature probes and runtime invariants. dd can accept invalid flags, undersized altstacks, and malformed mask operations that Linux rejects.

Isolated proof:

```sh
aarch64-linux-gnu-gcc -O2 -static -Wall -Wextra -o scratch-worker-U/poc_signal_validation scratch-worker-U/poc_signal_validation.c
```

Native returned `EINVAL/EINVAL/ENOMEM`; dd returned success for all three cases.

## `pselect6` And `ppoll` Ignore Temporary Signal Masks

Priority: P1
Impact: signal-driven waits can sleep through deliverable signals
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-U`.

Evidence:

- `pselect6` validates fdsets/timeout but ignores the raw mask argument: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:619`.
- It calls host `pselect` with a null mask: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:663`.
- `ppoll` has the same class, calling plain `poll`: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:681`.

Why this is bad:

Linux atomically swaps the signal mask for `pselect6`/`ppoll`. Event loops rely on this to avoid missed wakeups. Ignoring the mask can turn a signal wakeup into a full timeout or hang.

Isolated proof:

```sh
aarch64-linux-gnu-gcc -O2 -static -Wall -Wextra -o scratch-worker-U/poc_pselect_mask scratch-worker-U/poc_pselect_mask.c
```

Native woke on `SIGUSR1` in about `104ms` with `EINTR`; dd slept the full second and returned timeout with the handler not run.

## Pipe Size Fcntls Fake Success On Invalid Fds

Priority: P2
Impact: pipe capability probes get wrong results
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-U`.

Evidence:

- `F_SETPIPE_SZ` / `F_GETPIPE_SZ` return synthetic pipe sizes without first validating the fd or pipe type: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:1123`.

Why this is bad:

Linux returns `EBADF` for invalid fds and rejects non-pipe fds. dd reports sizes and success, so callers can believe a regular file or bad fd is a pipe.

Isolated proof:

```sh
aarch64-linux-gnu-gcc -O2 -static -Wall -Wextra -o scratch-worker-U/poc_fcntl_pipesz scratch-worker-U/poc_fcntl_pipesz.c
```

Native returned errors for a bad fd and `/dev/null`; dd reported pipe sizes and success.

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

## `clock_nanosleep(TIMER_ABSTIME)` Swallows Interrupts

Priority: P1
Impact: signal-driven timers can sleep until the full deadline
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-V-jit-runtime-20260710`.

Evidence:

- Absolute `clock_nanosleep` loops on host `EINTR`: `dd-jit-darwin/src/runtime/os/linux/syscall/time.c:356`.
- The loop returns `0` after reaching the deadline: `dd-jit-darwin/src/runtime/os/linux/syscall/time.c:378`.

Why this is bad:

Linux returns `EINTR` when a deliverable signal interrupts `clock_nanosleep`. dd can hide the interrupt, run the handler, sleep until the original deadline, and report success.

Isolated proof:

```sh
mac bash -lc 'DDJIT_DIR=$OUT $OUT/ddjit-linux_aarch64 scratch-worker-V/clock_abs_eintr.aarch64'
```

Native printed `rc=4 hit=1 elapsed_ms=101`; dd printed `rc=0 hit=1 elapsed_ms=1002`.

## `eventfd` Counter Overflow Wraps To Zero

Priority: P1
Impact: event counters and wake state can be silently lost
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AC-signal-runtime-20260710`.

Evidence:

- Eventfd write adds to the counter without checking saturation: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:611`.

Why this is bad:

Linux rejects eventfd writes that would overflow the counter with `EAGAIN` and preserves the prior value. dd accepts the write, wraps the counter to zero, and loses the pending event state.

Isolated proof:

```sh
DDWAKE_ROLE=__none ddjit-linux_aarch64 target/ac-probes/eventfd_overflow
```

Linux observed `w2=-1 e2=11 r=8 got=18446744073709551614`; dd observed `w2=8 e2=0 r=-1 er=11 got=0`.

## `dup(eventfd)` Loses Eventfd Semantics

Priority: P1
Impact: duplicated fds do not share the eventfd object
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AC-signal-runtime-20260710`.

Evidence:

- `dup` carries path/socket/memfd metadata but not eventfd peer/counter state: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:900`.
- `dup2`/`dup3` and `F_DUPFD` have the same class of risk: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:956`, `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:1210`.

Why this is bad:

Linux duplicated eventfds refer to the same eventfd object and read/write the same 8-byte counter. dd can turn the duplicate into an ordinary fd-like path, corrupting event loops that dup descriptors.

Isolated proof:

```sh
DDWAKE_ROLE=__none ddjit-linux_aarch64 target/ac-probes/eventfd_dup
```

Linux observed `rd=8 got=5`; dd observed `rd=1 got=1`.

## `signalfd` Update Keeps Stale Signals And Short Reads Consume Events

Priority: P1
Impact: signal event loops can receive masked-out signals and lose pending records
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AC-signal-runtime-20260710`.

Evidence:

- `signalfd` update ORs new masks instead of replacing them: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:793`.
- Signalfd read lacks a `count < 128` `EINVAL` guard and consumes before validating the user count: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:340`.

Why this is bad:

Linux updates an existing signalfd mask to the new set. It also rejects short reads without consuming the pending signal. dd can keep stale signals enabled and consume an event while returning a short-read-shaped success.

Isolated proof:

```sh
DDWAKE_ROLE=__none ddjit-linux_aarch64 target/ac-probes/signalfd_update_shortread
```

Linux observed `usr1_n=-1 usr1_e=11 ... short_n=-1 short_e=22 final_n=128 final_sig=12`; dd observed `usr1_n=128 usr1_sig=10 short_n=128 short_e=0 final_n=-1 final_e=11`.

## `dup(timerfd)` Loses Timerfd Semantics

Priority: P2
Impact: duplicated timer fds cannot read timer expirations
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AC-signal-runtime-20260710`.

Evidence:

- Timerfd metadata is stamped only on the original fd: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:826`.
- Timerfd read is gated by the per-fd timer table: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:438`.
- Duplication does not carry that state: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:900`.

Why this is bad:

Linux duplicated timerfds share the same timer object. dd loses the virtual timer metadata, so reads from the duplicated fd fail instead of returning the expiration count.

Isolated proof:

```sh
DDWAKE_ROLE=__none ddjit-linux_aarch64 target/ac-probes/timerfd_dup
```

Linux observed `rd=8 re=0 got=1`; dd observed `rd=-1 re=6 got=0`.

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

## `inotify_rm_watch` Can Close An Unrelated Fd

Priority: P1
Impact: bad watch descriptors can silently close arbitrary guest fd numbers
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AT-jit-fd-event-20260710`.

Evidence:

- `inotify_rm_watch` closes `(int)a1` and returns success without verifying that `wd` belongs to the inotify instance: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:610`.

Why this is bad:

Linux rejects invalid watch descriptors with `EINVAL` and leaves other fds alone. dd can close a victim fd whose number matches the bad `wd`, causing silent fd loss.

Isolated proof:

```sh
./scratch-t186/ddjit-aarch64 ./scratch-AT/.inotify_rm.bin
```

Linux observed `rm=-1 rm_errno=22 victim_open=1`; dd observed `rm=0 rm_errno=0 victim_open=0`.

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

## Short `read(timerfd)` Consumes The Expiration

Priority: P1
Impact: timer wakeups can be lost after an invalid short read
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BJ2-fd-event-20260710`.

Evidence:

- Timerfd read calls `kevent` before validating that `count >= 8`: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:438`.
- It can return `8` even when the user buffer is too small: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:456`.

Why this is bad:

Linux rejects short timerfd reads with `EINVAL` and leaves the expiration pending. dd reports success and drains the event, so the next read can return `EAGAIN`.

Observed proof:

```text
Linux: short_read=-1 errno=22 small=0xaaaaaaaa next_read=8 next_errno=0 val=1
dd:    short_read=8 errno=0 small=0xaaaaaaaa next_read=-1 next_errno=11 val=0
```

## Short `read(inotify)` Consumes The Event

Priority: P1
Impact: file-watch events can be lost after an invalid short read
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BJ2-fd-event-20260710`.

Evidence:

- Inotify read drains `kevent` before ensuring the buffer can hold one `struct inotify_event`: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:367`, `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:384`.
- If the buffer is too small, the loop breaks and returns the current offset: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:422`, `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:435`.

Why this is bad:

Linux rejects too-small inotify reads with `EINVAL` and preserves the queued event. dd returns `0`, consumes the event, and leaves the next read with no event.

Observed proof:

```text
Linux: wd=1 short_read=-1 errno=22 next_read=16 next_errno=0 next_wd=1 mask=0x2
dd:    wd=5 short_read=0 errno=0 next_read=-1 next_errno=11 next_wd=-1 mask=0x0
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

## `epoll_pwait` Ignores Temporary Signal Mask

Priority: P1
Impact: signal-interruptible epoll waits can sleep through unblocked signals
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BQ2-fd-event-20260710`.

Evidence:

- The syscall dispatch path accepts and converts the sigmask pointer: `dd-jit-darwin/src/runtime/os/linux/syscall/dispatch.c:882`.
- The event wait path does not apply the temporary mask: `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:405`, `dd-jit-darwin/src/runtime/os/linux/syscall/event.c:480`.

Why this is bad:

`epoll_pwait` should temporarily replace the signal mask while waiting. dd ignores that mask, so a signal that should interrupt the wait remains blocked until after timeout.

Observed proof:

```text
Linux: ret=-1 errno=4 hit=1 elapsed_ms=105
dd:    ret=0 errno=0 hit=0 elapsed_ms=800
```

## `eventfd` Read With Null Buffer Reports Success

Priority: P2
Impact: bad guest pointers can fake reads instead of returning `EFAULT`
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BQ2-fd-event-20260710`.

Evidence:

- Eventfd read validates the user buffer only when the pointer is non-null: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:466`.
- The read path can then report an 8-byte success: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:519`.

Why this is bad:

Linux rejects `read(eventfd, NULL, 8)` with `EFAULT`. dd returns success, so bad-pointer probes can see a false read and lose compatibility with runtimes that depend on precise errno behavior.

Observed proof:

```text
Linux: bad=-1 bad_errno=14 good=-1 good_errno=11 val=0
dd:    bad=8 bad_errno=0 good=-1 good_errno=11 val=0
```

## `FUTEX_WAIT_BITSET` / `FUTEX_WAKE_BITSET` Ignore Masks

Priority: P1
Impact: futex waiters can wake on non-matching bitsets and invalid zero masks
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-wait-futex-20260710`.

Evidence:

- Futex syscall dispatch routes bitset operations through the shared futex path: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:638`.
- Wait handling treats bitset waits like plain waits: `dd-jit-darwin/src/runtime/os/linux/thread.c:760`.
- Wake handling treats bitset wakes like plain wakes: `dd-jit-darwin/src/runtime/os/linux/thread.c:816`.

Why this is bad:

Linux rejects `val3 == 0` for bitset futex operations and wakes only waiters whose masks overlap. dd ignores masks and wakes a waiter even when the wake bitset does not match.

Observed proof:

```text
qemu: waiter r=-1 errno=110 timedout=1 woke=0; zero_wait_einval=1 zero_wake_einval=1 mismatch_wake=0
dd:   waiter r=0 errno=0 timedout=0 woke=1; zero_wait_einval=0 zero_wake_einval=0 mismatch_wake=1
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

## `tcgetpgrp` `tcsetpgrp` Fake Success On Non-TTY FDs

Priority: P1
Impact: terminal control probes accept regular fds as controlling terminals
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-pgrp-signal-20260710`.

Evidence:

- `TIOCGPGRP` falls back to `getpgrp()` and `TIOCSPGRP` ignores failures: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:751`.

Why this is bad:

Linux returns `ENOTTY` when terminal process-group ioctls target a non-tty fd. dd reports success, causing terminal detection and foreground process-group setup to believe a regular file is a usable tty.

Observed proof:

```text
Linux/qemu: tty_pgrp get_enotty=1 set_enotty=1
dd:         tty_pgrp get_enotty=0 set_enotty=0 raw_get=12413 raw_set=0
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

## aarch64 `mincore` Accepts PROT_NONE Vec

Priority: P2
Impact: syscall output can write through an inaccessible guest buffer
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-jit-perm-fault-20260710`.

Evidence:

- aarch64 `mincore` fast path passes the guest `vec` pointer to host `mincore`: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:745`, `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:747`.
- The path bypasses guest `PROT_NONE` range validation: `dd-jit-darwin/src/runtime/os/linux/syscall/mem.c:779`, `dd-jit-darwin/src/runtime/os/linux/thread.c:334`.

Why this is bad:

Linux returns `EFAULT` when `vec` points to an inaccessible page. dd/aarch64 reports success because host memory is still writable, bypassing guest protection bookkeeping and corrupting fault compatibility.

Observed proof:

```text
Linux/aarch64: mincore_vec_protnone rc=-1 errno=14
dd/aarch64:    mincore_vec_protnone rc=0 errno=0
```

## `unlinkat` Ignores Unknown Flags And Deletes The File

Priority: P1
Impact: invalid unlink calls can delete data instead of failing
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-runtime-fs-syscalls-20260710`.

Evidence:

- `unlinkat` flag handling masks only `AT_REMOVEDIR` and does not reject unknown bits: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:900`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1027`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1054`.

Why this is bad:

Linux returns `EINVAL` and preserves the target when `unlinkat` receives unknown flags. dd returns success and deletes the file, turning a bad feature probe or corrupted flag value into data loss.

Observed proof:

```text
Linux/qemu: bad_errno=22 after_bad_exists=1 cleanup_errno=0
dd:         bad_errno=0 after_bad_exists=0 cleanup_errno=2
```

## `fallocate` Accepts Invalid Modes And Mutates Data

Priority: P1
Impact: invalid allocation mode combinations can shrink, grow, or zero files
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-runtime-fs-syscalls-20260710`.

Evidence:

- `fallocate` validates only unknown bits, then dispatches by the first matching mode bit: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1480`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1535`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1565`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1600`.

Why this is bad:

Linux rejects combinations such as `COLLAPSE_RANGE|KEEP_SIZE`, `INSERT_RANGE|KEEP_SIZE`, and `ZERO_RANGE|COLLAPSE_RANGE`. dd performs the first matching operation and reports success, silently changing file size or contents.

Observed proof:

```text
Linux/qemu: invalid combinations return 95; size/data unchanged
dd:         return 0; collapse shrinks 26 -> 22, insert grows 26 -> 30, zero combo overwrites bytes
```

## `fallocate` Range Overflow Reports Success

Priority: P1
Impact: impossible allocation ranges are reported as successfully reserved
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-runtime-fs-syscalls-20260710`.

Evidence:

- Range end calculations do not check `off + len` overflow: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1536`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1566`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1647`.

Why this is bad:

Linux returns `EFBIG` for overflowing allocation ranges. dd returns success for `off=LLONG_MAX-10,len=100`, so applications can believe disk space or sparse extents were reserved when nothing valid happened.

Observed proof:

```text
Linux/qemu: errno=27
dd:         errno=0
```

## `utimensat` Ignores Unknown Flags And Updates Timestamps

Priority: P2
Impact: invalid timestamp calls mutate file metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-runtime-fs-syscalls-20260710`.

Evidence:

- `utimensat` checks `AT_SYMLINK_NOFOLLOW` but drops other flag bits: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2870`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2907`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2926`.

Why this is bad:

Linux rejects unknown `utimensat` flags with `EINVAL` and leaves timestamps unchanged. dd returns success and updates `mtime`, corrupting metadata on an invalid call.

Observed proof:

```text
Linux/qemu: EINVAL; mtime remains 1000
dd:         success; mtime becomes 2000
```

## `fchown` `fchownat` Fake Success And Corrupt Ownership

Priority: P1
Impact: failed ownership changes can report success and poison reported metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-runtime-fs-syscalls-BR-20260710`.

Evidence:

- `fchown` and `fchownat` ignore host failure in non-rootfs paths and return success: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1762`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1793`.
- Synthetic owner xattr is still written: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1799`.

Why this is bad:

Linux rejects bad fds, missing paths, non-directory dirfds, bad flags, and unprivileged UID changes. dd reports success and can change synthetic owner metadata to an impossible UID, corrupting what later stat calls report.

Observed proof:

```text
Linux/qemu: badfd_ebadf=1 missing_enoent=1 notdir_enotdir=1 badflags_einval=1 poison_eperm=1 uid_unchanged=1 uid_poisoned=0
dd:         badfd_ebadf=0 missing_enoent=0 notdir_enotdir=0 badflags_einval=0 poison_eperm=0 uid_unchanged=0 uid_poisoned=1
```

## `openat2` Ignores ABI Validation And Resolve Restrictions

Priority: P1
Impact: invalid `openat2` calls and forbidden symlink traversal succeed
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-runtime-fs-syscalls-BR-20260710`.

Evidence:

- `openat2` reads `open_how` directly and ignores `size`, extension bytes, mode validation, and `resolve` restrictions: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1807`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1811`.

Why this is bad:

Linux validates the `openat2` ABI and enforces `resolve` flags. dd accepts null/small/oversized structures, invalid mode combinations, invalid resolve bits, and follows symlinks under `RESOLVE_NO_SYMLINKS`.

Observed proof:

```text
Linux/qemu: null_efault=1 small_einval=1 big_e2big=1 mode_einval=1 resolve_einval=1 symlink_eloop=1
dd:         null_efault=0 small_einval=0 big_e2big=0 mode_einval=0 resolve_einval=0 symlink_eloop=0
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
