// Recognize the compiler's otherwise redundant frame around a direct tail call:
//
//   stp x29,x30,[sp,#-16]!; small register-only setup; ldp x29,x30,[sp],#16; b target
//
// x17 is engine-private in steal mode, so it can carry the architectural LR
// between the real stack store/load.  The stack accesses remain native and at
// their original guest PCs (fault/unwind semantics are unchanged); this only
// avoids repeatedly round-tripping x30 through cpu->x[30] in the generic
// stolen-register mangler.  Keep the recognizer intentionally exact.
static uint64_t scan_tail_x30_carry(uint64_t pc) {
    if (!g_steal1617 || g_noibslim || jit_guest_bus_active()) return 0;
    // The recognizer may inspect the eight setup instructions plus the tail
    // instruction following the restoring LDP.  Refuse to speculate across an
    // unmapped guest boundary.
    if (!hl_host_range_mapped((uintptr_t)pc, 10 * sizeof(uint32_t))) return 0;
    if (a64_fetch_instruction(pc, NULL) != 0xA9BF7BFDu) return 0;
    for (int i = 1; i <= 8; i++) {
        uint32_t in = a64_fetch_instruction(pc + (uint64_t)i * 4, NULL);
        if (in == 0xA8C17BFDu) {
            uint32_t tail = a64_fetch_instruction(pc + (uint64_t)(i + 1) * 4, NULL);
            return (tail & 0xFC000000u) == 0x14000000u ? pc + (uint64_t)i * 4 : 0;
        }
        // No memory, control flow, system, SIMD, PC-relative operation, or
        // stolen guest register may occur while host x17 carries the LR.
        if ((in & 0x0A000000u) == 0x08000000u || (in & 0x7C000000u) == 0x14000000u ||
            (in & 0x1C000000u) == 0x10000000u || (in & 0x0E000000u) == 0x0E000000u || (in & 0xFFC00000u) == 0xD5000000u)
            return 0;
        int mask = gpr_field_mask(in);
        if (uses_x18(in, mask)) return 0;
    }
    return 0;
}

struct deferred_branch {
    uint32_t *patch;
    uint64_t target;
    uint32_t instruction;
};

struct inline_context {
    uint64_t target;
    uint64_t resume;
    uint64_t return_pc;
    uint64_t expected_x30;
};

static void finish_block(uint64_t start, uint64_t guest_start, uint64_t guest_end, void *host, void *body,
                         uint64_t provenance_host, uint64_t provenance_guest, int provenance_fault_capable,
                         const struct deferred_branch *deferred, int deferred_count) {
    if (provenance_fault_capable) jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
    for (int i = 0; i < deferred_count; i++) {
        int64_t distance = ((uint8_t *)g_cp - (uint8_t *)deferred[i].patch) / 4;
        *deferred[i].patch = recode_cond(deferred[i].instruction, distance);
        emit_chain_exit(deferred[i].target);
    }
    chain_exit_dedup_finish();
    if (g_irq_patch || g_t2_irq_patch) {
        int pad_removed_t2_exit = g_t2_irq_patch != NULL;
        uint8_t *stub = g_cp;
        if (g_irq_patch) {
            uint32_t *patch = g_irq_patch;
            g_irq_patch = NULL;
            *patch = 0xB5000000u | (((uint32_t)((stub - (uint8_t *)patch) / 4) & 0x7FFFF) << 5) | 16;
        }
        if (g_t2_irq_patch) {
            uint32_t *patch = g_t2_irq_patch;
            g_t2_irq_patch = NULL;
            *patch = 0xB5000000u | (((uint32_t)((stub - (uint8_t *)patch) / 4) & 0x7FFFF) << 5) | 16;
        }
        uint8_t *exit_begin = g_cp;
        emit_exit_const(start, R_BRANCH);
        size_t exit_bytes = (size_t)(g_cp - exit_begin);
        if (pad_removed_t2_exit)
            for (size_t offset = 0; offset < exit_bytes; offset += 4)
                emit32(0xD503201Fu);
    }
    emit_a64_bus_stub();
    emit_a64_soft_stub();
    size_t emitted_bytes = (size_t)(g_cp - (uint8_t *)host);
    if (emitted_bytes >= (1u << 20))
        HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT,
                "large block guest=%#llx source=%#llx-%#llx bytes=%zu bus_sites=%u soft_sites=%u",
                (unsigned long long)start, (unsigned long long)guest_start, (unsigned long long)guest_end,
                emitted_bytes, g_bus_stub_patch_count, g_soft_stub_patch_count);
    g_last_body = body;
    g_last_guest_start = guest_start;
    g_last_guest_end = guest_end;
    if (!g_tier2_build) {
        map_put(start, guest_start, guest_end, host, body);
        txpg_mark(start, guest_end);
    }
}

static int translate_tls_instruction(uint32_t instruction) {
    if ((instruction & 0xFFFFFFE0u) == 0xD53BD040u) {
        int reg = instruction & 31;
        if (is_stolen(reg)) {
            if (stealfast_on()) {
                e_ldr(16, CPUREG, OFF_TLS);
                e_str(16, CPUREG, reg * 8);
            } else {
                x18_prolog();
                e_ldr(0, 1, OFF_TLS);
                e_str(0, 1, reg * 8);
                x18_epilog();
            }
        } else if (stealfast_on()) {
            e_ldr(reg, CPUREG, OFF_TLS);
        } else {
            e_load_cpu(reg);
            e_ldr(reg, reg, OFF_TLS);
        }
        return 1;
    }
    if ((instruction & 0xFFFFFFE0u) != 0xD51BD040u) return 0;

    int reg = instruction & 31;
    int scratch = reg == 16 ? 15 : 16;
    if (is_stolen(reg)) {
        if (stealfast_on()) {
            e_ldr(16, CPUREG, reg * 8);
            e_str(16, CPUREG, OFF_TLS);
        } else {
            x18_prolog();
            e_ldr(0, 1, reg * 8);
            e_str(0, 1, OFF_TLS);
            x18_epilog();
        }
    } else if (stealfast_on()) {
        e_str(reg, CPUREG, OFF_TLS);
    } else {
        e_str(scratch, CPUREG, (int)OFF_MSCRATCH);
        e_load_cpu(scratch);
        e_str(reg, scratch, OFF_TLS);
        e_ldr(scratch, CPUREG, (int)OFF_MSCRATCH);
    }
    return 1;
}

