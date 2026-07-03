# dd perf — CPython overhead breakdown & CPython-specific levers (canonical engine v0.9.23)

Read-only profiling pass on the pinned canonical engine (NOT rebuilt):
`DDJIT_DIR=/Users/x/dd/dd/target/release/build/ddjit-dfb5094c39030e80/out`.
Goal: (1) re-establish the current CPython overhead split at instruction granularity, updating the
#375 numbers (which predate IRQSLIM/stealfast/dual-map), and (2) scout **non-block-transition,
CPython-specific** levers so the two fix agents (arm block-transition, x86 instruction-tax) don't miss an
orthogonal win.

## Method

- Real `python:3.12-alpine`, both arches, driven straight through the engine (no daemon):
  `ddjit-linux_{aarch64,x86_64} --rootfs pyroot /usr/local/bin/python3.12 /bench.py R N`. Rootfses:
  aarch64 from the ctx-dispatch pyroot, x86_64 exported from `amd64/python:3.12-alpine`
  (`target/perf-py-v0923/pyroot-{arm,x86}`).
- Kernel (`target/perf-py-v0923/bench.py`): an allocation/dealloc-heavy loop mixing **integer** arithmetic
  (int box churn), **dict** insert/update (old-value DECREF), **string** creation (`str(i)`), and **list**
  append/clear (bulk alloc/dealloc) — a representative CPython eval-loop workload.
- Signals: `PROF` (crossings / ibtc / translations / wx / flag counters), `IBPROF` (arm indirect traffic +
  per-site monomorphic/order-k hit rates), `MAPDUMP` (paired translation-map + code-cache dump), mac
  `/usr/bin/sample` leaf-PC histogram of the engine PID, an offline PC→bucket classifier
  (`target/perf-py-v0923/classify_arm.py`, uses the stolen-reg discriminator), and A/B kill-switch deltas
  (the causal "addressable %").
- Host: mac load ~2.3–3.1 / 18 cores (fine). Linux side load ~1.1. Two other agents (py-x86, py-blocktrans)
  had engines running at 99% CPU in their own worktrees — left untouched; no orphans of mine.

### Baseline timings (bench.py R=8 N=200000)

| | native (orbstack) | dd | dd / native | dd / native-arm |
|---|---|---|---|---|
| arm64 | 0.164 s | 0.353 s | **2.15×** | 2.15× |
| amd64 | 0.362 s (Rosetta) | 0.697 s | **1.93× vs Rosetta** | 4.25× |

Consistent with the #375-era python rows (arm 3.05× / amd 2.08× measured on a heavier daemon workload).

---

## 1. Confirmed current breakdown (both arches)

**Headline structural fact (both arches): ~96–100 % of CPU is inside the translated code cache; the C
dispatcher is only ~1–3 %.** The mac `sample` shows every hot leaf PC in the anonymous RX code-cache
region; the engine binary (`ddjit-linux_*`) and dylibs get almost no samples (x86 `run_guest` ≈ 85–220 /
6168 main-thread samples ≈ 1.4–3.6 %; arm engine-range samples = 0). So the #375 buckets
(`ic_probe`/`hash`/`irq_poll`) are **inline emitted stubs within the cache**, dominated by CPython's
per-bytecode indirect dispatch — not C-side helpers.

### ARM — two views (they must be read together)

**(a) PC occupancy** (offline classifier over the paired sample+MAPDUMP; stolen-reg x16/x17 ⇒ engine stub,
ldr/str to guest regs ⇒ cpu_ldst, else guest):

| bucket | v0.9.23 occupancy | #375 (v?) | note |
|---|---|---|---|
| guest (arithmetic incl. refcount inc/dec) | 10.8 % | 34.6 % | pure compute is small |
| cpu_ldst (guest loads/stores, refcount loads, obj-field derefs) | 18.3 % | 11.8 % | memory traffic |
| ic_probe (inline IBTC per-site IC compare + `br`) | 23.0 % | 16.9 % | indirect dispatch |
| hash (inline IBTC shared-hash: ubfx+slot+ldp+sub+cbnz) | 14.3 % | 10.2 % | indirect dispatch |
| "irq_poll" (block-entry `ldr x16,[x28,#irq]; cbnz`) | 33.6 % | 19.1 % | **occupancy artifact — see (b)** |

