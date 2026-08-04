// Timer error-surface validation: negative/oversized fields -> EINVAL, bad which, EFAULT,
// short timerfd read -> EINVAL, gettime on unarmed.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

int main(void){
    struct itimerspec its; struct itimerval iv;

    // setitimer: negative usec -> EINVAL.
    memset(&iv,0,sizeof iv); iv.it_value.tv_usec=-1; errno=0;
    int s_neg = setitimer(ITIMER_REAL,&iv,NULL)==-1 && errno==EINVAL;
    // setitimer: usec >= 1e6 -> EINVAL.
    memset(&iv,0,sizeof iv); iv.it_value.tv_usec=1000000; errno=0;
    int s_big = setitimer(ITIMER_REAL,&iv,NULL)==-1 && errno==EINVAL;
    // setitimer bad which -> EINVAL.
    memset(&iv,0,sizeof iv); iv.it_value.tv_sec=1; errno=0;
    int s_which = setitimer(99,&iv,NULL)==-1 && errno==EINVAL;
    // getitimer bad which -> EINVAL.
    errno=0; int g_which = getitimer(99,&iv)==-1 && errno==EINVAL;
    // getitimer EFAULT.
    errno=0; int g_fault = getitimer(ITIMER_REAL,(struct itimerval*)0x1)==-1 && errno==EFAULT;

    // timerfd: negative it_value.tv_nsec -> EINVAL.
    int fd=timerfd_create(CLOCK_MONOTONIC,0);
    memset(&its,0,sizeof its); its.it_value.tv_nsec=-5; errno=0;
    int tf_neg = timerfd_settime(fd,0,&its,NULL)==-1 && errno==EINVAL;
    // timerfd: tv_nsec >= 1e9 -> EINVAL.
    memset(&its,0,sizeof its); its.it_value.tv_nsec=1000000000L; errno=0;
    int tf_big = timerfd_settime(fd,0,&its,NULL)==-1 && errno==EINVAL;
    // timerfd: bad flags to settime -> EINVAL.
    memset(&its,0,sizeof its); its.it_value.tv_sec=1; errno=0;
    int tf_flags = timerfd_settime(fd,0x99,&its,NULL)==-1 && errno==EINVAL;
    // short read (<8) -> EINVAL.
    memset(&its,0,sizeof its); its.it_value.tv_nsec=15*1000*1000;
    timerfd_settime(fd,0,&its,NULL);
    usleep(30*1000);
    char small[4]; errno=0;
    int tf_short = read(fd,small,4)==-1 && errno==EINVAL;
    close(fd);
    // gettime on never-armed fd -> {0,0}.
    fd=timerfd_create(CLOCK_MONOTONIC,0);
    struct itimerspec g; memset(&g,0xff,sizeof g); timerfd_gettime(fd,&g);
    int tf_unarmed = g.it_value.tv_sec==0 && g.it_value.tv_nsec==0 && g.it_interval.tv_sec==0;
    close(fd);

    // timer_settime: negative interval -> EINVAL.
    timer_t tid; struct sigevent sev; memset(&sev,0,sizeof sev); sev.sigev_notify=SIGEV_NONE;
    timer_create(CLOCK_MONOTONIC,&sev,&tid);
    memset(&its,0,sizeof its); its.it_value.tv_sec=1; its.it_interval.tv_nsec=-1; errno=0;
    int pt_neg = timer_settime(tid,0,&its,NULL)==-1 && errno==EINVAL;
    // timer_settime: tv_nsec too big -> EINVAL.
    memset(&its,0,sizeof its); its.it_value.tv_nsec=2000000000L; errno=0;
    int pt_big = timer_settime(tid,0,&its,NULL)==-1 && errno==EINVAL;
    timer_delete(tid);

    // nanosleep negative tv_sec -> EINVAL.
    struct timespec bad={-1,0}; errno=0;
    int ns_neg = nanosleep(&bad,NULL)==-1 && errno==EINVAL;

    printf("err s_neg=%d s_big=%d s_which=%d g_which=%d g_fault=%d tf_neg=%d tf_big=%d tf_flags=%d tf_short=%d tf_unarmed=%d pt_neg=%d pt_big=%d ns_neg=%d\n",
        s_neg,s_big,s_which,g_which,g_fault,tf_neg,tf_big,tf_flags,tf_short,tf_unarmed,pt_neg,pt_big,ns_neg);
    return 0;
}
