static void emit_fold_mem(uint32_t in, int emit_bus_guard) {
    int mask = gpr_field_mask(in), base = (in >> 5) & 31;
    // LSE atomic memory ops (LDADD/SWP/...) carry an operand register Rs at [20:16] that gpr_field_mask
    // does not flag; mark it (bit2) so the scratch picker never aliases Rs and a stolen Rs is mangled.
    if ((in & 0x3B200C00u) == 0x38200000u) mask |= 4;
    int regoff = (in & 0x3B200C00u) == 0x38200800u;
    int wb = 0; // single-register writeback: 1 = pre-index, 2 = post-index
    int64_t wbimm = 0;
    if (((in >> 27) & 7) == 7 && !((in >> 24) & 1)) {
        int o = (in >> 10) & 3;
        wb = (o == 3) ? 1 : (o == 1) ? 2 : 0;
        if (wb) wbimm = sext((uint64_t)((in >> 12) & 0x1FF), 9);
    } else if ((in & 0x3A000000u) == 0x28000000u) {
        int o = (in >> 23) & 3;
        wb = o == 3 ? 1 : o == 1 ? 2 : 0;
        if (wb) wbimm = sext((uint64_t)((in >> 15) & 0x7f), 7) * (int64_t)(a64_mem_bytes(in) / 2);
    }
    int sc[4];
    fold_mem_scratch(in, sc); // shared with fault-time reconstruction: same slot mapping
    int Sb = sc[0], T = sc[1], T2 = sc[2], Tm = regoff ? sc[3] : -1;
    // Spill scratch originals to cpu->mscratch[4..7], NOT the stack: the fold runs on EVERY guest memory op,
    // and an async host signal (e.g. Go's SIGURG preemption) would clobber a [sp,#-N] red-zone slot. This
    // mirrors emit_mangled_x18 (it uses mscratch[0..3]); the slots are disjoint so the nested call is safe.
    int M = (int)OFF_MSCRATCH;
    e_str(Sb, CPUREG, M + 32);
    e_str(T, CPUREG, M + 40);
    e_str(T2, CPUREG, M + 48);
    if (regoff) e_str(Tm, CPUREG, M + 56);
    if (base == 31)
        e_mov_from_sp(Sb);
    else if (is_stolen(base))
        e_ldr(Sb, CPUREG, base * 8); // guest base from cpu->x[base]
    else
        e_movr(Sb, base); // guest base from the live host reg
    if (regoff) {
        int rm = (in >> 16) & 31, opt = (in >> 13) & 7, S = (in >> 12) & 1, v = (in >> 26) & 1;
        int sz = v ? ((((in >> 22) & 3) >> 1) << 2) | ((in >> 30) & 3) : (in >> 30) & 3;
        int amt = S ? sz : 0, mreg = rm;
        if (is_stolen(rm)) {
            e_ldr(Tm, CPUREG, rm * 8); // index from cpu->x[rm]
            mreg = Tm;
        }
        // Sb = Xn + extend(Xm)  (extended-register add; option/amount mirror the load's index extend)
        emit32(0x8B200000u | (mreg << 16) | (opt << 13) | ((unsigned)(amt & 7) << 10) | (Sb << 5) | Sb);
    }
    int64_t access_off = regoff ? 0 : a64_fold_mem_offset(in, wb);
    if (access_off) {
        e_movconst(T, (uint64_t)(access_off < 0 ? -access_off : access_off));
        emit32((access_off < 0 ? 0xCB000000u : 0x8B000000u) | ((unsigned)T << 16) | ((unsigned)Sb << 5) |
               (unsigned)Sb); /* sub/add Sb,Sb,T */
    }
    if (guestbase_on()) {
        // Bias iff the EA falls in THIS image's span [g_nonpie_lo, g_nonpie_hi). Fast path: a >= 4GiB address is
        // never the low non-PIE image (stack/heap/mmap/ld.so/libc all live above the 4GiB __PAGEZERO) -> skip
        // with no flag traffic (the common case). For a < 4GiB EA, do the exact two-sided range test.
        emit32(0xD360FC00u | (Sb << 5) | T); // lsr T, Sb, #32
        uint32_t *p_hi = (uint32_t *)g_cp;
        emit32(0);
        emit32(0xD53B4200u | T2); // mrs T2,nzcv
        e_movconst(T, g_nonpie_lo);
        emit32(0xEB000000u | (T << 16) | (Sb << 5) | 31);
        uint32_t *p_lo1 = (uint32_t *)g_cp;
        emit32(0);
        e_movconst(T, g_nonpie_hi);
        emit32(0xEB000000u | (T << 16) | (Sb << 5) | 31);
        uint32_t *p_lo2 = (uint32_t *)g_cp;
        emit32(0);
        e_movconst(T, g_nonpie_bias);
        emit32(0x8B000000u | (T << 16) | (Sb << 5) | Sb);
        uint8_t *Llo = g_cp;
        emit32(0xD51B4200u | T2);
        uint8_t *Lhi = g_cp;
        *p_hi = 0xB5000000u | (((uint32_t)(((uint8_t *)Lhi - (uint8_t *)p_hi) / 4) & 0x7FFFF) << 5) | T;
        *p_lo1 = 0x54000000u | (((uint32_t)(((uint8_t *)Llo - (uint8_t *)p_lo1) / 4) & 0x7FFFF) << 5) | 3;
        *p_lo2 = 0x54000000u | (((uint32_t)(((uint8_t *)Llo - (uint8_t *)p_lo2) / 4) & 0x7FFFF) << 5) | 2;
    }
    uint32_t m;
    int emask = mask;
    if (regoff) {
        // de-index: register-offset -> unscaled [Sb,#0]; keep size/V/opc/Rt, clear bits[21:10] + base->Sb
        m = (in & ~0x003FFC00u & ~(0x1Fu << 5)) | ((unsigned)Sb << 5);
        emask &= ~4; // Rm now folded into Sb -> drop it from the mangle set
    } else if (wb) {
        // The architectural access address is already in Sb.  Convert the
        // pre/post-index opcode to an unscaled zero-offset access; writeback is
        // applied separately to the original guest base below.
        if ((in & 0x3A000000u) == 0x28000000u) {
            m = (in & ~(3u << 23) & ~(0x7fu << 15)) | (2u << 23);
        } else {
            m = in & ~(0x3u << 10) & ~(0x1FFu << 12);
        }
        m = (m & ~(0x1Fu << 5)) | ((unsigned)Sb << 5);
    } else {
        m = (in & ~(0x1Fu << 5)) | ((unsigned)Sb << 5);
        if ((in & 0x3b000000u) == 0x39000000u)
            m &= ~(0xfffu << 10); /* unsigned immediate */
        else if ((in & 0x3b200000u) == 0x38000000u)
            m &= ~(0x1ffu << 12); /* unscaled immediate */
        else if ((in & 0x3a000000u) == 0x28000000u)
            m &= ~(0x7fu << 15); /* pair immediate */
    }
    if (emit_bus_guard && jit_guest_bus_active()) {
        /* Preserve the exact EA, then restore scratch registers before
           emit_spill captures the architectural register file.  Spilling Sb,
           T, T2, or Tm while they contain translator temporaries silently
           corrupts guest registers on every guarded miss. */
        e_str(Sb, CPUREG, OFF_BUS_EA);
        e_ldr(16, CPUREG, OFF_BUS_FORCE);
        emit32(0xB9400000u | (16u << 5) | 16u);
        uint32_t *inactive_fast = (uint32_t *)g_cp;
        emit32(0);
        if (regoff) e_ldr(Tm, CPUREG, M + 56);
        e_ldr(Sb, CPUREG, M + 32);
        e_ldr(T, CPUREG, M + 40);
        e_ldr(T2, CPUREG, M + 48);
        emit_a64_bus_guard_saved(a64_mem_bytes(in), g_emit_gpc);
        e_ldr(Sb, CPUREG, OFF_BUS_EA);
        uint8_t *resume_inactive = g_cp;
        *inactive_fast =
            0x36000000u | (((uint32_t)((resume_inactive - (uint8_t *)inactive_fast) / 4) & 0x3FFFu) << 5) | 16u;
    }
    struct a64_soft_guard soft =
        emit_a64_soft_guard_begin(Sb, T, T2, a64_mem_bytes(in), a64_mem_required(in), g_emit_gpc);
    a64_soft_guard_restore(&soft, Sb, M + 32);
    a64_soft_guard_restore(&soft, T, M + 40);
    a64_soft_guard_restore(&soft, T2, M + 48);
    if (regoff) a64_soft_guard_restore(&soft, Tm, M + 56);
    if (uses_x18(m, emask))
        emit_mangled_x18(m, emask); // stolen Rt/Rt2/Rs (base now names non-stolen Sb)
    else
        emit32(m);
    emit_a64_soft_guard_end(&soft);
    if (wb) { // writeback updates the LOW guest base (Rt != base for loads -> safe to do after the access)
        unsigned a = (unsigned)(wbimm < 0 ? -wbimm : wbimm);
        if (is_stolen(base)) {
            e_ldr(T, CPUREG, base * 8);
            if (wbimm < 0)
                e_subi(T, T, a);
            else
                e_addi(T, T, a);
            e_str(T, CPUREG, base * 8);
        } else if (wbimm < 0)
            e_subi(base, base, a);
        else
            e_addi(base, base, a);
    }
    if (regoff) e_ldr(Tm, CPUREG, M + 56); // restore scratch originals
    e_ldr(Sb, CPUREG, M + 32);
    e_ldr(T, CPUREG, M + 40);
    e_ldr(T2, CPUREG, M + 48);
    emit_a64_soft_bounce_commit(g_emit_gpc + 4);
}