**(b) Causal cost** (A/B kill-switch elapsed deltas — the *addressable* truth; baseline 4.298 s, R=100):

| knob | elapsed | Δ vs base | reads as |
|---|---|---|---|
| baseline | 4.298 s | — | |
| `NOIRQCHECK=1` | 4.284 s | **~0 %** | the #292 async-signal poll is now **free** (IRQSLIM, v0.9.20) |
| `IBTC1WAY=1` | 4.388 s | +2.1 % | 2-way IBTC ≈ 1-way here (arm 64Ki table already fits) |
| `NOTIER2X=1` | 4.301 s | ~0 % | no tight self-loop; tier-2 fold irrelevant to eval loop |
| `NOIBSLIM=1` | 4.614 s | **+7.4 %** | indirect-dispatch slimming (per-site IC skip at eval `br`) |
| `NOSTITCH=1` | 5.120 s | **+19 %** | trace/superblock stitching (← block-transition agent) |
| `NOSTEALFAST=1` | 5.873 s | **+36.6 %** | v0.9.21 musl-PLT/stolen-reg fast paths |
| `NOSTEAL1617=1` | 7.232 s | **+68 %** | stealing host x16/x17 (frees 2 scratch regs) |

**Reconciliation.** The 33.6 % "irq_poll" occupancy is a **sampling-skid artifact, not a cost**: the hottest
single PC (`ldr x16,[x28,#irq]`) sits one instruction *after* a guest pointer-chase `ldr x0,[x0]` and one
instruction after a megamorphic `br` — so guest load-miss latency and BR-misprediction stalls skid onto the
following block-entry poll instruction. `NOIRQCHECK` deleting those two insns saves ~0 %, proving the poll
itself is free. **The real ARM cost is the megamorphic indirect dispatch** (ic_probe 23 % + hash 14.3 %
*directly on the probe*, plus the mispredict penalty that skids onto the "irq" line). Update vs #375:
`irq_poll` has gone from a real 19.1 % to causally ~0 % (IRQSLIM shipped); dispatch (`ic_probe`+`hash`) is
still the dominant real cost and has grown as a share.

### x86 — breakdown

Same structural picture (97 % in cache), but the dominant cost is **dispatcher round-trips + flag
synthesis**, not an inline probe:

- **Dispatcher crossings scale with guest work.** `PROF`: crossings = 1.41 M (R=4) → 13.87 M (R=40) — ~10×
  work ⇒ ~10× crossings, ≈ **1.7 dispatcher exits per loop iteration**. ARM by contrast stays resident:
  ~19.6 K crossings *total*, flat across work. Note `g_prof_miss` is incremented **only on aarch64**
  (`dispatch_hooks.h:91`), so x86's `ibtc_miss=0` is unmeasured and the 13.87 M `branch_cross` = **13.87 M
  genuine block-exits to the C dispatcher** (inline-IBTC misses + unchained indirect edges on CPython's
  megamorphic computed-goto). `IBTC1WAY=1` → **26.9 s (+404 %)** confirms the x86 eval loop is held together
  only by the 2-way inline IBTC and is acutely dispatch-bound.
- **Flag-synthesis tax** (A/B, baseline 5.326 s, R=60):

