// timerfd argument validation: bad clockid, bad create flags, out-of-range nsec in either
// field, and a negative tv_sec are all EINVAL and must not arm the timer.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static int try(int fd, int flags, struct itimerspec *v) {
    int r = timerfd_settime(fd, flags, v, NULL);
    return (r == -1) ? errno : 0;
}

int main(void) {
    int bad = timerfd_create(99, 0);
    int ebad = (bad == -1) ? errno : 0;
    int badflag = timerfd_create(CLOCK_MONOTONIC, 0x4);
    int ebadflag = (badflag == -1) ? errno : 0;
    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec a = {{0, 0}, {0, 1000000000}};
    struct itimerspec b = {{0, 1000000000}, {1, 0}};
    struct itimerspec c = {{0, 0}, {-1, 0}};
    struct itimerspec d = {{0, 0}, {0, -1}};
    struct itimerspec ok = {{0, 0}, {100, 0}};
    int e1 = try(fd, 0, &a), e2 = try(fd, 0, &b), e3 = try(fd, 0, &c), e4 = try(fd, 0, &d);
    int e5 = try(fd, 0x8, &ok);
    struct itimerspec cur = {{9, 9}, {9, 9}};
    timerfd_gettime(fd, &cur);
    int stillzero = (cur.it_value.tv_sec == 0 && cur.it_value.tv_nsec == 0);
    int ebadfd = (timerfd_settime(-1, 0, &ok, NULL) == -1) ? errno : 0;
    printf("bad=%d ebad=%d badflag=%d ebadflag=%d e1=%d e2=%d e3=%d e4=%d e5=%d stillzero=%d ebadfd=%d\n",
           bad, ebad, badflag, ebadflag, e1, e2, e3, e4, e5, stillzero, ebadfd);
    return 0;
}
