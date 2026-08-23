#include <stdint.h>

#if defined(__aarch64__)
#define GUEST_EXIT 93
#define GUEST_GETPID 172
#define GUEST_GETTID 178
#define GUEST_MMAP 222
#define GUEST_PENDING 136
#define GUEST_PROCMASK 135
#define GUEST_SIGACTION 134
#define GUEST_SIGALTSTACK 132
#define GUEST_TGKILL 131
#define GUEST_TKILL 130
#define GUEST_WRITE 64
#define CONTEXT_PC 440

static long call6(long number, long first, long second, long third, long fourth, long fifth, long sixth) {
    register long x0 __asm__("x0") = first;
    register long x1 __asm__("x1") = second;
    register long x2 __asm__("x2") = third;
    register long x3 __asm__("x3") = fourth;
    register long x4 __asm__("x4") = fifth;
    register long x5 __asm__("x5") = sixth;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8) : "memory");
    return x0;
}
#elif defined(__x86_64__)
#define GUEST_EXIT 60
#define GUEST_GETPID 39
#define GUEST_GETTID 186
#define GUEST_MMAP 9
#define GUEST_PENDING 127
#define GUEST_PROCMASK 14
#define GUEST_SIGACTION 13
#define GUEST_SIGALTSTACK 131
#define GUEST_TGKILL 234
#define GUEST_TKILL 200
#define GUEST_WRITE 1
#define CONTEXT_PC 168

__attribute__((naked)) static void signal_restorer(void) {
    __asm__ volatile("mov $15, %rax; syscall");
}

static long call6(long number, long first, long second, long third, long fourth, long fifth, long sixth) {
    register long r10 __asm__("r10") = fourth;
    register long r8 __asm__("r8") = fifth;
    register long r9 __asm__("r9") = sixth;
    long result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(number), "D"(first), "S"(second), "d"(third), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return result;
}
#else
#error unsupported guest architecture
#endif

#if defined(__aarch64__)
#define ACTION_FLAGS 0x08000004
#define ACTION_RESTORER 0
#else
#define ACTION_FLAGS 0x0c000004
#define ACTION_RESTORER signal_restorer
#endif

struct action {
    void (*handler)(int);
    uint64_t flags;
    void (*restorer)(void);
    uint64_t mask;
};

struct stack {
    void *pointer;
    int flags;
    int padding;
    uint64_t size;
};

__attribute__((noreturn)) static void finish(long status) {
    call6(GUEST_EXIT, status, 0, 0, 0, 0, 0);
    __builtin_unreachable();
}

static void success(void) {
    static const char result[] = "signal-ok\n";
    if (call6(GUEST_WRITE, 1, (long)result, sizeof(result) - 1, 0, 0, 0) != sizeof(result) - 1) finish(23);
    finish(0);
}

static void second(void);

static void redirect(unsigned char *context, void (*target)(void)) {
    *(uint64_t *)(context + CONTEXT_PC) = (uint64_t)(uintptr_t)target;
}

static void final_handler(int signal, void *information, unsigned char *context) {
    (void)information;
    redirect(context, signal == 10 ? success : (void (*)(void))finish);
}

static void first_handler(int signal, void *information, unsigned char *context) {
    struct stack current;
    (void)information;
    if (signal != 10 || call6(GUEST_SIGALTSTACK, 0, (long)&current, 0, 0, 0, 0) != 0 || current.flags != 1) finish(20);
    redirect(context, second);
}

static void install(void (*handler)(int)) {
    struct action action = {handler, ACTION_FLAGS, ACTION_RESTORER, 0};
    long result = call6(GUEST_SIGACTION, 10, (long)&action, 0, 8, 0, 0);
    if (result != 0) finish(-result);
}

static void second(void) {
    install((void (*)(int))final_handler);
    long pid = call6(GUEST_GETPID, 0, 0, 0, 0, 0, 0);
    long tid = call6(GUEST_GETTID, 0, 0, 0, 0, 0, 0);
    if (call6(GUEST_TGKILL, pid, tid, 10, 0, 0, 0) != 0) finish(21);
    finish(22);
}

void _start(void) {
    long memory = call6(GUEST_MMAP, 0, 16384, 3, 0x22, -1, 0);
    if (memory < 0) finish(10);
    struct stack alternate = {(void *)memory, 0, 0, 16384};
    if (call6(GUEST_SIGALTSTACK, (long)&alternate, 0, 0, 0, 0, 0) != 0) finish(11);
    install((void (*)(int))first_handler);

    uint64_t selected = 1ULL << 9;
    uint64_t old = 0;
    if (call6(GUEST_PROCMASK, 0, (long)&selected, (long)&old, 8, 0, 0) != 0 || old != 0) finish(12);
    long tid = call6(GUEST_GETTID, 0, 0, 0, 0, 0, 0);
    if (call6(GUEST_TKILL, tid, 10, 0, 0, 0, 0) != 0) finish(13);
    uint64_t pending = 0;
    if (call6(GUEST_PENDING, (long)&pending, 8, 0, 0, 0, 0) != 0 || (pending & selected) == 0) finish(14);
    if (call6(GUEST_PROCMASK, 1, (long)&selected, 0, 8, 0, 0) != 0) finish(15);
    finish(16);
}
