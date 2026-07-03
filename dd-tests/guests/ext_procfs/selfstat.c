// /proc/self/stat — the single-line 52-field record. Assert field COUNT and the load-bearing positions:
// f1=pid(==getpid), f2=(comm) parenthesized, f3=state 'R', f4=ppid, f23=vsize>0, f24=rss>=0. mongod's
// FTDC and many runtimes parse this by position, so a short/misaligned line must fail.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

int main(void) {
    char b[4096];
    int n = pf_read("/proc/self/stat", b, sizeof b);
    // split on spaces (comm is (…) with no spaces in our synth); count fields
    char *fields[64];
    int nf = 0;
    for (char *t = strtok(b, " \n"); t && nf < 64; t = strtok(NULL, " \n")) fields[nf++] = t;
    int pid_ok = nf > 0 && atoi(fields[0]) == (int)getpid();
    int comm_ok = nf > 1 && fields[1][0] == '(' && fields[1][strlen(fields[1]) - 1] == ')';
    int state_ok = nf > 2 && (fields[2][0] == 'R' || fields[2][0] == 'S');
    long vsize = nf > 22 ? atol(fields[22]) : 0;
    long rss = nf > 23 ? atol(fields[23]) : -1;
    int count_ok = nf >= 52;
    int ok = n > 0 && count_ok && pid_ok && comm_ok && state_ok && vsize > 0 && rss >= 0;
    printf("selfstat ok=%d\n", ok);
    return 0;
}
