<!-- WAVE-2 LTP widening — measurement + gap triage. Numbers are REAL, from
     out/bin/{arm64,x86_64} run under dd (mac bridge) vs native/qemu oracle,
     engine at HEAD v0.9.25 (commit 2f255a3), measured 2026-07-03. -->
# dd LTP conformance — WAVE 2 (widened coverage + new gap triage)

This wave **widens** the curated LTP syscall lane (`dd-tests/compliance/ltp/`)
across the categories that were thin in wave 1 — fs-metadata, xattr, timers,
sysv/POSIX-ipc, scheduling, credentials, process-control, memory-locking, and
epoll/eventfd/signalfd/inotify — then runs every widened binary **under dd on
both guest arches** and **under the native/qemu oracle**, and diffs them. It is
a **measurement + ranked-gap pass**, not a fix pass: the ranked DD-GAP list
below (with the exact failing `TFAIL` line + suspected subsystem) is the routing
input for the next fix wave.

Method, scoring, and the differential harness are unchanged from wave 1 — see
`docs/compliance-ltp.md`. `ok` = dd verdict == oracle PASS; `DD-GAP` = oracle
PASS but dd differs (a real bug); `TEARDOWN` = all assertions PASS but dd exits
nonzero (the systemic post-fork teardown crash, wave-1 GAP-1); `skip` = the
oracle itself is non-PASS here (root/fs/knob) so there is no ground truth and it
is excluded from dd's score.

---

## What was added

Appended **256 upstream LTP test lines** to `tests.list` (marked
`WAVE 2 widening`), spanning nine categories. Cross-compiled for both guest
arches with the real toolchains (`gcc`, `x86_64-linux-gnu-gcc`); identical
build result on both arches:

| | added (tests.list) | **built** | won't-build |
|---|---|---|---|
| fileio (stat/statx/chmod/chown/utime/truncate/rename/link/symlink/getcwd/umask/statfs) | 77 | **56** | 21 |
| xattr (set/get/list/remove/f*/lgetxattr) | 20 | **19** | 1 |
| timer (timer_create/settime/gettime/timerfd/setitimer/clock_nanosleep/clock_getres) | 23 | **19** | 4 |
| ipc (msg*/sem*/shm*/mq_*) | 27 | **2** | 25 |
| sched (setscheduler/getparam/setaffinity/getcpu/nice/priority) | 21 | **20** | 1 |
| cred (setuid/setgid/get*id/capget/capset/setfs*id) | 21 | **4** | 17 |
| proc (wait/waitid/waitpid/setpgid/getpgid/setsid/getsid/kill/tgkill/execve) | 25 | **22** | 3 |
| mm (mlock/mlock2/munlock/mlockall/mremap/madvise/mbind) | 20 | **14** | 6 |
| poll (epoll_create/ctl/wait/eventfd/eventfd2/signalfd/inotify) | 22 | **20** | 2 |
| **total** | **256** | **176** | **80** |

### The 80 won't-build tests are LTP-side, NOT dd gaps

Every build failure is a toolchain/helper limitation of the no-autotools lane
(same class documented in wave 1), reproduced identically on both arches:

