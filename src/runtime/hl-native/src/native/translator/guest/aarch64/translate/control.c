enum translation_step {
    TRANSLATION_UNHANDLED,
    TRANSLATION_CONTINUE,
    TRANSLATION_STOP,
};

struct unconditional_branch_state {
    uint64_t start;
    uint64_t *seen;
    int *seen_count;
    int *trace_blocks;
    int stitch_allowed;
};

struct inline_context {
    uint64_t target;
    uint64_t resume;
    uint64_t return_pc;
    uint64_t expected_x30;
};

struct direct_call_state {
    void *host;
    struct inline_context *contexts;
    int *context_count;
};

struct deferred_branch;

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

static enum translation_step translate_condition_branch(uint64_t *guest_pc, uint32_t instruction,
                                                        struct conditional_branch_state *state);
static enum translation_step translate_compare_zero_branch(uint64_t *guest_pc, uint32_t instruction,
                                                           struct conditional_branch_state *state);
static enum translation_step translate_test_bit_branch(uint64_t *guest_pc, uint32_t instruction,
                                                       struct conditional_branch_state *state);

static enum translation_step translate_conditional_control(uint64_t *guest_pc, uint32_t instruction,
                                                           struct conditional_branch_state *state) {
    enum translation_step step = translate_condition_branch(guest_pc, instruction, state);
    if (step != TRANSLATION_UNHANDLED) return step;
    step = translate_compare_zero_branch(guest_pc, instruction, state);
    if (step != TRANSLATION_UNHANDLED) return step;
    return translate_test_bit_branch(guest_pc, instruction, state);
}

static enum translation_step translate_unconditional_branch(uint64_t *guest_pc, uint32_t instruction,
                                                            struct unconditional_branch_state *state) {
    if ((instruction & 0xFC000000u) != 0x14000000u) return TRANSLATION_UNHANDLED;

    int64_t offset = sext(instruction & 0x3FFFFFF, 26) << 2;
    uint64_t target = *guest_pc + offset;
    if (state->stitch_allowed && target != state->start && !seen_has(state->seen, *state->seen_count, target) &&
        !map_body(target)) {
        state->seen[(*state->seen_count)++] = target;
        (*state->trace_blocks)++;
        *guest_pc = target;
        return TRANSLATION_CONTINUE;
    }
    emit_chain_exit(target);
    return TRANSLATION_STOP;
}

static enum translation_step translate_direct_call(uint64_t *guest_pc, uint32_t instruction,
                                                   struct direct_call_state *state) {
    if ((instruction & 0xFC000000u) != 0x94000000u) return TRANSLATION_UNHANDLED;

    int64_t offset = sext(instruction & 0x3FFFFFF, 26) << 2;
    uint64_t plt = *guest_pc + offset;
    if (!smc_seen() && hl_host_range_mapped((uintptr_t)plt, 16)) {
        uint32_t page_instruction = a64_fetch_instruction(plt, NULL);
        uint32_t load = a64_fetch_instruction(plt + 4, NULL);
        uint32_t add = a64_fetch_instruction(plt + 8, NULL);
        uint32_t branch = a64_fetch_instruction(plt + 12, NULL);
        if (!guestbase_on() && !jit_guest_bus_active() && (page_instruction & 0x9F00001Fu) == 0x90000010u &&
            (load & 0xFFC003FFu) == 0xF9400211u && (add & 0xFFC003FFu) == 0x91000210u && branch == 0xD61F0220u) {
            int64_t immediate = sext((((page_instruction >> 5) & 0x7FFFF) << 2) | ((page_instruction >> 29) & 3), 21)
                                << 12;
            uint64_t page = (pcrel_base(plt) & ~0xFFFull) + immediate;
            emit_set_x30(pcrel_base(*guest_pc) + 4);
            if (!emit_guest_adrp_page(16, page)) e_movconst(16, page);
            e_str(16, CPUREG, 16 * 8);
            uint64_t load_host = (uint64_t)g_cp;
            emit32(load);
            pcache_record_provenance(load_host, (uint64_t)g_cp, plt + 4);
            e_str(17, CPUREG, 17 * 8);
            emit32(add);
            e_str(16, CPUREG, 16 * 8);
            txpg_mark(plt, plt + 16);
            if (g_txln_active)
                for (uint64_t line = plt >> 6; line <= (plt + 15) >> 6; line++)
                    txln_put(line);
            emit_ibranch_ip2_ready(17, 1);
            return TRANSLATION_STOP;
        }
    }
    if (try_inline_outline_atomic(*guest_pc, *guest_pc + offset)) {
        *guest_pc += 4;
        return TRANSLATION_CONTINUE;
    }
    uint64_t ancestors[CTX_INLINE_DEPTH];
    for (int i = 0; i < *state->context_count; i++)
        ancestors[i] = state->contexts[i].target;
    uint64_t clone_return;
    int clone_cost;
    if (!smc_seen() && !jit_guest_bus_active() && *state->context_count < CTX_INLINE_DEPTH &&
        context_clone_candidate(*guest_pc + offset, ancestors, *state->context_count, &clone_return, &clone_cost) &&
        (g_cp - (uint8_t *)state->host) + clone_cost * 16 < TRACE_MAX_BYTES) {
        emit_set_x30(pcrel_base(*guest_pc) + 4);
        struct inline_context *context = &state->contexts[*state->context_count];
        context->target = *guest_pc + offset;
        context->resume = *guest_pc + 4;
        context->return_pc = clone_return;
        context->expected_x30 = pcrel_base(*guest_pc) + 4;
        (*state->context_count)++;
        *guest_pc += offset;
        return TRANSLATION_CONTINUE;
    }
    emit_bl_ras(*guest_pc, *guest_pc + offset);
    return TRANSLATION_STOP;
}

static enum translation_step translate_pointer_authentication(uint32_t instruction) {
    /* Husklet does not enforce pointer authentication. Signing guest x30 on a PAC-capable host would
       corrupt the shadow-stack return match, which expects the unsigned guest value. Neutralize the
       PAC/AUT hints and lower authenticated returns through the ordinary guest return path. */
    if ((instruction & 0xFFFFFF1Fu) == 0xD503231Fu) {
        emit32(0xD503201Fu);
        return TRANSLATION_CONTINUE;
    }
    if ((instruction & 0xFFFFFBFFu) == 0xD65F0BFFu) {
        shadowgate() == -1 ? emit_ibranch(30) : emit_shadow_ret();
        return TRANSLATION_STOP;
    }
    return TRANSLATION_UNHANDLED;
}
