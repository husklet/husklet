// sigaltstack bookkeeping: the initial stack is SS_DISABLE with a zero base, installing then
// querying round-trips the registered stack, SS_DISABLE clears it, and an undersized stack is
// ENOMEM while an unknown flag is EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    stack_t cur;
    memset(&cur, 0xff, sizeof cur);
    int g0 = sigaltstack(NULL, &cur);
    int init_disabled = (cur.ss_flags == SS_DISABLE) && (cur.ss_sp == NULL) && (cur.ss_size == 0);

    static char buf[65536];
    stack_t ss = {.ss_sp = buf, .ss_size = sizeof buf, .ss_flags = 0};
    int s1 = sigaltstack(&ss, NULL);
    memset(&cur, 0, sizeof cur);
    int g1 = sigaltstack(NULL, &cur);
    int roundtrip = (cur.ss_sp == buf) && (cur.ss_size == sizeof buf) && (cur.ss_flags == 0);

    stack_t small = {.ss_sp = buf, .ss_size = 1, .ss_flags = 0};
    int s2 = sigaltstack(&small, NULL);
    int e2 = (s2 == -1) ? errno : 0;
    stack_t badflag = {.ss_sp = buf, .ss_size = sizeof buf, .ss_flags = 0x40};
    int s3 = sigaltstack(&badflag, NULL);
    int e3 = (s3 == -1) ? errno : 0;
    // the failed calls must not have disturbed the installed stack
    memset(&cur, 0, sizeof cur);
    sigaltstack(NULL, &cur);
    int intact = (cur.ss_sp == buf) && (cur.ss_size == sizeof buf);

    stack_t off = {.ss_sp = NULL, .ss_size = 0, .ss_flags = SS_DISABLE};
    int s4 = sigaltstack(&off, NULL);
    memset(&cur, 0, sizeof cur);
    sigaltstack(NULL, &cur);
    int now_disabled = (cur.ss_flags == SS_DISABLE);
    printf("g0=%d init_disabled=%d s1=%d g1=%d roundtrip=%d s2=%d e2=%d s3=%d e3=%d intact=%d s4=%d now_disabled=%d\n",
           g0, init_disabled, s1, g1, roundtrip, s2, e2, s3, e3, intact, s4, now_disabled);
    return 0;
}
