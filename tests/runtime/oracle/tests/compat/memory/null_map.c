// memory-compat isolation: the NULL page must be inaccessible and unmappable by an unprivileged guest.
// A read or write of address 0 must fault (SIGSEGV, SEGV_MAPERR -- nothing mapped there), and mmap MAP_FIXED
// at 0 must fail with EPERM under the default mmap_min_addr so a guest cannot map the zero page and thereby
// mask its own NULL dereferences. Arch-neutral output.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static void handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    _exit((sig == SIGSEGV && si->si_code == SEGV_MAPERR) ? 100 : 101);
}

static void install(void) {
    struct sigaction sa = {0};
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
}

static void child_read(void) { install(); volatile int v = *(volatile int *)0; (void)v; _exit(42); }
static void child_write(void) { install(); *(volatile int *)0 = 1; _exit(42); }

static int run(void (*fn)(void)) {
    pid_t p = fork();
    if (p == 0) fn();
    int st = 0;
    waitpid(p, &st, 0);
    return WIFEXITED(st) && WEXITSTATUS(st) == 100;
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    void *r = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int fixed0_eperm = (r == MAP_FAILED && errno == EPERM);
    printf("null_read_maperr=%d null_write_maperr=%d fixed0_eperm=%d\n",
           run(child_read), run(child_write), fixed0_eperm);
    return 0;
}
