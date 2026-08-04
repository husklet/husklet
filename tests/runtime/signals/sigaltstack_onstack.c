// SA_ONSTACK runs the handler on the registered alternate stack. Inside the handler, the current
// stack pointer must lie within the alt-stack range and sigaltstack must report SS_ONSTACK.
// Arch-neutral: we test the local address against ss_sp bounds, not any absolute value.
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>

static char *alt_base;
static size_t alt_size;
static volatile sig_atomic_t in_range, onstack_flag, ran;

static void h(int s) {
    (void)s;
    ran++;
    char probe;
    char *sp = &probe;
    if (sp >= alt_base && sp < alt_base + alt_size) in_range = 1;
    stack_t cur;
    if (sigaltstack(NULL, &cur) == 0 && (cur.ss_flags & SS_ONSTACK)) onstack_flag = 1;
}

int main(void) {
    alt_size = SIGSTKSZ;
    alt_base = malloc(alt_size);
    stack_t ss = { .ss_sp = alt_base, .ss_size = alt_size, .ss_flags = 0 };
    sigaltstack(&ss, NULL);

    struct sigaction sa = {0};
    sa.sa_handler = h;
    sa.sa_flags = SA_ONSTACK;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    raise(SIGUSR1);
    printf("sigaltstack_onstack ran=%d in_range=%d ss_onstack=%d\n",
           ran == 1, in_range, onstack_flag);
    return 0;
}