// ---- AdvSIMD load/store STRUCTURE bias-fold (ld1/st1 .. ld4/st4, single & multiple, ld1r/ld2r/...) ----
// is_foldable_mem deliberately omits these (their effective address is a bare base Xn with no offset or
// index), so without this they fall to the nonpie_fixup safety net -- a SIGSEGV per access. glibc's NEON
// strlen/memcpy stream the image's LOW absolute pointers through `ld1 {v0.16b},[x1]`, which then traps once
// per 16 bytes (gcc -shared spins). Fold them exactly like emit_fold_mem: materialize the base in a scratch,
// add g_nonpie_bias iff it lands in the image span [lo,hi), and run the access against the biased scratch.
// The EA of a structure op IS the base (no immediate, no index), so the < image-span test on Xn is exact.
// Identifier: bit31=0, bits[29:25]=00110; bit24 = single(1)/multiple(0), bit23 = post-index writeback. Rt is
// a V register (no GP mangle needed) -- only the base Xn[9:5] and a register post-index Rm[20:16] are GP.
static int is_advsimd_struct(uint32_t in) {
    return (in & 0xBE000000u) == 0x0C000000u;
}

// Bytes transferred by an AdvSIMD structure op -- the implicit increment of an immediate post-index (Rm==31).
static int advsimd_struct_bytes(uint32_t in) {
    int q = (in >> 30) & 1;
    if (!((in >> 24) & 1)) { // load/store MULTIPLE structures: register count from opcode[15:12]
        int regs;
        switch ((in >> 12) & 0xF) {
        case 0x0:
        case 0x2: regs = 4; break; // LD4/ST4, LD1 x4
        case 0x4:
        case 0x6: regs = 3; break; // LD3/ST3, LD1 x3
        case 0x8:
        case 0xA: regs = 2; break; // LD2/ST2, LD1 x2
        case 0x7: regs = 1; break; // LD1 x1
        default: regs = 0; break;  // unallocated (never reached: the access would have faulted)
        }
        return regs * (q ? 16 : 8);
    }
    // load/store SINGLE structure: selem consecutive elements, each (1<<scale) bytes.
    int opcode = (in >> 13) & 7, R = (in >> 21) & 1, size = (in >> 10) & 3;
    int scale = (opcode >> 1) & 3, selem = (((opcode & 1) << 1) | R) + 1;
    if (scale == 3) scale = size; // LD#R replicate: element width is `size`
    return selem * (1 << scale);
}