static enum translation_step translate_system_instruction(uint64_t guest_pc, uint32_t instruction) {
    /* Keep the guest CPU/cache model independent of host EL0 access and cache geometry. DC ZVA lowers to
       the advertised 64-byte zero operation; cache maintenance either queues invalidation or commits it. */
    if ((instruction & 0xFFFFFFE0u) == 0xD53B0020u) {
        emit_cpu_model_value(instruction & 31, g_aarch64_cpu_model.ctr_el0);
        return TRANSLATION_CONTINUE;
    }
    if ((instruction & 0xFFFFFFE0u) == 0xD53B00E0u) {
        emit_cpu_model_value(instruction & 31, g_aarch64_cpu_model.dczid_el0);
        return TRANSLATION_CONTINUE;
    }
    if ((instruction & 0xFFFF0000u) == 0xD5380000u && !g_aarch64_cpu_model.user_id_registers) {
        emit32(0);
        return TRANSLATION_CONTINUE;
    }
    if ((instruction & 0xFFFFFFE0u) == 0xD50B7420u) {
        int source = (int)(instruction & 31u);
        if (is_stolen(source))
            e_ldr(16, CPUREG, source * 8);
        else
            e_movr(16, source);
        emit32(0x927AE610u);
        if (jit_guest_bus_active()) emit_a64_bus_guard(16, 64, guest_pc);
        struct a64_soft_guard soft = emit_a64_soft_guard_begin(16, 17, 18, 64, HL_LOGICAL_VMA_WRITE, guest_pc);
        for (unsigned offset = 0; offset < 64; offset += 16)
            emit32(0xA9000000u | (((offset / 8) & 0x7Fu) << 15) | (31u << 10) | (16u << 5) | 31u);
        emit_a64_soft_guard_end(&soft);
        return TRANSLATION_CONTINUE;
    }
    if ((instruction & 0xFFFFFFE0u) == 0xD50B7B20u) {
        emit32(0xD503201Fu);
        return TRANSLATION_CONTINUE;
    }
    if ((instruction & 0xFFFFFFE0u) == 0xD50B7520u && !smc_disabled()) {
        emit_smc_queue((int)(instruction & 31));
        return TRANSLATION_CONTINUE;
    }
    if (instruction == 0xD5033FDFu && !smc_disabled()) {
        emit_exit_const(guest_pc + 4, R_ICCOMMIT);
        return TRANSLATION_STOP;
    }
    return TRANSLATION_UNHANDLED;
}

static enum translation_step translate_pc_relative_address(uint64_t guest_pc, uint32_t instruction,
                                                           uint64_t *guest_end) {
    if ((instruction & 0x9F000000u) == 0x10000000u) {
        int destination = instruction & 31;
        int64_t immediate = sext((((instruction >> 5) & 0x7FFFF) << 2) | ((instruction >> 29) & 3), 21);
        uint64_t value = pcrel_base(guest_pc) + immediate;
        if (is_stolen(destination)) {
            if (stealfast_on()) {
                e_movconst(16, value);
                e_str(16, CPUREG, destination * 8);
            } else {
                x18_prolog();
                e_movconst(0, value);
                e_str(0, 1, destination * 8);
                x18_epilog();
            }
        } else {
            e_movconst(destination, value);
        }
        return TRANSLATION_CONTINUE;
    }
    if ((instruction & 0x9F000000u) != 0x90000000u) return TRANSLATION_UNHANDLED;

    int destination = instruction & 31;
    int64_t immediate = sext((((instruction >> 5) & 0x7FFFF) << 2) | ((instruction >> 29) & 3), 21) << 12;
    uint64_t value = (pcrel_base(guest_pc) & ~0xFFFull) + immediate;
    if (hl_host_range_mapped((uintptr_t)guest_pc, 16)) {
        uint32_t load = a64_fetch_instruction(guest_pc + 4, NULL);
        uint32_t add = a64_fetch_instruction(guest_pc + 8, NULL);
        uint32_t branch = a64_fetch_instruction(guest_pc + 12, NULL);
        if (!guestbase_on() && !jit_guest_bus_active() && (instruction & 0x9F00001Fu) == 0x90000010u &&
            (load & 0xFFC003FFu) == 0xF9400211u && (add & 0xFFC003FFu) == 0x91000210u && branch == 0xD61F0220u) {
            if (!emit_guest_adrp_page(16, value)) e_movconst(16, value);
            e_str(16, CPUREG, 16 * 8);
            uint64_t load_host = (uint64_t)g_cp;
            emit32(load);
            pcache_record_provenance(load_host, (uint64_t)g_cp, guest_pc + 4);
            e_str(17, CPUREG, 17 * 8);
            emit32(add);
            e_str(16, CPUREG, 16 * 8);
            if (!g_tier2_build && g_txln_active)
                for (uint64_t line = guest_pc >> 6; line <= (guest_pc + 12) >> 6; line++)
                    txln_put(line);
            if (guest_pc + 16 > *guest_end) *guest_end = guest_pc + 16;
            emit_ibranch_ip2_ready(17, 1);
            return TRANSLATION_STOP;
        }
    }
    if (is_stolen(destination)) {
        if (stealfast_on()) {
            if (!emit_guest_adrp_page(16, value)) e_movconst(16, value);
            e_str(16, CPUREG, destination * 8);
        } else {
            x18_prolog();
            if (!emit_guest_adrp_page(0, value)) e_movconst(0, value);
            e_str(0, 1, destination * 8);
            x18_epilog();
        }
    } else if (!emit_guest_adrp_page(destination, value)) {
        e_movconst(destination, value);
    }
    return TRANSLATION_CONTINUE;
}

