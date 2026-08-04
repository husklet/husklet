#if defined(__aarch64__)
#define GUEST_OPENAT 56
#define GUEST_CLOSE 57
#define GUEST_UNSHARE 97
#define GUEST_SETNS 268
#define GUEST_WRITE 64
#define GUEST_EXIT 93
#elif defined(__x86_64__)
#define GUEST_OPENAT 257
#define GUEST_CLOSE 3
#define GUEST_UNSHARE 272
#define GUEST_SETNS 308
#define GUEST_WRITE 1
#define GUEST_EXIT 60
#else
#error unsupported guest architecture
#endif

#define AT_FDCWD (-100)
#define CLONE_NEWUTS 0x04000000
#define CLONE_NEWUSER 0x10000000
#define CLONE_NEWNET 0x40000000
#define EBADF 9
#define EPERM 1
#define ENOSYS 38
#define EINVAL 22

static inline long guest_call(long number, long first, long second, long third) {
#if defined(__aarch64__)
    register long x0 __asm__("x0") = first;
    register long x1 __asm__("x1") = second;
    register long x2 __asm__("x2") = third;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
#else
    register long rax __asm__("rax") = number;
    register long rdi __asm__("rdi") = first;
    register long rsi __asm__("rsi") = second;
    register long rdx __asm__("rdx") = third;
    __asm__ volatile("syscall" : "+r"(rax) : "r"(rdi), "r"(rsi), "r"(rdx) : "rcx", "r11", "memory");
    return rax;
#endif
}

__attribute__((noreturn)) static inline void guest_exit(long status) {
    guest_call(GUEST_EXIT, status, 0, 0);
    __builtin_unreachable();
}

static inline long guest_write(const char *bytes, long length) {
    return guest_call(GUEST_WRITE, 1, (long)bytes, length);
}

void _start(void) {
    static const char path[] = "/proc/self/ns/uts";
    static const char message[] = "namespace-ok\n";
    long descriptor = guest_call(GUEST_OPENAT, AT_FDCWD, (long)path, 0);
    if (descriptor < 0) guest_exit(-descriptor);
    if (guest_call(GUEST_UNSHARE, 0, 0, 0) != 0) guest_exit(2);
    if (guest_call(GUEST_UNSHARE, 1, 0, 0) != -EINVAL) guest_exit(3);
    if (guest_call(GUEST_UNSHARE, CLONE_NEWUSER, 0, 0) != -ENOSYS) guest_exit(8);
    if (guest_call(GUEST_SETNS, -1, CLONE_NEWUTS, 0) != -EBADF) guest_exit(4);
    if (guest_call(GUEST_SETNS, descriptor, CLONE_NEWNET, 0) != -EINVAL) guest_exit(5);
    if (guest_call(GUEST_SETNS, descriptor, CLONE_NEWUTS, 0) != -EPERM) guest_exit(6);
    if (guest_call(GUEST_CLOSE, descriptor, 0, 0) != 0) guest_exit(7);
    guest_write(message, sizeof(message) - 1);
    guest_exit(0);
}
