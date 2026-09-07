/* Two deliberately separate translated blocks. The forward edge enters block
 * 2 and its backward conditional edge returns to block 1. Keeping the loop
 * state register-only isolates dispatcher/link overhead from memory lowering. */
__asm__(".global _start\n"
        ".type _start,%function\n"
        "_start:\n"
        "movz x0,#0x4240\n"
        "movk x0,#0xf,lsl #16\n" /* 1,000,000 iterations */
        "movz x1,#0\n"
        "1:\n"
        "add x1,x1,#1\n"
        "b 2f\n"
        ".p2align 4\n"
        "2:\n"
        "subs x0,x0,#1\n"
        "b.ne 1b\n"
        "movz x2,#0x4240\n"
        "movk x2,#0xf,lsl #16\n"
        "cmp x1,x2\n"
        "b.ne 99f\n"
        "movz x0,#42\n"
        "b 3f\n"
        "99:\n"
        "movz x0,#1\n"
        "3:\n"
        "movz x8,#93\n"
        "svc #0\n"
        ".size _start,.-_start\n");
