// sigurg_preempt.c -- #423 regression: an async SIGURG (Go's async-preempt signal) delivered at an
// ARBITRARY instruction boundary must never corrupt the interrupted guest frame's return address or its
// callee-saved registers. hl builds a Linux rt_sigframe on the guest stack to run the handler; if that
// frame is laid flush at (or above) the interrupted SP it overlaps the still-live interrupted frame and
// clobbers a stack return address -> the classic influxd/victoria-metrics SIGSEGV storm.
//
// The storm: a hammer thread tgkill()s SIGURG at the main thread as fast as it can while the main thread
// spins a JIT'd inner loop that (a) keeps ten distinct sentinels in the callee-saved registers x19..x28
// and (b) has a live return address on its own stack (each iteration does a bl/ret to a leaf, and the
// enclosing function's own LR is spilled). If any SIGURG delivery clobbers a callee-saved reg or the
// spilled RA, regs_survive() returns 0 (mismatch) or the program returns to garbage and crashes. A pure
// deterministic checksum is also computed so the oracle (native run) and the JIT must print byte-identical
// output. Handler installed with SA_ONSTACK + a sigaltstack, mirroring Go's gsignal stack.
#define _GNU_SOURCE
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static volatile sig_atomic_t g_hits = 0;
static volatile int g_run = 1;
static int g_target_tid;

static void on_sigurg(int s) {
    (void)s;
    g_hits++;
} // async-preempt handler: just return (Go's does far more, but returning is the stress case)

// Spins a loop that pins ten sentinels into x19..x28 (the aarch64 callee-saved GPRs), does a bl/ret to a
// leaf every iteration (so a return address is live in x30 / spilled on the frame throughout the storm),
// then verifies every sentinel survived. Returns 1 iff all ten callee-saved regs are intact. Written in
// inline asm so the register residency + the live RA are guaranteed regardless of the optimizer.
static int __attribute__((noinline)) regs_survive(long iters) {
    long bad = 1;
    __asm__ __volatile__(
        "movz x19, #0x1119\n movz x20, #0x2220\n movz x21, #0x3321\n movz x22, #0x4422\n"
        "movz x23, #0x5523\n movz x24, #0x6624\n movz x25, #0x7725\n movz x26, #0x8826\n"
        "movz x27, #0x9927\n movz x28, #0xAA28\n"
        "mov  x9, %[iters]\n"
        "1:\n"
        "  bl   2f\n"    // leaf call: LR now points at the following insn; must survive an async signal
        "  b    3f\n"    // (leaf returns here)
        "2:\n"
        "  add  x10, x10, #1\n"
        "  ret\n"        // return via x30 -- a clobbered LR here jumps to garbage
        "3:\n"
        "  subs x9, x9, #1\n"
        "  b.ne 1b\n"
        // verify each callee-saved sentinel is intact
        "  movz x11, #0x1119\n cmp x19, x11\n b.ne 9f\n"
        "  movz x11, #0x2220\n cmp x20, x11\n b.ne 9f\n"
        "  movz x11, #0x3321\n cmp x21, x11\n b.ne 9f\n"
        "  movz x11, #0x4422\n cmp x22, x11\n b.ne 9f\n"
        "  movz x11, #0x5523\n cmp x23, x11\n b.ne 9f\n"
        "  movz x11, #0x6624\n cmp x24, x11\n b.ne 9f\n"
        "  movz x11, #0x7725\n cmp x25, x11\n b.ne 9f\n"
        "  movz x11, #0x8826\n cmp x26, x11\n b.ne 9f\n"
        "  movz x11, #0x9927\n cmp x27, x11\n b.ne 9f\n"
        "  movz x11, #0xAA28\n cmp x28, x11\n b.ne 9f\n"
        "  mov  %[bad], #0\n"
        "9:\n"
        : [bad] "=&r"(bad)
        : [iters] "r"(iters)
        : "x9", "x10", "x11", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x30",
          "cc", "memory");
    return bad == 0;
}

// Deterministic rolling checksum over a leaf call chain (a return address is live on the stack every
// iteration). Identical native-vs-JIT, so the oracle compares it byte-for-byte.
static uint64_t __attribute__((noinline)) mix(uint64_t a, uint64_t b) {
    return (a ^ (a >> 27)) * 0x9E3779B97F4A7C15ull + b + 0x100000001B3ull;
}
static uint64_t __attribute__((noinline)) checksum_loop(uint64_t iters) {
    uint64_t acc = 0xCAFEF00DBAADF00Dull;
    for (uint64_t i = 0; i < iters; i++)
        acc = mix(acc, i);
    return acc;
}

static void *hammer(void *arg) {
    (void)arg;
    int pid = getpid();
    while (__atomic_load_n(&g_run, __ATOMIC_RELAXED)) {
        syscall(SYS_tgkill, pid, g_target_tid, SIGURG); // thread-directed async preempt at the main thread
        for (volatile int k = 0; k < 64; k++) {         // brief pace so the target makes forward progress
        }
    }
    return NULL;
}

int main(void) {
    static char altstack[65536];
    stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack, .ss_flags = 0};
    sigaltstack(&ss, NULL); // gsignal-style alternate stack (Go installs one per M)

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_sigurg;
    sa.sa_flags = SA_RESTART | SA_ONSTACK; // Go's async-preempt flags (minus SA_SIGINFO)
    sigaction(SIGURG, &sa, NULL);

    g_target_tid = (int)syscall(SYS_gettid); // the main thread is the storm target
    pthread_t th;
    pthread_create(&th, NULL, hammer, NULL);

    // Storm the JIT'd inner loop: bounded rounds, exit once we've clearly been preempted (>=40 hits) and
    // done a minimum of work, or on the hard cap. No unbounded spinning.
    int ok = 1;
    for (int round = 0; round < 6000; round++) {
        if (!regs_survive(30000)) {
            ok = 0;
            break;
        }
        if (round >= 200 && g_hits >= 40)
            break;
    }

    g_run = 0;
    pthread_join(th, NULL);

    uint64_t csum = checksum_loop(2000000); // deterministic; oracle-compared
    int stormed = g_hits > 0;
    printf("sigurg: regs=%d stormed=%d csum=%016llx\n", ok, stormed, (unsigned long long)csum);
    return ok && stormed ? 0 : 1;
}
