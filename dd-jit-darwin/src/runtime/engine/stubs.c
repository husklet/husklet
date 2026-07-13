// dd/runtime/engine -- block-ABI STUBS: prologue/spill, the indirect-branch IBTC + per-site monomorphic
// IC, exit trampolines, block chaining. The engine's emission semantics, built on the host ARM64 assembler
// (host/arm64/asm.c, #included before this file). Split out of the former engine/emit_arm64.c (C7).

static void block_return(void);
// No host register is permanently reserved, so ALL 31 guest GPRs (incl. x28) live
// in the real registers during a block. The cpu pointer is recovered from this
// global only at block boundaries (set by the dispatcher before each run_block).
static int g_trace;
static int g_systrace; // JTS=1: syscall-entry trace only (no per-block dump) -- debug aid
// DDDBG_NOCHAIN=1 (debug-only, default OFF, read once at init): skip direct block chaining so EVERY
// block re-enters the dispatcher -> the JT per-block trace logs every execution (exact per-block PC
// attribution, alignable with `qemu -d exec,nochain`). Mirrors x86's g_nochain. Zero-cost when unset.
static int g_dbg_nochain;
// DDDBG_GPRDUMP=1 (debug-only, default OFF): with JT, dump ALL guest GPRs (x0..x30 + sp) per block for a
// register-value differential vs `qemu -d cpu` (isolate a wrong-VALUE miscompile). Zero-cost when unset.
static int g_dbg_gprdump;
static const char *g_exe_path = "";
// IRQSLIM: gpc of the guest instruction currently being emitted (set each decode step in
// translate_block); used to classify a direct-branch edge as forward vs backward in emit_chain_exit.
static uint64_t g_emit_gpc;

// Prologue: entered with x0 = &cpu. Restore flags, load guest sp + ALL GPRs, x0 last.
static void emit_prologue(void) {
    e_ldr(9, 0, OFF_SP);
    e_mov_sp_from(9);
    e_ldr(9, 0, OFF_NZCV);
    // msr nzcv, x9 (restore flags)
    emit32(0xD51B4200u | 9);
    for (int t = 0; t < 32; t += 2)
        // guest V0..V31 (paired)
        e_ldp_q(t, t + 1, 0, OFF_V + t * 16);
    for (int r = 1; r <= 30; r++)
        // x18,x28 stolen: live only in cpu->x[]
        if (!is_stolen(r)) e_ldr(r, 0, r * 8);
    // mov x28, x0  -- reserve real x28 = cpu (x0 still = cpu here)
    emit32(0xAA0003FCu);
    e_ldr(0, 0, 0);
    // A1: with x16/x17 stolen (engine-private), an IBTC hit needs NO red-zone restore, so it lands
    // directly on `body` and this indirect-entry stub disappears. Legacy (NOSTEAL1617=1): the stub
    // restores the guest x16/x17 stashed by the probe -- a hit jumps to (body-8), direct entries
    // skip it via `b #12`.
    if (!g_steal1617) {
        // b #12 -> body
        emit32(0x14000003u);
        // body_ind: ldr x16, [sp,#-16]
        e_ldur(16, 31, -16);
        //           ldr x17, [sp,#-24]
        e_ldur(17, 31, -24);
    }
}

// (SIMD-clean syscall exit): when the guest vector file is already current in cpu->V, a plain
// R_SYSCALL block-exit can skip the 16 stp_q V-register save (emit_spill_gpr); the always-full prologue
// reload republishes cpu->V on re-entry, and every non-slim exit keeps the FULL spill. Currency is tracked
// at RUNTIME in cpu->vdirty: the first vector-writing instruction of a region stores the (nonzero) cpu
// pointer there (translate.c, gated by insn_touches_vreg over-approximating any V reference), and every
// FULL spill clears it. Runtime rather than a static per-block flag because blocks CHAIN without spilling,
// so a vector-dirty region can reach a statically-"clean" syscall block with host V != cpu->V. A syscall
// that itself writes cpu->V (sigreturn) is republished by the prologue reload, so V state is never lost.
static int g_blk_vdirty;   // per-region latch: has the vdirty-set store already been emitted this region?
static int g_slimsys = -1; // DDJIT_NOSLIMSYS=1 -> 0 = byte-identical full-spill baseline (A/B kill switch)

