// sigwaitinfo synchronously dequeues queued RT signals in priority order (lowest signo first,
// FIFO within a signo) returning the signo and si_value, with no handler running.
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    sigset_t set, old;
    sigemptyset(&set);
    for (int i = 0; i < 2; i++) sigaddset(&set, SIGRTMIN + i);
    sigprocmask(SIG_BLOCK, &set, &old);

    union sigval v;
    v.sival_int = 100; sigqueue(getpid(), SIGRTMIN + 1, v);
    v.sival_int = 200; sigqueue(getpid(), SIGRTMIN + 0, v);
    v.sival_int = 300; sigqueue(getpid(), SIGRTMIN + 0, v);

    int s0[3], val[3];
    for (int i = 0; i < 3; i++) {
        siginfo_t si;
        int got = sigwaitinfo(&set, &si);
        s0[i] = got - SIGRTMIN;
        val[i] = si.si_value.sival_int;
    }
    int ok = s0[0] == 0 && val[0] == 200 &&
             s0[1] == 0 && val[1] == 300 &&
             s0[2] == 1 && val[2] == 100;
    printf("sigwaitinfo order_ok=%d\n", ok);
    return 0;
}
