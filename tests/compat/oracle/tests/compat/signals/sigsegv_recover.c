// Synchronous SIGSEGV from a NULL dereference is caught with SA_SIGINFO: si_code == SEGV_MAPERR
// and si_addr == NULL (both arch-neutral on Linux). The handler recovers via siglongjmp and the
// program continues normally.
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>

static sigjmp_buf jb;
static volatile sig_atomic_t code_ok, addr_ok, caught;

static void h(int s, siginfo_t *si, void *u) {
    (void)s; (void)u;
    caught++;
    if (si->si_code == SEGV_MAPERR) code_ok = 1;
    if (si->si_addr == (void *)0) addr_ok = 1;
    siglongjmp(jb, 1);
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_sigaction = h;
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL);

    volatile int recovered = 0;
    if (sigsetjmp(jb, 1) == 0) {
        volatile int *p = (volatile int *)0;
        *p = 1; // fault
    } else {
        recovered = 1;
    }
    printf("sigsegv_recover caught=%d maperr=%d addr_null=%d recovered=%d\n",
           caught == 1, code_ok, addr_ok, recovered);
    return 0;
}