static int slimsys_on(void) {
    if (g_slimsys < 0) g_slimsys = getenv("DDJIT_NOSLIMSYS") ? 0 : 1;
    return g_slimsys;
}

// GPR + flags + SP spill, WITHOUT the V-register save. Leaves x0 = &cpu (callers rely on it). Must NOT
// touch the guest red zone [sp,#-16..] (see emit_spill).
static void emit_spill_gpr(void) {
    for (int r = 0; r <= 30; r++)
        // guest x0..x30 (x0 included -> no red-zone stash needed); skip x18 (volatile) + x28 (= cpu)
        if (!is_stolen(r)) e_str(r, CPUREG, r * 8);
    // x0 is saved now; reuse it as scratch
    emit32(0xD53B4200u | 0); // mrs x0, nzcv -> cpu->nzcv
    e_str(0, CPUREG, OFF_NZCV);
    e_mov_from_sp(0);
    e_str(0, CPUREG, OFF_SP); // cpu->sp
    e_movr(0, CPUREG);        // callers expect x0 = &cpu after the spill
}

// Spill: store all guest GPRs+sp+flags+V to cpu-> via x28 (= &cpu, stolen/maintained for the whole
// block). Must NOT touch the guest red zone [sp,#-16..]: the guest (e.g. Go runtime.clone.abi0) keeps
// live data just below SP across a syscall block-exit, and a real kernel preserves it. Leaves x0 = &cpu.
static void emit_spill(void) {
    for (int t = 0; t < 32; t += 2)
        // guest V0..V31 (paired)
        e_stp_q(t, t + 1, CPUREG, OFF_V + t * 16);
    emit_spill_gpr();
    // A full spill republishes host V -> cpu->V, so cpu->V is now current: clear the dirty flag (str xzr).
    e_str(31, CPUREG, (int)OFF_VDIRTY);
}

static void emit_exit_const(uint64_t pc, uint64_t reason) {
    // a plain R_SYSCALL exit skips the V spill WHEN cpu->V is current (cpu->vdirty==0). The
    // decision is at RUNTIME because blocks chain without spilling -- a vectorized region can chain into
    // this (statically clean) syscall block, leaving host V dirty vs cpu->V. x16 is engine scratch (stolen)
    // so it is dead here; gate on g_steal1617 so a free scratch reg exists. x0 = cpu after either spill.
    if (reason == R_SYSCALL && slimsys_on() && g_steal1617) {
        e_ldr(16, CPUREG, (int)OFF_VDIRTY);
        uint32_t *p_cb = (uint32_t *)g_cp;
        emit32(0); // cbnz x16, Lfull
        emit_spill_gpr();
        uint32_t *p_b = (uint32_t *)g_cp;
        emit32(0x14000000u); // b Lcont
        uint8_t *Lfull = g_cp;
        *p_cb = 0xB5000000u | (((uint32_t)(((uint8_t *)Lfull - (uint8_t *)p_cb) / 4) & 0x7FFFF) << 5) | 16;
        emit_spill();
        uint8_t *Lcont = g_cp;
        *p_b = 0x14000000u | ((uint32_t)(((uint8_t *)Lcont - (uint8_t *)p_b) / 4) & 0x3FFFFFF);
    } else {
        emit_spill();
    }
    e_movconst(9, pc);
    e_str(9, 0, OFF_PC);
    e_movconst(9, reason);
    e_str(9, 0, OFF_RSN);
    emit_blockret(9);
    // x0=cpu -> block_return
    e_br(9);
}

