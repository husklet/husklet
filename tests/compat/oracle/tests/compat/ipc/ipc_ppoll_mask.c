#define _GNU_SOURCE
// ppoll honours its timespec timeout and its signal mask: with SIGUSR1 blocked via the mask, a
// self-sent SIGUSR1 does not interrupt the wait, which returns 0 at timeout.
#include <unistd.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    struct pollfd pf = { .fd = p[0], .events = POLLIN };
    struct timespec z = {0, 0};
    int immediate = ppoll(&pf, 1, &z, NULL);   // 0, nothing ready
    // block USR1 through the ppoll mask; raise it; ppoll must ride out the short timeout
    sigset_t block; sigemptyset(&block); sigaddset(&block, SIGUSR1);
    signal(SIGUSR1, SIG_IGN);
    raise(SIGUSR1);
    struct timespec t = {0, 30*1000*1000};
    errno = 0;
    int timed = ppoll(&pf, 1, &t, &block);
    printf("ppoll immediate=%d masked_timeout=%d\n", immediate, timed);   // 0 0
    return 0;
}
