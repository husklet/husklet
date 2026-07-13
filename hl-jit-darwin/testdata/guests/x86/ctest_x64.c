// x86/ctest fixture: freestanding nolibc compute-and-exit guest. Runs 1000 rounds of the numerical-
// recipes LCG (x = x*1664525 + 1013904223 mod 2^32, x0 = 1) — 32-bit imul/add wraparound — and exits
// 7 only if the result is the precomputed constant, else 1. A miscompiled multiply/flag path shows up
// as the wrong exit code, not a crash. Static non-PIE, no libc (see build.sh).
static void sys_exit(long code) {
    __asm__ volatile("syscall" : : "a"(231L), "D"(code) : "rcx", "r11", "memory"); // SYS_exit_group
    __builtin_unreachable();
}

void _start(void) {
    unsigned int x = 1;
    for (int i = 0; i < 1000; i++)
        x = x * 1664525u + 1013904223u;
    sys_exit(x == 645503657u ? 7 : 1);
}
