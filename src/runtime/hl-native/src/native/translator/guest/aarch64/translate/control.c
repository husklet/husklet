enum translation_step {
    TRANSLATION_UNHANDLED,
    TRANSLATION_CONTINUE,
    TRANSLATION_STOP,
};

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
