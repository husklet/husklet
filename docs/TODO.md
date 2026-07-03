# dd — comprehensive TODO (what needs to be fixed / improved)

State: `main` at v0.9.15. Read `docs/AGENTS.md` first (build/run + the COMPLETENESS rule + the
CROSS-PLATFORM rule: every fix must be correct + tested on linux/x86_64, linux/aarch64, AND
darwin/aarch64). This file is the durable open-work map; shipped work lives in git history.

Two goals drive everything:
1. **x86-on-par-with-arm** — the x86 JIT must be as fast (and complete) as the arm frontend.
2. **Full Docker/Linux compliance on all 3 platforms** — driven by the Moby/vpnkit reference map in
   `.dev/research/moby-mapping.md` (gitignored; section refs `§N` below point into it for exact behavior).

---

## A. Docker/Linux compliance (Moby roadmap)

### Shipped (v0.9.13→v0.9.15)
DNS via host resolver + `/etc/resolv.conf` (§1) · reach-by-name / live name resolution (§11-1) ·
`-p` daemon-owned process-independent forwarder + host-IP (§2) · overlay opaque-dir-on-pull (§10-G1) ·
overlay dir-rename copy-up (§10-G2) · copy-up preserves mode/suid/mtime/xattr + real xattr passthrough
(§10-G3/5) · whiteout recreate + RENAME_EXCHANGE (§10-G6/7) · `--cpus` core-count cap (§9.3-1) ·
`--ulimit` (§9.3-3) · `--read-only` rootfs → EROFS (§9.3-2) · masked/readonly `/proc` paths (§3.3-3) ·
pty `/dev/pts/0` naming (tty/ttyname/reopen).

### REPAIR — exists but wrong (remaining)
- **P1 §3.3-2/5 /dev nodes**: `/dev/full` aliases /dev/null (no ENOSPC); `/dev/shm` flattened to /tmp
  files, not a shared tmpfs; `/dev/ptmx` a placeholder → back them properly.
