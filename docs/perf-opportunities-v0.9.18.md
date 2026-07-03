# dd perf opportunities — second pass (orthogonal to the first profiler)

Read-only discovery pass on the pinned canonical engine
`DDJIT_DIR=/Users/x/dd/dd/target/release/build/ddjit-dfb5094c39030e80/out` (not rebuilt). Goal: find
HIGH-VALUE levers the first profiler (`docs/perf-profile-v0.9.18.md`) did **not** cover. Its six levers
(SSSE3/SSE4 glue, crypto emission, syscall vector spill, IBTC quality, fast-syscall set, x86 cross-block
flags) are OFF-LIMITS and not re-reported here.

Method: deep read of `dd-jit/src/runtime/`, plus live `PROF`/`COLDPROF` runs and `NODUALMAP`/`NOTIER2X`/
`NOSTITCH`/`NOFLAGELIDE` A/B on the self-timed microbench through **both** engines via the `mac` bridge.
Host had concurrent agent builds — treat absolute ms as directional; the knob **deltas** are the signal.

---

## TL;DR

The compute/memory core is already near-optimal (see "Dead ends" — I ruled out every big hypothesis the
brief raised: TSO barriers, per-access address translation, register allocation, block chaining, scalar
FP). The genuinely new, orthogonal levers are structural / cold-start / memcpy-funnel:

| # | lever | workloads | arm/amd | ceiling | risk | why new |
|---|---|---|---|---|---|---|
| A | **Wire the dual-mapped W^X-free code cache into the x86 engine** (already shipped on arm; x86 still toggles `pthread_jit_write_protect_np` per translation + per IC-fill) | tar/gzip, python, sqlite, redis (all x86) | **amd only** | tar/gzip 2.07 → ~1.9; broad amd cold-start + steady IC-fill 1–5% | **safe** | first report said "tar/gzip … (ignore)"; this is x86-only and the fix already exists |
| B | **Advertise ERMS in cpuid → guest libc funnels bulk memcpy/memset through `rep movsb/stosb`** (dd lowers that to one host `memcpy`/`memset`) instead of translating glibc's SSE/AVX copy loops (which also hit the unhandled SSE-glue softmulator exits) | sqlite, redis, python (memcpy-heavy) | amd | sqlite/redis/python few-% each; compounds the first report's lever #1 | moderate | orthogonal to #1: removes copies from the SSE path entirely |
| C | **Drop the dead vector spill in `emit_rep_string`** (memcpy/memset provably never touch guest xmm, yet every `rep movs/stos` spills+reloads all 16 xmm) | any rep-string user; unlocks value only with (B) | amd | small alone; safe companion to (B) | **safe** | distinct site from the first report's syscall-spill lever #3, and unconditionally dead (no SIMD-dirty tracking needed) |
| D | **Batch/defer the x86 SMC source-page `mprotect`** done post-translate per new code page | tar/gzip cold start | amd | modest cold-start | moderate | x86-only cold-start tax the first report didn't isolate |