// SMC: exit a block at a guest `ic ivau, Xt` (R_ICFLUSH). Like emit_exit_const(pc, R_ICFLUSH) but it also
// spills the invalidated guest VA (cpu->x[va_reg]) into cpu->smc_va so the dispatcher can do PRECISE
// invalidation. emit_spill() has already written every NON-stolen guest reg to cpu->x[]; stolen regs
// (x16/x17/x18/x30) keep their guest value in cpu->x[] continuously, so cpu->x[va_reg] is correct either
// way -- read it back and stash it. pc resumes PAST the ic ivau.
static void emit_exit_icflush(uint64_t pc, int va_reg) {
    emit_spill(); // x0 = cpu; all guest regs now in cpu->x[]
    e_ldr(9, 0, va_reg * 8);
    // cpu->smc_va = cpu->x[va_reg]  (the invalidated VA)
    e_str(9, 0, (int)OFF_SMCVA);
    e_movconst(9, pc);
    e_str(9, 0, OFF_PC);
    e_movconst(9, R_ICFLUSH);
    e_str(9, 0, OFF_RSN);
    emit_blockret(9);
    e_br(9);
}

static void emit_exit_reg(int rn, uint64_t reason) {
    // x0 = cpu
    emit_spill();
    e_ldr(9, 0, rn * 8);
    // cpu->pc = cpu->x[rn]
    e_str(9, 0, OFF_PC);
    e_movconst(9, reason);
    e_str(9, 0, OFF_RSN);
    emit_blockret(9);
    e_br(9);
}

// IBTC: inline-cache an indirect branch (br/blr/ret). If the guest target equals this
// site's cached target, jump straight into the cached block's indirect-entry (body-8) --
// no spill, no V-register save, no dispatcher round-trip. On a miss, take the full exit
// and hand the cache-site address to the dispatcher, which fills it once the target is
// resolved. x16/x17 are the scratch pair; the indirect-entry stub restores them.

