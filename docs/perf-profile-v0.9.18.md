# dd perf profile — canonical engine v0.9.18

Read-only profiling pass. Goal: rank the biggest performance levers (impact × safety) so fix
agents can be targeted. Numbers are the OrbStack-relative ratios from `docs/benchmarks-workloads.csv`
(dd/orb; for the throughput rows higher-is-better so **ratio < 1.0 = dd slower**; for the ms rows
lower-is-better so **ratio > 1.0 = dd slower**).

| workload | arm dd/orb | amd dd/orb | nature |
|---|---|---|---|
| redis SET | 0.85 | 0.52 | syscall-bound (epoll/read/write) |
| redis GET | 0.90 | 0.61 | syscall-bound |
| sqlite insert | 2.45 | 2.77 | syscall + btree + VDBE dispatch |
| sqlite select | 4.50 (worst arm) | 3.96 | syscall (pread/fcntl) + VDBE dispatch |
| openssl AES-GCM | 0.93 | 0.099 (worst amd) | SSE crypto + shuffle glue |
| openssl SHA-256 | 0.97 | 0.31 | SSE crypto + shuffle glue |
| python loop | 3.05 | 2.08 | indirect dispatch + call/ret |
| tar/gzip | 1.70 | 2.07 | startup-dominated (ignore) |

## Method & caveats

- Pinned canonical engine `DDJIT_DIR=/Users/x/dd/dd/target/release/build/ddjit-dfb5094c39030e80/out`
  (ddjit-linux_x86_64 449 KB, ddjit-linux_aarch64 364 KB). Did **not** rebuild.
- Ran the self-timed microbench (`dd-tests/src/bin/bench.rs`, startup-excluded, BENCH_N=1 — single
  sample, directional not precise) to decompose DBT overhead by instruction class. This is the
  host-side "where does the time go" signal in lieu of a live `sample` of the daemon workload (the
  host had other agents' builds active per the standing caveat, and daemon-driven openssl needs an
  image pull that the host firewall can block). The class decomposition below maps cleanly onto the
  8 workloads, and every lever is additionally confirmed by reading the emitted code path.
- Static confirmation done by reading `dd-jit/src/runtime/translate/{x86_64,aarch64}/`.

### Microbench decomposition (×native-arm64; startup excluded)

| kernel | dd-arm64 | dd-x86 | reads as |
|---|---|---|---|
| simd | 1.05× | **3.14×** | SSE→NEON lowering + SSSE3/SSE4 block-exits |
| call | 1.55× | 1.99× | ret/indirect IBTC probe (both arches pay) |
| fp | 1.05× | 1.96× | x86 scalar-FP + loop flag/branch overhead |
| branch | 1.12× | 1.85× | jcc flag materialization |
| alu | 1.00× | 1.68× | x86 flag synthesis |
| mem | 0.70× | 0.95× | (cache-bound; no lever) |
| syscall | 0.27× | 0.27× | time syscalls served inline (not redis-representative) |

Takeaways: **arm** is near-native on compute (alu/fp/branch/simd 1.0–1.1×) and only pays on
`call` (1.55×) — so the arm workload gaps (sqlite 2.4–4.5×, python 3.05×) are **not compute**, they
are **syscall round-trips + indirect-dispatch**, not physics. **x86** additionally pays a flat
1.7–3.1× on ALU/flags/SIMD from cross-ISA translation, which is why every amd row trails its arm row.

## Where the time goes, per workload

