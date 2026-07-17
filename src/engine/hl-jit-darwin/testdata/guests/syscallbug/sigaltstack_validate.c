// syscall-compat regression: sigaltstack must reject an unknown flag mode (EINVAL) and a below-minimum
// stack size (ENOMEM), not accept every configuration and corrupt later SA_ONSTACK delivery.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>

static char stk[16384];

int main(void) {
    stack_t ss;
    ss.ss_sp = stk;
    ss.ss_size = sizeof stk;
    ss.ss_flags = 0x4; // not a valid ss_flags mode -> EINVAL
    int r1 = sigaltstack(&ss, NULL);
    int e1 = (r1 == -1) ? errno : 0;

    ss.ss_flags = 0;
    ss.ss_size = 128; // below MINSIGSTKSZ -> ENOMEM
    int r2 = sigaltstack(&ss, NULL);
    int e2 = (r2 == -1) ? errno : 0;

    ss.ss_flags = 0;
    ss.ss_size = sizeof stk; // valid -> success
    int r3 = sigaltstack(&ss, NULL);
    int e3 = (r3 == -1) ? errno : 0;

    printf("badflags=%d tiny=%d valid=%d\n", e1, e2, e3);
    return 0;
}