// A1 steal path: x16/x17 are engine-private, so the probe needs NO red-zone stash/restore and the
// monomorphic hit collapses to 5 instrs / 0 mem-ops:
//   ldr x16,Lsite_tgt ; sub x16,x16,xT ; cbnz x16,Lhash ; ldr x16,Lsite_body ; br x16  (-> body)
// The shared-hash miss tail uses x16/x17 freely (no guest values to preserve). For an indirect branch
// THROUGH a stolen reg (x16/x17/x30) the guest target lives in cpu->x[rn]; load it into the free host
// link reg x30 (also stolen) so the path has 3 distinct host regs: target(x30) + scratch x16/x17.
static void emit_ibranch_steal(int rn) {
    int treg = rn;
    if (rn == 16 || rn == 17 || rn == 30) {
        e_ldr(30, CPUREG, rn * 8);
        treg = 30;
    }
    // --- per-site monomorphic cache ---
    uint32_t *p_ldrt = (uint32_t *)g_cp;
    // ldr x16, Lsite_tgt
    emit32(0);
    // sub x16, x16, xTreg
    emit32(0xCB000000u | (treg << 16) | (16 << 5) | 16);
    uint32_t *p_cbslow = (uint32_t *)g_cp;
    // cbnz x16, Lhash
    emit32(0);
    uint32_t *p_ldrb = (uint32_t *)g_cp;
    // ldr x16, Lsite_body
    emit32(0);
    // HIT -> body (no restore stub)
    e_br(16);
    uint32_t *Lhash = (uint32_t *)g_cp;
    // --- shared hash IBTC ---
    // ubfx x16, xTreg, #2, #16  ((xTreg>>2) & (IBTC_N-1), IBTC_N=64Ki)
    emit32(0xD3424400u | (treg << 5) | 16);
    // x17 = &g_ibtc
    emit_ibtcptr(17);
    // add x16, x17, x16, lsl #4  (slot ptr)
    emit32(0x8B000000u | (16 << 16) | (4 << 10) | (17 << 5) | 16);
    // atomic 128-bit load {target,body} (LSE2): x17=slot.target, x16=slot.body
    e_ldp(17, 16, 16, 0);
    // sub x17, x17, xTreg
    emit32(0xCB000000u | (treg << 16) | (17 << 5) | 17);
    uint32_t *p_cbnz = (uint32_t *)g_cp;
    // cbnz x17, Lmiss
    emit32(0);
    // x16 = slot.body -> HIT -> jump
    e_br(16);
    uint32_t *miss = (uint32_t *)g_cp;
    // slow path: emit_spill skips stolen regs, so cpu->x[rn] (the guest target) is intact.
    emit_spill();
    // cpu->pc = guest target (cpu->x[rn])
    e_ldr(9, 0, rn * 8);
    e_str(9, 0, OFF_PC);
    e_movconst(9, R_BRANCH);
    e_str(9, 0, OFF_RSN);
    uint32_t *p_adr = (uint32_t *)g_cp;
    // adr x9, Lsite_tgt -> dispatcher fills both caches
    emit32(0);
    e_str(9, 0, OFF_ICSITE);
    emit_blockret(9);
    e_br(9);
    if ((uint64_t)g_cp & 7) emit32(0);
    uint8_t *Lt = g_cp;
    *(uint64_t *)g_cp = 0;
    g_cp += 8;
    uint8_t *Lb = g_cp;
    *(uint64_t *)g_cp = 0;
    g_cp += 8;
    *p_ldrt = 0x58000000u | (((uint32_t)((Lt - (uint8_t *)p_ldrt) / 4) & 0x7FFFF) << 5) | 16;
    *p_cbslow = 0xB5000000u | (((uint32_t)(((uint8_t *)Lhash - (uint8_t *)p_cbslow) / 4) & 0x7FFFF) << 5) | 16;
    *p_ldrb = 0x58000000u | (((uint32_t)((Lb - (uint8_t *)p_ldrb) / 4) & 0x7FFFF) << 5) | 16;
    *p_cbnz = 0xB5000000u | (((uint32_t)(((uint8_t *)miss - (uint8_t *)p_cbnz) / 4) & 0x7FFFF) << 5) | 17;
    int64_t ao = Lt - (uint8_t *)p_adr;
    *p_adr = 0x10000000u | ((uint32_t)(ao & 3) << 29) | (((uint32_t)(ao >> 2) & 0x7FFFF) << 5) | 9;
    pc_record_icsite(Lt); // {target,body} cache holds an arena body ptr -> neutralize on reload
}

