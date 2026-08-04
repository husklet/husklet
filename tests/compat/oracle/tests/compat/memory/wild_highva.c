// memory-compat regression / isolation: a guest load from an UNMAPPED high canonical address must fault as
// Linux does -- SIGSEGV with si_code SEGV_MAPERR (unmapped) -- and, with no handler installed, must terminate
// the process by SIGSEGV. The x86 guest fault path used to silently satisfy an ISOLATED wild access (no mapped
// neighbor) out of the lazy zero-page "safety net" budget: the read returned 0 instead of faulting, so a wild
// pointer could read unmapped high-VA memory undetected (an isolation/correctness hole). The aarch64 path,
// which has no lazy grower, always faulted; this locks in the same faithful behavior on both arches.
//
// Two observations per address, each in its own child so the raw wait status is deterministic:
//   - no handler: the process is terminated by SIGSEGV (WIFSIGNALED / WTERMSIG == SIGSEGV),
//   - handler installed: the fault is delivered as SIGSEGV with si_code SEGV_MAPERR.
// The probed addresses are canonical, page-aligned, and (barring an astronomically unlikely ASLR collision)
// unmapped in a freshly started static guest process. Arch-neutral output.
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static void handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    // 100 = SIGSEGV + SEGV_MAPERR (correct), 101 otherwise.
    _exit((sig == SIGSEGV && si->si_code == SEGV_MAPERR) ? 100 : 101);
}

static void child_read(uintptr_t addr, int with_handler) {
    if (with_handler) {
        struct sigaction sa = {0};
        sa.sa_sigaction = handler;
        sa.sa_flags = SA_SIGINFO;
        sigaction(SIGSEGV, &sa, NULL);
    }
    volatile uint8_t v = *(volatile uint8_t *)addr;
    (void)v;
    _exit(42); // reached only if the wild read was (wrongly) satisfied
}

static int run(uintptr_t addr, int with_handler) {
    pid_t p = fork();
    if (p == 0) child_read(addr, with_handler);
    int st = 0;
    waitpid(p, &st, 0);
    return st;
}

int main(void) {
    const uintptr_t addrs[] = {0x7ffffffff000ULL, 0x600000000000ULL, 0x123456789000ULL, 0x40000000000ULL};
    for (unsigned i = 0; i < sizeof addrs / sizeof addrs[0]; i++) {
        int sd = run(addrs[i], 0);
        int sh = run(addrs[i], 1);
        printf("addr=%#lx killed_segv=%d handler_maperr=%d\n", (unsigned long)addrs[i],
               WIFSIGNALED(sd) && WTERMSIG(sd) == SIGSEGV, WIFEXITED(sh) && WEXITSTATUS(sh) == 100);
    }
    return 0;
}
