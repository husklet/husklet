static void x18_prolog(void) {
    e_str(0, CPUREG, (int)OFF_MSCRATCH);
    e_str(1, CPUREG, (int)OFF_MSCRATCH + 8);
    e_load_cpu(1);
}

static void x18_epilog(void) {
    e_ldr(1, CPUREG, (int)OFF_MSCRATCH + 8);
    e_ldr(0, CPUREG, (int)OFF_MSCRATCH);
}

// A3 §B instrumentation (PROF=1 only): bump a 64-bit global counter from emitted code. Self-contained:
// stashes x9/x10 in cpu->mscratch[0..1] (NOT the [sp,#-N] red zone -- AArch64 has none, and this runs at
// a §B shadow push/ret point that can be reached with a shallow guest SP, so a below-SP store would fault
// exactly as the pixman self-loop did in 6d38d96c). mscratch[0..1] is free at all three call sites: the
// shadow-push call precedes that helper's own mscratch spill, and both shadow-ret calls follow its
// x0..x3 restore. x28 (CPUREG) = cpu holds for the whole block on every path. Gated on g_prof, so the
// non-PROF codegen is byte-identical to baseline (zero steady-state cost).
static void emit_prof_bump(void *ctr) {
    e_str(9, CPUREG, (int)OFF_MSCRATCH);
    e_str(10, CPUREG, (int)OFF_MSCRATCH + 8);
    e_adrp_add(9, (uint64_t)ctr); // x9 = &counter (plain RW data; adrp+add reaches it)
    e_ldr(10, 9, 0);
    e_addi(10, 10, 1);
    e_str(10, 9, 0);
    e_ldr(9, CPUREG, (int)OFF_MSCRATCH);
    e_ldr(10, CPUREG, (int)OFF_MSCRATCH + 8);
}

// §B: store a constant to cpu->x[30] (the stolen guest link reg). x28=cpu.
// IBSLIM: with x16/x17 stolen (A1 default) x16 is engine-private scratch here, so the legacy
// x0 spill/restore dance through cpu->mscratch (5 insns, 3 memory ops PER GUEST CALL: bl and blr
// both pass through this) collapses to movconst+str (typically 3 insns, 1 store). Guest x16 is
// untouched -- its value lives only in cpu->x[16] under the steal. NOIBSLIM=1 restores the exact
// legacy sequence for A/B.
static void emit_set_x30(uint64_t val) {
    if (g_steal1617 && !g_noibslim) {
        e_movconst(16, val);
        e_str(16, CPUREG, 30 * 8);
        return;
    }
    e_str(0, CPUREG, (int)OFF_MSCRATCH);
    e_movconst(0, val);
    e_str(0, CPUREG, 30 * 8);
    e_ldr(0, CPUREG, (int)OFF_MSCRATCH);
}

