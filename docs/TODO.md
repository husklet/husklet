# dd — comprehensive TODO (what needs to be fixed / improved)

State: `main` at v0.9.18. Read `docs/AGENTS.md` first (build/run + COMPLETENESS + CROSS-PLATFORM +
zero-tolerance on terminal/internals). Goals: (1) x86-on-par-with-arm; (2) full Docker/Linux compliance
on all 3 platforms; (3) every basic Linux internal flawless & oracle-validated, no stubs. Moby reference:
`.dev/research/moby-mapping.md` (`§N` = section). Shipped work is in git history, not here.

ACTIVE MISSION (perf): beat the #328 benchmark on BOTH arches, safely — no runtime corruption, every
perf change gated behind byte-exact differential + matrix subset. Worst gaps: arm sqlite-select 4.50x /
python 3.05x (call/ret + indirect dispatch -> #341 RAS returns); amd openssl AES-GCM 0.10x / SHA 0.31x
(x86 crypto -> ARM crypto + CPUID path-selection); amd redis 0.52x (x86 codegen/flags/syscall). Levers &
findings tracked in `docs/perf-profile-v0.9.18.md` (profiler output). See §C (x86 perf) / §D (arm perf).

## Recently shipped (v0.9.14 → v0.9.16)
DNS via host resolver (#261) · reach-by-name (#322) · `-p` process-independent forwarder (#320) ·
overlay data-correctness (opaque/rename/copy-up/xattr/whiteout, #349) · isolation `--cpus`/`--read-only`/
`--ulimit`/masked-paths (#350) · x86 crypto→ARM (18.2× AES-GCM) · x86 DIV, redis-crash, PF/AF ·
**terminal fixed** (pty master ioctl + `TERM=xterm`; dpkg/node/apt match Docker byte-exact) + pty
conformance suite · 14 linuxsys syscalls · docker build (classic, `DOCKER_BUILDKIT=0`) · ~50 xfails swept.

## Staged for the NEXT release (fixed, pending batch merge + validation)
- **Signals** #312 SA_RESTART / #313 sigwait / #316 SA_SIGINFO — clean-rebuild validated, no hang.
- **x86 gcc toolchain** #240 — the `set_static_spec` ICE (non-PIE `.data` rebase gated to static-only);
  unblocks all x86 gcc/g++/cc/rust; ~34 toolchain xfails removed. (RISKY loader change — validate the
  full non-PIE service surface, postgres/redis/go, in the batch matrix.)
- **amd apt** #331 — already worked on HEAD (stale xfails flipped): amd apt-get update/install/htop.
- **Moby lifecycle+volumes** — StopSignal/StopTimeout, tmpfs, anonymous volumes (+ image VOLUME seed),
  volume GC, HEALTHCHECK→State.Health, restart policies + durable manual-stop, inspect Mounts/state.
- **FS-op fidelity** #314 xattr / #315 prlimit / #318 O_PATH / #319 mincore + chmodchown/linksym/
  readdir-dtype (real cache-eviction/pagesize/O_PATH fixes) · **Moby §3**: /dev/full→ENOSPC,
  /proc/self/root|cwd, /etc/hostname, /dev/shm.
- **In progress (gating):** exhaustive `/proc /dev /sys` internals conformance suite (no stubs) +
  permissions + `ls -l` render tests, byte/field-exact vs the real-docker oracle on all 3 engines.
- **In progress:** DB scenario driver readiness (mysql/mariadb), mongo re-verify, toolchain fixture-env
  durability, single-arch image-store handling.

---

## A. Docker/Linux compliance (Moby) — REMAINING
REPAIR: §3.3-2/5 /dev/shm genuine per-subdir tmpfs (flat redirect only) + /run fresh tmpfs (needs
overlay opaque, §10-G1 done—wire it) · §10-G4 overlay hardlink index · §10-G5b image-baked
security.capability/SELinux xattrs · §6.3 darwin bind `ro` (engine-C jail) · §11-2 IPAM
(--subnet/--gateway/--ip) · §11-6 multi-network wiring · §11-3 `--network host` real passthrough ·
§7.3 attach/logs backpressure+persist · §7.3-5/6/7 exec exit-126/paused/resize · §8.3-4 full state
(OOMKilled/Dead) · §11-7 inspect Aliases/DNSNames/IPAMConfig · #295/#303 daemon image resolution ·
#276 container-id entropy.
IMPLEMENT: §11-4 network-scoped aliases · §11-5 connect/disconnect live reconcile · §7.3-3/4 detach
keys + attach query-params · §9.3-4/5 caps/no-new-privs/seccomp reflection · §8.3-7 pause/unpause ·
§9.3-6 memory swap/reservation reporting · §6.3-4/6/7 --volumes-from/driver-opts/ref-counting ·
BuildKit gRPC frontend (so `docker build` works without DOCKER_BUILDKIT=0).

## B. Engine correctness gaps
- **#317** readlinkat(dirfd,relpath) wrong · **#238** ptrace stub · **#239** overlay --rm stale/leak ·
  **#224** g_inotify not cleared on close.
- **#228** 0.0.0.0-bind unreachable via 127.0.0.1 on bridge · **#229** AF_UNIX datagram not overlay-routed ·
  **#231** nats scratch-image exec · **#280** pty slave parent-held-fd name · **#227** musl openpty ·
  **#223** pty master poll/EOF HUP.
- **#296** elf.c lazy-fault MAP_FIXED (16KB host page) · **#218/#226** x86 vDSO ptr / FAULT_ON nonpie ·
  **#212** mem.c munmap partial-tail leak.

## C. x86 performance + completeness
- **x86 opcode completeness to zero**: F16C/FMA + residual VEX in do_avx; x87 exotic sub-forms (#248/#249);
  16-bit SHLD/SHRD; MOVNTDQA/MASKMOVDQU; #208 busybox syscall-65573 decode desync.
- **#145** x86 flag residuals incl. shl/shr/sar $1 (D0-form) CF/OF vs qemu · **#344** vDSO clock_gettime ns.
- **x86 beta crash cluster**: #104/#135 guest-JIT (V8/PyPy) · #210/#213 x86 loader/TSO · #263 julia ·
  #250 Go1.25 · #123 node-e · #139 clang · #119 mongosh-SEA · #215 erlang.

## D. arm performance
- **#339** arm64 cross-process translation cache (go build 25×) · **#341** lean hardware-RAS returns ·
  **#337** postgres pgbench-i hang · **#251** aarch64 LDRSW-literal.

## E. Services still not up (arm64)
#300 s6-rc · #281 clickhouse · #284 influxd · #267/#270 BEAM · #187/#188 JVM · #268/#269 cargo/go-cache ·
#291 victoria-metrics · #304 mariadb-initdb · #334/#335 DB scenario-lane (harness, in progress).

## F. Distribution / release
- **#171** daemon reliably uses the CURRENT shipped engine (stale-`/Applications/dd.app` trap — the likely
  cause of "broken shell / empty /dev after install"). HIGH · **#197** clean release==debug · **GA-SOAK**.

## G. Deferred / housekeeping
#93 encoder de-dup · #220 stale-xfail sweep (oracle-artifact markers: x86 process-vm/clone3) · #78
gcc-bundle hello.c · #233 darwinjail cd test · mariadb-x86 hangs on the JIT (surfaced during CPU triage).

---

## H. xfail census (post-sweep — shrinking)
Was 138 markers → v0.9.16 dropped the matrix to 32 xfail; the staged batch flips ~20 more (signals,
toolchain amd, FS-op, DB postgres/redis/valkey). Scan: `grep -rn '\.xfail(' dd-tests/src/`. Remaining
after the batch: darwin kq-signal/bsd-spawn · x86 gcc `--version` (now fixed, verify flip) · mysql/mariadb/
mongo (harness, in progress) · web/httpd (verify) · a few oracle-artifacts to un-mark (#220). Every fix
re-checks XPASS on all applicable engines, then removes the marker. Target: zero real xfail.

## Process
Manager: delegate each gap to a minimal-scope isolated-worktree agent (TDD: failing test first, whole
subsystem, CROSS-PLATFORM). Agents run only their targeted check; manager runs the full matrix in the
BACKGROUND, 3-way-merges by path, batches validated wins into a release, reaps orphaned mac-bridge engines
between batches. Terminal + `/proc /dev /sys` internals + permissions are ZERO-TOLERANCE release gates.
