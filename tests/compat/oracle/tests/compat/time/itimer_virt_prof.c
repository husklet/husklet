// setitimer(ITIMER_VIRTUAL) delivers SIGVTALRM (counts user CPU time) and setitimer(ITIMER_PROF)
// delivers SIGPROF (user+system CPU). Both must fire while the process burns CPU. We drive each
// with a busy loop and a bounded iteration cap so a broken timer surfaces as a non-fire (0) rather
// than an infinite hang.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>

static volatile sig_atomic_t vt_fired, prof_fired;
static void vth(int s) { (void)s; vt_fired = 1; }
static void ph(int s) { (void)s; prof_fired = 1; }

static void burn_until(volatile sig_atomic_t *flag, long cap) {
    volatile double x = 1.0;
    for (long i = 0; i < cap && !*flag; i++) {
        for (int k = 0; k < 1000; k++) x = x * 1.0000001 + 1.0;
    }
    (void)x;
}

int main(void) {
    struct sigaction sv, sp;
    memset(&sv, 0, sizeof sv);
    sv.sa_handler = vth;
    sigemptyset(&sv.sa_mask);
    sigaction(SIGVTALRM, &sv, NULL);
    memset(&sp, 0, sizeof sp);
    sp.sa_handler = ph;
    sigemptyset(&sp.sa_mask);
    sigaction(SIGPROF, &sp, NULL);

    struct itimerval it;
    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = 20 * 1000; // 20ms of virtual time
    vt_fired = 0;
    setitimer(ITIMER_VIRTUAL, &it, NULL);
    burn_until(&vt_fired, 500L * 1000 * 1000);
    struct itimerval off;
    memset(&off, 0, sizeof off);
    setitimer(ITIMER_VIRTUAL, &off, NULL);

    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = 20 * 1000;
    prof_fired = 0;
    setitimer(ITIMER_PROF, &it, NULL);
    burn_until(&prof_fired, 500L * 1000 * 1000);
    setitimer(ITIMER_PROF, &off, NULL);

    printf("itimervp virtual=%d prof=%d\n", (int)vt_fired, (int)prof_fired);
    return 0;
}
