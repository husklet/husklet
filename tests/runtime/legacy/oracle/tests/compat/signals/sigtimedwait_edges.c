// sigtimedwait: a zero timeout with nothing pending is EAGAIN, a pending blocked signal is
// returned and dequeued, sigqueue value reaches si_value, SIGKILL cannot be waited for, and
// an out-of-range timeout is EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    sigset_t m;
    sigemptyset(&m);
    sigaddset(&m, SIGUSR1);
    sigprocmask(SIG_BLOCK, &m, NULL);
    struct timespec zero = {0, 0};
    siginfo_t si;
    int a = sigtimedwait(&m, &si, &zero);
    int ea = (a == -1) ? errno : 0;

    union sigval v = {.sival_int = 4242};
    sigqueue(getpid(), SIGUSR1, v);
    memset(&si, 0, sizeof si);
    int b = sigtimedwait(&m, &si, &zero);
    int val = si.si_value.sival_int;
    int code = si.si_code;
    int c = sigtimedwait(&m, &si, &zero);
    int ec = (c == -1) ? errno : 0;

    sigset_t k;
    sigemptyset(&k);
    sigaddset(&k, SIGKILL);
    int d = sigtimedwait(&k, &si, &zero);
    int ed = (d == -1) ? errno : 0;

    struct timespec bad = {0, 1000000000};
    int e = sigtimedwait(&m, &si, &bad);
    int ee = (e == -1) ? errno : 0;
    printf("a=%d ea=%d b=%d val=%d code=%d c=%d ec=%d d=%d ed=%d e=%d ee=%d sicode_queue=%d\n",
           a, ea, b, val, code, c, ec, d, ed, e, ee, code == SI_QUEUE);
    return 0;
}
