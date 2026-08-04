// x86/hx fixture: freestanding nolibc guest that COMPUTES 42 (never a literal) and prints it with a
// hand-rolled itoa — so the golden "42" proves arithmetic + div/mod lowering + the write path, not a
// constant-folded string. 42 = sum of 1..21 minus 189 = 231-189. Static non-PIE, no libc.
static long sys3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return r;
}

void _start(void) {
    long v = 0;
    for (int i = 1; i <= 21; i++)
        v += i;   // 231
    v -= 189;     // 42
    char buf[24];
    int p = sizeof buf;
    buf[--p] = '\n';
    do {
        buf[--p] = (char)('0' + (v % 10)); // idiv path
        v /= 10;
    } while (v);
    sys3(1, 1, (long)&buf[p], (long)(sizeof buf - p)); // SYS_write
    sys3(231, 0, 0, 0);                                // SYS_exit_group(0)
    __builtin_unreachable();
}
