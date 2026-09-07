/* Measurement-only kernel for the AArch64 data-processing (3 source) family.
 * The runtime correctness case covers every allocated form; this loop makes
 * the family's execution cost large enough to measure without startup noise. */
__asm__(".global _start\n"
        ".type _start,%function\n"
        "_start:\n"
        "movz x0,#1\n"
        "movz x1,#3\n"
        "movz x2,#5\n"
        "movz x10,#0x2d00\n"
        "movk x10,#0x131,lsl #16\n" /* 20,000,000 iterations */
        "1:\n"
        "madd x0,x0,x1,x2\n"
        "subs x10,x10,#1\n"
        "b.ne 1b\n"
        "movz x0,#42\n"
        "movz x8,#93\n"
        "svc #0\n"
        ".size _start,.-_start\n");