static void emit_ibranch(int rn) {
    if (rn == 18 || rn == 28) {
        emit_exit_reg(rn, R_BRANCH);
        return;
        // host x18 volatile / x28=cpu: can't hold target
    }
    if (g_steal1617) {
        emit_ibranch_steal(rn);
        return;
    }
    if (rn == 30)
        // ret/br/blr x30: load guest x30 into the FREE host link reg, then
        e_ldr(30, CPUREG, 30 * 8);
    // run the normal IBTC (per-site + shared hash) -- fast, lock-free, correct
    if (rn == 16 || rn == 17) {
        // Hot case: a function pointer called via x16/x17 (qsort comparator, vtable). The
        // target IS a scratch reg, read from its red-zone copy. A per-site monomorphic
        // cache turns the common "same target every time" case into a direct compare+jump;
        // a miss falls to the shared hash (recompute since x16/x17 are scratch).
        int other = (rn == 16) ? 17 : 16, tslot = (rn == 16) ? -16 : -24;
        e_stur(16, 31, -16);
        e_stur(17, 31, -24);
        // --- per-site monomorphic fast path: xRn (target) still live here ---
        uint32_t *p_ldrt = (uint32_t *)g_cp;
        // ldr xOTHER, Lsite_tgt
        emit32(0);
        // sub xOTHER, xOTHER, xRn
        emit32(0xCB000000u | (rn << 16) | (other << 5) | other);
        uint32_t *p_cbslow = (uint32_t *)g_cp;
        // cbnz xOTHER, Lslow
        emit32(0);
        uint32_t *p_ldrb = (uint32_t *)g_cp;
        // ldr x16, Lsite_body
        emit32(0);
        // HIT -> body_ind (restores x16/x17)
        e_br(16);
        uint32_t *Lslow = (uint32_t *)g_cp;
        // --- shared hash IBTC (recompute slot) ---
        // x16 = target
        e_ldur(16, 31, tslot);
        // stash target at [sp,-32]
        e_stur(16, 31, -32);
        // lsr x17, x16, #2
        emit32(0xD342FC00u | (16 << 5) | 17);
        // and x17, x17, #0xFFFF   (IBTC_N-1, IBTC_N=64Ki)
        emit32(0x92403C00u | (17 << 5) | 17);
        emit_ibtcptr(16);
        emit32(0x8B000000u | (17 << 16) | (4 << 10) | (16 << 5) | 16); // x16 = slot ptr
        // W5C: atomic 128-bit load of the {target,body} pair (single-copy atomic under LSE2 since the
        // slot is 16-byte aligned) -> a peer's 128-bit release publish is never observed torn. Non-
        // writeback ldp with Rt2==Rn is well-defined (only Rt1==Rt2 is constrained). x17=slot.target,
        // x16=slot.body. Target is a scratch reg here, so reload the guest target from the red zone to
        // compare; stash body across that reload (x16/x17 are the only free GPRs on this path).
        e_ldp(17, 16, 16, 0);
        // stash body at [sp,-40], reload guest target stashed at [sp,-32]
        e_stur(16, 31, -40);
        e_ldur(16, 31, -32);
        // sub x17, x17, x16  (slot.target - guest target)
        emit32(0xCB000000u | (16 << 16) | (17 << 5) | 17);
        uint32_t *p_cbnz = (uint32_t *)g_cp;
        // cbnz x17, miss
        emit32(0);
        // x16 = body ; HIT -> jump (body_ind restores x16/x17)
        e_ldur(16, 31, -40);
        e_br(16);
        uint32_t *miss = (uint32_t *)g_cp;
        e_ldur(16, 31, -16);
        e_ldur(17, 31, -24);
        emit_spill();
        e_ldr(9, 0, rn * 8);
        e_str(9, 0, OFF_PC);
        e_movconst(9, R_BRANCH);
        e_str(9, 0, OFF_RSN);
        uint32_t *p_adr = (uint32_t *)g_cp;
        // adr x9, Lsite_tgt -> dispatcher fills both caches
        emit32(0);
        e_str(9, 0, OFF_ICSITE);
        emit_blockret(9);
        e_br(9);
        if ((uint64_t)g_cp & 7) emit32(0);
        uint8_t *Lt = g_cp;
        *(uint64_t *)g_cp = 0;
        g_cp += 8;
        uint8_t *Lb = g_cp;
        *(uint64_t *)g_cp = 0;
        g_cp += 8;
        *p_ldrt = 0x58000000u | (((uint32_t)((Lt - (uint8_t *)p_ldrt) / 4) & 0x7FFFF) << 5) | other;
        *p_cbslow = 0xB5000000u | (((uint32_t)(((uint8_t *)Lslow - (uint8_t *)p_cbslow) / 4) & 0x7FFFF) << 5) | other;
        *p_ldrb = 0x58000000u | (((uint32_t)((Lb - (uint8_t *)p_ldrb) / 4) & 0x7FFFF) << 5) | 16;
        *p_cbnz = 0xB5000000u | (((uint32_t)(((uint8_t *)miss - (uint8_t *)p_cbnz) / 4) & 0x7FFFF) << 5) | 17;
        int64_t ao = Lt - (uint8_t *)p_adr;
        *p_adr = 0x10000000u | ((uint32_t)(ao & 3) << 29) | (((uint32_t)(ao >> 2) & 0x7FFFF) << 5) | 9;
        pc_record_icsite(Lt); // {target,body} cache holds an arena body ptr -> neutralize on reload
        return;
    }
    e_stur(16, 31, -16);
    // stash x16/x17 scratch
    e_stur(17, 31, -24);
    // --- per-site monomorphic cache (ahead of the shared hash; ret/br/blr are usually monomorphic) ---
    uint32_t *p_ldrt = (uint32_t *)g_cp;
    // ldr x16, Lsite_tgt
    emit32(0);
    // sub x16, x16, xRn   (xRn live; rn != 16/17 here)
    emit32(0xCB000000u | (rn << 16) | (16 << 5) | 16);
    uint32_t *p_cbslow = (uint32_t *)g_cp;
    // cbnz x16, Lhash
    emit32(0);
    uint32_t *p_ldrb = (uint32_t *)g_cp;
    // ldr x16, Lsite_body
    emit32(0);
    // HIT -> body_ind (restores x16/x17)
    e_br(16);
    uint32_t *Lhash = (uint32_t *)g_cp;
    // ubfx x16, xRn, #2, #16  ((xRn>>2) & (IBTC_N-1), IBTC_N=64Ki)
    emit32(0xD3424400u | (rn << 5) | 16);
    // x17 = &g_ibtc  (2 instr)
    emit_ibtcptr(17);
    // add x16, x17, x16, lsl #4  (slot ptr)
    emit32(0x8B000000u | (16 << 16) | (4 << 10) | (17 << 5) | 16);
    // W5C: atomic 128-bit load of {target,body} (single-copy atomic, LSE2) -> never torn.
    // x16=slot ptr -> x17=slot.target, x16=slot.body. The guest target stays live in xRn
    // (a normal guest reg; rn is never 16/17/18/28 here), so no red-zone reload is needed.
    e_ldp(17, 16, 16, 0);
    // sub x17, x17, xRn
    emit32(0xCB000000u | (rn << 16) | (17 << 5) | 17);
    uint32_t *p_cbnz = (uint32_t *)g_cp;
    // cbnz x17, Lmiss
    emit32(0);
    // x16 = slot.body (body_ind) -> HIT -> jump
    e_br(16);
    uint32_t *miss = (uint32_t *)g_cp;
    e_ldur(16, 31, -16);
    // restore scratch
    e_ldur(17, 31, -24);
    // slow path: x0 = cpu
    emit_spill();
    e_ldr(9, 0, rn * 8);
    // cpu->pc = guest target
    e_str(9, 0, OFF_PC);
    e_movconst(9, R_BRANCH);
    e_str(9, 0, OFF_RSN);
    uint32_t *p_adr = (uint32_t *)g_cp;
    // adr x9, Lsite_tgt -> dispatcher fills the per-site cache
    emit32(0);
    e_str(9, 0, OFF_ICSITE);
    emit_blockret(9);
    e_br(9);
    if ((uint64_t)g_cp & 7) emit32(0);
    uint8_t *Lt = g_cp;
    *(uint64_t *)g_cp = 0;
    // Lsite_tgt
    g_cp += 8;
    uint8_t *Lb = g_cp;
    *(uint64_t *)g_cp = 0;
    // Lsite_body
    g_cp += 8;
    *p_ldrt = 0x58000000u | (((uint32_t)((Lt - (uint8_t *)p_ldrt) / 4) & 0x7FFFF) << 5) | 16;
    *p_cbslow = 0xB5000000u | (((uint32_t)(((uint8_t *)Lhash - (uint8_t *)p_cbslow) / 4) & 0x7FFFF) << 5) | 16;
    *p_ldrb = 0x58000000u | (((uint32_t)((Lb - (uint8_t *)p_ldrb) / 4) & 0x7FFFF) << 5) | 16;
    *p_cbnz = 0xB5000000u | (((uint32_t)(((uint8_t *)miss - (uint8_t *)p_cbnz) / 4) & 0x7FFFF) << 5) | 17;
    int64_t ao = Lt - (uint8_t *)p_adr;
    *p_adr = 0x10000000u | ((uint32_t)(ao & 3) << 29) | (((uint32_t)(ao >> 2) & 0x7FFFF) << 5) | 9;
    pc_record_icsite(Lt); // {target,body} cache holds an arena body ptr -> neutralize on reload
}

