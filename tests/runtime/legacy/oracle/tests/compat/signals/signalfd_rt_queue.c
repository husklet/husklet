// signalfd drains multiple queued RT signals in priority order, exposing ssi_signo and ssi_int
// (the sigqueue value). Lowest signo first, FIFO within a signo.
#include <signal.h>
#include <stdio.h>
#include <sys/signalfd.h>
#include <unistd.h>

int main(void) {
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGRTMIN + 0);
    sigaddset(&mask, SIGRTMIN + 1);
    sigprocmask(SIG_BLOCK, &mask, NULL);

    int fd = signalfd(-1, &mask, 0);
    if (fd < 0) { printf("signalfd_rt_queue fd_fail\n"); return 1; }

    union sigval v;
    v.sival_int = 11; sigqueue(getpid(), SIGRTMIN + 1, v);
    v.sival_int = 22; sigqueue(getpid(), SIGRTMIN + 0, v);
    v.sival_int = 33; sigqueue(getpid(), SIGRTMIN + 0, v);

    int sg[3], iv[3];
    for (int i = 0; i < 3; i++) {
        struct signalfd_siginfo si;
        if (read(fd, &si, sizeof si) != (ssize_t)sizeof si) { printf("signalfd_rt_queue short\n"); return 1; }
        sg[i] = (int)si.ssi_signo - SIGRTMIN;
        iv[i] = si.ssi_int;
    }
    int ok = sg[0] == 0 && iv[0] == 22 &&
             sg[1] == 0 && iv[1] == 33 &&
             sg[2] == 1 && iv[2] == 11;
    printf("signalfd_rt_queue order_ok=%d\n", ok);
    return 0;
}
