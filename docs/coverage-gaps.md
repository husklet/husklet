# dd engine — prioritized coverage-gap census

_Roadmap to "tests fully pass on every machine" (zero xfails on all 3 test-matrix engines)._
_HEAD v0.9.12 · generated read-only (census only, no engine changes) · owner: manager to merge._

The **3 platforms** are the 3 test-matrix engines:

| id | engine | how it runs the guest |
| --- | --- | --- |
| **x86** | `linux/x86_64` (`Engine::LinuxX86_64` / `Target::AmdLinux`) | x86-64→ARM64 DBT (`translate/x86_64`) |
| **arm** | `linux/aarch64` (`Engine::LinuxAarch64` / `Target::ArmLinux`) | aarch64→ARM64 DBT (`translate/aarch64`) |
| **mac** | `darwin/aarch64` (`MAC`) | native macOS arm64 guest under darwinjail |

Three gap categories were gathered: **(1)** unimplemented syscalls (via `make coverage` static
lens + source cross-check), **(2)** translate/opcode gaps (`report_unimpl` / abort markers), **(3)**
every `.xfail()` in `dd-tests` (a test not passing = a gap), mapped to root cause + tracked task#.

---

## ⚠️ Coverage-tool correction (read first)

`dd-tests/tools/coverage.sh static` reports **88 canonical / 89 x86 "unimplemented" syscalls**. That
count is **inflated by 17 false positives** — two systematic blind spots in the tool's extractor:

1. **`aio.c` is not in `HANDLER_MODULES`.** The tool scans `sysv mem signal time io fs proc net event
   misc rare` but not `aio.c`, so `io_setup/io_destroy/io_submit/io_cancel/io_getevents` (**canonical
   0–4**) are flagged as gaps while they are in fact **implemented** (synchronous libaio emulation —
   unblocks nginx:alpine + innodb file-AIO; test `lsys/aio-pread` passes).
2. **`net.c` has two top-level `switch(nr)` blocks; the tool reads only the first (netlink).** The
   entire BSD socket family lives in the *second* switch and is fully implemented but flagged as gaps:
   **socket 198, socketpair 199, listen 201, accept 202, connect 203, getpeername 205, setsockopt 208,
   getsockopt 209, shutdown 210, accept4 242, recvmmsg 243, sendmmsg 269**. (Networking demonstrably
   works — databases, `nc`, web servers.)

**True canonical syscall gap count: 71** (88 − 17), all long-tail/niche. *Recommendation:* add `aio` to
`HANDLER_MODULES` and teach `handled_one` to union **all** `switch(nr)` blocks per module, so the tool
stops crying wolf. The 71 real gaps below are unaffected by this correction.

---

## Headline — gaps per platform × tier