// Fold an AdvSIMD load/store structure op (see is_advsimd_struct). Mirrors emit_fold_mem's range-gated bias
// (flag-safe: NZCV saved across the compares) but is simpler -- the EA is just the base, and Rt names a V
// register so it never needs mangling. The access is rebased onto the biased scratch as the no-offset form;
// any post-index writeback (immediate or register increment) is then applied to the LOW guest base, matching
// nonpie_fixup's writeback semantics. Caller gates on guestbase_on() && !in_excl && base != SP.
static void emit_fold_advsimd_struct(uint32_t in) {
    int base = (in >> 5) & 31, post = (in >> 23) & 1;
    int rm = post ? (int)((in >> 16) & 31) : 31; // post-index increment register (31 = immediate form)
    // Scratch set: Sb (biased base / effective addr), T (compares + temps), T2 (saved NZCV / wb temp). The
    // only GP operands to avoid are the base and, for a register post-index, Rm.
    unsigned usedmask = (1u << base) | (rm != 31 ? (1u << rm) : 0u);
    int sc[3], n = 0;
    for (int r = 0; r <= 30 && n < 3; r++)
        if (!(usedmask & (1u << r)) && !is_stolen(r)) sc[n++] = r;
    int Sb = sc[0], T = sc[1], T2 = sc[2];
    // Spill scratch originals to cpu->mscratch[4..6] (disjoint from emit_mangled_x18's [0..3]); NOT the stack,
    // since the fold runs on a hot memory op where an async host signal would clobber a [sp,#-N] red-zone slot.
    int M = (int)OFF_MSCRATCH;
    e_str(Sb, CPUREG, M + 32);
    e_str(T, CPUREG, M + 40);
    e_str(T2, CPUREG, M + 48);
    if (base == 31)
        e_mov_from_sp(Sb);
    else if (is_stolen(base))
        e_ldr(Sb, CPUREG, base * 8); // guest base from cpu->x[base]
    else
        e_movr(Sb, base); // guest base from the live host reg
    // Bias iff Sb is in [g_nonpie_lo, g_nonpie_hi); a >= 4GiB base is never the low image -> skip with no flag
    // traffic. The compares clobber NZCV, so save/restore the guest flags. (Same discriminator as emit_fold_mem.)
    if (guestbase_on()) {
        emit32(0xD360FC00u | (Sb << 5) | T); // lsr T, Sb, #32
        uint32_t *p_hi = (uint32_t *)g_cp;
        emit32(0);                // cbnz T, Lhi   (>= 4GiB -> skip, flags untouched)
        emit32(0xD53B4200u | T2); // mrs T2, nzcv  (save guest flags)
        e_movconst(T, g_nonpie_lo);
        emit32(0xEB000000u | (T << 16) | (Sb << 5) | 31); // cmp Sb, lo
        uint32_t *p_lo1 = (uint32_t *)g_cp;
        emit32(0); // b.lo Llo   (Sb < lo -> not image)
        e_movconst(T, g_nonpie_hi);
        emit32(0xEB000000u | (T << 16) | (Sb << 5) | 31); // cmp Sb, hi
        uint32_t *p_lo2 = (uint32_t *)g_cp;
        emit32(0); // b.hs Llo   (Sb >= hi -> not image)
        e_movconst(T, g_nonpie_bias);
        emit32(0x8B000000u | (T << 16) | (Sb << 5) | Sb); // add Sb, Sb, bias
        uint8_t *Llo = g_cp;
        emit32(0xD51B4200u | T2); // msr nzcv, T2  (restore guest flags)
        uint8_t *Lhi = g_cp;
        *p_hi = 0xB5000000u | (((uint32_t)(((uint8_t *)Lhi - (uint8_t *)p_hi) / 4) & 0x7FFFF) << 5) | T;
        *p_lo1 = 0x54000000u | (((uint32_t)(((uint8_t *)Llo - (uint8_t *)p_lo1) / 4) & 0x7FFFF) << 5) | 3; // b.lo
        *p_lo2 = 0x54000000u | (((uint32_t)(((uint8_t *)Llo - (uint8_t *)p_lo2) / 4) & 0x7FFFF) << 5) | 2; // b.hs
    }
    // De-index to the no-offset form against Sb: clear post-index (bit23) and Rm[20:16], rebase Xn -> Sb. The
    // V-register list, opcode, R, and size fields are untouched, so the transfer is identical -- only its
    // address is now the biased high pointer.
    emit_a64_bus_guard(Sb, (uint64_t)advsimd_struct_bytes(in), g_emit_gpc);
    struct a64_soft_guard soft =
        emit_a64_soft_guard_begin(Sb, T, T2, (uint64_t)advsimd_struct_bytes(in),
                                  (in & (1u << 22)) ? HL_LOGICAL_VMA_READ : HL_LOGICAL_VMA_WRITE, g_emit_gpc);
    a64_soft_guard_restore(&soft, Sb, M + 32);
    a64_soft_guard_restore(&soft, T, M + 40);
    a64_soft_guard_restore(&soft, T2, M + 48);
    emit32((in & ~(1u << 23) & ~(0x1Fu << 16) & ~(0x1Fu << 5)) | ((unsigned)Sb << 5));
    emit_a64_soft_guard_end(&soft);
    if (post) { // writeback the LOW guest base: Xn += (Rm==31 ? bytes transferred : Xm)
        if (rm == 31) {
            unsigned inc = (unsigned)advsimd_struct_bytes(in);
            if (is_stolen(base)) {
                e_ldr(T, CPUREG, base * 8);
                e_addi(T, T, inc);
                e_str(T, CPUREG, base * 8);
            } else
                e_addi(base, base, inc);
        } else {
            int idx = rm;
            if (is_stolen(rm)) {
                e_ldr(T, CPUREG, rm * 8); // increment from cpu->x[rm]
                idx = T;
            }
            if (is_stolen(base)) { // T2's original is spilled -> free as the base temp (T2 != idx always)
                e_ldr(T2, CPUREG, base * 8);
                emit32(0x8B000000u | ((unsigned)idx << 16) | (T2 << 5) | T2); // add T2, T2, idx
                e_str(T2, CPUREG, base * 8);
            } else if (base == 31) {
                e_mov_from_sp(T2);
                emit32(0x8B000000u | ((unsigned)idx << 16) | (T2 << 5) | T2);
                e_mov_sp_from(T2);
            } else
                emit32(0x8B000000u | ((unsigned)idx << 16) | (base << 5) | base); // add base, base, idx
        }
    }
    e_ldr(Sb, CPUREG, M + 32); // restore scratch originals
    e_ldr(T, CPUREG, M + 40);
    e_ldr(T2, CPUREG, M + 48);
    emit_a64_soft_bounce_commit(g_emit_gpc + 4);
}

