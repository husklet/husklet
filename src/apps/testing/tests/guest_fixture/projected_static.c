static long call3(long n, long a, long b, long c) {
    long result;
    __asm__ volatile("syscall" : "=a"(result) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return result;
}

void _start(void) {
    call3(1, 1, (long)"projected-static-ok\n", 20);
    call3(60, 0, 0, 0);
    __builtin_unreachable();
}
