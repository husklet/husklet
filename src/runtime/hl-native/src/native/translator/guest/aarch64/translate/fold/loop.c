static uint32_t recode_cond(uint32_t in, int64_t d) {
    // cbz/cbnz
    if ((in & 0x7E000000u) == 0x34000000u) return (in & 0xFF00001Fu) | ((uint32_t)(d & 0x7FFFF) << 5);
    // b.cond
    if ((in & 0xFF000010u) == 0x54000000u) return (in & 0xFF00000Fu) | ((uint32_t)(d & 0x7FFFF) << 5);
    // tbz/tbnz
    return (in & 0xFFF8001Fu) | ((uint32_t)(d & 0x3FFF) << 5);
}

// W4E tier-2: emit the in-cache back-edge hotness counter for a hot-candidate self-loop. Runs on the
// TAKEN (loop) edge in tier-1. Flag-free (sub-imm + cbnz never touch NZCV, so the guest's condition
// flags are preserved across the back-edge -- mandatory for bit-exactness when the loop body does not
// itself re-set the tested flags). Counts DOWN from g_t2thresh; on reaching zero it exits R_TIER2 so the
// dispatcher promotes the block, after which this stub is dead.
//
// SCRATCH: two host regs for the counter pointer + value. Under the x16/x17 steal (g_steal1617, the
// aarch64 default) those two host regs are ENGINE-PRIVATE at this block-boundary back-edge -- the exact
// invariant emit_irq_check already relies on to poll cpu->irq with no guest-reg stash -- so use them
// DIRECTLY with no memory spill. The legacy (NOSTEAL1617) path has no free host reg here, so it falls
// back to stashing x9/x10 in the [sp,#-16/-24] slots.
//
// Why the steal path must NOT use the [sp,#-N] slots: AArch64 has NO architectural red zone, so a store
// below the guest SP is only safe if that memory happens to be mapped+writable. A hot pixman NEON fill
// self-loop (`st1 {v0-v3},[x2],#32; subs; b.ge`) reaches this counter with the guest SP shallow on its
// stack; the page just under SP is an untouched anon page that faults on write (EXC_BAD_ACCESS), so the
// old unconditional `stur x9,[sp,#-16]` here crashed GTK4's software/pixman render. The engine already
// established this principle elsewhere -- emit_fold_mem / emit_mangled_x18 spill to cpu->mscratch rather
// than a [sp,#-N] slot precisely because a below-SP slot is not a safe scratch -- and this brings the
// tier-2 counter in line. (The counter never runs concurrently with a fold, and x9/x10 stay LIVE in
// their host regs on the steal path, so the R_TIER2 exit's emit_spill captures the correct guest values.)
static void emit_t2_counter(int slot, uint64_t start, void *body) {
    int rp = g_steal1617 ? 16 : 9;  // counter-pointer scratch
    int rv = g_steal1617 ? 17 : 10; // counter-value scratch
    if (!g_steal1617) {
        e_stur(9, 31, -16);
        e_stur(10, 31, -24);
    }
    // rp = &g_t2cnt[slot] (plain RW data; adrp+add reaches it)
    emit_t2cntptr(rp, slot); // recorded &g_t2cnt[slot] bake (fixed 4-insn + reloc when g_pcache)
    e_ldr(rv, rp, 0);
    // --count (sub immediate: flag-free)
    e_subi(rv, rv, 1);
    e_str(rv, rp, 0);
    uint32_t *p_cbnz = (uint32_t *)g_cp;
    // cbnz rv, Lcont (still counting -> keep looping; flag-free)
    emit32(0);
    // reached 0 -> restore scratch (legacy) + exit to the dispatcher to promote (pc = loop start)
    if (!g_steal1617) {
        e_ldur(9, 31, -16);
        e_ldur(10, 31, -24);
    }
    emit_exit_const(start, R_TIER2);
    uint8_t *Lcont = g_cp;
    *p_cbnz = 0xB5000000u | (((uint32_t)(((uint8_t *)Lcont - (uint8_t *)p_cbnz) / 4) & 0x7FFFF) << 5) | (unsigned)rv;
    if (!g_steal1617) {
        e_ldur(9, 31, -16);
        e_ldur(10, 31, -24);
    }
    // b body  (the loop back-edge, in-cache)
    int64_t d = ((uint8_t *)body - (uint8_t *)g_cp) / 4;
    emit32(0x14000000u | ((uint32_t)d & 0x3FFFFFFu));
}

