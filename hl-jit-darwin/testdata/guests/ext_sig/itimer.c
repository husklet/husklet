// setitimer(ITIMER_REAL) delivers SIGALRM after the interval; getitimer reports a pending timer;
// alarm() likewise raises SIGALRM. Portable POSIX -> golden verdict on every engine.
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <unistd.h>

static volatile sig_atomic_t fired = 0;
static void h(int s) { (void)s; fired++; }

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGALRM, &sa, NULL);

    struct itimerval it;
    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = 50000;          // 50 ms one-shot
    setitimer(ITIMER_REAL, &it, NULL);
    struct itimerval cur;
    getitimer(ITIMER_REAL, &cur);
    int pending = cur.it_value.tv_sec != 0 || cur.it_value.tv_usec != 0;
    while (!fired) pause();
    int timer_fired = fired == 1;

    // alarm() path
    alarm(1);
    fired = 0;
    unsigned rem = alarm(0);              // cancel, report remaining seconds
    int alarm_ok = rem >= 1;
    printf("itimer pending=%d fired=%d alarm=%d\n", pending, timer_fired, alarm_ok);
    return 0;
}
