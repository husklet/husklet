#if defined(__aarch64__)
static inline long call(long number, long first, long second, long third) {
    register long x0 __asm__("x0") = first;
    register long x1 __asm__("x1") = second;
    register long x2 __asm__("x2") = third;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
}

#define SYS_WRITE 64
#define SYS_EXIT 93
#define SYS_OPENAT 56
#define SYS_CLOSE 57
#define SYS_UNSHARE 97
#define SYS_SETNS 268
#elif defined(__x86_64__)
static inline long call(long number, long first, long second, long third) {
    register long rax __asm__("rax") = number;
    register long rdi __asm__("rdi") = first;
    register long rsi __asm__("rsi") = second;
    register long rdx __asm__("rdx") = third;
    __asm__ volatile("syscall" : "+r"(rax) : "r"(rdi), "r"(rsi), "r"(rdx) : "rcx", "r11", "memory");
    return rax;
}

#define SYS_WRITE 1
#define SYS_EXIT 60
#define SYS_OPENAT 257
#define SYS_CLOSE 3
#define SYS_UNSHARE 272
#define SYS_SETNS 308
#else
#error unsupported architecture
#endif

#define AT_FDCWD (-100)
#define CLONE_NEWUTS 0x04000000
#define CLONE_NEWNET 0x40000000
#define EPERM 1
#define EBADF 9
#define EINVAL 22

__attribute__((noreturn)) static void finish(long status) {
    call(SYS_EXIT, status, 0, 0);
    __builtin_unreachable();
}

void _start(void) {
    static const char path[] = "/proc/self/ns/uts";
    static const char output[] = "namespace-ok\n";
    long descriptor = call(SYS_OPENAT, AT_FDCWD, (long)path, 0);
    if (descriptor < 0) finish(-descriptor);
    if (call(SYS_UNSHARE, 0, 0, 0) != 0) finish(2);
    if (call(SYS_UNSHARE, 1, 0, 0) != -EINVAL) finish(3);
    if (call(SYS_SETNS, -1, CLONE_NEWUTS, 0) != -EBADF) finish(4);
    if (call(SYS_SETNS, descriptor, CLONE_NEWNET, 0) != -EINVAL) finish(5);
    if (call(SYS_SETNS, descriptor, CLONE_NEWUTS, 0) != -EPERM) finish(6);
    if (call(SYS_CLOSE, descriptor, 0, 0) != 0) finish(7);
    if (call(SYS_WRITE, 1, (long)output, sizeof(output) - 1) != sizeof(output) - 1) finish(8);
    finish(0);
}