static int translate_literal_load(uint64_t guest_pc, uint32_t instruction) {
    int64_t offset = sext((instruction >> 5) & 0x7FFFF, 19) << 2;
    if ((instruction & 0xBF000000u) == 0x18000000u) {
        int target = instruction & 31;
        int is_64_bit = (instruction >> 30) & 1;
        int bytes = is_64_bit ? 8 : 4;
        if (is_stolen(target)) {
            e_movconst(16, guest_pc + offset);
            emit_a64_bus_guard(16, bytes, guest_pc);
            struct a64_soft_guard soft = emit_a64_soft_guard_begin(16, 17, 18, bytes, HL_LOGICAL_VMA_READ, guest_pc);
            if (is_64_bit)
                e_ldr(16, 16, 0);
            else
                emit32(0xB9400000u | (16 << 5) | 16);
            emit_a64_soft_guard_end(&soft);
            e_str(16, CPUREG, target * 8);
        } else {
            e_movconst(target, guest_pc + offset);
            emit_a64_bus_guard(target, bytes, guest_pc);
            struct a64_soft_guard soft =
                emit_a64_soft_guard_begin(target, 16, 17, bytes, HL_LOGICAL_VMA_READ, guest_pc);
            if (is_64_bit)
                e_ldr(target, target, 0);
            else
                emit32(0xB9400000u | (target << 5) | target);
            emit_a64_soft_guard_end(&soft);
        }
        emit_a64_soft_bounce_commit(guest_pc + 4);
        return 1;
    }
    if ((instruction & 0xFF000000u) == 0x98000000u) {
        int target = instruction & 31;
        int stolen = is_stolen(target);
        int address = stolen ? 16 : target;
        e_movconst(address, guest_pc + offset);
        emit_a64_bus_guard(address, 4, guest_pc);
        struct a64_soft_guard soft =
            emit_a64_soft_guard_begin(address, stolen ? 17 : 16, stolen ? 18 : 17, 4, HL_LOGICAL_VMA_READ, guest_pc);
        emit32(0xB9800000u | (address << 5) | address);
        emit_a64_soft_guard_end(&soft);
        if (stolen) e_str(16, CPUREG, target * 8);
        emit_a64_soft_bounce_commit(guest_pc + 4);
        return 1;
    }
    if ((instruction & 0x3F000000u) == 0x1C000000u && ((instruction >> 30) & 3) != 3) {
        int vector = instruction & 31;
        int size_shift = (instruction >> 30) & 3;
        uint64_t bytes = UINT64_C(4) << size_shift;
        uint32_t load = size_shift == 0 ? 0xBD400000u : (size_shift == 1 ? 0xFD400000u : 0x3DC00000u);
        e_movconst(16, guest_pc + offset);
        emit_a64_bus_guard(16, bytes, guest_pc);
        struct a64_soft_guard soft = emit_a64_soft_guard_begin(16, 17, 18, bytes, HL_LOGICAL_VMA_READ, guest_pc);
        emit32(load | (16u << 5) | (uint32_t)vector);
        emit_a64_soft_guard_end(&soft);
        emit_a64_soft_bounce_commit(guest_pc + 4);
        return 1;
    }
    if ((instruction & 0xFF000000u) == 0xD8000000u || is_prfm_register_or_immediate(instruction)) {
        emit32(0xD503201Fu);
        return 1;
    }
    return 0;
}

static void translate_memory_or_fallback(uint64_t guest_pc, uint32_t instruction, int in_exclusive_region) {
    if ((guestbase_on() || jit_guest_soft_active()) && !in_exclusive_region &&
        (jit_guest_soft_active() || ((instruction >> 5) & 31) != 31)) {
        if (is_foldable_mem(instruction)) {
            if (jit_guest_bus_active()) emit_a64_bus_guard_instruction(instruction, guest_pc);
            emit_fold_mem(instruction, 0);
            return;
        }
        if (is_advsimd_struct(instruction)) {
            emit_fold_advsimd_struct(instruction);
            return;
        }
    }
    if (jit_guest_bus_active()) {
        if (!guestbase_on() && !in_exclusive_region && is_foldable_mem(instruction))
            emit_a64_bus_guard_instruction(instruction, guest_pc);
        if (!guestbase_on() && !in_exclusive_region && is_advsimd_struct(instruction))
            emit_a64_bus_guard_base((instruction >> 5) & 31, 0, (uint64_t)advsimd_struct_bytes(instruction), guest_pc);
        if ((instruction & 0x3A000000u) == 0x28000000u) {
            int mode = (instruction >> 23) & 3;
            if (mode == 1 || mode == 3) {
                uint64_t bytes = a64_mem_bytes(instruction);
                int64_t offset = sext((instruction >> 15) & 0x7f, 7) * (int64_t)(bytes / 2);
                emit_a64_bus_guard_base((instruction >> 5) & 31, mode == 3 ? offset : 0, bytes, guest_pc);
            }
        }
        if ((instruction & 0x3FC00000u) == 0x08400000u && !is_casp(instruction)) {
            uint64_t bytes = UINT64_C(1) << ((instruction >> 30) & 3);
            if ((instruction >> 21) & 1) bytes *= 2;
            emit_a64_bus_guard_base((instruction >> 5) & 31, 0, bytes, guest_pc);
        }
    }
    if (jit_guest_soft_active() && (((instruction & 0x3F000000u) == 0x08000000u) || is_casp(instruction))) {
        emit_a64_soft_exclusive(instruction);
        return;
    }
    if (is_casp(instruction)) {
        if (casp_uses_stolen(instruction))
            emit_casp_mangled(instruction, -1);
        else
            emit32(instruction);
        return;
    }
    if ((instruction & 0x7FE0FFE0u) == 0x2A0003E0u) {
        int destination = instruction & 31;
        int source = (instruction >> 16) & 31;
        if (destination != 31 && !is_stolen(destination) && is_stolen(source)) {
            if (instruction >> 31)
                e_ldr(destination, CPUREG, source * 8);
            else
                emit32(0xB9400000u | ((unsigned)(source * 2) << 10) | ((unsigned)CPUREG << 5) | (unsigned)destination);
            return;
        }
    }
    if (stealfast_on() && (instruction & 0x7FFFFFE0u) == 0x52800000u && is_stolen(instruction & 31)) {
        e_str(31, CPUREG, (int)(instruction & 31) * 8);
        return;
    }
    int register_fields = gpr_field_mask(instruction);
    if (uses_x18(instruction, register_fields))
        emit_mangled_x18(instruction, register_fields);
    else
        emit32(instruction);
}

static uint64_t translate_special_instruction(uint64_t guest_pc, uint64_t block_start, uint64_t tail_carry_load,
                                              uint32_t instruction, int *in_exclusive_region) {
    if (!*in_exclusive_region) {
        if (tail_carry_load && guest_pc == block_start) {
            e_ldr(17, CPUREG, 30 * 8);
            emit32((instruction & ~(31u << 10)) | (17u << 10));
            return 4;
        }
        if (tail_carry_load && guest_pc == tail_carry_load) {
            emit32((instruction & ~(31u << 10)) | (17u << 10));
            e_str(17, CPUREG, 30 * 8);
            return 4;
        }
        int atomic_bytes = try_lse_atomic(guest_pc);
        if (atomic_bytes) {
            if (!g_tier2_build && g_txln_active)
                for (uint64_t line = guest_pc >> 6; line <= (guest_pc + atomic_bytes - 1) >> 6; line++)
                    txln_put(line);
            return (uint64_t)atomic_bytes;
        }
    }
    if (is_i8mm_mmla(instruction)) {
        emit_i8mm_mmla(instruction);
        return 4;
    }
    if (is_bf16_bfcvt(instruction)) {
        emit_bf16_bfcvt(instruction);
        return 4;
    }
    if (is_bf16_bfdot(instruction)) {
        emit_bf16_bfdot(instruction);
        return 4;
    }
    if (!is_casp(instruction)) {
        if ((instruction & 0x3FC00000u) == 0x08400000u)
            *in_exclusive_region = 1;
        else if (*in_exclusive_region && (instruction & 0x3FC00000u) == 0x08000000u)
            *in_exclusive_region = 0;
    }
    return 0;
}