// §B shadow push: cpu->x[30] = gpc+4; sstk[ssp&1023] = (gpc+4, &Lcont); ssp++. x0..x2 spilled to
// cpu->mscratch (all guest regs are live across the call). Returns the `adr x1,Lcont` to backpatch.
static uint32_t *emit_shadow_push(uint64_t gpc) {
    int M = (int)OFF_MSCRATCH;
    if (g_prof) emit_prof_bump(&g_prof_shpush); // A3: count §B shadow pushes executed (PROF only)
    e_stp(0, 1, CPUREG, M);
    // spill x0..x3 -> mscratch (paired: 2 stp not 4 str)
    e_stp(2, 3, CPUREG, M + 16);
    // guest_ret is the guest-VISIBLE link value (spilled to the guest stack + matched on the ret),
    // so use the UN-BIASED (low) link vaddr for non-PIE; pcrel_base is identity for PIE.
    e_movconst(0, pcrel_base(gpc) + 4);
    // x0 = guest_ret; cpu->x[30] = guest_ret (ALWAYS)
    e_str(0, CPUREG, 30 * 8);
    // x1 = ssp (capped at 1024)
    e_ldr(1, CPUREG, (int)OFF_SSP);
    uint32_t *p_full = (uint32_t *)g_cp;
    // tbnz x1, #10, Lskip (ssp==1024 -> overflow; no flags)
    emit32(0);
    e_addlsl4(2, CPUREG, 1);
    // x2 = C + idx*16 + OFF_SSTK = &sstk[2*ssp]
    e_addi(2, 2, (unsigned)OFF_SSTK);
    uint32_t *p_adr = (uint32_t *)g_cp;
    // adr x3, Lcont (host_ret; backpatched)
    emit32(0);
    // sstk[2*ssp] = (guest_ret, host_ret=&Lcont)
    e_stp(0, 3, 2, 0);
    e_mov_from_sp(3);
    e_addlsl3(2, CPUREG, 1);
    // gsp[ssp] = current guest SP (frame disambiguator)
    e_str(3, 2, (int)OFF_GSP);
    e_addi(1, 1, 1);
    // ssp++
    e_str(1, CPUREG, (int)OFF_SSP);
    uint8_t *Lskip = g_cp;
    *p_full = 0x37000000u | (10u << 19) | (((uint32_t)(((uint8_t *)Lskip - (uint8_t *)p_full) / 4) & 0x3FFF) << 5) |
              // tbnz x1,#10
              1;
    e_ldp(0, 1, CPUREG, M);
    // restore x0..x3 (paired: 2 ldp not 4 ldr)
    e_ldp(2, 3, CPUREG, M + 16);
    return p_adr;
}

// §B profile gate: scan the target's entry block. A LEAF function (reaches `ret` with no bl/blr
// first) gains nothing from the shadow-RAS -- its monomorphic return is predicted by the per-site IC
// -- so paying the per-bl shadow push is pure overhead (floatk: sqrt/sin/pow). Only non-leaf targets
// (depth -> the hardware RAS predicts nested returns: stringk/recursion) get §B. Static, no profiling
// overhead; the ret auto-adapts (no frame pushed -> classify falls to the IC return).
// A3 §B depth-gate tuning. The baseline gate (scan_calls + target_is_leaf) is depth-2 static and
// MISCLASSIFIES two important cases as "leaf" (-> withholds §B -> the return falls to the IBTC):
//   (1) a function LARGER than the 64-insn scan window whose calls live past it (fib at -O2, most of
//       sqlite's VDBE helpers) -- the scan exhausts having seen no bl and reports "leaf";
//   (2) a "shallow" helper that calls only leaves but is itself called from MANY sites -- its single
//       return site is polymorphic, so the per-site IC thrashes (exactly what the RAS fixes).
// §B is self-validating (emit_shadow_ret checks guest_ret AND guest_sp; a wrong guess -> IBTC, never a
// misland), so the gate only ever trades cycles, never correctness. MEASUREMENT (see arm-a3.md) shows
// §B is NET-NEGATIVE on every return-heavy workload tested -- sqlite, qsort, AND the ideal polymorphic
// deep-recursion cases (longfib 2x, deepcall 1.4x) -- because the shadow push (~19 insn + sstk stores)
// plus the shadow-ret validate (~22 insn: guest_ret AND guest_sp compares) cost FAR more than the IBTC
// return path they replace (a monomorphic per-site IC hit, or even a thrashing shared-hash probe). The
// host RAS's 1-cycle `ret` is buried under ~40 insn of software bookkeeping. So the right tune is the
// OPPOSITE of "widen": DISABLE §B and return every ret through the proven IBTC. Levels (env, once):
//   -1 (DEFAULT)      -> §B OFF: no shadow push; every ret -> bare IBTC (IC + shared hash). The win.
//   -2 SHADOWGATE=-2  -> §B OFF on the push side, but ret keeps the shadow-ret stub (empty -> IBTC).
//    0 NOSHADOWTUNE=1 -> EXACT original §B-on gate (byte-identical baseline codegen). A/B kill switch.
//    1 SHADOWGATE=1   -> widen-fix: window-exhaustion = large/complex fn -> DEEP not leaf (measured: worse).
//    2 SHADOWGATE=2   -> widen more: ANY direct call -> §B (measured: worse / no better).
static int shadowgate(void) {
    return -1;
}