| platform | P0 (blocks real sw) | P1 (fidelity) | P2 (rare/test-only) | xfail sites |
| --- | --- | --- | --- | --- |
| **x86** (`linux/x86_64`) | 4 clusters (byte-SBB opcode, div-mistranslate, exec-loader, x86 codegen cluster) | ~6 | long tail | ~55 |
| **arm** (`linux/aarch64`) | 3 clusters (fork/jemalloc DB bring-up, exec-loader, docker-daemon) | ~10 | long tail | ~70 |
| **mac** (`darwin/aarch64`) | 0 | 2 (kqueue-signal, spawn-arch) | — | 2 |
| **shared / both-linux** | exec-loader-noent · mongo CPU-topology | signal/fs fidelity (#312–#319) | 71 niche syscalls | — |

Total real `.xfail()` call-sites: **134** (dominant: toolchains 31, databases 29, linuxsys 14, weird 8,
web 6). Zero-xfail is reached by clearing the **P0 clusters** (each retires many xfails at once), then
the enumerated P1 engine-fidelity list, then optionally the P2 long tail.

---

# P0 — blocks real software / correctness-critical

These are what actually stop showcase workloads or produce wrong output. Ordered by blast radius.

## P0 syscalls
None outstanding. The historically-fatal ones (io_setup for nginx/innodb, the socket family, netlink
`#289`) are already implemented — see the tool correction above. The 71 remaining syscall gaps are all
P1/P2 (no current guest reaches them; `coverage.sh dynamic` shows no `ACTIONABLE` hit).

## P0 opcodes / translate

| gap | plat | blocks (real workload) | effort | task# |
| --- | --- | --- | --- | --- |
| **byte-form ADC/SBB `0x1C`/`0x14` (+ group1 `/2`,`/3` byte)** — deferred to `report_unimpl` → guest SIGILL/silent 255 | **x86** | `python:3.12-slim`, `node:20-slim` silent exit 255 (`GAPS jit86-opcode-1c`); python C-ext (zlib/hashlib/ctypes) | **S** | jit86-opcode-1c |
| **`divq %r14` with divisor mistranslated to 0 → spurious SIGFPE** during gpgv RSA verify | **x86** | `apt-get update` on debian/ubuntu (amd64) | **M** | (queued, distros) |
| **x86 mincore under-reports residency** (only first touched page marked present) | **x86** | correctness — `memx/mincore` diverges | **S** | #319 |
| **x86-guest codegen cluster** (rustc/gcc/julia/x87/Go1.25/clang/node-e/MOV-Sreg/…) | **x86** | rustc `#210`, gcc `#240`, julia `#263`, x87 `#248`, Go1.25 `#249`, clang `#139`, node `-e` `#123`, mongosh SEA `#119`, PyPy `#135`, MOV-Sreg `#183`, syscall-65573 `#208`, ELF-loader `#213` | **L** (deferred beta) | #210/#240/#248/#249/#183/#208/#213/#139/#123/#119/#135 |
| **arm-guest mistranslations** (residual) | **arm** | clickhouse `#281`, influxd Go codegen `#284`, LDRSW-literal `#251` | **M–L** | #281/#284/#251 |

## P0 loader / process (the single biggest xfail cluster)

| gap | plat | blocks | effort | task# |
| --- | --- | --- | --- | --- |
| **`exec-loader-noent`** — execve of a freshly-produced/entry binary fails "open: No such file or directory"; non-PIE `ET_EXEC` static loader + fork+exec path | **x86+arm** | `rustc` (×3), `go build`/`go version` (×4), `httpd`/apache (×6), gcc/cc1 + go/rustc under toolchains (much of the 31), dotnet build, ghc (as/ld fork), `hello-world` static image, nats scratch-exec `#231` | **L** | exec-loader-noent (rel. #213, #231) |
| **fork-per-connection / jemalloc bring-up gap (arm)** | **arm** | `postgres`/`redis`/`mysql`/`mariadb`/`valkey` full server bring-up (databases ×29 xfail) | **M–L** | databases fork-exec (rel. #285) |
| **mongo tcmalloc CPU-topology — `NumPossibleCPUs` empty set → abort** | **x86+arm** | `mongod` (databases + weird) | **M** | mongo-cpu-topology |

## P0 docker-daemon (scenario xfails on the daemon path, mostly arm)

| gap | plat | blocks | effort | task# |
| --- | --- | --- | --- | --- |
| **`run -p` host-port publish** (+ `ps` Ports / `docker port`) | arm | runflags (×1) + observe ports/port (×2) | **M** | **#320** |
| **cross-container reach by name on a user network** (embedded DNS + routing) | arm | dockernet `nc srv` by name (×1) | **M** | **#322** |
| **`docker build`** (no `/build` endpoint) | arm | buildcmd (×2) | **L** | **#321** |
| **`run --network none` not honored** (eth0 still present) | arm | runflags (×1) | **S** | **#325** |
| **`docker restart` leaves container not-Running** | arm | lifecycle (×1) | **S** | **#323** |
| **`docker cp` host→ctr single-file-to-new-path** | arm | cpcmd (×1) | **S** | **#324** |

---

# P1 — common but non-blocking / graceful-degradation (fidelity)

Currently a sane ENOSYS/no-op/near-miss is tolerated; implementing removes the xfail and improves
fidelity. Almost all are in-process `dd-tests/cases` (native-Linux oracle catches the divergence).

## P1 syscalls / behaviour (Linux both arches unless noted) — the `#312–#319` audit set

| gap | plat | what it blocks | effort | task# |
| --- | --- | --- | --- | --- |
| **SA_RESTART not honored** — interrupted `read` returns EINTR instead of restarting | x86+arm | servers rely on it; `signalx/sarestart` | **M** | **#312** |
| **sigwait/sigwaitinfo — no delivery** | x86+arm | `signalx/sigwait` | **M** | **#313** |
| **SA_SIGINFO `si_pid`=0** (sender pid not filled) | x86+arm | `signalx/siginfo` | **S** | **#316** |
| **xattr set/remove is a no-op** (`*xattrat` 463–466 also unimpl) | x86+arm | `fsx` xattr cases | **M** | **#314** |
| **O_PATH fd treated as readable** (should EBADF) | x86+arm | `fsx/opath` | **S** | **#318** |
| **readlinkat(dirfd) wrong** | x86+arm | fs relative-symlink | **S** | **#317** |
| **prlimit set not reflected** (get still shows old soft limit) | x86+arm | `processx/prlimit` | **S** | **#315** |
| **fchmod (fd-based) is a no-op** | x86+arm | `posix/chmodchown` | **S** | posix-fchmod |
| **stale st_nlink after unlink of a hardlink** | x86+arm | `posix/linksym` | **S** | posix-nlink |
| **rewinddir does not reset getdents enumeration** | x86+arm | `posix/readdir-dtype` | **S** | posix-rewinddir |
| **O_TMPFILE unnamed file** unsupported | x86+arm | `cases/otmpfile` | **M** | (cases) |
| **statfs returns hardcoded (not real fs) geometry** | x86+arm | `cases/statfs` | **S** | (cases) |
| **/proc/self/fd resolution** incomplete | x86+arm | `cases/procfd` | **S** | (cases) |
| **mprotect fault semantics** diverge | x86+arm | `cases/mprotect` | **M** | (cases) |
| **synthetic auxv vector wrong (arm)** | arm | `completeness/auxval` | **S** | (completeness) |
| **spinlock (x86)** divergence | x86 | `threads/spinlock` | **M** | ext-spinlock-x86 |

## P1 linuxsys emulation gaps (Linux-only syscalls emulated on macOS host)

| gap | plat | test | effort | task# |
| --- | --- | --- | --- | --- |
| timerfd_gettime reports 0 remaining for armed one-shot | x86+arm | `lsys/timerfd-gettime` | S | lsys-timerfd-gettime |
| signalfd realtime-queue: only first siginfo carries right ssi_signo | x86+arm | `lsys/signalfd-rt` | M | lsys-signalfd-rt |
| inotify rename events (IN_MOVED_FROM/TO) never generated | x86+arm | `lsys/inotify-moves` | M | lsys-inotify-moves |
| memfd F_SEAL_WRITE not enforced | x86+arm | `lsys/memfd-seal` | M | lsys-memfd-seal |
| PR_GET_NO_NEW_PRIVS / DUMPABLE / PDEATHSIG not tracked | x86+arm | `lsys/prctl-*` | S | lsys-prctl-* |
| pidfd_send_signal doesn't deliver (arm) | arm | `lsys/pidfd-signal` | M | lsys-pidfd |
| io_uring_setup fails (arm) | arm | `lsys/io-uring` | M | lsys-io-uring (#274/#271 done x86) |
| fanotify_init errno != EPERM (arm) | arm | `lsys/fanotify` | S | lsys-fanotify |

## P1 mac (darwin/aarch64)

| gap | plat | test | effort | task# |
| --- | --- | --- | --- | --- |
| **kqueue EVFILT_SIGNAL never fires** (other 4 filters work) | mac | `darwin/kq-signal` | M | darwin-kqueue-signal |
| **posix_spawn of a system binary fails** — darwinjail.dylib inherited into arm64e child → dyld arch-abort | mac | `darwin/bsd-spawn` | M | darwin-spawn-jail-arch |

## P1 — toolchain version banners (amd64)

`make`/`ld`/`as`/`gcc --version` under x86 emit nothing — same jit86 cluster as the div/opcode bugs
(toolchains, `Target::AmdLinux`). Effort S once the x86 codegen cluster settles.

---

# P2 — rare / exotic / test-only

## The 71 unimplemented canonical syscalls (return −ENOSYS; no guest currently reaches them)

Grouped as `coverage.sh report` clusters. Each is unimplemented *on purpose* until real software needs
it; `coverage.sh dynamic` flags none as `ACTIONABLE`. A handful have a dedicated xfail probe (noted).

| cluster | syscalls (canonical #) | note |
| --- | --- | --- |
| **async-io / io_uring data path** | io_pgetevents(292); io_uring_enter/register denied by profile | (io_setup 0–4 **are** implemented) |
| **new mount API** | open_tree(428) move_mount(429) fsopen(430) fsconfig(431) fsmount(432) fspick(433) mount_setattr(442) open_tree_attr(467) | container never remounts |
| **landlock + LSM** | landlock_create_ruleset(444) add_rule(445) restrict_self(446) lsm_get/set_self_attr(459/460) lsm_list_modules(461) | |
| **futex2** | futex_waitv(449) futex_wake(454) futex_wait(455) futex_requeue(456) | glibc still uses classic futex |
| **numa** | (mbind/get/set_mempolicy/migrate_pages/move_pages handled elsewhere; set_mempolicy_home_node niche) | |
| **keyring** | add_key(217) request_key(218) keyctl(219) | |
| **admin / privileged (blocked in a container anyway)** | init_module(105) delete_module(106) finit_module(273) kexec_load(104) kexec_file_load(294) reboot(142) swapon/off(224/225) settimeofday(170) acct(89) personality(92)* quotactl(60)/quotactl_fd(443) nfsservctl(42) vhangup(58) | *personality has an xfail probe `lsys/personality` |
| **tracing / perf / misc** | perf_event_open(241) fanotify_init/mark(262/263) kcmp(272) lookup_dcookie(18) mq_notify(184) ioprio_set/get(30/31) vmsplice(75)* pkey_mprotect/alloc/free(288/289/290) execveat(281) get_robust_list(100) restart_syscall(128) open_by_handle_at(265) remap_file_pages(234) | *vmsplice + tee have xfail probes `lsys/vmsplice`,`lsys/tee` |
| **modern niche** | pidfd_getfd(438) process_madvise(440) process_mrelease(448) epoll_pwait2(441) cachestat(451) mseal(462) map_shadow_stack(453) statmount(457) listmount(458) listns(470) rseq_slice_yield(471) setxattrat/getxattrat/listxattrat/removexattrat(463–466) file_getattr/file_setattr(468/469) | `*xattrat` relate to #314 |

Effort per syscall: mostly **S** (thin shim/ENOSYS-to-emulation), a few **M** (mount-API, futex2,
io_uring data path). None block current showcase targets.

## P2 opcode/translate long tail (x86)
- RCL/RCR **by CL** shift form → `report_unimpl` (immediate/by-1 forms implemented).
- Residual `report_unimpl` arms in `translate.c` (0F/1B map) + AVX/EVEX `avx_unimpl` and SSE3b
  `unimpl` — reached only by exotic vector forms; clean abort (status 70), never silent-wrong.

## P2 oracle-artifact xfails — **NOT engine gaps** (do not "fix")
These xfail because the **qemu-user oracle** lacks the syscall while dd is *correct*; they XPASS only if
the oracle changes. Leave marked; they are not roadmap items.
- `completeness/clone3` (x86), `processx/clone3` (x86) — dd does clone3 correctly; qemu ENOSYS.
- `completeness/process-vm` (x86), `linuxsys/process-vm` (x86) — dd reads correctly; qemu lacks it.

---

# Recommended fix order → zero-xfail on all 3

Ordered by (xfails retired ÷ effort). Each P0 cluster clears many xfails at once.

1. **x86 byte-form ADC/SBB `0x1C`** (`jit86-opcode-1c`, **S**) — unblocks python-3.12/node-20 on amd64
   + all python C-ext weird cases. Highest ratio.
2. **x86 `divq` divisor mistranslation** (**M**) — unblocks `apt-get update` on debian+ubuntu amd64.
3. **`exec-loader-noent`** (**L**, both arches) — the single biggest cluster: rustc, go, httpd, gcc/cc1,
   dotnet, ghc, hello-world, nats. One fix retires ~30+ toolchain/language/web/utilities xfails.
4. **DB fork/jemalloc bring-up (arm)** (**M–L**) — retires the 29 databases xfails.
5. **mongo CPU-topology** (**M**) — mongod on both arches.
6. **docker-daemon G2 set** #320 (-p) → #322 (DNS) → #325 (network none) → #323 (restart) → #324 (cp)
   → #321 (build). Mostly S–M each; clears observe/runflags/dockernet/lifecycle/cpcmd/buildcmd.
7. **Signal/fs fidelity audit #312–#319** (mostly S–M) — clears signalx/fsx/posix/processx/memx cases.
8. **linuxsys emulation gaps** (S–M each) — timerfd/signalfd/inotify-moves/memfd-seal/prctl/pidfd/
   io-uring(arm)/fanotify(arm).
9. **mac**: kqueue-signal + spawn-jail-arch (M each) — the only 2 darwin xfails.
10. **x86-guest codegen cluster** (**L**, deferred to beta per TODO §Beta) — rustc/gcc/julia/x87/Go1.25/
    clang/… This is the long pole for a *fully* green x86 lane; arm64 reaches zero-xfail well before it.
11. **P2 syscall long tail** — implement on demand only when `coverage.sh dynamic` flags one ACTIONABLE.

**Bottom line:** arm64 is a finite, mostly 1-file-each list (steps 3–9) away from zero-xfail; x86 needs
the same plus its codegen cluster (step 10). `mac` is effectively there (2 gaps). Fixing the coverage
tool's two blind spots (aio module + multi-switch union) is a free accuracy win worth doing first.