struct conditional_branch_state {
    uint64_t start;
    void *body;
    uint64_t *seen;
    int *seen_count;
    int *trace_blocks;
    int *conditional_count;
    struct deferred_branch *deferred;
    int *deferred_count;
    int in_exclusive_region;
    int stitch_allowed;
};

static enum translation_step translate_test_bit_branch(uint64_t *guest_pc, uint32_t instruction,
                                                       struct conditional_branch_state *state) {
    if ((instruction & 0x7E000000u) != 0x36000000u) return TRANSLATION_UNHANDLED;

    int bit_low = (instruction >> 19) & 0x1F;
    int bit_high = (instruction >> 31) & 1;
    int operation = (instruction >> 24) & 1;
    int target_register = instruction & 31;
    int64_t offset = sext((instruction >> 5) & 0x3FFF, 14) << 2;
    uint64_t taken = *guest_pc + offset;
    uint64_t fallthrough = *guest_pc + 4;
    if (state->in_exclusive_region) {
        int index = (*state->deferred_count)++;
        state->deferred[index].patch = (uint32_t *)g_cp;
        state->deferred[index].target = taken;
        state->deferred[index].instruction = instruction;
        emit32(0);
        *guest_pc = fallthrough;
        return TRANSLATION_CONTINUE;
    }
    if (taken == state->start && !g_notier2 && !is_stolen(target_register) &&
        !loop_has_rmw_hazard(state->start, *guest_pc)) {
        int slot = g_tier2_build ? 0 : t2_slot(state->start);
        if (g_tier2_build || slot >= 0) {
            emit_selfloop(instruction, state->start, fallthrough, state->body, slot);
            return TRANSLATION_STOP;
        }
    }
    if (state->stitch_allowed && !is_stolen(target_register) && fallthrough != state->start &&
        !seen_has(state->seen, *state->seen_count, fallthrough) && !map_body(fallthrough)) {
        stitch_cond(instruction ^ (1u << 24), taken);
        state->seen[(*state->seen_count)++] = fallthrough;
        (*state->trace_blocks)++;
        (*state->conditional_count)++;
        *guest_pc = fallthrough;
        return TRANSLATION_CONTINUE;
    }
    int emitted_register = target_register;
    if (is_stolen(target_register)) {
        emitted_register = stealfast_on() ? 16 : 0;
        if (!stealfast_on()) e_str(0, CPUREG, (int)OFF_MSCRATCH);
        e_ldr(emitted_register, CPUREG, target_register * 8);
    }
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    if (is_stolen(target_register) && !stealfast_on()) e_ldr(0, CPUREG, (int)OFF_MSCRATCH);
    emit_chain_exit(fallthrough);
    int64_t distance = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
    *patch = 0x36000000u | ((unsigned)bit_high << 31) | ((unsigned)operation << 24) | ((unsigned)bit_low << 19) |
             ((uint32_t)(distance & 0x3FFF) << 5) | (unsigned)emitted_register;
    if (is_stolen(target_register) && !stealfast_on()) e_ldr(0, CPUREG, (int)OFF_MSCRATCH);
    emit_chain_exit(taken);
    return TRANSLATION_STOP;
}

static enum translation_step translate_compare_zero_branch(uint64_t *guest_pc, uint32_t instruction,
                                                           struct conditional_branch_state *state) {
    if ((instruction & 0x7E000000u) != 0x34000000u) return TRANSLATION_UNHANDLED;

    int64_t offset = sext((instruction >> 5) & 0x7FFFF, 19) << 2;
    uint64_t taken = *guest_pc + offset;
    uint64_t fallthrough = *guest_pc + 4;
    int is_64_bit = instruction >> 31;
    int operation = (instruction >> 24) & 1;
    int target_register = instruction & 31;
    if (state->in_exclusive_region) {
        int index = (*state->deferred_count)++;
        state->deferred[index].patch = (uint32_t *)g_cp;
        state->deferred[index].target = taken;
        state->deferred[index].instruction = instruction;
        emit32(0);
        *guest_pc = fallthrough;
        return TRANSLATION_CONTINUE;
    }
    uint32_t previous = *guest_pc >= state->start + 4 ? a64_fetch_instruction(*guest_pc - 4, NULL) : 0;
    uint32_t first = taken < *guest_pc ? a64_fetch_instruction(taken, NULL) : 0;
    int previous_load = (previous & 0x0A000000u) == 0x08000000u && ((previous >> 22) & 1u);
    int first_load = (first & 0x0A000000u) == 0x08000000u && ((first >> 22) & 1u);
    if (taken < *guest_pc && previous_load && first_load) emit32(0xD503203Fu);
    if (taken == state->start && !g_notier2 && !is_stolen(target_register) &&
        !loop_has_rmw_hazard(state->start, *guest_pc)) {
        int slot = g_tier2_build ? 0 : t2_slot(state->start);
        if (g_tier2_build || slot >= 0) {
            emit_selfloop(instruction, state->start, fallthrough, state->body, slot);
            return TRANSLATION_STOP;
        }
    }
    if (state->stitch_allowed && !is_stolen(target_register) && fallthrough != state->start &&
        !seen_has(state->seen, *state->seen_count, fallthrough) && !map_body(fallthrough)) {
        stitch_cond(instruction ^ (1u << 24), taken);
        state->seen[(*state->seen_count)++] = fallthrough;
        (*state->trace_blocks)++;
        (*state->conditional_count)++;
        *guest_pc = fallthrough;
        return TRANSLATION_CONTINUE;
    }
    int emitted_register = target_register;
    if (is_stolen(target_register)) {
        emitted_register = stealfast_on() ? 16 : 0;
        if (!stealfast_on()) e_str(0, CPUREG, (int)OFF_MSCRATCH);
        e_ldr(emitted_register, CPUREG, target_register * 8);
    }
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    if (is_stolen(target_register) && !stealfast_on()) e_ldr(0, CPUREG, (int)OFF_MSCRATCH);
    emit_chain_exit(fallthrough);
    int64_t distance = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
    *patch = 0x34000000u | ((unsigned)is_64_bit << 31) | ((unsigned)operation << 24) |
             ((uint32_t)(distance & 0x7FFFF) << 5) | (unsigned)emitted_register;
    if (is_stolen(target_register) && !stealfast_on()) e_ldr(0, CPUREG, (int)OFF_MSCRATCH);
    emit_chain_exit(taken);
    return TRANSLATION_STOP;
}