- **P1 §3.3-6 /etc/hostname** never written though the daemon stores it (`runtime.rs:139`) → write it
  beside `/etc/hosts` (relates #276).
- **P1 §8.3-3 lifecycle stop** hardcoded SIGTERM→SIGKILL/10s; ignores image `Config.StopSignal` +
  per-container StopTimeout (nginx SIGQUIT, postgres SIGINT).
- **P1 §10-G4 overlay hardlinks**: two lower hardlinks become independent upper files on copy-up → a
  persistent `(dev,ino)→upper` index. **P1 §10-G5b** raw image-baked `security.capability`/SELinux
  xattrs need daemon `tar --xattrs` extraction.
- **P2 §6.3 volumes parse/GC**: bare `-v /path` anon-vol dropped; rm/prune ignore `c.mounts`; darwin
  drops `ro`; inspect `Mounts[]` omits `--mount`/Name/Driver.
- **P2 §11-2 IPAM**: hardcoded 172.x; drops `--subnet/--gateway/--ip` → honor them, allocate across the
  real prefix.
- **P2 §11-6 multi-network wiring**: engine gets only the first `(netid,ip)`; pass all pairs.
- **P2 §11-3 `--network host`**: doesn't share host stack / IP identity; can't reach user peers.
- **P2 §7.3 attach/logs**: `Lagged` silently drops chunks; logs in-memory only (lost on restart).
- **P2 §7.3-5/6/7 exec**: failed exec exits 127 (should be **126**); exec-into-paused allowed;
  ConsoleSize unparsed + resize race.
- **P2 §8.3-4 lifecycle state machine**: no Restarting/Dead/RemovalInProgress/Paused precedence;
  OOMKilled never set.
- **P2 §11-7 inspect shapes**: missing Aliases/DNSNames/IPAMConfig/IPv6/Links/SandboxID/network
  Options/Labels/Internal.
- **#295/#303 daemon image resolution**: pull doesn't refresh config on an already-present tag; nginx:*
  resolves to a local cross-repo basename without pulling.
- **#276 cosmetics**: container ID is a 16-hex value tiled 4×; default hostname hardcoded `jit`.

### IMPLEMENT — no code path yet (remaining)
- **P1 §6.3-1 tmpfs mounts** (`--tmpfs`, `--mount type=tmpfs`).
- **P1 §6.3-2/5 anonymous volumes** from image `VOLUME` + `populateVolumes` seed-from-image.
- **P1 §8.3-1 HEALTHCHECK** engine (interval/timeout/retries/start-period → `State.Health`).
- **P1 §8.3-2/5 restart policies** `on-failure[:N]/always/unless-stopped` + backoff + durable manual-stop.
- **P1 §3.3-2/7 /dev/mqueue** backing + `/run` fresh tmpfs.
- **#321 `docker build`** (no `/build` endpoint).
- **P2 §6.3-4/6/7 `--volumes-from`**, driver-opts (nfs/cifs) real mount, ref-counting.
- **P2 §11-4 network-scoped aliases** (`--network-alias`) · **§11-5 connect/disconnect live reconcile**.
- **P2 §7.3-3/4 detach keys** (Ctrl-P Ctrl-Q) + attach stream/logs query-params.
- **P2 §9.3-4/5 caps** wiring (CapAdd/Drop/priv → CapEff/Bnd) + no-new-privileges + seccomp reflection.
- **P2 §3.3-4 /proc/self/{root,cwd}** magic symlinks.
- **P2 §8.3-7 pause/unpause** (freezer-equivalent; guard exec-into-paused).
- **P2 §9.3-6 memory** swap/reservation/high + `memory.events` reporting (JVM/systemd).

---

## B. Engine correctness gaps (Linux syscall/ABI)
- **#312** SA_RESTART not honored (interrupted read→EINTR) · **#313** sigwait/sigwaitinfo no delivery ·
  **#316** SA_SIGINFO si_pid=0 — the signal cluster (retry carefully; prior attempt hung sigwait).
- **#315** prlimit(2) SET not reflected on re-read · **#317** readlinkat(dirfd,relpath) wrong ·
  **#318** O_PATH fd wrongly readable · **#319** x86 mincore under-reports · **#314** verify setxattr
  now persists (overlay xattr work may have closed it).
- **#238** ptrace is a stub (strace/gdb/ltrace) · **#239** overlay --rm stale-positive + per-container
  upper leak · **#224** g_inotify not cleared on close.
- **#228** 0.0.0.0-bind unreachable via 127.0.0.1 on bridge · **#229** AF_UNIX datagram (/dev/log) not
  overlay-routed · **#231** nats scratch-image exec fails.
- **#223** pty master poll/EOF (script hangs) · **#227** musl openpty /dev/pts/N · **#280** pty slave
  host-path leak in /proc/self/fd (ctty case fixed; parent-held slave fd remains).
- **#296** elf.c lazy-fault MAP_FIXED (16KB-host-page) · **#218/#226** x86 vDSO ptr / FAULT_ON nonpie.

---

## C. x86 performance + completeness (the mission)
- **x86 TRANSLATION COMPLETENESS**: close the remaining opcode gaps to zero (base map:
  docs/coverage-gaps.md + the census in the x-completeness worktree). Largest remaining: **F16C/FMA +
  residual VEX** in `do_avx`; **x87 exotic sub-forms** (#248/#249); **16-bit SHLD/SHRD**;
  **MOVNTDQA/MASKMOVDQU**; **#208** busybox syscall-65573 decode desync.
- **#145** x86 flag residuals — incl. the confirmed `shl/shr/sar $1` (D0-form) CF/OF divergence vs qemu.
- **#344** x86 vDSO clock_gettime nanosecond value wrong (`date +%s%N`).
- **x86 codegen crashes/miscompiles (beta cluster)**: #104/#135 guest-JIT (V8/PyPy), #210/#213 x86
  loader/TSO, #240 gcc BSS-at-16KB-offset, #263 julia, #250 Go1.25, #123 node-e, #139 clang, #119
  mongosh-SEA, #215 erlang boot.

## D. arm performance
- **#339** arm64 has NO cross-process translation cache → port the fork-server/pcache (go build 25×;
  helps npm/pip/shell).
- **#341** lean hardware-RAS returns (non-tail ret is an IBTC probe; call/ret 5.4×, in-mem sqlite 1.84×).
- **#337** postgres pgbench -i hangs at CREATE INDEX/VACUUM · **#251** aarch64 LDRSW-literal.

---

## E. Real-software / services still not up (arm64 GA showcase)
- **#300** s6-rc-init `/run/s6-rc/servicedirs` (linuxserver/* images) · **#281** clickhouse (arm64 JIT) ·
  **#284** influxd SIGSEGV (Go) · **#267/#270** Erlang/BEAM (rabbitmq/elixir) · **#187/#188** JVM ·
  **#268/#269** cargo build / go build-cache · **#291** victoria-metrics · **#304** mariadb initdb ·
  **#337** postgres pgbench.

## F. Distribution / release (GA gates)
- **#171** daemon reliably uses the CURRENT shipped engine (the stale-`/Applications/dd.app` trap — the
  likely cause of user-reported "broken shell / empty /dev" after install). HIGH.
- **#197** verify clean release engine == debug (no release-only regressions).
- **GA-SOAK**: N-container × multi-hour endurance (RSS/fd/deadlock flat, leak/hang-free).

## G. Deferred / housekeeping
- **#93** encoder de-dup refactor · **#220** stale-xfail sweep · **#78** gcc-bundle hello.c ·
  **#233** darwinjail cd test coverage · **#332/#333/#334/#335** x86 recheck-divq / exec-loader-noent /
  DB test-lane / mongo-tcmalloc xfail triage.

---

## Process (short)
Manager coordinates: delegate each gap to a minimal-scope isolated-worktree agent (TDD: write the
failing test first, then repair the whole subsystem — see AGENTS.md COMPLETENESS + CROSS-PLATFORM).
Agents run only their targeted check; the manager runs the full matrix in the BACKGROUND, 3-way-merges
by path, batches validated wins into a release. Correct-behavior reference: `.dev/research/moby-mapping.md`.