| knob | elapsed | Δ | |
|---|---|---|---|
| `NOFLAGELIDE=1` | 5.670 s | +6.5 % | intra-block dead-flag elision |
| `NOPFAFELIM=1` | 5.892 s | +10.6 % | PF/AF elimination (parity/aux-carry) |
| `NOXBLOCKFLAGS=1` | 5.927 s | +11.3 % | cross-block flag elision |
| `NOLAZY=1` | 6.302 s | **+18.3 %** | lazy-flag model (largest single flag lever) |
| `NOSSEOPT=1` | 5.561 s | +4.4 % | SSE lowering (float/memcpy) |
| `NOSTITCH=1` | 5.489 s | +3.1 % | stitching (less than arm's +19 %) |
| `NOIRQCHECK=1` | 5.271 s | ~0 % | poll free on x86 too |
| `IBTC1WAY=1` | 26.865 s | **+404 %** | dispatch is IBTC-critical |

x86 addressable split: dispatcher-miss traffic (biggest, see levers) + overlapping flag tax (~18 % ceiling,
already mostly mature machinery) + a few % SSE.

---

## 2. Ranked CPython-specific levers (orthogonal to the two fix agents' mandates)

| # | lever | arch | evidence | ceiling (est.) | owner |
|---|---|---|---|---|---|
| **P1** | **Port the ARM inline indirect-dispatch machinery to x86** — x86 eats **13.87 M C-dispatcher round-trips** (~1.7 / iter) where ARM stays resident (~0). x86 `emit_ibranch` has a 2-way inline IBTC but **no per-site monomorphic IC** and **no IBSLIM** (both live only in arm `emit_ibranch_steal`/`is_interp_dispatch_br`). Add a per-site last-target guess + widen the x86 IBTC (it indexes only 13 bits / 8 Ki sets vs arm's 16 bits / 64 Ki). | **x86** | crossings 13.87 M vs arm 19.6 K; `IBTC1WAY` +404 %; sample ~97 % in-cache but spill/reload on every miss is in-cache (hidden). | **large** — the single biggest x86 CPython gap; plausibly closes much of the 1.93×→~1.4× | x86 lane (adjacent to its tax mandate) |
| **P2** | **History-keyed (order-k) dispatch for the megamorphic eval-loop `br`** — `IBPROF`: hot site (rank 0) = 83 M hits, **only 19.3 % monomorphic** (per-site IC nearly dead) but **84.65 % order-3 predictable** (dt=284 handlers). A depth-3 context-keyed predictor at recognized interpreter-dispatch sites would convert most of the ic_probe+hash cost into a predicted direct branch. Prototype gate already exists (`CTXDISP`, `is_interp_dispatch_br`). | both (arm first) | IBPROF o1/o2/o3 = 37/60/84.65 %; arm ic_probe+hash ≈ 37 % occupancy | **large on arm** (attacks the dominant real cost); needs the block-transition agent's trace machinery, so **coordinate** | block-transition lane (shared) |
| **P3** | **x86 refcount DECREF flag path** — CPython does a DECREF (`dec/sub [mem],1; jz dealloc`) on nearly every object touch. The RMW is fine (load-op-store, non-LOCK under the GIL), but the **ZF for the zero-test** is a per-DECREF flag materialization. It is already covered by lazy-flags + PF/AF-elim (part of the measured ~18 % flag tax, `NOLAZY` +18.3 %), **but** because DECREF's `jz` consumes ZF immediately, it is the ideal case for a fused "sub-and-branch-on-zero" idiom (recognize `dec [mem]; jz` → emit the store + a single `cbz`-style test, never materialize NZCV). | x86 | flag-tax knobs; `translate.c:474` inc/dec in PF/AF set; hottest guest pattern is DECREF | **moderate** (a slice of the 18 % flag tax) | x86 lane |
| **P4** | **Widen the arm shared IBTC / cheapen the hash** — arm hash bucket = 14.3 % occupancy: ubfx + `emit_ibtcptr` + add-slot + `ldp` + sub + cbnz per indirect branch, 777 M times. `NOIBSLIM` +7.4 % shows the per-site skip helps; a cheaper hash (fold the ibtc base into a stolen reg to drop `emit_ibtcptr`, or a set-associative last-way hint) trims the fixed per-dispatch tax. | arm | IBPROF 777 M indirect (~194 / iter); hash 14.3 % occupancy | **moderate** | block-transition lane |
| **P5** | **Extend stitching across the eval-loop dispatch on x86** — `NOSTITCH` costs arm +19 % but x86 only +3.1 %, i.e. x86 stitching is under-delivering on CPython; more of the x86 eval loop stays as single blocks that exit to the dispatcher (feeds P1). | x86 | arm +19 % vs x86 +3.1 % NOSTITCH asymmetry | **moderate** | x86 lane |

---

## 3. The two explicitly-flagged hypotheses — both RESOLVED with hard evidence

### Adaptive-specialization SMC churn — **RULED OUT (not a hidden slice).**
`PROF translations` is **flat across a 10× increase in work**: ARM 18639 (R=4) → 18637 (R=40); x86 37260 →
37252. `xlate_ms` constant (~66 ms arm / ~64 ms x86). CPython 3.12's PEP 659 adaptive specialization
rewrites its **bytecode array** (data the eval loop interprets), **not** executable guest machine code, so
dd's SMC gate (`txpg` set of translated pages, `cache.c:105`) never fires and there is **zero retranslation
churn**. `wx_toggles=0 dualmap=1` on both arches (the v0.9.19 x86 dual-map landed). *No CPython-specific SMC
handling is needed — this is already optimal.*

### Reference-counting codegen — **NOT a hidden unoptimized slice.**
- **ARM:** INCREF/DECREF compile to guest `ldr/add/str` (+ a `cbz`-style zero test), translated ~1:1
  (verbatim aarch64). It lives in the `cpu_ldst` (18.3 %) + `guest` (10.8 %) buckets as *inherent* CPython
  memory/compute work — there is no dd-side inefficiency to remove; the lever is reducing dispatch *around*
  it, not the RMW itself.
- **x86:** `inc/dec [mem]` is a normal load-op-store RMW (non-LOCK; CPython 3.12 is GIL-protected, no
  atomic), handled by the PF/AF + lazy-flag machinery (`translate.c:448,474`, `emit.c:201`
  `e_nzcv_save_keepC` preserves CF). The DECREF zero-test is a *share* of the measured ~18 % flag tax, **not**
  an unfused RMW or a mis-materialized flag. The one incremental win is the fused `dec [mem]; jz` idiom (P3),
  a slice of the flag tax — not a large hidden cost.

### Also ruled out (measured):
- **pymalloc / arena mmap traffic:** total syscalls for the whole run = 632 (R=40) — essentially none per
  iteration. The free-list fast path is pure in-guest pointer-chase (translated 1:1); arena mmaps are rare.
  Not a measurable slice.
- **GIL / eval-breaker double-check:** single-threaded, `fwait=0`, no futex traffic. The eval-breaker is an
  in-guest load+compare; dd's `irq_poll` is orthogonal and causally free (`NOIRQCHECK` ~0 %). No double-check
  cost.

---

## TL;DR for the manager

1. **The current CPython cost is dispatch, not compute or refcount.** ARM real cost ≈ the megamorphic
   indirect-dispatch complex (ic_probe+hash, ~37 % occupancy; `NOSTITCH` +19 %, `NOIBSLIM` +7.4 %); x86 real
   cost ≈ **13.87 M C-dispatcher round-trips/run** (`IBTC1WAY` +404 %) **plus** ~18 % flag tax.
2. **Biggest orthogonal find: x86 lacks the arm inline-dispatch machinery (P1)** — per-site IC + IBSLIM +
   a wide IBTC exist only on aarch64. Porting them is likely the single largest x86 CPython win and is
   *adjacent to but distinct from* the x86 instruction-tax mandate. Flag it to the x86 agent.
3. **Second find: history-keyed dispatch (P2)** — the hot eval `br` is 84.65 % order-3 predictable; the
   block-transition agent's trace machinery is the right home for it.
4. **Both flagged hypotheses are dead ends:** adaptive-specialization SMC churn (translations flat across
   10× work) and refcount codegen (1:1 on arm, inside the flag tax on x86) are **not** hidden slices — do
   not spend cycles there.

Artifacts: `target/perf-py-v0923/{bench.py,pyroot-arm,pyroot-x86,arm.sample.txt,arm.md.{map,bin},
x86.sample.txt,x86.md.map,classify_arm.py}`.