Ranking rationale: **A first** — it is the safest possible change (byte-identical code path already runs
on the aarch64 engine; it is literally an un-wired init), it is x86-only (the mission's differentiator),
and it attacks the tar/gzip row the first report told everyone to ignore. **B** is the highest-ceiling new
idea but needs a differential-test pass. **C** is a trivially-safe companion that only pays off once **B**
routes copies through `rep movsb`. **D** is a smaller cold-start cleanup.

---

## Lever A — x86 engine never got the dual-mapped (W^X-toggle-free) code cache  ★ do first

**The find.** The engine has a fully-built dual-mapping JIT cache: a plain anon RW region plus a
`mach_vm_remap` RX alias of the same physical pages, so the writer and the executor use different VAs and
the engine **never** flips page permissions (`engine/cache.c:11-50`; the gate `jit_wprot()` is a no-op when
`g_dualmap`, `cache.c:26-30`). The **aarch64** target turns it on at init
(`targets/linux_aarch64.c:424-434`: `dualmap_alloc` → `g_dualmap = 1`). The **x86_64** target does **not** —
`targets/linux_x86_64.c:181` just does the single `MAP_JIT` mmap and leaves `g_dualmap == 0`, so
`jit_wprot()` calls `pthread_jit_write_protect_np()` on **every** translation, every inline-cache fill, and
every chain back-patch. `g_dualmap` is never assigned `1` anywhere in the x86 path.

**Measurement (live, this host).**
- x86 `microbench fp`, `PROF=1`: `translations=1210 wx_toggles=1208 dualmap=0` — ~1 toggle per block.
- x86 `microbench memcpy`: `translations=1273 wx_toggles=1270 dualmap=0`.
- arm `microbench alu`, default: `dualmap=1 wx_toggles=0`; `NODUALMAP=1`: `wx_toggles=1520` for
  `translations=574` (~2.6 toggles/block: open+close + IC/chain writes). The `xlate_ms` delta from
  toggling was ~0.1–0.2 ms over ~950 extra toggles → **~100–200 ns per toggle** (an APRR write + ISB).

**Why it's real and where the ceiling comes from.** Steady-state hot loops are chained (regs-live `b body`,
`cache.c:314-323`) and translate/toggle ~0, so this is **not** a hot-loop tax — it is (1) **cold start**:
every block the process ever translates costs a toggle, so a translation-heavy short-lived container
(tar/gzip: thousands–tens-of-thousands of blocks) pays ms of toggles it wouldn't on arm; and (2)
**steady-state IC re-fills**: on x86 every write into the code cache (inline-cache fill, chain patch)
toggles, so a megamorphic dispatcher that keeps re-filling ICs (python, sqlite VDBE) keeps paying. Ceiling:
low single-digit % on the amd cold-start rows (tar/gzip 2.07 is the concrete target) plus a smaller
steady-state IC benefit; the value is that it is **essentially free and zero-risk to land**.

**Location / first step.** Copy the `targets/linux_aarch64.c:424-441` dual-map block into
`targets/linux_x86_64.c` init (`~:181`), replacing the single `MAP_JIT` mmap; keep the `NODUALMAP`
fallback. The absolute-handoff conversions (`J_RX`/`J_RW`, `cache.c:22-23`) and the flush/fork paths
(`cache.c:673-772`) are already `g_dualmap`-aware, so the shared cache.c needs no change — this is almost
purely an init wiring change. Validate: `PROF=1` shows `dualmap=1 wx_toggles=0` on x86, then re-diff the
matrix for the x86 engine (icache/`ic ivau` flush must target the RX alias — already handled by `J_RX`).

**Risk: safe.** Identical code already ships and passes on the aarch64 engine.

---

## Lever B — advertise ERMS so guest libc bulk-copies take the fast `rep movsb` path

**The find.** dd's cpuid does **not** set ERMS (Enhanced REP MOVSB = leaf 7 subleaf 0, EBX bit 9):
`translate/x86_64/x86_ops.c:187-190` sets only BMI1/BMI2/SHA. So glibc x86-64 selects its **SSE/AVX
unaligned** memcpy/memmove/memset variants, not `rep movsb/stosb`. Confirmed live: the x86 `memcpy`
microbench reports `repstr=0 repstr_elems=0` — the guest never issued a rep-string op; it ran a vectorized
loop instead. Meanwhile dd already lowers `rep movs`/`rep stos` to a **single host `memcpy`/`memset`**,
bit-exact (`translate/x86_64/translate/repstr.c:30-133`, `translate_string` at `:143`).