// W4E tier-2: store-to-load-forwarding hazard guard. Folding the back-edge tightens the loop enough that
// a store immediately followed by a load of the SAME address (e.g. a volatile / aliased RMW of one stack
// slot every iteration) starts hitting an Apple-Silicon store-forwarding replay -- measured as a ~3.7x
// slowdown on a `volatile` counter loop, while the extra tier-1 trampoline branch happened to mask it. So
// if the loop body contains a store whose (size,base,offset) a later load reuses, leave the loop on tier-1
// (no counter, no fold). Pure-store, load-only, and distinct-address load+store loops are NOT flagged and
// still tier up (measured wins). Scans the guest body [start, endpc).
static int loop_has_rmw_hazard(uint64_t start, uint64_t endpc) {
    uint64_t stores[32];
    int ns = 0;
    for (uint64_t p = start; p < endpc; p += 4) {
        uint32_t in = a64_fetch_instruction(p, NULL);
        uint64_t key = 0;
        int opc = -1;
        // load/store unsigned imm12
        if ((in & 0x3B000000u) == 0x39000000u) {
            opc = (in >> 22) & 3;
            key = ((uint64_t)((in >> 30) & 3) << 24) | (((in >> 5) & 31) << 12) | ((in >> 10) & 0xFFF);
        }
        // STUR/LDUR unscaled imm9
        else if ((in & 0x3B200C00u) == 0x38000000u) {
            opc = (in >> 22) & 3;
            key = (1ull << 40) | ((uint64_t)((in >> 30) & 3) << 24) | (((in >> 5) & 31) << 12) | ((in >> 12) & 0x1FF);
        }
        if (opc == 0) {
            if (ns < 32) stores[ns++] = key; // a store
        } else if (opc > 0) {
            for (int i = 0; i < ns; i++)
                if (stores[i] == key) return 1; // a load reusing a stored address -> hazard
        }
    }
    return 0;
}

// W4E tier-2: when a hot self-loop's FIRST instruction writes a vector register (the memcpy/memcmp
// stream loops), the once-per-region cpu->vdirty-set store lands at the loop top and, under the folded
// back-edge, re-executes every iteration (measured ~1.7x on a 1 MiB memcpy vs native). The store is
// idempotent -- vdirty is a sticky flag cleared only by a full spill -- so it need run only ONCE per
// loop ENTRY. During the tier-2 recompile the store is hoisted above a fresh async poll placed at the
// loop top; this holds the loop re-entry point (after the store, at that poll) so emit_selfloop folds
// the back-edge to it instead of to `body`. NULL when no hoist applies (fall back to `body`).
static uint8_t *g_t2_loop_top;
static uint32_t *g_t2_irq_patch;

// W4E tier-2: emit a single-block self-loop's terminating conditional (taken target == block start).
//   tier-1 build: cond -> Lcnt (counter) ; fall-through = loop exit. The counter promotes when hot.
//   tier-2 build: cond -> body DIRECTLY (the fold) ; fall-through = loop exit. One taken branch/iter
//                 instead of tier-1's `b.cond Ltaken; b body` -- native-equivalent. Bit-identical control
//                 flow (same condition, same taken target = loop top). `body` is always a few insns above,
//                 well inside the conditional's imm19/imm14 reach.
static void emit_selfloop(uint32_t in, uint64_t start, uint64_t fall, void *body, int slot) {
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    emit_chain_exit(fall);
    if (g_tier2_build) {
        // Fold the back-edge past the hoisted vdirty store when one was emitted at the loop top
        // (g_t2_loop_top); the store then runs once per entry, never per iteration.
        uint8_t *tgt = g_t2_loop_top ? g_t2_loop_top : (uint8_t *)body;
        int64_t d = (tgt - (uint8_t *)patch) / 4;
        *patch = recode_cond(in, d);
        return;
    }
    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
    *patch = recode_cond(in, d);
    emit_t2_counter(slot, start, body);
}

// ---- LSE atomics idiom upgrade ----
// Distro binaries are built ARMv8.0-baseline, so every atomic is an ldxr/stxr retry
// loop. Apple Silicon has FEAT_LSE: recognize the loop and emit a single atomic op
// (2.29x faster, and it removes the load/store-exclusive monitor region that
// complicates the translator). AL ordering is always safe.
// Emit ONE instruction of an LSE-atomic-loop rewrite, applying the same non-PIE bias-fold / stolen-reg
// mangling the main decode loop would. `is_mem`: the access (swp/ldadd/.../casal) -- a non-PIE LOW [Xn]
// gets +bias (emit_fold_mem, which also mangles stolen Rt/Rn/Rs and re-derives its own field mask, incl
// the atomic value operand Rs). `mask` is used only off the fold path (PIE, or SP-based): the gpr fields
// to mangle if they name a stolen reg. CRUCIAL: the original ldxr/stxr monitor fallback is UNUSABLE when
// an operand is stolen or the base is a low non-PIE pointer -- the per-insn ldr/str it injects between the
// load- and store-exclusive clear the monitor so stxr retries forever. Each rewritten LSE op is a SINGLE
// instruction (no monitor), so the injected spill/fill is harmless, making this the correct path. The
// common clean PIE case still lowers to the bare op -> byte-identical to before.
