#include "abi.h"
#include <stdint.h>

#if defined(__aarch64__)
#define GUEST_MMAP 222
#define GUEST_PENDING 136
#define GUEST_PROCMASK 135
#define GUEST_SIGACTION 134
#define GUEST_SIGALTSTACK 132
#define GUEST_TGKILL 131
#define GUEST_TKILL 130
#define GUEST_GETTID 178
#define CONTEXT_PC 440
static long call6(long n, long a, long b, long c, long d, long e, long f) {
    register long x0 __asm__("x0") = a; register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c; register long x3 __asm__("x3") = d;
    register long x4 __asm__("x4") = e; register long x5 __asm__("x5") = f;
    register long x8 __asm__("x8") = n;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3),
        "r"(x4), "r"(x5), "r"(x8) : "memory"); return x0;
}
#elif defined(__x86_64__)
#define GUEST_MMAP 9
#define GUEST_PENDING 127
#define GUEST_PROCMASK 14
#define GUEST_SIGACTION 13
#define GUEST_SIGALTSTACK 131
#define GUEST_TGKILL 234
#define GUEST_TKILL 200
#define GUEST_GETTID 186
#define CONTEXT_PC 168
static long call6(long n, long a, long b, long c, long d, long e, long f) {
    register long r10 __asm__("r10") = d; register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f; long result;
    __asm__ volatile("syscall" : "=a"(result) : "a"(n), "D"(a), "S"(b),
        "d"(c), "r"(r10), "r"(r8), "r"(r9) : "rcx", "r11", "memory");
    return result;
}
#endif

struct action { void (*handler)(int); uint64_t flags; void (*restorer)(void); uint64_t mask; };
struct stack { void *pointer; int flags; int padding; uint64_t size; };

static void success(void) { guest_exit(0); }
static void second(void);

static void redirect(unsigned char *context, void (*target)(void)) {
    *(uint64_t *)(context + CONTEXT_PC) = (uint64_t)(uintptr_t)target;
}

static void final_handler(int signal, void *information, unsigned char *context) {
    (void)information;
    redirect(context, signal == 10 ? success : (void (*)(void))guest_exit);
}

static void first_handler(int signal, void *information, unsigned char *context) {
    (void)information;
    struct stack current;
    if (signal != 10 || call6(GUEST_SIGALTSTACK, 0, (long)&current, 0, 0, 0, 0) != 0
        || current.flags != 1) guest_exit(20);
    redirect(context, second);
}

static void install(void (*handler)(int)) {
    struct action action = { handler, 0x08000004, 0, 0 };
    long result = call6(GUEST_SIGACTION, 10, (long)&action, 0, 8, 0, 0);
    if (result != 0) guest_exit(-result);
}

static void second(void) {
    install((void (*)(int))final_handler);
    long pid = guest_call(GUEST_GETPID, 0, 0, 0);
    long tid = guest_call(GUEST_GETTID, 0, 0, 0);
    if (call6(GUEST_TGKILL, pid, tid, 10, 0, 0, 0) != 0) guest_exit(21);
    guest_exit(22);
}

void _start(void) {
    long memory = call6(GUEST_MMAP, 0, 16384, 3, 0x22, -1, 0);
    if (memory < 0) guest_exit(10);
    struct stack alternate = { (void *)memory, 0, 0, 16384 };
    if (call6(GUEST_SIGALTSTACK, (long)&alternate, 0, 0, 0, 0, 0) != 0) guest_exit(11);
    install((void (*)(int))first_handler);
    uint64_t selected = 1ULL << 9;
    uint64_t old = 0;
    if (call6(GUEST_PROCMASK, 0, (long)&selected, (long)&old, 8, 0, 0) != 0
        || old != 0) guest_exit(12);
    long tid = guest_call(GUEST_GETTID, 0, 0, 0);
    if (call6(GUEST_TKILL, tid, 10, 0, 0, 0, 0) != 0) guest_exit(13);
    uint64_t pending = 0;
    if (call6(GUEST_PENDING, (long)&pending, 8, 0, 0, 0, 0) != 0
        || (pending & selected) == 0) guest_exit(14);
    if (call6(GUEST_PROCMASK, 1, (long)&selected, 0, 8, 0, 0) != 0) guest_exit(15);
    guest_exit(16);
}