**Why it's a lever.** glibc's SSE/AVX copy loops are exactly the code that (a) must be translated as a long
vector loop and (b) can hit the unhandled SSSE3/SSE4 glue that block-exits to the `do_sse3b` softmulator
(the first report's lever #1). Advertising ERMS makes glibc route memcpy/memmove/memset through
`rep movsb/stosb`, which dd turns into one host libc call — removing those copies from the SSE path
altogether. sqlite (btree page/record copies), redis (reply buffers), and python (str/bytes/dict copies)
are all memcpy-heavy, so this compounds lever #1 on the amd rows. It is orthogonal: #1 makes the SSE glue
cheap; B removes the copies from the SSE path entirely.

**Location / first step.** Set EBX bit 9 at `x86_ops.c:189`. Then verify glibc actually switches (re-run
the `memcpy` microbench: expect `repstr>0`) and add a byte-exact differential vs the oracle for
memcpy/memmove/memset across sizes, alignments, and forward-overlap (the `dd_rep_movs` smear path,
`repstr.c:34-60`). **Risk: moderate** — it changes which guest-libc code path runs; must confirm the
ERMS `memmove` backward-overlap case (glibc still copies backward with SSE, not rep, so the forward-only
helper is only ever handed forward/disjoint copies) and that nothing else keys off ERMS.

## Lever C — `emit_rep_string` spills+reloads all 16 guest xmm that memcpy/memset can't touch (safe companion to B)

`emit_rep_string` (`translate/x86_64/translate/repstr.c:104-105,132`) brackets the host `memcpy`/`memset`
call with a full `emit_spill()` + `emit_reload()` — 16 GPR **and 16 xmm** (8×`stp_q` / 8×`ldp_q`) + flags.
The C helper provably touches no guest xmm, so the entire vector spill/reload is **dead** on every
rep-string op. Unlike the first report's syscall-spill lever #3 (which needs per-block SIMD-dirty
tracking because signal/AVX/sigreturn exits read xmm), here the vector spill is **unconditionally** dead —
just marshal RDI/RSI/RCX/RAX and skip the xmm save. Small alone (and rep-string is rarely hit **until**
lever B routes libc copies through it), but a clean, zero-risk win that makes B pay more. First step: add a
GPR+flags-only spill/reload variant used by `emit_rep_string`. **Risk: safe.**

## Lever D — x86 SMC source-page write-protect is a per-code-page mprotect at cold start

x86 (only) runs a post-translate step that write-protects each guest 4 KB page it has translated a block
from, so a later guest write to code faults and drops the stale translation (`engine/dispatch.c:63,153`;
`translate/x86_64/elf.c:689-694`; page set in `engine/cache.c:89-97`). aarch64 has no equivalent
(`dispatch.c:63` "aarch64 has none"). This is fault-based (no per-store guard in emitted code — good), but
the `mprotect` per newly-touched code page is an x86-only cold-start cost that stacks with lever A on
tar/gzip. Modest and correctness-coupled; batch the mprotect over contiguous freshly-translated pages
rather than one call per page. **Risk: moderate** (must not narrow the SMC guarantee). First step: coalesce
adjacent pages in the post-translate hook.

Minor / not in the 8 benchmarks: the non-PIE Go "guest_base bias-fold" emits a **4-insn** bias sequence
(`lsr;cbnz;movconst;add`) on **every** memory dereference (`translate/x86_64/decode.c:347-355`,
`ea_bias17`). It is inert for PIE (`guestfold_on()==0`) so none of the 8 workloads pay it, but it is a large
per-access tax on non-PIE Go binaries (`go build` etc.) — worth a targeted fix (e.g. map the low image so
no runtime bias is needed) if Go throughput becomes a target.

---

## Dead ends (measured/read and ruled out — don't chase these)

