#include <stdint.h>

#define CLONE_FLAGS 0x01250f00UL
#define FUTEX_WAIT 0
#define FUTEX_WAKE 1
#define FUTEX_WAITERS 0x80000000U
#define FUTEX_OWNER_DIED 0x40000000U

struct robust_head {
    uint64_t next;
    int64_t offset;
    uint64_t pending;
};

struct robust_node {
    uint64_t next;
    uint32_t futex;
};

static volatile uint32_t child_tid __attribute__((aligned(65536))) = 1;
static volatile uint32_t ready = 1;
static volatile uint32_t gate = 1;
static volatile uint32_t attempts = 1;
static struct robust_head head = {1, 1, 1};
static struct robust_node node = {1, 1};
static uint8_t child_stack[4096] __attribute__((aligned(16))) = {1};

#if defined(__x86_64__)
static long call6(long number, long a, long b, long c, long d, long e, long f) {
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    long result;
    __asm__ volatile("syscall" : "=a"(result)
        : "a"(number), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory");
    return result;
}
#define SYS_CLONE 56
#define SYS_EXIT 60
#define SYS_FUTEX 202
#define SYS_ROBUST 273
#define CLONE_CALL() call6(SYS_CLONE, CLONE_FLAGS, (long)(child_stack + sizeof(child_stack)), 0, (long)&child_tid, 0, 0)
#elif defined(__aarch64__)
static long call6(long number, long a, long b, long c, long d, long e, long f) {
    register long x0 __asm__("x0") = a;
    register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c;
    register long x3 __asm__("x3") = d;
    register long x4 __asm__("x4") = e;
    register long x5 __asm__("x5") = f;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc 0" : "+r"(x0)
        : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8) : "memory");
    return x0;
}
#define SYS_CLONE 220
#define SYS_EXIT 93
#define SYS_FUTEX 98
#define SYS_ROBUST 99
#define CLONE_CALL() call6(SYS_CLONE, CLONE_FLAGS, (long)(child_stack + sizeof(child_stack)), 0, 0, (long)&child_tid, 0)
#else
#error unsupported guest architecture
#endif

static void finish(long status) {
    call6(SYS_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) {}
}

void _start(void) {
    child_tid = 0;
    ready = 0;
    gate = 0;
    attempts = 0;
    long cloned = CLONE_CALL();
    if (cloned < 0) finish(10);
    if (cloned == 0) {
        uint32_t tid = child_tid;
        node.next = (uint64_t)(uintptr_t)&head;
        node.futex = tid | FUTEX_WAITERS;
        head.next = (uint64_t)(uintptr_t)&node;
        head.offset = 8;
        head.pending = 0;
        if (call6(SYS_ROBUST, (long)&head, 24, 0, 0, 0, 0) != 0) finish(11);
        ready = 1;
        call6(SYS_FUTEX, (long)&ready, FUTEX_WAKE, 1, 0, 0, 0);
        while (gate == 0) call6(SYS_FUTEX, (long)&gate, FUTEX_WAIT, 0, 0, 0, 0);
        finish(0);
    }
    if ((uint32_t)cloned != child_tid) finish(20);
    while (ready == 0) call6(SYS_FUTEX, (long)&ready, FUTEX_WAIT, 0, 0, 0, 0);
    while (call6(SYS_FUTEX, (long)&gate, FUTEX_WAKE, 1, 0, 0, 0) != 1) {
        if (++attempts == 100) finish(23);
    }
    gate = 1;
    call6(SYS_FUTEX, (long)&gate, FUTEX_WAKE, 1, 0, 0, 0);
    while (child_tid != 0) {
        uint32_t observed = child_tid;
        call6(SYS_FUTEX, (long)&child_tid, FUTEX_WAIT, observed, 0, 0, 0);
    }
    if (node.futex != (FUTEX_WAITERS | FUTEX_OWNER_DIED)) finish(22);
    finish(0);
}
