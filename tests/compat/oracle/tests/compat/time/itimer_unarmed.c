// getitimer on an unarmed interval timer reports all-zero value/interval for each of ITIMER_REAL,
// ITIMER_VIRTUAL and ITIMER_PROF. setitimer/getitimer with an invalid `which` -> EINVAL. Arming and
// disarming ITIMER_REAL returns to the all-zero state.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>

static int is_zero(int which) {
    struct itimerval v;
    memset(&v, 0xff, sizeof v);
    if (getitimer(which, &v) != 0) return 0;
    return v.it_value.tv_sec == 0 && v.it_value.tv_usec == 0 && v.it_interval.tv_sec == 0 &&
           v.it_interval.tv_usec == 0;
}

int main(void) {
    int real0 = is_zero(ITIMER_REAL);
    int virt0 = is_zero(ITIMER_VIRTUAL);
    int prof0 = is_zero(ITIMER_PROF);

    // Invalid `which` -> EINVAL on both get and set.
    struct itimerval v;
    memset(&v, 0, sizeof v);
    errno = 0;
    int get_bad = getitimer(99, &v) == -1 && errno == EINVAL;
    errno = 0;
    int set_bad = setitimer(99, &v, NULL) == -1 && errno == EINVAL;

    // Arm then disarm ITIMER_REAL -> back to zero.
    struct itimerval arm;
    memset(&arm, 0, sizeof arm);
    arm.it_value.tv_sec = 100;
    setitimer(ITIMER_REAL, &arm, NULL);
    struct itimerval off;
    memset(&off, 0, sizeof off);
    setitimer(ITIMER_REAL, &off, NULL);
    int rearmed_zero = is_zero(ITIMER_REAL);

    printf("itimerunarmed real=%d virt=%d prof=%d get_bad=%d set_bad=%d rezero=%d\n", real0, virt0,
           prof0, get_bad, set_bad, rearmed_zero);
    return 0;
}
