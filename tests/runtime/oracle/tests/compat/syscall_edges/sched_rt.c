// syscall-compat regression: an unprivileged sched_setscheduler(SCHED_FIFO) must report EPERM, not a
// synthetic success that never installs real-time scheduling.
#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    struct sched_param p;
    p.sched_priority = 1;
    long r = syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &p);
    printf("sched_fifo_errno=%d\n", r == -1 ? errno : 0);
    return 0;
}