- **openssl AES-GCM / SHA-256 (amd 0.099 / 0.31):** AES-NI / SHA-NI / PCLMULQDQ are now lowered
  inline to ARM crypto (#342, `translate/x86_64/translate/crypto.c`) — that is why amd AES-GCM already
  jumped 10.7× vs #328. The **remaining** killer is the SSSE3/SSE4 *glue* around the crypto: `pshufb`
  (byte-swap of the CTR counter and GHASH input), `palignr`, `pmovzx/sx`, `pblend`. These legacy
  (non-VEX) forms are **not** lowered — they hit `translate.c:945` `if (I.map3)` and exit the block
  to the C softmulator `do_sse3b` (`avx.c:1259` pshufb, `:828` palignr, `:1354` pmovsx…) **once per
  16-byte block**. Every exit = spill 16 GPR + 16 vector regs + nzcv (`emit.c:454` `emit_spill`),
  a C call, re-decode, scalar emulation, reload — shattering the otherwise-chainable crypto loop.
- **python loop (arm 3.05 / amd 2.08):** CPython's eval loop is one giant computed-goto indirect
  dispatch + very heavy call/ret (refcounting, PyObject calls). Maps onto the `call` kernel (1.55×
  arm / 1.99× amd) but amplified because *everything* is indirect. Each guest `ret`/indirect goes
  through the inline 2-way IBTC probe (`emit.c:718` `emit_ibranch`): ~10–14 data-dependent host
  insns + an unpredictable `BR`. amd adds the per-instruction flag-synthesis tax on top.
- **sqlite select/insert (arm 4.5/2.45, amd 3.96/2.77):** syscall-bound — each point-select does
  fcntl locks + pread btree pages + lseek. The fcntl-lock fix already took arm 320→115 ms. Remaining
  cost is the **per-syscall block-exit round-trip** (full GPR+vector spill/fill around every
  `R_SYSCALL`, `emit.c:461` `emit_exit_const`→`emit_spill`) plus VDBE indirect dispatch. This is why
  sqlite-select is the worst arm row despite arm being near-native on compute.
- **redis (arm 0.85–0.90, amd 0.52–0.61):** epoll/read/write bound. Same syscall round-trip; the
  inline fast-syscall set (`emit.c:473`, W4F) only covers clock_gettime/gettimeofday/rt_sigprocmask/
  sched_yield, not the redis hot path. amd additionally pays flag-synthesis on the request-parsing
  loop.

Note: the **§B shadow-return-stack** idea (#341: predict `ret` via a hardware-RAS-style shadow
stack) is a **measured dead end** — `translate/aarch64/translate.c:514-525` documents it as
NET-NEGATIVE on every return-heavy workload (sqlite, qsort, deep recursion) because the ~19-insn push
+ ~22-insn validate cost more than the IBTC; it ships **disabled** (`g_shadowgate = -2`, both arches
return through the IBTC). So the ret lever is IBTC *quality*, not adding a RAS.

## Ranked levers (impact × safety)

| # | lever | workloads | arm/amd | ceiling (est.) | location | risk | first step |
|---|---|---|---|---|---|---|---|
| 1 | Lower the SSSE3/SSE4 **shuffle family inline** (pshufb→TBL, palignr→EXT, pmovzx/sx, pblend) instead of `do_sse3b` block-exit | openssl AES-GCM+SHA, all SSE code | amd (huge), arm (small) | AES-GCM 0.10→~0.45, SHA 0.31→~0.6; simd 3.14→~1.8× | `translate.c:945-954`; extend `translate/x86_64/translate/crypto.c` (add a `translate_ssse3`); ops live in `avx.c:1259/828/1354` | moderate | pshufb→`TBL Vd.16B,{Vn},Vm` (+ x86's 0x80-bit "zero" mask semantics), palignr→`EXT`, pmovzx/sx→`UXTL/SXTL`; add byte-exact differential tests vs oracle (same recipe as #342) |
| 2 | **Tighten crypto emission** — AESENC is 5 insns (EOR-zero+VMOV+AESE+AESMC+EOR-key) vs native 2; hoist a persistent zero vector, drop the per-insn VMOV; streamline PCLMULQDQ half-staging | openssl AES-GCM | amd | +20–40% on AES-GCM atop #1 | `crypto.c:86-195` (AESENC block `:92-106`, PCLMUL `:171-190`) | moderate | keep one loop-invariant zero reg live across the block; restructure AESENC to reuse dst in place; re-diff vs oracle |
| 3 | **Slim the syscall block-exit** — omit the v0..v15 vector spill (8×`stp_q`) on R_SYSCALL exits when the block is SIMD-clean; reload lazily | redis, sqlite (both arches) | arm + amd | redis/sqlite ~10–20% per-syscall | `emit.c:454` `emit_spill`, `:461` `emit_exit_const` | moderate | track SIMD-dirty per block; emit a GPR+nzcv-only spill for R_SYSCALL, keep full spill for signal/AVX/sigreturn exits (they read xmm) |
| 4 | **IBTC quality for polymorphic sites** — CPython/VDBE have one mega-switch indirect site with 100+ targets that thrashes even the 2-way `g_xibtc`; measure per-site hit rate (x86 lacks the arm `IBPROF`) and add a per-site inline-cache or larger set-assoc | python, sqlite VDBE | arm + amd | python 3.05→~2.0× arm / 2.08→~1.5× amd | `emit.c:718-776` `emit_ibranch`; `engine_glue.c:29-47` (`XIBTC_SETS`, 2-way) | moderate | port the arm `IBPROF` counter to the x86 probe, dump hit-rate on python/sqlite, then trial 4-way or a per-site monomorphic guess (bake last-target `cmp;b.eq body`) before the hash probe |
| 5 | **Extend inline fast-syscall set** for redis — the epoll/read/write hot path always takes the full spill→service round-trip | redis | arm + amd | redis 0.52/0.85 → +10% | `emit.c:473-520` (fastsys ladder), `os/linux/syscall/helpers.c` epoll bookkeeping | moderate | inline the eventfd/epoll fast cases and trivial getpid-class calls already in service; streamline epoll_wait ready-list copy |
| 6 | **x86 flag-synthesis broadening** — extend dead-flag elimination across block boundaries (today intra-block only, `translate.c:961-995`) and audit remaining eager materializes | everything on amd (python, sqlite, redis) | amd only | broad amd 5–15% | `translate.c` flag emitters, `translate/x86_64/translate/trace.c` (lazy model, tier-2 fold) | **risky** | this is the correctness core (FL_SUB/ADD/LOGIC + PF/AF #346 + tier-2 fold already mature); only proceed with exhaustive oracle diff + the existing NOLAZY/NOFLAGELIDE kill-switches for A/B |

### Ranking rationale
- **#1 is the single highest-value lever**: it directly attacks the worst gap on the board
  (AES-GCM amd 0.099) and the second-worst (SHA amd 0.31), it is the same proven pattern as the
  already-shipped #342 crypto inline, and it self-validates byte-exactly against the oracle. Do this
  first.
- **#2** compounds #1 on AES-GCM specifically; small, contained.
- **#3** is the best *arm* lever (arm is compute-clean, so its sqlite/redis gaps are pure
  syscall round-trip) and also helps amd; medium blast radius, needs a signal-path carve-out.
- **#4** is the python/sqlite-VDBE lever for both arches; needs measurement first (add x86 IBPROF)
  because the fix (assoc vs inline-cache) depends on the observed hit-rate.
- **#5** is a smaller redis-specific win.
- **#6** is high-maturity / high-risk / modest-ceiling — do last, guarded by kill-switches.

## Knobs available for the fix agents (env-gated, from the source)

- Trace/prof: `JT` (per-block trace), `JTS` (syscall trace), `PROF` (dispatch/IBTC-fill/tier-2/SMC
  counters), `COLDPROF` (cold-start timing), `IBPROF` (arm-only indirect-branch traffic — **port to
  x86 for lever #4**), `DDEPOLLPROF` (epoll), `DD_FAULTCOUNT`.
- A/B kill-switches to isolate a lever: `IBTC1WAY` (1-way IBTC), `NOSSEOPT`, `NOEAOPT`, `NOSTITCH`
  (trace/superblock), `NOTIER2X`, `NOFLAGELIDE`, `NOLAZY`, `NOREPCMP`, `NOSMC`, `NOGUESTFOLD`,
  `DDJIT_NOFASTSYS`, `DDJIT_NOSIGINLINE`.

## Recommended confirmation before/after a fix
- `BENCH_N=5 BENCH_K=simd,call,fp make bench` for the compute levers (#1,#2,#4,#6).
- A live `sample <engine-pid>` (or `xctrace`) on the mac side during a daemon openssl AES-GCM run to
  confirm `do_sse3b`/`emit_spill` dominance before #1, and during a python run to confirm
  `emit_ibranch` dominance before #4 — do this on a quiet host (no concurrent agent builds).
- Re-run the `docs/benchmarks-workloads.csv` rows through the daemon for the real ratio delta.
