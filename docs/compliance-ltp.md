<!-- SCORECARD PLACEHOLDER — numbers filled from out/results.tsv by the lane run. -->
# dd Linux-kernel conformance — LTP syscall scorecard

**What this is.** dd is a userspace-kernel dynamic binary translator (x86_64 +
aarch64 Linux guests → macOS arm64, gVisor lineage). The
[Linux Test Project](https://github.com/linux-test-project/ltp) (LTP) syscall
suite is the standard yardstick for exactly this class of runtime — it is what
gVisor uses to score its own syscall surface. This lane builds a curated CORE
subset of real LTP syscall tests and runs each one **under dd on both guest
arches** and **under a native/qemu ground-truth oracle**, then diffs the two.
A test where dd's verdict differs from the oracle's is a **real dd compliance
gap** (a bug); a test the oracle itself cannot pass here (environment/privilege)
is excluded from dd's score, never silently counted.

- Lane: `dd-tests/compliance/ltp/` (`build.sh`, `run.sh`, `tests.list`, `config.h`).
- Re-run: **`make ltp`** (see "How to re-run" below).
- LTP pinned at commit `ae4a01208fa2` (linux-test-project/ltp).

---

## Method (why the numbers are legitimate)

**Real LTP binaries, no autotools.** LTP normally builds via autoconf/automake
plus a generated per-arch syscall table. Neither autotools is available on this
toolchain host, so the lane drives LTP's own pure-shell generators directly
(`generate_syscalls.sh` for `lapi/syscalls.h`; a one-line `ltp-version.h`) and
supplies a hand-built `include/config.h` (vendored as `config.h`) that mirrors
what `./configure` would detect on a modern glibc host. It then cross-compiles
the LTP "new API" harness (`lib/*.c` → `libltp.a`) and each curated test into a
**static-PIE binary per arch** with the real cross toolchains
(`gcc` for aarch64, `x86_64-linux-gnu-gcc` for x86_64). These are genuine LTP
test programs, not re-implementations.

**Differential scoring.** Each binary runs twice:

| lane | aarch64 | x86_64 |
|------|---------|--------|
| **dd** | `ddjit-linux_aarch64` | `ddjit-linux_x86_64` |
| **oracle** | native (this host is arm64) | `qemu-x86_64` (user-mode) |

Each run is classified from the test's own LTP **Summary** block
(`passed`/`failed`/`broken`/`skipped`) plus its exit status, into a verdict:
`PASS` / `FAIL` / `BROK` / `CONF` (config-skipped) / `CRASH` / `TIMEOUT`.
Then:

- oracle `PASS` **and** dd `PASS` → **ok**
- oracle `PASS` **and** dd anything-else → **DD-GAP** (a real dd bug)
- oracle **not** `PASS` (CONF/BROK/CRASH/…) → **skip** — no valid ground truth
  on this host, so it is *excluded from dd's score* and logged, never counted
  against dd.

The headline pass-rate is **ok / (oracle-PASS tests)**, per arch. Every DD-GAP
below was reproduced (dd output vs oracle output shown in the bug entries), not
asserted.

---

## Scorecard

Measured 2026-07-03 against the engine at HEAD (`v0.9.25`, commit `2f255a3`).
Two rates are reported because dd exhibits one systemic *harness-teardown* crash
(see gap #1) that is distinct from per-syscall correctness:

- **syscall-assertion PASS** — every `TPASS`/`TFAIL` assertion the test makes
  matches the native/qemu oracle (credits tests that pass all assertions but
  then hit the teardown crash).
- **clean-run** — additionally exits byte-identically to the oracle (the
  teardown crash counts against this).

| lane | tests run | oracle-nonpass (excluded) | scored | syscall-assertion PASS | clean-run | teardown-only | real syscall gaps |
|------|-----------|---------------------------|--------|------------------------|-----------|---------------|-------------------|
| **dd-arm64** (vs native) | 119 | 24 | 95 | **60/95 = 63.2%** | 47/95 = 49.5% | 13 | 35 |
| **dd-x86_64** (vs qemu, subset) | 30 | 6 | 24 | **17/24 = 70.8%** | 14/24 = 58.3% | 3 | 7 |

`dd-arm64` covers 119 of the 149 curated tests (the run was cut off in the
`s*`–`w*` tail by a hang on a blocking-syscall test — see "Known lane limits").
`dd-x86_64` is a 30-test fast subset (blocking-syscall tests excluded) run to get
real x86 numbers; widening it is mechanical (`LTP_ARCHES=x86_64 make ltp`). The
x86 gap set so far (brk01, dup201, epoll_create1_01, gettimeofday01, mincore03,
munmap03, read02) is a **subset of the arm64 gaps** — i.e. the same bugs surface
on both arches, none are x86-only in this subset.

---

## Wave-3 update (2026-07-03): support-lib build-glue expansion (#414)

The curated list had **80 tests/arch that failed to COMPILE** (missing LTP
support-library glue, not dd bugs). Those are now built and scored. Nothing in
the engine changed; only the lane's `build.sh` / `config.h` / `tests.list`.

**Compile coverage (both arches, identical):**

| | before | after |
|---|---|---|
| tests compiling | 353 / 433 | **430 / 433** |
| fail-to-compile | 80 | **0** (3 excluded, justified below) |

**Build-glue fixes** (all in `dd-tests/compliance/ltp/`):
- `-fpermissive` added to the test CFLAGS — down-grades gcc-14's now-hard
  *incompatible-pointer* permerror (old-API tests pass `void(*)(int)` where
  `tst_sig`/`signal` want `void(*)(void)`). Fixed ~16 fileio/mm/proc/sched tests.
- `-I testcases/kernel/syscalls/utils` — supplies the shared per-family test
  headers that live there, not in `include/`: `compat_16.h` + `compat_tst_16.h`
  (the 16-bit uid/gid chown + cred tests) and `mq.h` + `mq_timed.h` (mqueue).
- `config.h`: `HAVE_STRUCT_FILE_ATTR` (modern UAPI `<linux/fs.h>` now defines
  `struct file_attr` — stop lapi re-declaring it: statx04/05/08/09, setxattr03,
  utimensat01) and `HAVE_SYS_TIMERFD_H` + `HAVE_TIMERFD_{CREATE,GETTIME,SETTIME}`
  (pull `TFD_*` from glibc `<sys/timerfd.h>`: timerfd01/02/04, timerfd_settime02).
- `build.sh` lib build now folds in the SysV-IPC helper
  `libs/newipc/tse_newipc.c` (`getipckey`, `probe_free_addr`, …) so msg*/sem*/
  shm*/kill05 link, and adds a **`-std=gnu89` fallback** (modern flags first,
  gnu89 only on failure) for the K&R-flavoured old-API sources — `parse_opts.c`
  (`usc_test_looping`, `usc_global_setup_hook`), `tst_sig.c`, `random_range.c` —
  and the handful of old-API tests (signalfd4, mremap05, renameat201/202).

**Excluded (3, build-env limits — NOT dd gaps, justified in `tests.list`):**
- `mbind01`, `mbind02` — need `libnuma`/`<numaif.h>` (`numa_helper.c`); the
  cross-build host has no `libnuma-dev`. Re-enable when the devel lib is present.
- `semctl01` — its test table uses unprototyped `void (*func_setup)()` fields but
  calls them with an argument; gcc-14 reads `()` as `(void)` and rejects it, and
  no flag rescues it (gnu89 then breaks the modern `tst_safe_*` headers it pulls).

**Newly-compiling tests scored (77/arch, dd vs native/qemu):**

| lane | ok | DD-GAP | oracle-nonpass (skip) |
|------|----|--------|-----------------------|
| dd-arm64   | 17 | 25 | 35 |
| dd-x86_64  | 20 | 21 | 36 |

`ok` (pass both, added coverage): chown01, fchown01, getresgid01, getresuid01,
getuid01, geteuid01, mremap04, rename14, renameat202, sched_yield01, setgid01,
setsid01, setuid01, shmdt02, signalfd4_01, signalfd4_02, timerfd02, plus
getgroups01/msgget01/semop01 on x86_64.

### New gaps found (grouped by root cause)

- **GAP-W3a — SysV IPC control ops unimplemented (both arches).** `msgctl()` and
  `semctl()` return **ENOSYS(38)** → msgctl01, semget01 (setup), and cascades.
  `msgget01`/`msgrcv01` setup also break. dd does not implement the System V
  msg/sem control surface.
- **GAP-W3b — arm64-only EFAULT on pointer-arg syscalls (x86 PASSes).** On
  aarch64, `getgroups()` (getgroups01), `semop()` (semop01: 4× `TFAIL semop()
  EFAULT`), and `msgsnd()` (msgsnd01) all return **EFAULT(14)** for a *valid*
  user buffer; the same tests pass under the x86_64 engine. Strong signal of an
  arm64 argument-pointer translation/validation bug for these handlers.
- **GAP-W3c — POSIX mqueue.** `mq_notify()` → **ENOSYS** (mq_notify01, both).
  `mq_timedsend`/`mq_timedreceive` return the wrong errno (**EAGAIN** where
  **EINVAL** is expected for a bad `msg_len`/abs-timeout) — mq_timedsend01,
  mq_timedreceive01, both arches.
- **GAP-W3d — timerfd absolute-time / settime hangs.** `timerfd_settime02`
  **TIMES OUT** (both); `timerfd01` completes 1 of 12 assertions then hangs at
  the `TFD_TIMER_ABSTIME` re-arm. timerfd02 (flags only) passes — the abs-time
  settime path is the gap.
- **GAP-W3e — mremap MREMAP_FIXED.** `mremap03` **HANGS on arm64 / CRASHes
  (exit 255) on x86_64**. `mremap05` (both): `MREMAP_FIXED` returns a live
  address where the kernel rejects the request (`MREMAP_FIXED` w/o `MAYMOVE`,
  unaligned new_addr, overlap all wrongly succeed).
- **GAP-W3f — *at() family errno/flag handling.** `fstatat01`: `fstatat()`
  *succeeds* where the oracle expects a failure (missing error path).
  `linkat01`: returns **EOPNOTSUPP(95)** where **ENOTDIR/EXDEV** expected.
  `symlinkat01`: **EOPNOTSUPP** where success expected. `renameat201`: **ENOENT**
  where **EINVAL** expected for a bad `renameat2` flag.
- **GAP-W3g — /proc SysV-IPC surface missing.** `/proc/sysvipc/shm` (shmget03)
  and `/proc/sys/kernel/sem` (semget05) return **ENOENT** — the tests BROK/CONF.
- **GAP-W3h — bare-binary uid mapping (low priority / environment).** cred tests
  run as a bare static binary see the *host* uid **501** while
  `/proc/self/status` reports **0**, and `getpwuid()` has no matching passwd
  entry → getegid01/02, geteuid02, getgid03, getuid03 TFAIL/TBROK. This is the
  no-container-userns bare-run harness, not a syscall bug; would resolve under a
  real container rootfs. Noted, not ranked with the syscall gaps.

---

## Per-category breakdown (dd-arm64: syscall-PASS / oracle-PASS)

| category | score | notes |
|----------|-------|-------|
| fileio   | 29/38 | strong; gaps in dup2/fcntl-flags, link/lstat setup |
| proc     | 17/21 | strong; fork/getpid pass assertions (teardown-crash) |
| mm       | 9/17  | mmap/munmap/mincore/madvise semantics + EFAULT |
| poll     | 2/6   | poll/select/pselect/epoll readiness diverge |
| misc     | 2/4   | getrusage/sched_getaffinity |
| time     | 1/4   | gettimeofday/nanosleep EFAULT + value checks |
| net      | 0/3   | bind/connect/sendto |
| signal   | 0/2   | pause() wake |

(`skip` = the oracle itself returns TCONF/BROK on this host — 24 arm64 tests,
e.g. tests needing root, specific fs, or `/proc` knobs — never counted against dd.)

---

## Ranked dd≠oracle failures (compliance gaps → bugs to hunt)

Grouped by suspected root cause, most-impactful first. Every entry was
reproduced dd-vs-oracle. Re-run any one with:
`DDJIT_DIR=<out> <engine> out/bin/arm64/<test>` vs the native run of the same binary.

### GAP-1 — Post-fork harness teardown crashes (exit 255) — SYSTEMIC, 13+ tests
- **Symptom**: tests using `.forks_child`/`SAFE_FORK` print *every* `TPASS`
  correctly, then the LTP parent process crashes with exit 255 instead of
  printing `Summary:` and exiting 0. Non-forking tests (e.g. `read01`) exit
  cleanly. Correlates exactly with forking.
- **Repro**: `getpid01` → dd prints 100×`TPASS` then `rc=255`; native prints
  `passed 100` + `rc=0`. Also fork01/03/07/08/10, clone01, mmap01/03, fstat03,
  getpid02, getppid02, exit_group01.
- **Suspected subsystem**: fork/clone child→parent exit path — waitpid/SIGCHLD
  reaping or `exit_group` teardown in the parent engine after a forked child
  exits (cf. memory note "#285 fork child inherited locked engine mutexes").
  This one bug alone is the difference between the 63% and 49% arm64 rates.

### GAP-2 — Bad-userpointer EFAULT not enforced (time/mm/misc)
- **Symptom**: syscalls given an invalid user address return success instead of
  `-EFAULT`; the test that asserts EFAULT then fails.
- **Repro**: `gettimeofday01` → native: 3×`TPASS` (all EFAULT); dd: one
  `TFAIL: … succeeded`. Also `nanosleep01`, `getrusage02`, `mincore03`,
  likely `munmap03`.
- **Suspected subsystem**: syscall arg copy-in/access_ok — several handlers deref
  the guest pointer without validating it (no EFAULT path).

### GAP-3 — poll/select/pselect/epoll readiness model
- **Symptom**: readiness/timeout results diverge from the oracle.
- **Repro**: `select01` (dd exit 33 vs native PASS), `poll02` (FAIL),
  `pselect01` (FAIL), `epoll_create1_01` (FAIL). `select02` **hangs** (dd never
  returns → 15s timeout) — a blocking-select wake bug.
- **Suspected subsystem**: the kqueue-backed poll/select/epoll emulation.

### GAP-4 — mmap/munmap/mincore/madvise memory semantics
- **Symptom**: mm edge cases diverge or crash.
- **Repro**: `mmap08`/`mmap12` (FAIL), `munmap01` (truncated, exit 255),
  `munmap03` (FAIL), `mincore02` (BROK), `mincore04` (crash 255),
  `madvise10` (crash 255).
- **Suspected subsystem**: mmap flag/offset validation + mincore residency +
  madvise advice handling.

### GAP-5 — pause() / signal wake
- **Symptom**: `pause01`/`pause02` BROK (exit 255) — pause() not woken/returned
  as native does.
- **Suspected subsystem**: signal delivery interrupting a blocked `pause`.

### GAP-6 — dup2 / fcntl flag semantics
- **Repro**: `dup03`, `dup201` (dup2 corner: same-fd / close-on-exec), `fcntl05`,
  `fcntl13` (F_GETFL/F_SETFL or F_GETLK flags) — all `TFAIL` vs native PASS.
- **Suspected subsystem**: dup2/dup3 flag handling; fcntl F_*FL/F_*LK.

### GAP-7 — link/lstat setup (BROK, exit 2/6)
- **Repro**: `link02`, `link05`, `lstat01`, `lstat02` — dd BROK during setup
  (hardlink/symlink creation or stat of the created node in the tmpdir).
- **Suspected subsystem**: link(2)/symlink+lstat metadata in the overlay/tmpfs.

### GAP-8 — net: bind/connect/sendto
- **Repro**: `bind01` (BROK 2), `connect01` (crash 255), `sendto02` (dd CONF vs
  native PASS — dd reports the path unsupported). Reachable in the bare (no
  container-net) model here; deeper net is out of scope for this lane.
- **Suspected subsystem**: AF_INET/AF_UNIX bind/connect/sendto emulation.

### Also failing (lower-frequency, likely their own small gaps)
`prctl02`/`prctl03` (prctl subfunction), `getrlimit02` (BROK), `nanosleep02`
(crash 255), `sched_getaffinity01` (FAIL), `read02` (FAIL). See
`out/results.tsv` for the full row-by-row table.

---

## Curated CORE subset — coverage & what was skipped

The curated list (`tests.list`, 149 tests/arch) spans the surface dd emulates:
file I/O (open/read/write/pread/pwrite/lseek/close/dup/dup2/dup3/pipe2/stat/
fstat/statx/lstat/access/faccessat/fcntl/readlink/rename/link/symlink/unlink/
mkdir/getdents), memory (mmap/munmap/mprotect/madvise/brk/mremap/msync/mincore),
process (fork/clone/wait4/waitpid/getpid/getppid/exit_group/prctl/getrlimit/
setrlimit), signals (signal/sigsuspend/sigpending/rt_sigqueueinfo/pause), time
(clock_gettime/nanosleep/gettimeofday/times), readiness (poll/select/pselect/
epoll_create1), and basic net/misc (socket/bind/connect/sendto/uname/getrusage/
sched_getaffinity/getdomainname).

**Deliberately excluded (logged, not silently dropped):**

- **Legacy-API-only tests** whose only LTP variant uses the old `tst_sig()` /
  `usc_*` harness that no longer builds against glibc-2.34+/gcc-15 (non-constant
  `SIGSTKSZ`, tightened `tst_sig` prototype): `rt_sigaction*`, `rt_sigprocmask*`,
  `sigaltstack*`, plus the old-API `mprotect01-03`, `msync01-03`, `mremap01-02`,
  `mincore01`, `setrlimit01`, `openat03`, `symlink03`, `pwrite01`, `recvfrom01`.
  Where a new-API variant of the same syscall exists it was substituted
  (`mprotect05`, `msync04`, `mremap06/07`, `mincore02-04`, `setrlimit02/03`);
  signal-delivery coverage is preserved via the new-API `signal/sigsuspend/
  sigpending/rt_sigqueueinfo/pause` tests, which drive the same `rt_sig*` kernel
  surface.
- **Multi-binary test groups** needing a runtime helper executable
  (`sigwait/sigwaitinfo/sigtimedwait/rt_sigtimedwait`, `execve*`, `*_child`
  companions) — out of scope for a single-binary lane.
- **Surface dd does not target**: kernel modules, KVM, hugepages, cgroup-v1 raw,
  device-node, quota, sysctl-heavy, and privilege/namespace-fixture tests.

**A full-LTP expansion** would add the rest of `testcases/kernel/syscalls`
(~1300 tests: the remaining fcntl/ioctl/xattr/mount/namespace/AIO/keyctl/
timerfd/inotify/epoll families and every numbered variant), the `mm/` stress
tests, and — via a helper-aware builder — the multi-binary groups above. The
CORE subset here is the high-signal core; expansion is mechanical once the lane
exists.

---

## Known lane limits (honest caveats)

- **Blocking-syscall tests hang the runner.** A test that blocks forever under
  dd (e.g. `select02`, `pause`, some sig-wait tests) is not killed cleanly:
  `timeout` reaps the local `mac` bridge client but the mac-side engine lingers,
  so the sequential runner stalls. This truncated the full arm64 run at 119/149
  and is why the x86 lane here is a blocking-free subset. **Fix for the lane**:
  run each test detached with a hard mac-side kill on timeout (or run under the
  container model with pids-max), then the full 149×2 completes unattended.
  These hangs are themselves a dd signal (GAP-3/GAP-5).
- **Retries** (3× on no-output results) absorb the transient orphan-engine
  saturation that fork-heavy neighbours cause on the shared mac host; a
  deterministic dd failure still reproduces on every attempt.
- Numbers are for the CORE subset. A **full-LTP** expansion (~1300 syscall tests)
  would broaden every family and is mechanical once the hang-handling above lands.

## How to re-run the lane

```bash
# from the repo root, with a built engine (make jit) — pins DDJIT_DIR itself:
make ltp

# or directly, pinning your engine out-dir and narrowing:
export DDJIT_DIR="$(ls -dt target*/release/build/ddjit-*/out | head -1)"
bash dd-tests/compliance/ltp/build.sh              # fetch(pinned)+build both arches
LTP_ARCHES="arm64" LTP_TIMEOUT=20 bash dd-tests/compliance/ltp/run.sh
# results: dd-tests/compliance/ltp/out/results.tsv  (+ scorecard on stdout)
```

Requirements: the cross toolchains (`gcc`, `x86_64-linux-gnu-gcc`),
`qemu-x86_64` for the x86 oracle, and — on a Linux dev host — the `mac` bridge
to run the macOS engine (mirrors `dd-jit::SpawnConfig::command`). The pinned LTP
checkout is fetched once into `out/ltp-src` and reused.
