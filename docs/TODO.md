# dd — GA readiness

**GA target:** "download the DMG → reliably run your Linux containers/images on macOS."
**Scope decision:** **arm64-first GA** (native Apple-Silicon path). `x86_64`-guest ships **beta**
(functional, but the codegen/guest-JIT cluster is post-1.0). Flip to "hold GA until x86 is
production-grade" only on explicit call.

**State:** `main` at v0.9.11 + N commits. Basics matrix **1265 / 0** both arches. Perf harness live
(`make perf`); genuine self-timed bench in progress (#326). Docker-command conformance **60/69**.

---

## ✅ Shipped this arc (v0.9.8 → v0.9.11+)
BUG201 (16KB-host-page MADV_DONTNEED) · #286 host-page-safe mmap-FIXED/munmap · #292 async-signal
preemption · #297 pcache 2nd-run-SIGSEGV · #285/#305 go build + npm/ruby (fork mutex + phantom
thread-registry) · #301/#302 exec-from-volume + venv symlink · #273 loopback isolation · #306/#308
pty winsize (htop/node `-it`) · #289/#293/#294/#277 netlink + busybox ip/ifconfig + minio · #287 image
$PATH · #282/#283/#257/#264/#258 postgres16-17 / nginx / redis-setpriv / mysql-shebang / image-config ·
#290/#298/#299 s6-overlay chain · #274/#271 connect-errno / io_uring · #259/#272/#265/#266 htop-procfs
/ /dev / .NET · #309 perf table · #311 +49 compat cases · #310 +69 docker scenarios.
**docker run serves out-of-box (arm64):** postgres 15/16/17, redis/valkey, mysql, mariadb, mongo,
nginx/caddy/httpd, nats, etcd, prometheus/vault/grafana, traefik. go build / npm / ruby / python-venv work.

**Genuine perf (self-timed, startup excluded):** arm64-guest ≈ native (ALU 1.00×, FP 1.15×); x86→ARM
≈ 1.5–3× native compute (competitive with / better than QEMU — 5.8× faster on FP).

---

## GA gates (must-fix, arm64)

### G1 — Distribution / release
- [x] Developer-ID **signing** — each release is already signed. ✅
- [ ] **GA-DIST**: confirm **notarize + staple** on the signed DMG so Gatekeeper is clean on other
      Macs (notarization was flaky via orbstack clock skew + `xcrun` PATH — verify it's now done, or
      confirm signed-only is acceptable for the distribution channel). Downgraded from blocker → verify.
- [ ] **#197** verify clean release engine == debug (no release-only regressions).
- [ ] **#171** release engine placement plumbing (daemon reliably uses the shipped engine).

### G2 — Docker daemon conformance (gaps from #310)
- [ ] **#320** `run -p` host port publishing (+ `ps` Ports / `docker port`) — VERIFY then fix. HIGH.
- [ ] **#321** `docker build` (no `/build` endpoint) — feature.
- [ ] **#322** cross-container reach by name on a user network (embedded DNS + routing).
- [ ] **#323** `docker restart` leaves container not-Running.
- [ ] **#324** `docker cp` host→ctr single-file-to-new-path.
- [ ] **#325** `run --network none` not honored.
- [ ] **#295 / #303** stale image-store config on pull / cross-repo basename collision (nginx→linuxserver).
- [ ] **#276** container-ID entropy + default hostname cosmetics.

### G3 — Showcase services that still don't come up (arm64)
- [ ] **#300** s6-rc-init `/run/s6-rc/servicedirs` (unblocks linuxserver/* images).
- [ ] **#281** clickhouse (arm64 JIT mistranslation) · **#284** influxd SIGSEGV (Go codegen).
- [ ] **#267 / #270** Erlang/BEAM (rabbitmq/elixir) arm64 SIGBUS + erl_child_setup.
- [ ] **#187 / #188** JVM arm hang / x86 `/proc/cpuinfo` CPUID flags.
- [ ] **#268 / #269** cargo build of real crates / go build-cache unlinkat.
- [ ] **#291** victoria-metrics — retest post-#286 (likely fixed) · **#304** mariadb initdb intermittent under load.

### G4 — Engine correctness gaps (from #311 + audits)
- [ ] **#312** SA_RESTART not honored (interrupted read → EINTR) — servers rely on it.
- [ ] **#313** sigwait/sigwaitinfo no delivery · **#314** xattr set/remove is a no-op.
- [ ] **#315** prlimit set not reflected · **#316** SA_SIGINFO si_pid=0 · **#317** readlinkat(dirfd) wrong · **#318** O_PATH readable · **#319** x86 mincore under-reports.
- [ ] **#296** elf.c lazy-fault MAP_FIXED (16KB-host-page class) · **#212** munmap partial-tail tracking leak.
- [ ] **#218** vDSO time ptr unchecked (x86) · **#226** FAULT_ON skips nonpie_fixup · g_inotify not cleared on close.

### G5 — Isolation / security
- [ ] **#238** ptrace is a stub (strace/gdb/ltrace broken).
- [ ] **#239** overlay rm-r stale-positive + per-container upper leak across `--rm`.
- [ ] **#228** 0.0.0.0 bind unreachable via 127.0.0.1 on bridge · **#229** AF_UNIX datagram (/dev/log) not overlay-routed.

### G6 — Interactive / terminal
- [ ] **#223** pty master poll/EOF (script hangs) · **#227** musl openpty /dev/pts · **#280** pty devpts naming/ttyname.
- [ ] **#307** apt `archives/partial` chmod ENOENT (cosmetic).

### G7 — Stability / soak (release confidence)
- [ ] **GA-SOAK**: N-container × multi-hour endurance run — RSS/fd/deadlock must stay flat; leak- & hang-free.
- [ ] **#326** genuine `make bench` (in progress) → track perf over time.

### G8 — Networking
- [ ] **#261** apt IPv4-only (defaults to IPv6, stalls) · **#231** nats scratch-image exec.

---

## Beta / post-1.0 (NOT arm64-GA blocking)
- **x86_64-guest cluster:** #210 rustc, #240 gcc, #263 julia, #248 x87, #249 Go1.25, #183 MOV-Sreg,
  #208 syscall-65573, #213 ELF-loader, #139 clang, #123 node-e, #119 mongosh-SEA, #135 PyPy.
- **Guest-JIT (hard):** #113 V8 TurboFan / .NET RyuJIT, #104 TurboFan large-array.
- **aarch64 residual:** #251 LDRSW-literal.
- **Darwin-container lane:** #233 darwinjail cd coverage, #234 no-mount DD_VOLUMES leak.
- **Housekeeping:** #220 stale xfail-marker sweep, #93 refactor (asm.c extract / encoder de-dup), #78 test-lane hello.c.

---

## Distance to GA (read)
Functionally **close** on arm64 — container / dev-loop / service surface is largely there. The real
distance is **G1 (notarized DMG)** + **G2 docker-conformance (esp. -p publish, build, cross-container
DNS)** + **G7 soak**. G3–G6 are a finite, enumerated list (mostly 1-file engine or daemon changes).
x86_64-guest is deferred to beta so GA stays a focused push.

## Process
dev-day = discovery agent (verify CORRECT output, not just no-crash; pin engine, repeat, note load).
Manager delegates each bug to a disjoint isolated-build-dir/worktree agent, 3-way-merges by explicit
path (worktree bases often predate HEAD), validates fresh-engine matrix (1265/0), commits, tags at
batch boundaries. Push authorized. Bench/perf: `make perf` (oracle-vs-jit) + `make bench` (self-timed).