static enum translation_step translate_condition_branch(uint64_t *guest_pc, uint32_t instruction,
                                                        struct conditional_branch_state *state) {
    if ((instruction & 0xFF000010u) != 0x54000000u) return TRANSLATION_UNHANDLED;

    int condition = instruction & 0xF;
    int64_t offset = sext((instruction >> 5) & 0x7FFFF, 19) << 2;
    uint64_t taken = *guest_pc + offset;
    uint64_t fallthrough = *guest_pc + 4;
    if (state->in_exclusive_region) {
        int index = (*state->deferred_count)++;
        state->deferred[index].patch = (uint32_t *)g_cp;
        state->deferred[index].target = taken;
        state->deferred[index].instruction = instruction;
        emit32(0);
        *guest_pc = fallthrough;
        return TRANSLATION_CONTINUE;
    }
    if (taken == state->start && !g_notier2 && !loop_has_rmw_hazard(state->start, *guest_pc)) {
        int slot = g_tier2_build ? 0 : t2_slot(state->start);
        if (g_tier2_build || slot >= 0) {
            emit_selfloop(instruction, state->start, fallthrough, state->body, slot);
            return TRANSLATION_STOP;
        }
    }
    /* AL and NV both execute unconditionally in A64, so neither may use the inverted-condition stitch. */
    if (state->stitch_allowed && condition < 0xE && fallthrough != state->start &&
        !seen_has(state->seen, *state->seen_count, fallthrough) && !map_body(fallthrough)) {
        stitch_cond(instruction ^ 1u, taken);
        state->seen[(*state->seen_count)++] = fallthrough;
        (*state->trace_blocks)++;
        (*state->conditional_count)++;
        *guest_pc = fallthrough;
        return TRANSLATION_CONTINUE;
    }
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    emit_chain_exit(fallthrough);
    int64_t distance = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
    *patch = 0x54000000u | ((uint32_t)(distance & 0x7FFFF) << 5) | (unsigned)condition;
    emit_chain_exit(taken);
    return TRANSLATION_STOP;
}

static enum translation_step translate_indirect_control(uint64_t *guest_pc, uint32_t instruction,
                                                        struct inline_context *contexts, int *context_count) {
    if ((instruction & 0xFFFFFC1Fu) == 0xD65F0000u) {
        int source = (instruction >> 5) & 31;
        if (source == 30 && *context_count > 0 && *guest_pc == contexts[*context_count - 1].return_pc) {
            struct inline_context *context = &contexts[*context_count - 1];
            e_movr(16, 30);
            e_movconst(17, context->expected_x30);
            emit32(0xCB000000u | (17u << 16) | (16u << 5) | 16u);
            uint32_t *zero = (uint32_t *)g_cp;
            emit32(0);
            emit_ibranch(30);
            *zero = 0xB4000000u | (((uint32_t)((g_cp - (uint8_t *)zero) / 4) & 0x7FFFF) << 5) | 16;
            *guest_pc = context->resume;
            (*context_count)--;
            return TRANSLATION_CONTINUE;
        }
        if (source == 30)
            shadowgate() == -1 ? emit_ibranch(30) : emit_shadow_ret();
        else
            emit_ibranch(source);
        return TRANSLATION_STOP;
    }
    if ((instruction & 0xFFFFFC1Fu) == 0xD61F0000u) {
        int source = (instruction >> 5) & 31;
        if (g_steal1617 && !g_noibslim && !is_stolen(source) && is_interp_dispatch_br(*guest_pc, source))
            emit_hash_tail(source);
        else
            emit_ibranch(source);
        return TRANSLATION_STOP;
    }
    if ((instruction & 0xFFFFFC1Fu) != 0xD63F0000u) return TRANSLATION_UNHANDLED;

    int source = (instruction >> 5) & 31;
    if (source == 30) {
        e_ldr(17, CPUREG, 30 * 8);
        emit_set_x30(pcrel_base(*guest_pc) + 4);
        emit_ibranch_ip2_ready(17, 1);
    } else {
        emit_set_x30(pcrel_base(*guest_pc) + 4);
        emit_ibranch(source);
    }
    return TRANSLATION_STOP;
}

