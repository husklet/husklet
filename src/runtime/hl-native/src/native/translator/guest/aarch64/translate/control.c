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
