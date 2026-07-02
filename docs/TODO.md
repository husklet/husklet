# dd — open work

State: `main` at v0.9.13. Matrix 1269/0 both arches. Read `docs/AGENTS.md` first (build/run/workflow +
the completeness rule). This file lists only what is OPEN. Shipped work lives in git history, not here.

## Mission: x86 on par with arm (fast, complete, correct)
arm64 guests run ~native. x86_64 must reach parity via smart translation, and be COMPLETE (every
opcode/syscall, not hot-patches). Landed so far: DIV r/m64 24.8x->3.5x (#327), AES-GCM crypto 18.2x
(#342), PF/AF dead-flag elim (#346), redis x86 codegen crash fixed (#343). Perf tables: docs/benchmarks*.csv.

### x86 perf + completeness
- x86 TRANSLATION COMPLETENESS SWEEP — enumerate + implement every remaining opcode/prefix/SIMD/syscall
  gap to zero (base map: docs/coverage-gaps.md). Not one-offs; whole families with differential tests.
- #344 x86 vDSO clock_gettime nanosecond value wrong (`date +%s%N`).
- #341 lean hardware-RAS returns (call/ret 5.4x) — broad lever, both arches.
- Residual x86 correctness cluster (beta): #104/#135 guest-JIT, #210/#213 x86 loader/TSO, #240 gcc BSS,
  #263 julia, #249/#250 x87/Go1.25, #183/#190/#208 opcodes, #123/#139 node-e/clang, #145/#120 flags.

### arm perf
- #339 no cross-process translation cache on arm64 (go build 25x) — port fork-server/pcache.
- #337 postgres pgbench -i hangs at CREATE INDEX/VACUUM.

## Active user-reported (this session)
- #347 mac container (darwinjail): `cd ..`/`ls` on a parent-mounted folder shows nothing; exit is slow.
- #348 procfs COMPLETENESS: full `/proc` + `/proc/sys` surface (triggered by missing boot_id).
- #261 apt on VPN host: Ign/failed-to-fetch — likely IPv6-over-VPN or MTU; needs the user's step-2/3
  output to fix dd-side (don't advertise unroutable v6 / inherit egress MTU).

## GA gates (arm64-first)
- Release: #197 clean release==debug, #171 daemon uses the shipped engine reliably.
- Docker daemon: #320 `-p` publish (forwarder must be process-independent), #321 `docker build`,
  #295/#303 stale-image-store/cross-repo pull, #276 container-id entropy.
- Services still down: #300 s6-rc, #281 clickhouse, #284 influxd, #267/#270 BEAM, #187/#188 JVM,
  #268/#269 cargo/go-build-cache, #291 victoria-metrics, #304 mariadb-initdb.
- Engine correctness: #312-#319 (SA_RESTART/sigwait/xattr/prlimit/SA_SIGINFO/readlinkat/O_PATH/mincore),
  #296 elf lazy-fault MAP_FIXED, #212 munmap tail leak, #218 vDSO ptr, #226 FAULT_ON nonpie, #224 inotify.
- Isolation/net: #238 ptrace stub, #239 overlay --rm leak, #228/#229 bridge bind / AF_UNIX dgram,
  #231 nats scratch exec, #261 apt IPv6/MTU.
- Terminal: #223 pty poll/EOF, #227 musl openpty, #280 pty devpts naming.
- Soak: N-container multi-hour endurance (RSS/fd/deadlock flat).

## Deferred / housekeeping
- #93 encoder de-dup refactor, #220 stale-xfail sweep, #78 gcc-bundle hello.c, #233/#234 darwin lane,
  #251 aarch64 LDRSW-literal, #328/#329 bench+census upkeep.

## Process (short)
Manager coordinates: delegate each gap to a minimal-scope isolated-worktree agent (agent runs only its
targeted check), run the full matrix in the BACKGROUND, 3-way-merge by path, batch validated wins into a
release. Enforce completeness. Details: docs/AGENTS.md §8.
