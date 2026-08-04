// x86/hello fixture: the smallest possible x86-64 Linux guest — a freestanding nolibc binary whose
// _start issues exactly two raw syscalls: write(1, "hi\n", 3) then exit_group(42). No glibc startup,
// no auxv walking, no TLS init beyond the kernel ABI — so it pins the engine's bare entry path
// (ELF load → _start → syscall) for a static non-PIE (ET_EXEC) image with an absolute .rodata ref.
// Build: x86_64-linux-gnu-gcc -O2 -static -no-pie -nostdlib
static long sys3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return r;
}

void _start(void) {
    sys3(1, 1, (long)"hi\n", 3); // SYS_write
    sys3(231, 42, 0, 0);         // SYS_exit_group(42)
    __builtin_unreachable();
}