- **x86 TSO memory barriers.** Hypothesized as the flat amd tax. **Not present.** Ordinary guest loads/
  stores emit plain `LDR`/`STR` (`emit.c:56-84`, `e_load`/`e_store` = `0x39400000`/`0x39000000` families,
  no acquire/release). Only `mfence/lfence/sfence` map to `dmb ish` (`translate.c:2969-2976`) and only
  `LOCK`-prefixed RMW uses ordered LSE. There is **no per-load/per-store fence**, so the uniform amd ALU/mem
  overhead is cross-ISA flag synthesis (first report's #6), not fencing. No `NOTSO` knob needed.
- **Guest→host address translation.** Hypothesized per-access gmap lookup/bounds. **Optimal.** For PIE
  guests the mapping is **identity** — guest pointer *is* host pointer; `emit_ea` folds base+index<<scale+
  disp into the ARM addressing mode (`decode.c:358-401`, `ea_imm_fold`/`emit_load_mem` fold [base+disp]
  into a single `ldr`/`ldur`, `:407-433`) with **no** lookup, bounds check, or added base register. The
  bias add exists only for non-PIE (`ea_bias17`, inert for PIE).
- **Register allocation / spill.** **Optimal.** x86 GPRs are statically pinned to host regs
  (rax=x0…r15=x15, `abi.h`), xmm→v0..v15, flags→nzcv, cpu→x28. No dynamic RA. Full 16-GPR+16-xmm reload
  happens **only** in the block prologue on a cold/dispatcher entry (`emit.c:442-450`); chained and
  IBTC-hit transitions jump to the **regs-live body** (`cache.c:314-323`, map stores `{host, body}`), so
  steady state pays no reload.
- **Direct block chaining / superblocks.** **Optimal / working.** Direct branches are back-patched to
  `b target.body` with regs live, no dispatcher round-trip (`emit_chain_exit`, `stubs.c:459-471`); trace/
  superblock stitching is on by default (`translate/x86_64/translate/trace.c`). A/B: `NOSTITCH=1` **hurt**
  branch (+5.6%) and call (+4%) on x86 → stitching is already delivering.
- **rep movs/stos lowering.** Already one host `memcpy`/`memset` (`repstr.c`); `rep cmps/scas` = one C
  round-trip with host `memcmp`/`memchr` inside (`repstr.c:191-201`). (The remaining waste is lever C's dead
  xmm spill, and the fact that libc rarely emits rep-string at all — lever B.)
- **Scalar & packed SSE FP.** **Optimal.** `addsd/mulsd/divsd/sqrtsd/…` lower to native
  `FADD/FMUL/FDIV/FSQRT` (scalar `0x1E20xxxx`, packed `0x4E20xxxx`; `translate.c:2610-2651`). The microbench
  "fp 1.96× amd" is the loop's flag/branch materialization (first report's #6), not the arithmetic.
- **Atomics / locks.** **Optimal.** `LOCK` RMW → native LSE `ldaddal/ldsetal/ldeoral/ldclral`
  (`translate.c:678-697`, `lock_rmw`), `xchg [mem]` → `swpal` (`:1319-1320`); no C round-trip. Contended
  `futex` is still a syscall (that's the existing syscall-round-trip levers #3/#5, not a new one).
- **SMC per-store guard.** No emitted per-store check — it is fault-based page protection (lever D). No hot-
  path cost for the 8 (none self-modify). `NOSMC` A/B not relevant to them.
- **Tier-2 / stitch thresholds "too conservative".** **Not.** `NOTIER2X=1` was within noise on
  alu/branch/fp/call (tier-2 fold saves ~2% on the tightest loops); `NOSTITCH=1` made things worse. The
  thresholds are not leaving obvious headroom; tuning them is a dead end.
- **x87 FP.** C-helper-based and genuinely slow (`translate/x86_64/translate/x87.c`, 80-bit converted in C),
  but **unused by the 8 benchmarks** (glibc/python/openssl use SSE2 scalar). Not a lever for this board;
  only matters for legacy/Fortran-style x87 guests.

## Knobs used
`PROF` (`translations`/`wx_toggles`/`dualmap`/`xlate_ms`/`tier2`), `COLDPROF`, `NODUALMAP` (arm A/B to
price the W^X toggle), `NOTIER2X`, `NOSTITCH`, `NOFLAGELIDE`, `NOREP`. Engine driven directly:
`env <knob> $DDJIT_DIR/ddjit-linux_{x86_64,aarch64} <guest> <kernel>`; guests in `target/bench/`.
