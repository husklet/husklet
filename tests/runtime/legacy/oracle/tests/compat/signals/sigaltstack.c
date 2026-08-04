// sigaltstack + SA_ONSTACK: the handler must run on the alternate signal stack.
// Portable POSIX -> golden verdict on every engine.
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static char altbuf[SIGSTKSZ];
static volatile sig_atomic_t on_alt = 0, ran = 0;
static char *alt_lo, *alt_hi;

static void h(int s) {
    (void)s;
    int local;
    char *p = (char *)&local;
    on_alt = (p >= alt_lo && p < alt_hi);   // handler's stack var lives inside the alt stack
    ran = 1;
}

int main(void) {
    stack_t ss = { .ss_sp = altbuf, .ss_size = sizeof altbuf, .ss_flags = 0 };
    int set = sigaltstack(&ss, NULL) == 0;
    alt_lo = altbuf;
    alt_hi = altbuf + sizeof altbuf;

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sa.sa_flags = SA_ONSTACK;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);
    raise(SIGUSR1);

    stack_t cur;
    sigaltstack(NULL, &cur);
    int query_ok = cur.ss_sp == (void *)altbuf && cur.ss_size == sizeof altbuf;
    printf("sigaltstack set=%d ran=%d on_alt=%d query=%d\n", set, (int)ran, (int)on_alt, query_ok);
    return 0;
}
