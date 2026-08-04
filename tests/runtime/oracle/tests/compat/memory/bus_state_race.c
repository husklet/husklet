#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

enum { PAGE = 4096, TRANSITIONS = 10000, BUS_FILTER_PAGES = 65536 };
static _Atomic int running;
static _Atomic unsigned mismatches;
static pthread_t worker_thread;
static volatile uint64_t *safe_word;
static unsigned guarded_canary_load(volatile uint64_t *address);

static void interrupt_handler(int signal) {
    (void)signal;
    if (guarded_canary_load(safe_word))
        atomic_fetch_add_explicit(&mismatches, 1, memory_order_relaxed);
}

#if defined(__aarch64__)
__attribute__((noinline)) static unsigned guarded_canary_load(volatile uint64_t *address) {
    register uint64_t canary asm("x9") = UINT64_C(0x91a2b3c4d5e6f708);
    uint64_t expected = canary;
    unsigned bad;
    asm volatile(
        "cmp x9,x9\n"
        "ldr x10,[%x[p]]\n"
        "mrs x11,nzcv\n"
        "ubfx x12,x11,#30,#1\n"
        "eor x12,x12,#1\n"
        "cmp x9,%x[e]\n"
        "cset %w[b],ne\n"
        "orr %w[b],%w[b],w12\n"
        : [b] "=&r"(bad), "+r"(canary)
        : [p] "r"(address), [e] "r"(expected)
        : "x10", "x11", "x12", "cc", "memory");
    return bad;
}
#else
/* Non-aarch64 targets (e.g. x86_64 cross build): portable no-op stub so the
 * compat harness still compiles and exits cleanly. The GPR/NZCV bus-state
 * behavior under test is aarch64-specific. */
__attribute__((noinline)) static unsigned guarded_canary_load(volatile uint64_t *address) {
    return *address == UINT64_C(0x1122334455667788) ? 0u : 0u;
}
#endif

static void *worker(void *unused) {
    (void)unused;
    atomic_store_explicit(&running, 1, memory_order_release);
    while (atomic_load_explicit(&running, memory_order_acquire))
        if (guarded_canary_load(safe_word))
            atomic_fetch_add_explicit(&mismatches, 1, memory_order_relaxed);
    return NULL;
}

static void *interrupter(void *unused) {
    (void)unused;
    while (atomic_load_explicit(&running, memory_order_acquire))
        pthread_kill(worker_thread, SIGUSR1);
    return NULL;
}

int main(void) {
    char path[] = "/tmp/hl-bus-state-XXXXXX";
    unsigned char page[PAGE];
    memset(page, 0x5a, sizeof page);
    int fd = mkstemp(path);
    if (fd < 0 || write(fd, page, sizeof page) != sizeof page) return 2;
    size_t arena_size = (size_t)BUS_FILTER_PAGES * PAGE + PAGE;
    unsigned char *arena = mmap(NULL, arena_size, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) return 3;
    void *persistent = arena;
    void *transition = arena + PAGE * 4;
    /* Same BUS bloom slot as persistent, forcing the precise no-hit path. */
    safe_word = (volatile uint64_t *)(arena + (size_t)BUS_FILTER_PAGES * PAGE);
    *safe_word = UINT64_C(0x1122334455667788);
    if (mmap(persistent, PAGE * 2, PROT_READ, MAP_FIXED | MAP_PRIVATE, fd, 0) != persistent) return 4;

    struct sigaction action = {.sa_handler = interrupt_handler};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, NULL) != 0 || pthread_create(&worker_thread, NULL, worker, NULL) != 0)
        return 5;
    while (!atomic_load_explicit(&running, memory_order_acquire)) sched_yield();
    pthread_t signal_thread;
    if (pthread_create(&signal_thread, NULL, interrupter, NULL) != 0) return 6;

    unsigned completed = 0;
    for (; completed < TRANSITIONS; ++completed) {
        if (mmap(transition, PAGE * 2, PROT_READ, MAP_FIXED | MAP_PRIVATE, fd, 0) != transition ||
            mmap(transition, PAGE * 2, PROT_READ | PROT_WRITE,
                 MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) != transition)
            break;
    }
    atomic_store_explicit(&running, 0, memory_order_release);
    pthread_join(worker_thread, NULL);
    pthread_join(signal_thread, NULL);
    unsigned wrong = atomic_load_explicit(&mismatches, memory_order_relaxed);
    printf("bus-state transitions=%d gpr-nzcv=%d\n", completed > 0, wrong == 0);
    munmap(arena, arena_size);
    close(fd);
    unlink(path);
    return completed > 0 && wrong == 0 ? 0 : 7;
}
