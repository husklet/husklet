/*
 * Every BUS-active memory site retains its compact force/page-filter fast
 * path, but all sites in a translated block share one cold spill/query/reload
 * stub. Large runtimes can keep BUS tracking active for their entire lifetime;
 * duplicating that cold path at every memory operation needlessly exhausts the
 * code cache even though almost every filter probe misses.
 */
#define BUS_STUB_PATCH_MAX 65536
static uint32_t *g_bus_stub_patches[BUS_STUB_PATCH_MAX];
static uint32_t g_bus_stub_patch_count;

static void patch_adr(uint32_t *instruction, uint8_t *target, unsigned reg) {
    int64_t displacement = target - (uint8_t *)instruction;
    if (displacement < -(INT64_C(1) << 20) || displacement >= (INT64_C(1) << 20)) {
        static const char message[] = "BUS metadata address out of range";
        (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
        _exit(70);
    }
    uint64_t immediate = (uint64_t)displacement & UINT64_C(0x1fffff);
    *instruction =
        0x10000000u | (uint32_t)((immediate & 3u) << 29) | (uint32_t)(((immediate >> 2) & 0x7ffffu) << 5) | reg;
}

static void emit_a64_bus_guard_saved(uint64_t bytes, uint64_t pc) {
    /* x16 carries the state loaded by the caller. */
    uint32_t *force_slow = (uint32_t *)g_cp;
    emit32(0); /* tbnz w16,#1,slow */
    /* Use only engine-reserved x16/x17. A live guest register cannot be
       parked in shared per-thread scratch here: an asynchronous signal may
       re-enter translated code and run another guard before this one resumes. */
    e_ldr(17, CPUREG, OFF_BUS_EA);
    emit32(0xD34CFC00u | (17u << 5) | 17u);                            /* lsr x17,x17,#12: page */
    emit32(0xD3400000u | (6u << 16) | (15u << 10) | (17u << 5) | 16u); /* ubfx x16,x17,#6,#10 */
    e_ldr(17, CPUREG, OFF_BUS_FILTER);
    emit32(0x8B000000u | (16u << 16) | (3u << 10) | (17u << 5) | 16u); /* add x16,x17,x16,lsl#3 */
    e_ldr(16, 16, 0);
    e_ldr(17, CPUREG, OFF_BUS_EA);
    emit32(0xD34CFC00u | (17u << 5) | 17u);
    emit32(0x9AD12610u); /* lsrv x16,x16,x17 */
    uint32_t *filter_miss = (uint32_t *)g_cp;
    emit32(0); /* tbz x18,#0,resume */
    uint8_t *slow = g_cp;
    *force_slow = 0x37000000u | (1u << 19) | (((uint32_t)((slow - (uint8_t *)force_slow) / 4) & 0x3FFFu) << 5) | 16u;
    /*
     * Carry only engine-reserved registers into the shared stub. emit_spill()
     * deliberately preserves x16/x17, so an asynchronous signal cannot
     * overwrite site metadata in per-thread mutable scratch.
     */
    e_ldr(16, CPUREG, OFF_BUS_EA);
    uint32_t *metadata_address = (uint32_t *)g_cp;
    emit32(0); /* adr x17,immutable_site_metadata */
    if (g_bus_stub_patch_count >= BUS_STUB_PATCH_MAX) {
        static const char message[] = "too many BUS guards in one translated block";
        (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
        _exit(70);
    }
    g_bus_stub_patches[g_bus_stub_patch_count++] = (uint32_t *)g_cp;
    emit32(0); /* b shared_bus_stub */
    uint8_t *metadata = g_cp;
    patch_adr(metadata_address, metadata, 17);
    memcpy(g_cp, &bytes, sizeof(bytes));
    g_cp += sizeof(bytes);
    memcpy(g_cp, &pc, sizeof(pc));
    g_cp += sizeof(pc);
    uint8_t *resume_slot = g_cp;
    g_cp += sizeof(uint64_t);
    uint8_t *resume_fast = g_cp;
    uint64_t resume_rx = (uint64_t)J_RX(resume_fast);
    memcpy(resume_slot, &resume_rx, sizeof(resume_rx));
    *filter_miss = 0x36000000u | (((uint32_t)((resume_fast - (uint8_t *)filter_miss) / 4) & 0x3FFFu) << 5) | 16u;
}

static void emit_a64_bus_stub(void) {
    if (!g_bus_stub_patch_count) return;
    uint8_t *stub = g_cp;
    for (uint32_t i = 0; i < g_bus_stub_patch_count; i++) {
        int64_t displacement = (stub - (uint8_t *)g_bus_stub_patches[i]) / 4;
        if (displacement < -(INT64_C(1) << 25) || displacement >= (INT64_C(1) << 25)) {
            static const char message[] = "BUS stub branch out of range";
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            _exit(70);
        }
        *g_bus_stub_patches[i] = 0x14000000u | ((uint32_t)displacement & 0x03ffffffu);
    }
    emit_spill();
    e_movr(19, 17); /* callee-saved immutable metadata pointer across query */
    e_movr(0, 16);
    e_ldr(1, 19, 0);
    emit_busfaultptr(16);
    emit32(0xD63F0000u | (16u << 5)); /* blr x16 */
    uint32_t *clear = (uint32_t *)g_cp;
    emit32(0); /* cbz x0,clear */
    e_str(0, CPUREG, OFF_FAULT_ADDR);
    e_ldr(9, 19, 8);
    e_str(9, CPUREG, OFF_PC);
    e_movconst(9, R_BUS);
    e_str(9, CPUREG, OFF_RSN);
    e_movr(0, CPUREG);
    emit_blockret(9);
    e_br(9);
    uint8_t *resume = g_cp;
    *clear = 0xB4000000u | (((uint32_t)((resume - (uint8_t *)clear) / 4) & 0x7ffffu) << 5);
    e_ldr(16, 19, 16);
    e_ldr(9, CPUREG, OFF_SP);
    e_mov_sp_from(9);
    e_ldr(9, CPUREG, OFF_NZCV);
    emit32(0xD51B4200u | 9);
    for (int t = 0; t < 32; t += 2)
        e_ldp_q(t, t + 1, CPUREG, OFF_V + t * 16);
    for (int r = 1; r <= 30; r++)
        if (!is_stolen(r)) e_ldr(r, CPUREG, r * 8);
    e_ldr(0, CPUREG, 0);
    e_br(16);
}

static void emit_a64_bus_guard(int ea, uint64_t bytes, uint64_t pc) {
    if (!jit_guest_bus_active()) return;
    /* The inline BUS ABI reserves x16/x17 as engine registers. Target
       initialization fixes g_steal1617 on and exposes no legacy override. */
    assert(g_steal1617);
    e_str(ea, CPUREG, OFF_BUS_EA);
    e_ldr(16, CPUREG, OFF_BUS_FORCE);
    emit32(0xB9400000u | (16u << 5) | 16u);
    uint32_t *inactive_fast = (uint32_t *)g_cp;
    emit32(0);
    emit_a64_bus_guard_saved(bytes, pc);
    uint8_t *resume_inactive = g_cp;
    e_ldr(ea, CPUREG, OFF_BUS_EA);
    *inactive_fast =
        0x36000000u | (((uint32_t)((resume_inactive - (uint8_t *)inactive_fast) / 4) & 0x3FFFu) << 5) | 16u;
}

static void emit_a64_bus_guard_base(int base, int64_t offset, uint64_t bytes, uint64_t pc) {
    if (!jit_guest_bus_active()) return;
    if (base == 31)
        e_mov_from_sp(16);
    else if (is_stolen(base))
        e_ldr(16, CPUREG, base * 8);
    else
        e_movr(16, base);
    if (offset < 0)
        e_subi(16, 16, (unsigned)(-offset));
    else if (offset > 0)
        e_addi(16, 16, (unsigned)offset);
    emit_a64_bus_guard(16, bytes, pc);
}

/* Compute and guard the architectural guest EA while preserving the original
   memory opcode. BUS observation must not broaden non-PIE bias folding. */
static void emit_a64_bus_guard_instruction(uint32_t in, uint64_t pc) {
    int base = (int)((in >> 5) & 31u);
    int regoff = (in & 0x3B200C00u) == 0x38200800u;
    if (base == 31)
        e_mov_from_sp(16);
    else if (is_stolen(base))
        e_ldr(16, CPUREG, base * 8);
    else
        e_movr(16, base);
    if (regoff) {
        int rm = (int)((in >> 16) & 31u), opt = (int)((in >> 13) & 7u);
        int vector = (int)((in >> 26) & 1u);
        int size = vector ? (int)((((in >> 22) & 3u) >> 1) << 2) | (int)((in >> 30) & 3u) : (int)((in >> 30) & 3u);
        int amount = ((in >> 12) & 1u) ? size : 0;
        if (is_stolen(rm))
            e_ldr(17, CPUREG, rm * 8);
        else
            e_movr(17, rm);
        emit32(0x8B200000u | (17u << 16) | ((unsigned)opt << 13) | ((unsigned)(amount & 7) << 10) | (16u << 5) | 16u);
    } else {
        int64_t offset = a64_fold_mem_offset(in, 0);
        if (((in >> 27) & 7u) == 7u && !((in >> 24) & 1u)) {
            int mode = (int)((in >> 10) & 3u);
            if (mode == 1) offset = 0;
        }
        if (offset != 0) {
            uint64_t magnitude = (uint64_t)(offset < 0 ? -offset : offset);
            e_movconst(17, magnitude);
            emit32((offset < 0 ? 0xCB000000u : 0x8B000000u) | (17u << 16) | (16u << 5) | 16u);
        }
    }
    emit_a64_bus_guard(16, a64_mem_bytes(in), pc);
}

// Scratch-slot assignment for a folded memory op. Picks the non-stolen host GP registers whose live guest
// values emit_fold_mem spills to cpu->mscratch[4..7] (Sb,T,T2,Tm). Factored out so fault-time register
// reconstruction (sigframe_capture_fault) uses the EXACT same slot mapping the emitter chose. Fills
// slots[0..n-1] with the chosen register numbers (Sb=slots[0], T=slots[1], T2=slots[2], Tm=slots[3] for the
// register-offset form) and returns the count: 4 for register-offset, else 3. Mirrors gpr_field_mask + the
// LSE-Rs (bit2) fixup so the "used" set matches the emitter exactly.
static int fold_mem_scratch(uint32_t insn, int slots[4]) {
    int mask = gpr_field_mask(insn);
    if ((insn & 0x3B200C00u) == 0x38200000u) mask |= 4; // LSE atomic value operand Rs[20:16]
    int regoff = (insn & 0x3B200C00u) == 0x38200800u;
    int used = 0;
    static const int shifts[4] = {0, 5, 16, 10}, mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if (mask & mbits[k]) used |= 1u << ((insn >> shifts[k]) & 31);
    int need = regoff ? 4 : 3, n = 0;
    for (int r = 0; r <= 30 && n < need; r++)
        if (!(used & (1u << r)) && !is_stolen(r)) slots[n++] = r;
    return n;
}

// Emit a folded memory op: compute the guest effective address into a scratch Sb, add g_nonpie_bias iff
// that address is a LOW image address (< 4GiB; everything else -- stack/heap/mmap/libs -- is >= the
// engine's 4GiB __PAGEZERO), then the access re-pointed at Sb. Flag-free (loads/stores must not disturb the
// guest NZCV): only mov/ldr/add/lsr/cbnz. Scratch originals are spilled to cpu->mscratch (NOT the stack:
// the fold runs on every memory op, where an async host signal would clobber a red-zone slot). For the
// register-offset form the full EA (Xn + extend(Xm)) is materialized and the access is de-indexed to a
// plain [Sb] (unscaled, #0) so the single < 4GiB test is on the real target. Pre/post-index writeback is
// de-indexed too: the access runs against the biased Sb, then the writeback updates the LOW guest base. Any
// stolen Rt/Rt2/Rs is handled by reusing emit_mangled_x18 on the re-based instruction (base field -> Sb).
