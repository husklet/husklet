// memory-compat isolation: precise fault boundaries around a live private-anon mapping. On Linux the page
// immediately PAST a mapping's end, and the page immediately BELOW it, are unmapped holes -- reading either
// faults SIGSEGV/SEGV_MAPERR (an off-by-one past a buffer must not silently read a neighbor). The engine
// reserves a large zero-filled guard tail past every anon mmap and keeps an adjacency grow cushion, so a
// read just past (or just below) a mapping is currently satisfied out of those private zero pages instead of
// faulting. That over-read never crosses into another mapping's data (consecutive guest mmaps are separated
// by the guard, verified: over-reading 128 KB reveals only zero), so it is a fault-fidelity divergence, not a
// cross-mapping leak -- but Linux faults where the engine does not. The last in-mapping byte stays readable.
// Arch-neutral output. Expected values are the Linux oracle (all faults=1); flip to `active` when enforced.
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static uintptr_t g_addr;

static void handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    _exit((sig == SIGSEGV && si->si_code == SEGV_MAPERR) ? 100 : 101);
}

static void child(void) {
    struct sigaction sa = {0};
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    volatile uint8_t v = *(volatile uint8_t *)g_addr;
    (void)v;
    _exit(42);
}

static int faults(uintptr_t addr) {
    g_addr = addr;
    pid_t p = fork();
    if (p == 0) child();
    int st = 0;
    waitpid(p, &st, 0);
    return WIFEXITED(st) && WEXITSTATUS(st) == 100;
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    // Reserve 3 pages, unmap the outer two so the middle page is a lone mapping bordered by holes.
    unsigned char *base = mmap(NULL, ps * 3, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) return 2;
    munmap(base, ps);
    munmap(base + ps * 2, ps);
    unsigned char *mid = base + ps;
    volatile uint8_t last = mid[ps - 1]; // last in-mapping byte: readable
    (void)last;
    printf("last_ok=1 past_end_faults=%d below_faults=%d\n",
           faults((uintptr_t)mid + ps), faults((uintptr_t)mid - 1));
    return 0;
}