// ---- IBSLIM: recognized interpreter-dispatch `br` sites ----
// A site recognized by is_interp_dispatch_br (a `br xN` fed by a load from a table of code
// pointers -- CPython computed goto, sqlite VDBE, any switch-dispatch interpreter) is megamorphic
// by construction: its per-site monomorphic IC hits ~5% (measured, CPython-shaped bench) yet costs
// a literal load + compare + branch on EVERY dispatch. emit_hash_tail is the slimmed replacement:
// straight to the shared-hash IBTC (whose hit rate at such sites is ~99.997%). Steal-mode only
// (x16/x17/x30 engine-private); rn is a live, never-stolen guest reg. Miss exits with ic_site=1
// (shared-hash-only fill: there are no per-site literals here).
static void emit_hash_tail(int rn) {
    emit32(0xD3424400u | (rn << 5) | 16); // ubfx x16, xRn, #2, #16  ((xRn>>2) & (IBTC_N-1))
    // RECORDED bakes only (emit_ibtcptr / emit_blockret, byte-identical to the raw emitters when
    // the pcache is off). A raw e_adrp_add/e_movconst here escaped pcache relocation, so a WARM run's
    // first recognized-dispatch miss branched to the SAVING process's block_return / read the saving
    // process's &g_ibtc (adrp is PC-relative to a differently-based arena) -> silent death on load.
    emit_ibtcptr(17);
    emit32(0x8B000000u | (16 << 16) | (4 << 10) | (17 << 5) | 16); // add x16, x17, x16, lsl #4
    e_ldp(17, 16, 16, 0);                                          // {x17,x16} = {slot.target, slot.body} (LSE2)
    emit32(0xCB000000u | (rn << 16) | (17 << 5) | 17);             // sub x17, x17, xRn (no flags)
    uint32_t *p_cbnz = (uint32_t *)g_cp;
    emit32(0); // cbnz x17, Lmiss
    e_br(16);  // HIT -> body (regs live)
    uint32_t *miss = (uint32_t *)g_cp;
    emit_spill();
    e_ldr(9, 0, rn * 8);
    e_str(9, 0, OFF_PC); // cpu->pc = guest target
    e_movconst(9, R_BRANCH);
    e_str(9, 0, OFF_RSN);
    e_movconst(9, 1); // ic_site = 1 -> dispatcher fills the shared hash only
    e_str(9, 0, OFF_ICSITE);
    emit_blockret(9); // recorded (see the emit_ibtcptr note above)
    e_br(9);
    *p_cbnz = 0xB5000000u | (((uint32_t)(((uint8_t *)miss - (uint8_t *)p_cbnz) / 4) & 0x7FFFF) << 5) | 17;
}