| reason | n | examples | why |
|---|---|---|---|
| **old 16-bit-uid compat header** (`compat_tst_16.h`/`compat_16.h`) | 21 | setuid01, setgid01, getuid01, chown01, fchown01, getresuid01, getgroups01 | these tests `#include` a generated 16-vs-32-bit-uid compat header that only autotools emits |
| **old libipc helper** (`getipckey`, `libipc.a`) | 22 | msgget01, semget01, shmget02, semctl01, shmat01, kill05 | SysV-IPC tests link LTP's `libipc` (`ipcget.c`) which isn't part of the new-API `libltp.a` |
| **old-API `tst_sig`/sighandler prototype** | 16 | fstatat01, linkat01, setsid01, rename11, semctl01, sched_yield01, mlockall01, mremap03/04 | legacy harness incompatible with glibc-2.34+/gcc-15 (tightened `tst_sig`/sighandler types) |
| **kernel-UAPI header clash** (`struct file_attr` redefinition) | 6 | statx04, statx05, statx08, statx09, setxattr03, utimensat01 | this LTP pin predates the host's `linux/fs.h` `struct file_attr`; the two collide |
| **old-API `parse_opts`** | 5 | renameat201, renameat202, signalfd4_01/02, mremap05 | pull `tst_parse_opts.o`→ old `parse_opts()` (a lib source that doesn't build on gcc-15) |
| **numa helper** (`numa_helper.h`/`NUMA_ERROR_MSG`) | 2 | mbind01, mbind02 | multi-source NUMA helper, out of scope for a single-binary lane |
| **local mqueue helper header** (`mq.h`/`mq_timed.h`) | 3 | mq_notify01, mq_timedsend01, mq_timedreceive01 | per-test companion header not on the include path |
| **timerfd lapi gap** (`TFD_CLOEXEC`/`TIMER_ABSTIME`/`safe_timerfd`) | 5 | timerfd01, timerfd02, timerfd04, timerfd_settime02 | `lapi/timerfd.h` + `tst_safe_timerfd.c` don't build on this glibc |

None require a dd change. A helper-aware builder (link `libipc`, generate the
16-bit-uid compat header, bump the LTP pin past the `file_attr` header clash)
would recover most of these mechanically in a future wave.

---

## Scorecard (widened / new tests only)

Engine at HEAD `v0.9.25` (`2f255a3`). Two rates, as in wave 1:
**syscall-assertion PASS** credits tests whose every `TPASS`/`TFAIL` matches the
oracle (includes the teardown-crash tests); **clean-run** also requires an
identical exit.

| lane | new tests run | oracle-nonpass (excluded) | **scored** | syscall-assertion PASS | clean-run | teardown-only | real gaps |
|------|--------------:|--------------------------:|-----------:|-----------------------:|----------:|--------------:|----------:|
| **dd-arm64** (vs native) | 176 | 94 | **82** | **44/82 = 53.7%** | 33/82 = 40.2% | 11 | 38 |
| **dd-x86_64** (vs qemu) | 176 | 97 | **79** | **46/79 = 58.2%** | 35/79 = 44.3% | 11 | 33 |

Both arches were run to completion (single dd attempt, 8 s timeout). The gap
sets are **near-identical across arches** — these are OS-emulation gaps, not
codegen — with one cluster (C) that is **arm64-only** and one lone x86-only gap
(`epoll_create02`). 94/97 of the excluded tests are `oracle=CONF` (need root, a
specific fs, or a `/proc` knob absent under the non-root oracle); 3 are
`oracle=BROK`.

### Per-category (syscall-PASS / oracle-PASS)

| category | dd-arm64 | dd-x86_64 | notes |
|----------|:--------:|:---------:|-------|
| fileio | 9/16 | 12/15 | *at()/EFAULT + arm-only valid-pointer EFAULT (cluster C) |
| proc   | 12/20 | 12/20 | pid-arg errno family (ESRCH vs EPERM/EINVAL) + teardown |
| poll   | 9/13 | 8/13 | epoll/signalfd invalid-fd/flags not rejected |
| timer  | 5/14 | 5/14 | invalid clockid/fd/timerid not rejected (biggest single subsystem) |
| sched  | 4/10 | 4/9 | invalid pid/policy not rejected; getcpu affinity; nice value |
| mm     | 4/6 | 4/5 | mlock residency/Locked accounting |
| cred   | 1/3 | 1/3 | capget over-reports capabilities |
| **xattr** | **0/0** | **0/0** | **19 built, ALL oracle-CONF** — LTP xattr tests need root (`TCONF: Test needs to be run as root`); unscorable on this non-root host |
| **ipc** | **0/0** | **0/0** | only mq_open/mq_unlink built (2); both oracle-CONF (need `/dev/mqueue`); all SysV-IPC binaries failed to build (libipc) |

> Honest caveat: **xattr and ipc gained coverage but scored zero tests here** —
> not because dd passed or failed, but because the oracle itself can't run them
> as an unprivileged process on this host. Scoring them needs either a
> root-capable lane or the container model (which supplies a private
> `/dev/mqueue` and an xattr-capable upper dir). Flagged for the fix wave.

---

## Ranked NEW DD-GAPs (dd ≠ oracle) — routing list for the fix wave

Grouped by suspected root cause, highest-impact first. Every entry is a real
reproduced dd-vs-oracle divergence on the widened set; each shows the exact
failing `TFAIL`/`TBROK` line. Repro:
`DDJIT_DIR=<out> ddjit-linux_<arch> out/bin/<arch>/<test>` vs the native run.

### NEW-GAP-A — Invalid-argument errno not enforced (accepts bad clockid/fd/pid/signal/flags) — SYSTEMIC, ~24 tests, BOTH arches
The dominant new cluster. dd's handlers accept arguments Linux rejects: they
return success (or the wrong errno) where the kernel returns
`EINVAL`/`ESRCH`/`EBADF`. Split into two sub-patterns:

**A1 — permissive success (no validation):**
- `timer_create03`: `TFAIL: timer_create() succeeded for invalid notification type` (native: `EINVAL`)
- `timer_gettime01`: `TFAIL: timer_gettime(-1) = 0: SUCCESS` (native: `EINVAL`)
- `timerfd_create01`: `TFAIL: timerfd_create(...) invalid retval 3: SUCCESS` (native: `EINVAL` for bad clockid/flags)
- `timerfd_gettime01` / `timerfd_settime01`: `TFAIL: timerfd_gettime()/settime() succeeded unexpectedly` (native: `EBADF` on bad fd)
- `clock_getres01`: `TFAIL: clock_getres(-1, ...) failed: SUCCESS` (native: `EINVAL`)
- `clock_nanosleep01`: `TFAIL: returned 0, expected -1, expected errno: EINVAL: SUCCESS`
- `sched_setscheduler01`: `TFAIL: sched_setscheduler(1, 99, ...) succeeded` (native: `ESRCH`/`EINVAL`)
- `sched_getparam03`: `TFAIL: sched_getparam() with non-existing pid succeeded` (native: `ESRCH`)
- `kill03`: `TFAIL: kill should fail but not, return 0` (native: `EINVAL` for out-of-range signo)
- `tgkill03`: `TFAIL: Invalid tgid should have failed with EINVAL: SUCCESS`
- `setpgid02`: `TFAIL: setpgid(66541, 4194304) succeeded` (native: `EINVAL`/`ESRCH`)
- `waitid02`: `TFAIL: waitid(P_ALL, 0, infop, WNOHANG) expected EINVAL: ECHILD`
- `waitpid04`: `TFAIL: waitpid(...,0xffffffff) expected EINVAL: ECHILD` (bad options)
- `epoll_create1_02`: `TFAIL: epoll_create1(-1) invalid retval 3: SUCCESS` (native: `EINVAL`)
- `epoll_ctl02`: `TFAIL: epoll_ctl(...) if epfd is an invalid fd succeeded` (native: `EBADF`)
- `signalfd02`: `TFAIL: fd is invalid succeeded` (native: `EBADF`)
- `epoll_create02` (x86-only): `TFAIL: epoll_create(size) …` invalid retval
- `getcpu02`: `TFAIL: getcpu(bad_ptr) succeeded` (native: `EFAULT`)

**A2 — wrong errno *family* for a nonexistent pid** (returns `EPERM`/`EINVAL`
where Linux returns `ESRCH`) — strongly suggests a guest-pid→host-pid
translation that maps "no such pid" onto EPERM/EINVAL instead of ESRCH:
- `getpgid02`: `TFAIL: getpgid(-99) expected ESRCH: EPERM`
- `getsid02`: `TFAIL: getsid(unused_pid) expected ESRCH: EPERM`
- `getpriority02`: `TFAIL: getpriority(0, -1) should fail with ESRCH: EINVAL`

- **Suspected subsystems**: the invalid-arg reject paths in
  `os/linux/syscall/{time,sched,signal,process,poll}.c` — timer/timerfd/clock
  handlers (clockid/timerid/fd validation), epoll/signalfd (fd/flag validation),
  and the pid-targeting family (`kill`/`tgkill`/`waitid`/`waitpid`/`setpgid`/
  `getpgid`/`getsid`/`getpriority`/`sched_*`) where the guest-pid lookup must
  yield `ESRCH` for an unmapped pid.

### NEW-GAP-B — Bad-user-pointer EFAULT not enforced — BOTH arches (extends wave-1 GAP-2)
- `capget02`: `TFAIL: capget() with bad address data succeeded` (native: `EFAULT`)
- `statfs02`: `TFAIL: statfs() expected EFAULT: ENOENT` (native: `EFAULT`)
- `fchmodat02`: `TFAIL: fchmodat() with invalid address expected EFAULT: ENOTDIR`
- (`getcpu02` bad-ptr also, listed under A)
- **Suspected subsystem**: syscall arg copy-in / `access_ok` — several handlers
  deref the guest pointer (or fall through to a path lookup) without an EFAULT
  gate.

### NEW-GAP-C — arm64-ONLY: EFAULT / SIGSEGV on a VALID user pointer for path+buffer syscalls
dd-x86_64 passes these; dd-arm64 rejects a *valid* argument. Confirmed
deterministic and arm-specific (x86 dd = PASS):
- `truncate02`: `TFAIL: truncate(testfile, 256) failed: EFAULT` (x86 dd: `TPASS truncate succeeded`)
- `getcwd02`: `TBROK: chdir(/tmp) failed: EFAULT` (x86 dd: PASS)
- `chmod08`: `TBROK: Failed to update the access/modification time … EFAULT` (utimensat on a valid path; x86 dd: PASS)
- `getcwd01`: `TBROK: Test killed by SIGSEGV!` (getcwd with an unvalidated buffer crashes rather than returning EFAULT; qemu oracle also mishandles, so scored on arm only)
- **Suspected subsystem**: the **aarch64** guest syscall argument marshaling
  (copyin/copyinstr / access check) for `truncate(2)`, `chdir(2)`,
  `utimensat(2)`, `getcwd(2)` — the x86_64 path is correct, so the bug is on the
  arm64 arg-translation path specifically.

### NEW-GAP-D — capget over-reports capabilities
- `capget01`: `TFAIL: capget() gets CAP_NET_RAW unexpectedly in pE` — dd hands an
  ordinary (non-root) process an effective cap set it should not have.
- **Suspected subsystem**: `capget` emulation returns a full/root capability
  mask instead of the process's actual (empty) set.

### NEW-GAP-E — /proc/<pid>/ absent for non-self pids
- `getpgid01`: `TBROK: Failed to open FILE '/proc/1/stat' for reading: ENOENT` —
  the test reads `/proc/1/stat` as ground truth; dd only materializes
  `/proc/self`.
- **Suspected subsystem**: `/proc/<pid>/` completeness (at minimum pid 1/init).

### NEW-GAP-F — getcpu ignores affinity; mlock residency accounting
- `getcpu01`: `TFAIL: getcpu() returned wrong value expected cpuid:17, returned value cpuid: 0` — getcpu returns a constant 0, ignoring a prior `sched_setaffinity` pin.
- `mlock05`: `TFAIL: Rss (1114112) != MMAPLEN (1048576)` / `TFAIL: Locked (0) != MMAPLEN` — post-`mlock` `VmRSS`/`Locked` in `/proc/self/smaps` are wrong.
- `munlockall01`: `TBROK: Locked memory after mlockall() should be > 0` — `Locked` stays 0 after `mlockall`.
- **Suspected subsystem**: `getcpu`/affinity wiring, and `/proc/self/smaps`
  `Locked:` + RSS accounting for `mlock`/`mlockall` (qemu oracle also can't
  score these, so they land on arm only).

### NEW-GAP-G — readlinkat with a real dirfd
- `readlinkat01`: `TFAIL: readlinkat(5, , , 1024) failed: ENOTDIR` then `TFAIL: Wrong filename in buffer ''` — dd rejects a valid open directory fd.
- **Suspected subsystem**: `readlinkat` (and likely the `*at()` dirfd resolution
  path) treating a valid dirfd as not-a-directory.

### NEW-GAP-H — nice() resulting-priority off by one
- `nice02`: `TFAIL: Process priority 20, expected 19` — after `nice()`, the
  reported priority is off (clamp/return-value semantics).
- **Suspected subsystem**: `nice`/`setpriority` value+clamp handling.

### Pre-existing systemic (wave-1 GAP-1) still present — post-fork teardown crash (11 tests)
Forking/`SAFE_FORK` tests print every `TPASS` correctly, then the LTP parent
exits 255 instead of `Summary:`+0. On the widened set: `wait02`, `waitid01`,
`waitpid03`, `setpgid01`, `getsid01`, `kill06`, `kill08`, `tgkill01`,
`setitimer01`, `fstatfs02`, `eventfd2_03`. Same single root cause as wave-1
GAP-1 (fork child → parent exit/reap teardown); this one bug is the whole gap
between the 53.7% and 40.2% arm64 rates.

---

## Trivial fixes landed this wave

**None.** Every new DD-GAP is a real handler-level correctness issue (missing
errno/validation path, arg-marshaling, `/proc` surface, or capability model),
not a one-line errno tweak I could land and verify with confidence, so per the
measurement-not-fix scope they are all left as routed gaps above. One config
experiment (`HAVE_SYS_XATTR_H` in `config.h`, to un-stub the xattr family) was
tried and **reverted**: the xattr tests still `TCONF` (need root) so it unlocked
nothing scorable and it regressed two builds (`removexattr01/02`).

## How to re-run

```bash
export DDJIT_DIR="$(ls -dt target*/release/build/ddjit-*/out | head -1)"
bash dd-tests/compliance/ltp/build.sh                 # builds 325 tests/arch (176 wave-2)
LTP_ARCHES="arm64 x86_64" LTP_TIMEOUT=12 bash dd-tests/compliance/ltp/run.sh
# results: dd-tests/compliance/ltp/out/results.tsv (+ scorecard on stdout)
```

Note: `run.sh`'s sequential timeout handling can be starved by the many
blocking-syscall tests in the widened set (timers, clock_nanosleep, epoll_wait,
mq_*) on the shared mac-bridge host; this wave's numbers were produced with a
single-attempt, 8 s-timeout scorer over the same binaries to guarantee
completion. Hardening `run.sh` to run each test fully detached with a hard
mac-side kill (per the wave-1 "Known lane limits") lets the full widened
`176×2` run unattended.
