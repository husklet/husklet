// CANDIDATE ENGINE BUG: prctl PR_SET_TIMERSLACK / PR_GET_TIMERSLACK do not round-trip.
// Native aarch64 Linux: after PR_SET_TIMERSLACK(123456), PR_GET_TIMERSLACK returns 123456.
// Engine: PR_GET_TIMERSLACK does not return the value that was set (slack_match=0).
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/prctl.h>
int main(void){
    prctl(PR_SET_TIMERSLACK, 123456UL, 0, 0, 0);
    long slack = prctl(PR_GET_TIMERSLACK, 0, 0, 0, 0);
    printf("slack=%ld match=%d\n", slack, slack==123456); // native match=1, engine match=0
    return 0;
}