// A direct-branch exit to a CONSTANT target. If the target is already translated,
// emit a single `b target.body` (regs stay live, no dispatcher round-trip). Otherwise
// emit a full spill-exit whose first instruction is remembered so it can later be
// back-patched into that `b` once the target gets translated.
static void emit_chain_exit(uint64_t target) {
    void *body = map_body(target);
    uint32_t *slot = (uint32_t *)g_cp;
    // IRQSLIM: a FORWARD direct edge may enter past the block's 2-insn poll (body+8) -- any
    // in-cache cycle still polls via its backward or indirect edge (see g_fwdskip in cache.c).
    // g_emit_gpc is the guest pc of the branch being translated.
    int fwd = g_fwdskip && target > g_emit_gpc;
    if (body) {
        int64_t d = (((uint8_t *)body + (fwd ? g_fwdskip : 0)) - (uint8_t *)slot) / 4;
        // b target.body(+8)
        emit32(0x14000000u | ((uint32_t)d & 0x3FFFFFFu));
        return;
    }
    add_pend3(slot, target, 0, fwd);
    // slot (= first insn) is patched to `b body(+8)` later
    emit_exit_const(target, R_BRANCH);
}

static int64_t sext(uint64_t v, int bits) {
    uint64_t m = 1ull << (bits - 1);
    return (int64_t)((v ^ m) - m);
}