// Scan target's straight-line extent (bounded by forward-branch reach). Returns -1 if a blr (unknown
// callee) -- or, when tuned, if the scan window is exhausted with no clean terminal (large/complex fn,
// treat as deep) -- else the count of direct-call (bl) targets, writing up to `max` of them to calls[].
static int scan_calls(uint64_t target, uint64_t calls[], int max) {
    int64_t reach = 0;
    int n = 0;
    for (int i = 0; i < 64; i++) {
        uint32_t in = a64_fetch_instruction(target + (uint64_t)i * 4, NULL);
        // blr -> unknown callee
        if ((in & 0xFFFFFC1Fu) == 0xD63F0000u) return -1;
        if ((in & 0xFC000000u) == 0x94000000u) {
            if (n < max) calls[n] = target + (uint64_t)i * 4 + ((uint64_t)sext(in & 0x3FFFFFF, 26) << 2);
            n++;
            // bl
        }
        int64_t off = 0;
        int isb = 0;
        if ((in & 0xFF000010u) == 0x54000000u) {
            off = sext((in >> 5) & 0x7FFFF, 19);
            isb = 1;
            // b.cond
        } else if ((in & 0x7E000000u) == 0x34000000u) {
            off = sext((in >> 5) & 0x7FFFF, 19);
            isb = 1;
            // cbz/cbnz
        } else if ((in & 0x7E000000u) == 0x36000000u) {
            off = sext((in >> 5) & 0x3FFF, 14);
            isb = 1;
            // tbz/tbnz
        } else if ((in & 0xFC000000u) == 0x14000000u) {
            off = sext(in & 0x3FFFFFF, 26);
            isb = 1;
            // b
        }
        if (isb && off > 0 && i + off < 64 && i + off > reach) reach = i + off;
        if ((in & 0xFFFFFC1Fu) == 0xD65F0000u || (in & 0xFC000000u) == 0x14000000u || (in & 0xFFFFFC1Fu) == 0xD61F0000u)
            // terminal past all branches
            if (i >= reach) return n;
    }
    // window exhausted with no clean terminal: a function larger than the scan window. Baseline reported
    // whatever bl count it happened to see (usually 0 -> "leaf"); tuned treats it as deep/unknown.
    return shadowgate() ? -1 : n;
}

static int is_leaf0(uint64_t t) {
    uint64_t c[1];
    return scan_calls(t, c, 0) == 0;
    // no calls at all
}

// §B helps only on DEPTH (the RAS predicts nested returns). A leaf or a depth-2 "shallow" function
// (all its calls go to leaves: sqrt/sin/pow's helpers) gains nothing -> keep cheap Stage-B. Only a
// function that calls a NON-leaf (or recurses, or calls indirectly) is deep enough to pay the push.
static int target_is_leaf(uint64_t target) {
    uint64_t calls[16];
    int n = scan_calls(target, calls, 16);
    // SHADOWGATE=-1: never §B (every bl -> leaf path, every ret -> IBTC). Floor experiment.
    if (shadowgate() < 0) return 1;
    // indirect callee / large-complex fn (tuned) -> assume deep -> §B
    if (n < 0) return 0;
    // true leaf (no calls at all: sqrt/sin/pow) -> Stage-B, regardless of level
    if (n == 0) return 1;
    // L2/L3: ANY direct call -> §B (covers multiply-called shallow helpers whose ret site is polymorphic)
    if (shadowgate() >= 2) return 0;
    for (int i = 0; i < n && i < 16; i++)
        // calls a non-leaf -> deep -> §B
        if (!is_leaf0(calls[i])) return 0;
    // all-leaf-calls (shallow) -> Stage-B
    return 1;
}

#define CTX_INLINE_DEPTH 3
#define CTX_INLINE_INSNS 64