// For instructions that WRITE a stolen reg via a legacy special path (adr/adrp/mrs): save x0,x1
// to cpu->mscratch, x1 := cpu. The case then computes a value into x0 and stores it to cpu->x[stolen];
// x18_epilog restores x0,x1. Literal loads do not use this path: target
// initialization permanently reserves x16/x17, so their guarded address
// materialization has one supported implementation.
//
// Spill target is cpu->mscratch[0..1], NOT the guest [sp,#-N] "red zone": AArch64 has NO architectural
// red zone, so a store below the guest SP faults whenever the page under SP is unmapped (a shallow guest
// stack) -- the exact crash class 6d38d96c/7de3a17a closed for the steal path. x16/x17 are NOT free here
// (NOSTEAL1617 keeps them as guest values), so this mirrors the mscratch spill in emit_mangled_x18 /
// emit_fold_mem. x28 (CPUREG) = cpu is the whole-block invariant (prologue `mov x28,x0`; guest x28 is
// stolen), so it reaches mscratch on every path. Slots [0..1] are free during the bracket: this helper
// wraps a SINGLE special-write whose body only touches cpu->x[stolen] + guest memory -- never mscratch,
// never a nested mangle/fold -- so it cannot alias the saved x0/x1.
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
static void emit_atomic_part(uint32_t in, int mask, int is_mem) {
    if (is_mem && (guestbase_on() || jit_guest_soft_active()) && (jit_guest_soft_active() || ((in >> 5) & 31) != 31)) {
        if (jit_guest_bus_active()) emit_a64_bus_guard_instruction(in, g_emit_gpc);
        emit_fold_mem(in, 0);
    } else if (uses_x18(in, mask))
        emit_mangled_x18(in, mask);
    else
        emit32(in);
}

