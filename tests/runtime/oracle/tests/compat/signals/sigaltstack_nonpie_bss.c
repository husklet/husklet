// The two axes the signals corpus missed together (findings 3.16). Every other fixture here is
// static-PIE, and sigaltstack_onstack.c malloc()s its alternate stack -- so nothing exercised the one
// shape that breaks: a NON-PIE ET_EXEC whose alternate stack is a .bss array. The image is mapped high
// at +bias while `altstk` keeps its LOW link vaddr, and the engine wrote the signal frame straight
// through that low address, dying with a host SIGSEGV before the handler's first instruction.
//
// The frame the guest is handed must be entirely in GUEST coordinates: SP and the siginfo/ucontext
// arguments inside altstk[], uc_stack.ss_sp equal to &altstk[0], and sigaltstack(NULL,&old) reporting
// SS_ONSTACK. Nesting a second handler while running on the alt stack proves the frame is not restarted
// at the stack top, and returning from both proves rt_sigreturn reads the frame back from the same place.
// Nothing absolute is printed, so native and both engines agree byte-for-byte.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <ucontext.h>
#include <unistd.h>

#define ALTSZ (64 * 1024)
static char altstk[ALTSZ] __attribute__((aligned(16)));
static char *outer_sp;
static volatile sig_atomic_t ran_outer, ran_nested, resumed;

static int on_alt(const void *p) {
    return (const char *)p >= altstk && (const char *)p < altstk + ALTSZ;
}

static void nested(int s, siginfo_t *si, void *ucv) {
    char here;
    ucontext_t *uc = (ucontext_t *)ucv;
    stack_t cur;
    sigaltstack(NULL, &cur);
    // A nested handler keeps growing the SAME stack: its frame must sit BELOW the outer one, not back
    // at the top (which would overwrite the frame the outer handler is still using).
    printf("nested signo=%d sp_on_alt=%d below_outer=%d si_on_alt=%d uc_on_alt=%d ss_sp=%d onstack=%d\n",
           s == SIGUSR2 && si->si_signo == SIGUSR2, on_alt(&here), &here < outer_sp, on_alt(si), on_alt(uc),
           uc->uc_stack.ss_sp == (void *)altstk, (cur.ss_flags & SS_ONSTACK) != 0);
    ran_nested = 1;
}

static void outer(int s, siginfo_t *si, void *ucv) {
    char here;
    ucontext_t *uc = (ucontext_t *)ucv;
    stack_t cur;
    outer_sp = &here;
    sigaltstack(NULL, &cur);
    printf("outer signo=%d sp_on_alt=%d si_on_alt=%d uc_on_alt=%d ss_sp=%d ss_size=%d onstack=%d\n",
           s == SIGUSR1 && si->si_signo == SIGUSR1, on_alt(&here), on_alt(si), on_alt(uc),
           uc->uc_stack.ss_sp == (void *)altstk, uc->uc_stack.ss_size == ALTSZ,
           (cur.ss_flags & SS_ONSTACK) != 0);
    ran_outer = 1;
    raise(SIGUSR2);
    // Reached only if the nested frame did not clobber this one on its way in or out.
    resumed = 1;
}

int main(void) {
    stack_t ss;
    ss.ss_sp = altstk;
    ss.ss_size = ALTSZ;
    ss.ss_flags = 0;
    if (sigaltstack(&ss, NULL) != 0) {
        printf("sigaltstack failed\n");
        return 1;
    }
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = outer;
    sa.sa_flags = SA_ONSTACK | SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);
    sa.sa_sigaction = nested;
    sigaction(SIGUSR2, &sa, NULL);

    raise(SIGUSR1);
    // Back on the ordinary stack, with the interrupted context restored by rt_sigreturn.
    char here;
    stack_t after;
    sigaltstack(NULL, &after);
    printf("main outer=%d nested=%d resumed=%d sp_off_alt=%d ss_sp=%d onstack=%d\n", ran_outer == 1,
           ran_nested == 1, resumed == 1, !on_alt(&here), after.ss_sp == (void *)altstk,
           (after.ss_flags & SS_ONSTACK) == 0);
    return 0;
}
