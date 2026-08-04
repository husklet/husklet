// setitimer(ITIMER_REAL) delivers SIGALRM. A periodic real timer fires N times; getitimer reports
// a remaining value that decreases between two reads without re-arming. alarm() interop: setting a
// zero itimer disarms.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <unistd.h>

static volatile sig_atomic_t fires;
static void h(int s) { (void)s; fires++; }

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_RESTART;
    sigaction(SIGALRM, &sa, NULL);

    // Periodic 20ms real timer: count at least 3 fires within a bounded wait.
    struct itimerval it;
    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = 20 * 1000;
    it.it_interval.tv_usec = 20 * 1000;
    fires = 0;
    setitimer(ITIMER_REAL, &it, NULL);
    for (int i = 0; i < 200 && fires < 3; i++) usleep(5000);
    int fired_n = fires >= 3;

    // getitimer remaining is within (0, interval].
    struct itimerval cur;
    getitimer(ITIMER_REAL, &cur);
    long long rem1 = cur.it_value.tv_sec * 1000000LL + cur.it_value.tv_usec;
    int rem_sane = rem1 > 0 && rem1 <= 20 * 1000;

    // Disarm.
    struct itimerval off;
    memset(&off, 0, sizeof off);
    setitimer(ITIMER_REAL, &off, NULL);
    getitimer(ITIMER_REAL, &cur);
    int disarmed = cur.it_value.tv_sec == 0 && cur.it_value.tv_usec == 0;

    // A single-shot 30ms timer's remaining decreases across a 10ms sleep.
    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = 300 * 1000; // 300ms one-shot
    setitimer(ITIMER_REAL, &it, NULL);
    struct itimerval a, b;
    getitimer(ITIMER_REAL, &a);
    usleep(50 * 1000);
    getitimer(ITIMER_REAL, &b);
    long long na = a.it_value.tv_sec * 1000000LL + a.it_value.tv_usec;
    long long nb = b.it_value.tv_sec * 1000000LL + b.it_value.tv_usec;
    int decreasing = nb < na;
    setitimer(ITIMER_REAL, &off, NULL);

    printf("itimerreal fired=%d rem_sane=%d disarmed=%d decreasing=%d\n", fired_n, rem_sane, disarmed,
           decreasing);
    return 0;
}
