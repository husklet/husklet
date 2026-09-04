/* Exact first-slice program for the AArch64-on-x86 DBT.  It intentionally
 * exercises discarded XZR writes, W-register zero extension, MOVK merge, and
 * the dispatcher-owned SVC path before exiting 42. */
__asm__(".global _start\n"
        ".type _start,%function\n"
        "_start:\n"
        "movz x0,#0xffff,lsl #32\n"
        "movz xzr,#0x1234\n"
        "movz w0,#0\n"
        "movk w0,#42\n"
        "movz x8,#93\n"
        "svc #0\n"
        ".size _start,.-_start\n");
