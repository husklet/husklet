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
#define SYS_UNAME 160
#define MACHINE "aarch64"
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
#define SYS_UNAME 63
#define MACHINE "x86_64"
#else
#error unsupported architecture
#endif

#define EFAULT 14
#define FIELD_SIZE 65

struct name {
    char sysname[FIELD_SIZE];
    char nodename[FIELD_SIZE];
    char release[FIELD_SIZE];
    char version[FIELD_SIZE];
    char machine[FIELD_SIZE];
    char domainname[FIELD_SIZE];
};

static int equal(const char *left, const char *right) {
    while (*left && *left == *right) {
        ++left;
        ++right;
    }
    return *left == *right;
}

__attribute__((noreturn)) static void finish(long status) {
    call(SYS_EXIT, status, 0, 0);
    __builtin_unreachable();
}

void _start(void) {
    static struct name identity;
    static const char output[] = "uname-ok\n";

    if (call(SYS_UNAME, 0, 0, 0) != -EFAULT) finish(1);
    if (call(SYS_UNAME, (long)&identity, 0, 0) != 0) finish(2);
    if (!equal(identity.sysname, "Linux")) finish(3);
    if (!equal(identity.nodename, "jit")) finish(4);
    if (!equal(identity.release, "6.1.0")) finish(5);
    if (!equal(identity.version, "#1 jit")) finish(6);
    if (!equal(identity.machine, MACHINE)) finish(7);
    if (identity.domainname[0] != 0) finish(8);
    if (call(SYS_WRITE, 1, (long)output, sizeof(output) - 1) != sizeof(output) - 1) finish(9);
    finish(0);
}
