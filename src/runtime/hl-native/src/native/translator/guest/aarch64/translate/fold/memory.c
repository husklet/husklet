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