static void *translate_block(uint64_t gpc) {
    /* Observe writes made through another MAP_SHARED alias before decoding
       an executable view backed by an emulated host-page snapshot. */
    uint64_t source_page = gpc & ~UINT64_C(0xfff);
    filemap_refresh_emulated(source_page, source_page + UINT64_C(0x1000));
    // W4E tier-2: read NOTIER2 / TIER2_THRESHOLD once (idempotent) before any self-loop detection.
    tier2_env_init();
    // gpc is mutated by the decode loop; key the cache by START
    uint64_t start = gpc;
    chain_exit_dedup_reset();
    g_bus_stub_patch_count = 0;
    g_soft_stub_patch_count = 0;
    g_soft_legacy_stub_patch_count = 0;
    g_soft_resolver_patch_count = 0;
    uint64_t guest_start = gpc;
    uint64_t guest_end = gpc + 4;
    uint64_t tail_carry_ldp = scan_tail_x30_carry(gpc);
    g_blk_vdirty = 0;     // reset per block; set below when a V-writing insn is emitted
    g_t2_loop_top = NULL; // reset per block; set only in the tier-2 vdirty-hoist path below
    g_t2_irq_patch = NULL;
    void *host = g_cp;
    emit_prologue();
    // Keep the hot chained/IBTC entry stable independently of prologue size.
    // Cold dispatcher entry runs this padding once; hot entries target `body`
    // below and skip it.
    while ((uintptr_t)g_cp & 15)
        emit32(0xD503201Fu);
    // chained jumps land here (regs already live)
    void *body = g_cp;
    // poll cpu->irq at the body entry so a caught async signal reaches a no-syscall guest loop.
    emit_irq_check(start);
    // ldxr/ldaxr..stxr/stlxr exclusive regions must stay in ONE block with no injected
    // memory ops between them, else the monitor clears and stxr retries forever. While
    // inside such a region, conditional branches are emitted inline and their exits are
    // deferred to stubs after the store-exclusive.
    int in_excl = 0;

    struct deferred_branch defer[64];

    int ndefer = 0;
    uint64_t provenance_host = 0;
    uint64_t provenance_guest = 0;
    int provenance_fault_capable = 0;
    // opt4 region state: guest block-starts inlined into this region + a block budget. The
    // region STOPS (falls to the baseline single-block exit) at any dispatcher-mediated edge
    // (indirect br/blr, bl/call, ret, svc/syscall), inside an exclusive monitor region, or on
    // hitting the 16-block / 16 KB bound -- "when unsure, end the region".
    if (g_stitch < 0) g_stitch = 1;
    uint64_t seen[TRACE_MAX_BLK];
    int nseen = 0, trace_blk = 0;
    // opt4 conditional-stitch budget: each conditional fall-through laid inline is a SPECULATION -- the
    // guest may instead take the (chain-exit) branch, leaving the inlined tail dead. Deadness compounds
    // per conditional passed (measured on sqlite: depth-1 fall-throughs 28% never-executed, rising to
    // >85% by the 6th). Unconditional `b` edges follow the guaranteed path and are NOT budgeted, so
    // straight-line/loop-body traces still stitch freely; only chains of hard-to-predict conditionals are
    // cut. Ending a region early is always semantics-preserving: intermediate block-starts are never
    // registered in g_map, so the truncated successor self-heals as an on-demand fresh translation via the
    // ordinary chain-exit path (identical to the NOSTITCH baseline, just re-anchored deeper).
    int ncond = 0;

    struct inline_context ctx[CTX_INLINE_DEPTH];

    int nctx = 0;
#ifndef STITCH_MAX_COND
#define STITCH_MAX_COND 3
#endif
    // SMC precise gate (line-granular source set): record every 64B guest line this block is actually
    // decoded from, AS WE DECODE, instead of marking the whole contiguous [start,guest_end) hull after the
    // loop (see txpg_mark). For an opt4-stitched superblock the hull also spans the address GAPS between
    // the scattered inlined sub-blocks -- lines that hold no translated code -- so the post-loop hull
    // marked ~15x more lines than the block truly sourced (measured ~29 vs ~2 per block on sqlite), and
    // each txln_put is a cache miss into the 16MB line set. Marking only the decoded lines is a strict
    // subset of the hull yet still a complete superset of the REAL source lines (every translated byte came
    // from a decoded instruction), so txln_flush_class stays correct -- it can never miss a genuinely
    // self-modified source line. Skipped under g_tier2_build (the promoter never marks), matching the
    // post-loop guard.
    uint64_t tx_last_line = ~UINT64_C(0);
#define STITCH_OK                                                                                                      \
    (g_stitch && !smc_seen() && !in_excl && trace_blk < TRACE_MAX_BLK - 1 && ncond < STITCH_MAX_COND &&                \
     (g_cp - (uint8_t *)host) < TRACE_MAX_BYTES)
    for (;;) {
        // A basic block is not necessarily small: generated programs can contain tens of thousands of
        // straight-line instructions before their first control-flow edge.  File-backed BUS guards can
        // expand each guest memory operation into hundreds of host bytes.  Bound normal regions by emitted
        // size so the dispatcher's CACHE_EMIT_HEADROOM admission guarantee remains true.  Splitting at an
        // arbitrary instruction boundary is equivalent to an ordinary chain exit; exclusive sequences are
        // exempt because an injected dispatcher edge would clear the architectural monitor.
        if (!in_excl && ((size_t)(g_cp - (uint8_t *)host) >= CACHE_EMIT_HEADROOM / 2 ||
                         g_bus_stub_patch_count >= BUS_STUB_PATCH_MAX - 4)) {
            emit_chain_exit(gpc);
            break;
        }
        int fetch_ok;
        uint32_t in = a64_fetch_instruction(gpc, &fetch_ok);
        if (!fetch_ok) {
            e_movconst(9, gpc);
            e_str(9, CPUREG, OFF_FAULT_ADDR);
            emit_exit_const(gpc, R_FETCHFAULT);
            break;
        }
        if (stealfast_on() && !g_tier2_build && !in_excl && !guestbase_on() && !jit_guest_bus_active() &&
            !g_nonpie_lo) {
            int mask, read, write, fault;
            if (stolen_forward_classify(in, &mask, &read, &write, &fault) &&
                (stolen_forward_field(in, read, 16) || stolen_forward_field(in, read, 28))) {
                int count = 0, touches = 0, last_touch = -1;
                int need16 = 0, need28 = 0;
                for (; count < 12; count++) {
                    uint32_t win = a64_fetch_instruction(gpc + (uint64_t)count * 4, NULL);
                    int wm, wr, ww, wf;
                    if (!stolen_forward_classify(win, &wm, &wr, &ww, &wf)) break;
                    int r16 = stolen_forward_field(win, wr, 16);
                    int r28 = stolen_forward_field(win, wr, 28);
                    if (r16 || r28) {
                        touches++;
                        last_touch = count;
                        need16 |= r16;
                        need28 |= r28;
                    }
                }
                if (touches >= 3) {
                    int window = last_touch + 1;
                    if (provenance_fault_capable)
                        jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
                    /* Load x16 first: the second load still needs real x28. */
                    if (need16) e_ldr(16, CPUREG, 16 * 8);
                    if (need28) e_ldr(17, CPUREG, 28 * 8);
                    for (int i = 0; i < window; i++) {
                        uint64_t pc = gpc + (uint64_t)i * 4;
                        uint32_t win = a64_fetch_instruction(pc, NULL);
                        int wm, wr, ww, wf;
                        int ok = stolen_forward_classify(win, &wm, &wr, &ww, &wf);
                        assert(ok);
                        uint64_t hstart = (uint64_t)g_cp;
                        emit32(stolen_forward_rewrite(win, wm));
                        if (stolen_forward_field(win, ww, 16)) e_str(16, CPUREG, 16 * 8);
                        if (stolen_forward_field(win, ww, 28)) e_str(17, CPUREG, 28 * 8);
                        if (wf) jit_instruction_map_put(hstart, (uint64_t)g_cp, pc);
                    }
                    if (g_txln_active) {
                        uint64_t last = (gpc + (uint64_t)window * 4 - 1) >> 6;
                        for (uint64_t line = gpc >> 6; line <= last; line++)
                            txln_put(line);
                        tx_last_line = last;
                    }
                    if (gpc < guest_start) guest_start = gpc;
                    gpc += (uint64_t)window * 4;
                    if (gpc > guest_end) guest_end = gpc;
                    provenance_fault_capable = 0;
                    continue;
                }
            }
        }
        if (stealfast_on() && !g_tier2_build && !in_excl && !guestbase_on() && !jit_guest_bus_active() &&
            !g_nonpie_lo) {
            int mask, read, write;
            if (x28_alu_window_classify(in, &mask, &read, &write) && x28_alu_window_field(in, read)) {
                int count = 0, reads = 0, last_read = -1;
                for (; count < 12; count++) {
                    uint32_t win = a64_fetch_instruction(gpc + (uint64_t)count * 4, NULL);
                    int wm, wr, ww;
                    if (!x28_alu_window_classify(win, &wm, &wr, &ww)) break;
                    if (x28_alu_window_field(win, wr)) {
                        reads++;
                        last_read = count;
                    }
                }
                if (reads >= 3) {
                    int window = last_read + 1;
                    if (provenance_fault_capable)
                        jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
                    e_ldr(17, CPUREG, 28 * 8);
                    for (int i = 0; i < window; i++) {
                        uint64_t pc = gpc + (uint64_t)i * 4;
                        uint32_t win = a64_fetch_instruction(pc, NULL);
                        int wm, wr, ww;
                        int ok = x28_alu_window_classify(win, &wm, &wr, &ww);
                        assert(ok);
                        emit32(x28_alu_window_rewrite(win, wm));
                        if (x28_alu_window_field(win, ww)) e_str(17, CPUREG, 28 * 8);
                    }
                    if (g_txln_active) {
                        uint64_t last = (gpc + (uint64_t)window * 4 - 1) >> 6;
                        for (uint64_t line = gpc >> 6; line <= last; line++)
                            txln_put(line);
                        tx_last_line = last;
                    }
                    if (gpc < guest_start) guest_start = gpc;
                    gpc += (uint64_t)window * 4;
                    if (gpc > guest_end) guest_end = gpc;
                    provenance_fault_capable = 0;
                    continue;
                }
            }
        }
        if (!g_tier2_build && g_txln_active) {
            uint64_t tx_ln = gpc >> 6;
            if (tx_ln != tx_last_line) {
                txln_put(tx_ln);
                tx_last_line = tx_ln;
            }
        }
        if (gpc < guest_start) guest_start = gpc;
        if (gpc + 4 > guest_end) guest_end = gpc + 4;
        if (provenance_fault_capable) jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
        provenance_host = (uint64_t)g_cp;
        provenance_guest = gpc;
        uint32_t provenance_major = (in >> 25) & 0xFu;
        provenance_fault_capable =
            ((in & 0x0A000000u) == 0x08000000u) || provenance_major == 0xA || provenance_major == 0xB;
        g_emit_gpc = gpc; // IRQSLIM: tag the current guest PC for the forward/backward edge test in emit_chain_exit
        // at the FIRST vector-touching instruction of the region, store the (nonzero) cpu pointer
        // into cpu->vdirty so a later (possibly chained-to) syscall exit takes the full V spill. Emitted
        // once per region (g_blk_vdirty latch); flag-neutral `str` runs before the vector write. Regions are
        // linear (taken branches exit, only fall-through continues), so the first write dominates all later
        // vector writes -> one store covers every path. Zero cost on vector-free (integer/syscall) blocks.
        if (!g_blk_vdirty && insn_touches_vreg(in)) {
            e_str(CPUREG, CPUREG, (int)OFF_VDIRTY);
            g_blk_vdirty = 1;
            // W4E tier-2 vdirty hoist: only in the promoter recompile, and only when this V-writing
            // insn is the block's FIRST (== the self-loop top). Emit a fresh inline async poll right
            // after the store and record its address so the folded back-edge lands here -- skipping the
            // idempotent store while still polling cpu->irq every iteration (IRQSLIM back-edge invariant).
            // Every block ENTRY still runs the store first: the header poll path (body+0) falls straight
            // in, and a forward chain (body+g_fwdskip) lands exactly on the store. Non-self-loop V-first
            // blocks harmlessly gain one extra entry poll; g_t2_loop_top then goes unused.
            if (g_tier2_build && gpc == start) {
                g_t2_loop_top = g_cp;
                e_ldr(16, CPUREG, OFF_IRQ); // ldr x16, [cpu, #irq]
                g_t2_irq_patch = (uint32_t *)g_cp;
                emit32(0); // cbnz x16, shared out-of-line Lirq
            }
        }

        uint64_t consumed = translate_special_instruction(gpc, start, tail_carry_ldp, in, &in_excl);
        if (consumed) {
            gpc += consumed;
            continue;
        }
        // Defensive: the deferred-branch table is fixed-size. If a region ever fills it (pathological
        // or mis-decoded -- a real LDXR..STXR pair never holds this many conditional branches), end the
        // region here so the branches below take the normal exit path instead of overflowing defer[].
        if (in_excl && ndefer >= (int)(sizeof defer / sizeof defer[0])) in_excl = 0;

        // svc #0
        if (in == 0xD4000001u) {
            emit_exit_const(gpc, R_SYSCALL);
            break;
        }
        // b
        if ((in & 0xFC000000u) == 0x14000000u) {
            int64_t off = sext(in & 0x3FFFFFF, 26) << 2;
            uint64_t tgt = gpc + off;
            // opt4: follow the unconditional edge INLINE if its target is a fresh block (not the
            // region head, not already inlined, not already translated) -> the inter-block `b`
            // disappears. Otherwise chain normally (existing block / loop back-edge).
            if (STITCH_OK && tgt != start && !seen_has(seen, nseen, tgt) && !map_body(tgt)) {
                seen[nseen++] = tgt;
                trace_blk++;
                gpc = tgt;
                continue;
            }
            emit_chain_exit(tgt);
            break;
        }
        // bl
        if ((in & 0xFC000000u) == 0x94000000u) {
            int64_t off = sext(in & 0x3FFFFFF, 26) << 2;
            // Fuse a direct call to the exact canonical four-insn PLT veneer:
            //   adrp x16,page; ldr x17,[x16,#got]; add x16,x16,#lo; br x17
            // Preserve every architectural effect and the real fault-capable GOT
            // load; only the extra translated-block hop and its entry poll vanish.
            uint64_t plt = gpc + off;
            if (smc_seen()) goto no_bl_plt_fuse;
            if (!hl_host_range_mapped((uintptr_t)plt, 16)) goto no_bl_plt_fuse;
            uint32_t p0 = a64_fetch_instruction(plt, NULL);
            uint32_t p1 = a64_fetch_instruction(plt + 4, NULL);
            uint32_t p2 = a64_fetch_instruction(plt + 8, NULL);
            uint32_t p3 = a64_fetch_instruction(plt + 12, NULL);
            if (!guestbase_on() && !jit_guest_bus_active() && (p0 & 0x9F00001Fu) == 0x90000010u &&
                (p1 & 0xFFC003FFu) == 0xF9400211u && (p2 & 0xFFC003FFu) == 0x91000210u && p3 == 0xD61F0220u) {
                int64_t pimm = sext((((p0 >> 5) & 0x7FFFF) << 2) | ((p0 >> 29) & 3), 21) << 12;
                uint64_t page = (pcrel_base(plt) & ~0xFFFull) + pimm;
                emit_set_x30(pcrel_base(gpc) + 4);
                if (!emit_guest_adrp_page(16, page)) e_movconst(16, page);
                e_str(16, CPUREG, 16 * 8);
                uint64_t load_host = (uint64_t)g_cp;
                emit32(p1);
                pcache_record_provenance(load_host, (uint64_t)g_cp, plt + 4);
                e_str(17, CPUREG, 17 * 8);
                emit32(p2);
                e_str(16, CPUREG, 16 * 8);
                txpg_mark(plt, plt + 16);
                if (g_txln_active)
                    for (uint64_t line = plt >> 6; line <= (plt + 15) >> 6; line++)
                        txln_put(line);
                emit_ibranch_ip2_ready(17, 1);
                break;
            }
        no_bl_plt_fuse:
            // Inline an LSE outline-atomic helper call to a single host atomic op (elides the call +
            // return dispatch, the dominant atomics tax); only fires in the verbatim-safe regime.
            if (try_inline_outline_atomic(gpc, gpc + off)) {
                gpc += 4;
                continue;
            }
            uint64_t ancestors[CTX_INLINE_DEPTH];
            for (int i = 0; i < nctx; i++)
                ancestors[i] = ctx[i].target;
            uint64_t clone_ret;
            int clone_cost;
            /*
             * A BUS-active generation expands every cloned memory operation
             * with a runtime guard. Cloning then duplicates both hot guards
             * and cold stubs, accelerating cache rotation while removing only
             * a call/return pair. Keep ordinary context cloning unchanged, but
             * use the normal RAS call path while BUS observation is active.
             */
            if (!smc_seen() && !jit_guest_bus_active() && nctx < CTX_INLINE_DEPTH &&
                context_clone_candidate(gpc + off, ancestors, nctx, &clone_ret, &clone_cost) &&
                (g_cp - (uint8_t *)host) + clone_cost * 16 < TRACE_MAX_BYTES) {
                emit_set_x30(pcrel_base(gpc) + 4);
                ctx[nctx].target = gpc + off;
                ctx[nctx].resume = gpc + 4;
                ctx[nctx].return_pc = clone_ret;
                ctx[nctx].expected_x30 = pcrel_base(gpc) + 4;
                nctx++;
                gpc += off;
                continue;
            }
            emit_bl_ras(gpc, gpc + off);
            // §B: shadow push + host bl (RAS) + Lcont continuation
            break;
        }
        enum translation_step indirect_step = translate_indirect_control(&gpc, in, ctx, &nctx);
        if (indirect_step == TRANSLATION_CONTINUE) continue;
        if (indirect_step == TRANSLATION_STOP) break;
        if ((in & 0xFF000010u) == 0x54000000u) {
            struct conditional_branch_state branch_state = {
                .start = start,
                .body = body,
                .seen = seen,
                .seen_count = &nseen,
                .trace_blocks = &trace_blk,
                .conditional_count = &ncond,
                .deferred = defer,
                .deferred_count = &ndefer,
                .in_exclusive_region = in_excl,
                .stitch_allowed = STITCH_OK,
            };
            enum translation_step step = translate_condition_branch(&gpc, in, &branch_state);
            if (step == TRANSLATION_CONTINUE) continue;
            if (step == TRANSLATION_STOP) break;
        }
        if ((in & 0x7E000000u) == 0x34000000u) {
            struct conditional_branch_state branch_state = {
                .start = start,
                .body = body,
                .seen = seen,
                .seen_count = &nseen,
                .trace_blocks = &trace_blk,
                .conditional_count = &ncond,
                .deferred = defer,
                .deferred_count = &ndefer,
                .in_exclusive_region = in_excl,
                .stitch_allowed = STITCH_OK,
            };
            enum translation_step step = translate_compare_zero_branch(&gpc, in, &branch_state);
            if (step == TRANSLATION_CONTINUE) continue;
            if (step == TRANSLATION_STOP) break;
        }
        if ((in & 0x7E000000u) == 0x36000000u) {
            struct conditional_branch_state branch_state = {
                .start = start,
                .body = body,
                .seen = seen,
                .seen_count = &nseen,
                .trace_blocks = &trace_blk,
                .conditional_count = &ncond,
                .deferred = defer,
                .deferred_count = &ndefer,
                .in_exclusive_region = in_excl,
                .stitch_allowed = STITCH_OK,
            };
            enum translation_step test_bit_step = translate_test_bit_branch(&gpc, in, &branch_state);
            if (test_bit_step == TRANSLATION_CONTINUE) continue;
            if (test_bit_step == TRANSLATION_STOP) break;
        }

        if (translate_tls_instruction(in)) {
            gpc += 4;
            continue;
        }

        enum translation_step system_step = translate_system_instruction(gpc, in);
        if (system_step == TRANSLATION_CONTINUE) {
            gpc += 4;
            continue;
        }
        if (system_step == TRANSLATION_STOP) break;

        enum translation_step address_step = translate_pc_relative_address(gpc, in, &guest_end);
        if (address_step == TRANSLATION_CONTINUE) {
            gpc += 4;
            continue;
        }
        if (address_step == TRANSLATION_STOP) break;
        if (translate_literal_load(gpc, in)) {
            gpc += 4;
            continue;
        }

        enum translation_step authentication_step = translate_pointer_authentication(in);
        if (authentication_step == TRANSLATION_CONTINUE) {
            gpc += 4;
            continue;
        }
        if (authentication_step == TRANSLATION_STOP) break;

        translate_memory_or_fallback(gpc, in, in_excl);
        gpc += 4;
    }
    finish_block(start, guest_start, guest_end, host, body, provenance_host, provenance_guest, provenance_fault_capable,
                 defer, ndefer);
    // patch_links_to is MOVED to the dispatcher, AFTER the new block's icache is invalidated:
    // chaining an existing block X -> this new block before its code is icache-coherent on a peer
    // core lets that core fetch stale instructions. Only chain to it once it's visible everywhere.
    return host;
}

#undef STITCH_OK