static int context_clone_candidate(uint64_t target, const uint64_t ancestors[], int depth, uint64_t *retpc, int *cost) {
    if (depth >= CTX_INLINE_DEPTH) return 0;
    for (int i = 0; i < depth; i++)
        if (ancestors[i] == target) return 0;
    uint64_t anc[CTX_INLINE_DEPTH];
    for (int i = 0; i < depth; i++)
        anc[i] = ancestors[i];
    anc[depth] = target;
    int total = 0;
    /*
     * A candidate is decoded sequentially inside one VM region.  Query that
     * containing region once instead of asking the host kernel about the same
     * page before every instruction (mach_vm_region is especially costly).
     * Retain the old boundary behavior by rejecting an instruction whose last
     * byte is outside the containing readable region.
     */
    hl_host_region region;
    if (!hl_host_region_query((uintptr_t)target, &region) || target < region.address ||
        !(region.protection & HL_HOST_REGION_READ))
        return 0;
    uint64_t region_offset = target - region.address;
    if (region_offset >= region.size) return 0;
    uint64_t remaining = region.size - region_offset;
    for (int i = 0; i < CTX_INLINE_INSNS; i++) {
        uint64_t offset = (uint64_t)i * 4;
        if (offset > remaining || remaining - offset < 4 || offset > UINT64_MAX - target) return 0;
        uint64_t pc = target + offset;
        uint32_t in = a64_fetch_instruction(pc, NULL);
        total++;
        if ((in & 31u) == 30u && (in & 0xFC000000u) != 0x94000000u && (in & 0xFFFFFC1Fu) != 0xD65F0000u) return 0;
        if (in == 0xD4000001u || (in & 0xFC000000u) == 0x14000000u || (in & 0xFFFFFC1Fu) == 0xD61F0000u ||
            (in & 0xFFFFFC1Fu) == 0xD63F0000u)
            return 0;
        if ((in & 0xFC000000u) == 0x94000000u) {
            int64_t off = sext(in & 0x3FFFFFF, 26) << 2;
            uint64_t child_ret;
            int child_cost;
            if (!context_clone_candidate(pc + off, anc, depth + 1, &child_ret, &child_cost)) return 0;
            total += child_cost;
            if (total > CTX_INLINE_INSNS * CTX_INLINE_DEPTH) return 0;
        }
        if ((in & 0xFFFFFC1Fu) == 0xD65F0000u) {
            if (((in >> 5) & 31) != 30) return 0;
            *retpc = pc;
            *cost = total;
            return 1;
        }
    }
    return 0;
}

// §B guest bl: push shadow, host `bl body(target)` (RAS pushes &Lcont), Lcont continues at gpc+4.
static void emit_bl_ras(uint64_t gpc, uint64_t target) {
    /*
     * Shadow returns and host BL edges retain host PCs in otherwise unrelated
     * blocks.  Once SMC is active use the dispatcher-only leaf path so a
     * targeted source invalidation has no hidden ingress to the old body.
     */
    if (smc_seen() || target_is_leaf(target)) {
        if (g_prof) g_prof_bl_leaf++; // A3: depth-gate steered this bl to the cheap leaf Stage-B path
        // Guest LR is a guest-VISIBLE value (the guest spills it to its stack), so it must be the
        // UN-BIASED (low) link vaddr -- non-PIE runs gpc HIGH; the dispatcher re-biases low->high on
        // the ret. pcrel_base is identity for PIE (no codegen change for the matrix).
        emit_set_x30(pcrel_base(gpc) + 4);
        emit_chain_exit(target);
        return;
        // leaf -> cheap Stage-B (IC return)
    }
    if (g_prof) g_prof_bl_shadow++; // A3: depth-gate steered this bl to §B (shadow push + RAS ret)
    uint32_t *p_adr = emit_shadow_push(gpc);
    void *body = map_body(target);
    uint32_t *slot = (uint32_t *)g_cp;
    if (body) {
        int64_t d = ((uint8_t *)body - (uint8_t *)slot) / 4;
        emit32(0x94000000u | ((uint32_t)d & 0x3FFFFFFu));
        // host bl body (RAS pushes &Lcont)
    } else {
        add_pend2(slot, target, 1);
        emit_exit_const(target, R_BRANCH);
        // not translated yet: spill-exit (slot patched to `bl body`)
    }
    // host ret lands here
    uint8_t *Lcont = g_cp;
    int64_t ao = Lcont - (uint8_t *)p_adr;
    *p_adr = 0x10000000u | ((uint32_t)(ao & 3) << 29) | (((uint32_t)((ao >> 2) & 0x7FFFF)) << 5) | 3u;
    // after the call returns -> gpc+4
    emit_chain_exit(gpc + 4);
}