// The rewritten LSE op replaces a whole ldxr/stxr RETRY LOOP, and that loop only ever falls out of
// `cbnz Ws, loop` with the store-exclusive status register Ws == 0. A single LSE instruction never
// touches Ws, so without this the guest keeps the stale pre-loop value of Ws -- an architectural
// divergence the differential ISA fuzzer (tests/fuzz/isa/aarch64) caught directly. Emit it LAST: in
// the original loop the stxr writes Ws after every other operand, so a Ws that aliases Wt/Ws2/Wm
// must also end up zero.
static void emit_lse_status_zero(int Ws) {
    if (Ws == 31) return;                               /* stxr wzr: the status is architecturally discarded */
    emit_atomic_part(0x2A1F03E0u | (uint32_t)Ws, 1, 0); /* mov Ws, wzr */
}

// Returns bytes consumed (12 or 16) if a known atomic loop at gpc was rewritten, else 0.
static int try_lse_atomic(uint64_t gpc) {
    uint32_t i0 = a64_fetch_instruction(gpc, NULL);
    // load-exclusive?
    if ((i0 & 0x3F400000u) != 0x08400000u) return 0;
    int sz = (i0 >> 30) & 3;
    // word/dword only
    if (sz < 2) return 0;
    // non-pair
    if (((i0 >> 16) & 0x1F) != 0x1F || ((i0 >> 10) & 0x1F) != 0x1F) return 0;
    int Wt = i0 & 31, Xn = (i0 >> 5) & 31;
    uint32_t i1 = a64_fetch_instruction(gpc + 4, NULL);

    // SWP:  ldxr Wt,[Xn]; stxr Ws,Wv,[Xn]; cbnz Ws,loop
    if ((i1 & 0x3F400000u) == 0x08000000u && ((i1 >> 30) & 3) == sz && ((i1 >> 10) & 0x1F) == 0x1F &&
        ((i1 >> 5) & 31) == Xn) {
        int Ws = (i1 >> 16) & 31, Wv = i1 & 31;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL);
        if ((i2 & 0xFF000000u) == 0x35000000u && (i2 & 31) == Ws &&
            (gpc + 8 + (uint64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
            // A bare `swpal` in place of this swap loop is a deterministic lost-wakeup for multithreaded
            // musl: node's V8 workers park forever in __unlock's `a_swap(l,0)==2 && __wake` because the
            // swp'd old value doesn't drive the wake (node:alpine hung >400s; the exclusive pair completes
            // in 0.28s, matching the docker oracle). The `ldadd*`/`casal` idioms below are unaffected. So
            // upgrade ONLY when the exclusive-pair fallback is UNUSABLE -- i.e. when translating it verbatim
            // would inject a monitor-clearing ldr/str between the ldxr and stxr (a stolen operand needs a
            // cpu-slot mangle, or a non-PIE low base needs a bias-fold), which would spin the stxr forever.
            // The common clean-PIE case (no stolen operand, no fold) keeps the proven exclusive pair.
            if (guestbase_on() || is_stolen(Wt) || is_stolen(Xn) || is_stolen(Ws) || is_stolen(Wv)) {
                // swpal Wv, Wt, [Xn] (a single LSE op; emit_atomic_part folds/mangles the corner cases).
                emit_atomic_part(0xB8E08000u | (sz == 3 ? 0x40000000u : 0) | (Wv << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 12;
            }
        }
    }
    // LDADD/LDSET/LDEOR/LDCLR/LDADD-neg:  ldxr Wt,[Xn]; <op> Ws2,Wt,Wm; stxr Ws,Ws2,[Xn]; cbnz Ws,loop
    // 0 add 1 orr 2 eor 3 and 4 sub
    int op = -1;
    if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0x8B000000u : 0x0B000000u))
        op = 0;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xAA000000u : 0x2A000000u))
        op = 1;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xCA000000u : 0x4A000000u))
        op = 2;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0x8A000000u : 0x0A000000u))
        op = 3;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xCB000000u : 0x4B000000u))
        op = 4;
    if (op >= 0) {
        int Ws2 = i1 & 31, n = (i1 >> 5) & 31, m = (i1 >> 16) & 31, Wm = -1;
        if (op == 4) {
            if (n == Wt) Wm = m;
            // sub: not commutative, Rn must be Wt
        } else {
            if (n == Wt)
                Wm = m;
            else if (m == Wt)
                Wm = n;
        }
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        if (Wm >= 0 && (i2 & 0x3F400000u) == 0x08000000u && ((i2 >> 30) & 3) == sz && (i2 & 31) == Ws2 &&
            ((i2 >> 5) & 31) == Xn && ((i2 >> 10) & 0x1F) == 0x1F) {
            int Ws = (i2 >> 16) & 31;
            if ((i3 & 0xFF000000u) == 0x35000000u && (i3 & 31) == Ws &&
                (gpc + 12 + (uint64_t)(sext((i3 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
                // op>=3 borrows Ws as a scratch holding ~Wm / -Wm across two ops -> it must not alias Wm.
                if (op >= 3 && Wm == Ws) return 0;
                uint32_t szb = sz == 3 ? 0x40000000u : 0, szd = sz == 3 ? 0x80000000u : 0;
                if (op <= 2) {
                    uint32_t lse = op == 0 ? 0xB8E00000u : op == 1 ? 0xB8E03000u : 0xB8E02000u;
                    // ldaddal/ldsetal/ldeoral Wm, Wt, [Xn]
                    emit_atomic_part(lse | szb | (Wm << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                } else if (op == 3) {
                    // fetch_and: *Xn &= Wm  ==  ldclr ~Wm:  mvn Ws,Wm (orn Ws,wzr,Wm); ldclral Ws, Wt, [Xn]
                    emit_atomic_part(0x2A200000u | szd | (Wm << 16) | (31 << 5) | Ws, 1 | 4, 0);
                    emit_atomic_part(0xB8E01000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                } else {
                    // fetch_sub: *Xn -= Wm  ==  ldadd -Wm:  neg Ws,Wm (sub Ws,wzr,Wm); ldaddal Ws, Wt, [Xn]
                    emit_atomic_part(0x4B000000u | szd | (Wm << 16) | (31 << 5) | Ws, 1 | 4, 0);
                    emit_atomic_part(0xB8E00000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                }
                // reconstruct the new value (re-emit the original op) for any following guest code
                emit_atomic_part(i1, gpr_field_mask(i1), 0);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 16;
            }
        }
    }
    // LDADD immediate (fetch_add of a constant -- the headline refcount/counter case):
    //   ldxr Wt,[Xn]; add Ws2,Wt,#imm (sh=0); stxr Ws,Ws2,[Xn]; cbnz Ws,loop
    uint32_t addib = sz == 3 ? 0x91000000u : 0x11000000u;
    if ((i1 & 0xFFC00000u) == addib && ((i1 >> 5) & 31) == Wt) {
        int Ws2 = i1 & 31;
        unsigned imm = (i1 >> 10) & 0xFFF;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        if ((i2 & 0x3F400000u) == 0x08000000u && ((i2 >> 30) & 3) == sz && (i2 & 31) == Ws2 && ((i2 >> 5) & 31) == Xn &&
            ((i2 >> 10) & 0x1F) == 0x1F) {
            int Ws = (i2 >> 16) & 31;
            if ((i3 & 0xFF000000u) == 0x35000000u && (i3 & 31) == Ws &&
                (gpc + 12 + (uint64_t)(sext((i3 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
                uint32_t szb = sz == 3 ? 0x40000000u : 0;
                // Ws (dead status reg) = imm  (movz Ws, #imm; e_movz always uses the 64-bit form)
                emit_atomic_part(0xD2800000u | ((imm & 0xFFFFu) << 5) | Ws, 1, 0);
                // ldaddal Ws, Wt, [Xn]
                emit_atomic_part(0xB8E00000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                // re-emit add Ws2, Wt, #imm (reconstruct the new value)
                emit_atomic_part(i1, gpr_field_mask(i1), 0);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 16;
            }
        }
    }
    // CAS:  ldxr Wt,[Xn]; cmp Wt,Wexp; b.ne out; stxr Ws,Wnew,[Xn]; cbnz Ws,loop; out:
    // subs wzr, Wt, Wexp (cmp)
    uint32_t subsb = sz == 3 ? 0xEB00001Fu : 0x6B00001Fu;
    if ((i1 & 0xFFE0FC1Fu) == subsb && ((i1 >> 5) & 31) == Wt) {
        int Wexp = (i1 >> 16) & 31;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        uint32_t i4 = a64_fetch_instruction(gpc + 16, NULL);
        // b.ne
        if ((i2 & 0xFF00001Fu) == 0x54000001u && (i3 & 0x3F400000u) == 0x08000000u && ((i3 >> 30) & 3) == sz &&
            ((i3 >> 10) & 0x1F) == 0x1F && ((i3 >> 5) & 31) == Xn && (i4 & 0xFF000000u) == 0x35000000u &&
            (i4 & 31) == ((i3 >> 16) & 31) &&
            // cbnz -> loop
            (gpc + 16 + (uint64_t)(sext((i4 >> 5) & 0x7FFFF, 19) << 2)) == gpc
            // b.ne -> out
            && (gpc + 8 + (uint64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2)) == gpc + 20) {
            int Wnew = i3 & 31;
            // casal carries the compare/old value in Wt, so Wt must differ from Wexp (a stolen Wt flows
            // through its cpu slot across the three ops). The bare ldxr/stxr fallback would spin on a stolen
            // operand / low non-PIE [Xn], so route every part through emit_atomic_part.
            if (Wt == Wexp) return 0;
            uint32_t szd = sz == 3 ? 0x80000000u : 0;
            // mov Wt, Wexp (orr Wt, wzr, Wexp): Rd=Wt[0], Rm=Wexp[16]
            emit_atomic_part(0x2A000000u | szd | (Wexp << 16) | (31 << 5) | Wt, 1 | 4, 0);
            // casal Wt, Wnew, [Xn]; Wt = old:  Rs=Wt[16], Rn=Xn[5], Rt=Wnew[0]
            emit_atomic_part((sz == 3 ? 0xC8E0FC00u : 0x88E0FC00u) | (Wt << 16) | (Xn << 5) | Wnew, 1 | 2 | 4 | 8, 1);
            // cmp Wt, Wexp (reproduce NZCV): subs wzr, Wt, Wexp -> Rn=Wt[5], Rm=Wexp[16]
            emit_atomic_part(0x6B00001Fu | szd | (Wexp << 16) | (Wt << 5), 2 | 4, 0);
            // The guest loop reaches `stxr Ws` only when the compare matched; on the b.ne-out path Ws
            // keeps its pre-loop value. Reproduce both with a csel off the NZCV just recomputed.
            {
                int Ws = (i3 >> 16) & 31;
                // 64-bit csel: the not-taken path must preserve the FULL guest register (a 32-bit csel
                // would zero the top half of an untouched Ws); the taken path selects xzr, and stxr's
                // W-sized status write is zero-extending anyway, so 0 is right for it too.
                if (Ws != 31)
                    emit_atomic_part(0x9A800000u | ((uint32_t)Ws << 16) | (31u << 5) | (uint32_t)Ws, 1 | 2 | 4, 0);
            }
            g_lse_n++;
            return 20;
        }
    }
    return 0;
}

// ---- LSE outline-atomic call inlining ----
// GCC/LLVM emit every C atomic as a `bl __aarch64_<op><sz>_<order>` outline helper (the distro/musl and
// -mno-outline-atomics-ignored toolchains still do). The helper is a fixed 5-insn leaf:
//     adrp x16,#page ; ldrb w16,[x16,#off] ; cbz w16, Lfallback ; <host LSE op> ; ret   Lfallback: ldxr/stxr..
// The gated byte is __aarch64_have_lse_atomics -- ALWAYS 1 here (we advertise HWCAP_ATOMICS and the host
// has FEAT_LSE), so the fast-path single LSE op IS the architectural effect of the call. Inline that one
// op at the call site: elide the bl + adrp/ldrb/cbz + ret AND the block-split/return dispatch (the call
// idiom is ~2 helper round-trips per atomic in tight code -- the dominant hl-vs-native atomics tax, since
// the LSE op itself already lowers 1:1). The op reads/writes guest memory with its native [Xn] base, so
// inline ONLY when an in-stream copy of that op would be emitted verbatim too: guestbase off (PIE/static-
// PIE) and BUS inactive. Otherwise fall through to the normal call (the helper still runs correctly).
// Returns 1 if it inlined (caller advances past the bl and keeps the block going), else 0.
static int try_inline_outline_atomic(uint64_t gpc, uint64_t target) {
    /*
     * This optimization embeds an instruction read from an out-of-line helper
     * in the caller translation.  The initial SMC prime removes all such
     * pre-SMC callers; do not create new hidden source dependencies once map
     * entries are individually invalidatable.
     */
    if (smc_seen() || guestbase_on() || jit_guest_bus_active()) return 0;
    uint32_t i0 = a64_fetch_instruction(target, NULL), i1 = a64_fetch_instruction(target + 4, NULL);
    uint32_t i2 = a64_fetch_instruction(target + 8, NULL), i3 = a64_fetch_instruction(target + 12, NULL);
    uint32_t i4 = a64_fetch_instruction(target + 16, NULL);
    // adrp x16, #page
    if ((i0 & 0x9F00001Fu) != 0x90000010u) return 0;
    // ldrb w16, [x16, #imm12]
    if ((i1 & 0xFFC003FFu) != 0x39400210u) return 0;
    // cbz w16, Lfallback  (byte==0 -> ldxr/stxr fallback; byte!=0 falls through to the LSE op i3)
    if ((i2 & 0xFF00001Fu) != 0x34000010u) return 0;
    // the cbz must jump PAST i3 (forward, skipping the fast-path op) so the fall-through reaches i3
    if ((int64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2) < 8) return 0;
    // i4 = ret x30 (the helper is a leaf; x30 is preserved across it)
    if (i4 != 0xD65F03C0u) return 0;
    // i3 must be a single-[Xn]-base LSE atomic memory op (LDADD/SWP/LDSET/...) or a CAS (single).
    int is_lse = (i3 & 0x3F200C00u) == 0x38200000u;
    int is_cas = (i3 & 0x3FA07C00u) == 0x08A07C00u;
    if (!is_lse && !is_cas) return 0;
    // no stolen operand (Rs[20:16], Rn[9:5], Rt[4:0]) -> the op is safe to copy verbatim
    if (is_stolen(i3 & 31) || is_stolen((i3 >> 5) & 31) || is_stolen((i3 >> 16) & 31)) return 0;
    // architectural x30 after the (elided) bl+ret is the return address; set it so a later reader / signal
    // / unwinder sees exactly what the real call would have left. (Un-biased low vaddr for non-PIE; identity
    // for PIE -- but guestbase is off here anyway.)
    emit_set_x30(pcrel_base(gpc) + 4);
    emit32(i3);
    g_lse_n++;
    return 1;
}

// ---- tier-2 substrate: the purity gate (the analyze() of trace_pipeline.c) ----
// Given a formed trace's instructions, return 1 only if it is safe to MEMOIZE:
// no syscall (svc) and no memory access at all -- so the result is fully determined
// by the input registers and there are no side effects. Conservative by construction:
// any load/store or syscall -> impure -> emit unoptimized (side effects must run).
// This is the gate that refuses the impure region in the pipeline (a wrong gate here
// is a miscompile). Linear in trace length, run once on promotion. Verified by
// TIER2_SELFTEST; wired into specialization when trace formation (the "form trace"
// step) lands -- the remaining substrate brick.
static int region_pure(const uint32_t *code, int n) {
    for (int i = 0; i < n; i++) {
        uint32_t in = code[i];
        // svc -> side effect
        if (in == 0xD4000001u) return 0;
        // any load/store -> not register-determined
        if ((in & 0x0A000000u) == 0x08000000u) return 0;
    }
    // pure: register-to-register computation only
    return 1;
}

// ---- §B shadow-stack return prediction: the validated mechanism (PoC: shadow_stack.c) ----
// At a guest `bl`, record the guest return address. At a guest `ret`, classify the guest's x30:
//   FAST    -> matches the top of the shadow stack: the normal return; take a host `ret` (the
//              hardware RAS predicts it in ~1 insn instead of the ~14-insn ret-IBTC).
//   UNWIND  -> matches a deeper frame (longjmp / multi-frame return): pop to it, still correct.
//   FOREIGN -> not on the shadow (computed/tail return): fall back to the IBTC.
// Conservative: ONLY the FAST path takes the host ret; UNWIND/FOREIGN fall back, so a return can
// never land at the wrong target. The codegen that emits host bl/ret + the x30 steal wires onto
// this (the one subtlety past the PoC is x30's dual role: host return address vs guest-visible
// link value -- handled by keeping guest x30 in cpu->x[30] and validating here).
enum { SS_FAST, SS_UNWIND, SS_FOREIGN };

static inline void shadow_push(struct cpu *c, uint64_t guest_ret, uint64_t host_ret) {
    if (c->ssp < 1024) {
        c->sstk[2 * c->ssp] = guest_ret;
        c->sstk[2 * c->ssp + 1] = host_ret;
        c->ssp++;
    }
}

// matches on guest_ret (even index)
static int shadow_classify(struct cpu *c, uint64_t guest_x30) {
    if (c->ssp > 0 && c->sstk[2 * (c->ssp - 1)] == guest_x30) {
        c->ssp--;
        return SS_FAST;
    }
    for (uint64_t d = 2; d <= c->ssp && d <= 64; d++)
        if (c->sstk[2 * (c->ssp - d)] == guest_x30) {
            c->ssp -= d;
            return SS_UNWIND;
        }
    return SS_FOREIGN;
}

// ---- opt4: greedy superblock / trace formation ----
// Follow unconditional `b` edges INLINE, and lay conditional fall-through successors INLINE
// (inverting the guest condition so the TAKEN side becomes a tiny out-of-line chain-exit).
// A region is bounded to TRACE_MAX_BYTES / TRACE_MAX_BLK; intermediate guest block-starts are
// deliberately NOT registered in g_map -- any edge that later enters mid-region self-heals by
// re-translating a fresh (always-correct) duplicate, wired up through the existing
// add_pend/patch_links_to back-patch path. NOSTITCH=1 -> g_stitch=0 -> exact single-block
// baseline (env read once; set-once + idempotent under the JIT lock).
#define TRACE_MAX_BLK 16
#define TRACE_MAX_BYTES (16 * 1024)
static int g_stitch = -1;

static int seen_has(const uint64_t *seen, int n, uint64_t v) {
    for (int i = 0; i < n; i++)
        if (seen[i] == v) return 1;
    return 0;
}

// Lay a conditional's fall-through inline: `inv` is the branch insn with its condition/op
// already inverted, so when the guest would NOT take it we keep falling through. Emit the
// inverted branch (skips the taken-side exit), the taken chain-exit, then patch the branch to
// jump just past it. The patched offset is always tiny (the taken exit is ~1 insn if chained,
// ~30 if it spills) -> in range even for tbz/tbnz's 14-bit field.
static void stitch_cond(uint32_t inv, uint64_t taken) {
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    emit_chain_exit(taken);
    *patch = recode_cond(inv, ((uint8_t *)g_cp - (uint8_t *)patch) / 4);
}