// §B guest ret: if cpu->x[30] == shadow-top guest_ret, pop + real x30=host_ret + host `ret`
// (hardware-RAS predicted). Else fall back to the dispatcher reading cpu->x[30]. Never lands wrong.
static void emit_shadow_ret(void) {
    int M = (int)OFF_MSCRATCH;
    e_stp(0, 1, CPUREG, M);
    // spill x0..x3 (paired)
    e_stp(2, 3, CPUREG, M + 16);
    // x0 = ssp
    e_ldr(0, CPUREG, (int)OFF_SSP);
    uint32_t *p_cbz = (uint32_t *)g_cp;
    // cbz x0, Lfb (empty shadow)
    emit32(0);
    // x0 = ssp-1 = idx (ssp<=1024 -> no wrap)
    e_subi(0, 0, 1);
    e_addlsl4(1, CPUREG, 0);
    // x1 = &sstk[2*idx]
    e_addi(1, 1, (unsigned)OFF_SSTK);
    // x2 = guest_ret, x3 = host_ret
    e_ldp(2, 3, 1, 0);
    // x1 = cpu->x[30] (guest return target)
    e_ldr(1, CPUREG, 30 * 8);
    // sub x1, x2, x1 (guest_ret - x30; no flags)
    emit32(0xCB000000u | (1 << 16) | (2 << 5) | 1);
    uint32_t *p_cb1 = (uint32_t *)g_cp;
    // cbnz x1, Lfb (foreign/longjmp)
    emit32(0);
    e_addlsl3(1, CPUREG, 0);
    // x2 = gsp[idx] (guest SP captured at the bl)
    e_ldr(2, 1, (int)OFF_GSP);
    // x1 = current guest SP
    e_mov_from_sp(1);
    // sub x1, x1, x2 (sp - gsp; no flags)
    emit32(0xCB000000u | (2 << 16) | (1 << 5) | 1);
    uint32_t *p_cb2 = (uint32_t *)g_cp;
    // cbnz x1, Lfb (guest_ret matched but wrong frame -> slow)
    emit32(0);
    // FAST: ssp-- (pop)
    e_str(0, CPUREG, (int)OFF_SSP);
    // real x30 = host_ret
    e_movr(30, 3);
    e_ldp(0, 1, CPUREG, M);
    // restore x0..x3 (paired)
    e_ldp(2, 3, CPUREG, M + 16);
    if (g_prof) emit_prof_bump(&g_prof_shret_hit); // A3: §B predicted-return FAST hit (PROF only)
    // host ret -> &Lcont (hardware-RAS predicted)
    e_hret();
    uint8_t *Lfb = g_cp;
    *p_cbz = 0xB4000000u | (((uint32_t)(((uint8_t *)Lfb - (uint8_t *)p_cbz) / 4) & 0x7FFFF) << 5) | 0;
    // cbnz x1
    *p_cb1 = 0xB5000000u | (((uint32_t)(((uint8_t *)Lfb - (uint8_t *)p_cb1) / 4) & 0x7FFFF) << 5) | 1;
    // cbnz x1
    *p_cb2 = 0xB5000000u | (((uint32_t)(((uint8_t *)Lfb - (uint8_t *)p_cb2) / 4) & 0x7FFFF) << 5) | 1;
    e_ldp(0, 1, CPUREG, M);
    // restore x0..x3 (paired)
    e_ldp(2, 3, CPUREG, M + 16);
    if (g_prof) emit_prof_bump(&g_prof_shret_fb); // A3: §B return fell to the IBTC fallback (PROF only)
    // UNWIND/FOREIGN -> IBTC (per-site IC + hash), NOT the dispatcher
    emit_ibranch(30);
}

// ---------------- the translator ----------------
// Translate the basic block at guest address gpc; returns host entry pointer.
// re-target a cond branch to offset d (instrs)
