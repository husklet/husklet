// Timer interactions vs the native oracle (normalized 0/1 verdicts):
//   * alarm() and setitimer(ITIMER_REAL) share one underlying timer: each observes the other's arm.
//   * alarm() overrides a running ITIMER_REAL, returns the previous remaining, and clears the interval.
//   * an ITIMER_REAL signal without SA_RESTART interrupts nanosleep: EINTR with rem<requested.
//   * value=0 (interval!=0) disarms setitimer(ITIMER_REAL) and timerfd.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t hits=0;
static void h(int s){(void)s; hits++;}

int main(void){
    // 1) setitimer(ITIMER_REAL) then alarm(0) returns the shared timer's rounded-up remaining.
    struct itimerval iv; memset(&iv,0,sizeof iv); iv.it_value.tv_sec=42;
    setitimer(ITIMER_REAL,&iv,NULL);
    unsigned prev = alarm(0);
    int alarm_itimer = prev>=41 && prev<=42;

    // 2) alarm() then getitimer(ITIMER_REAL) reflects it.
    alarm(30);
    struct itimerval cur; memset(&cur,0,sizeof cur); getitimer(ITIMER_REAL,&cur);
    int itimer_alarm = cur.it_value.tv_sec>=28 && cur.it_value.tv_sec<=30;
    alarm(0);

    // 3) alarm(N) overrides a running ITIMER_REAL, returns previous remaining, clears interval.
    memset(&iv,0,sizeof iv); iv.it_value.tv_sec=20; iv.it_interval.tv_sec=7;
    setitimer(ITIMER_REAL,&iv,NULL);
    unsigned p2 = alarm(5);
    int alarm_override = p2>=19 && p2<=20;
    memset(&cur,0,sizeof cur); getitimer(ITIMER_REAL,&cur);
    int alarm_clears = cur.it_value.tv_sec>=4 && cur.it_value.tv_sec<=5 && cur.it_interval.tv_sec==0;
    alarm(0);

    // 4) an ITIMER_REAL signal (no SA_RESTART) interrupts nanosleep: EINTR, rem partially filled.
    struct sigaction sa; memset(&sa,0,sizeof sa); sa.sa_handler=h; // no SA_RESTART
    sigaction(SIGALRM,&sa,NULL);
    hits=0;
    memset(&iv,0,sizeof iv); iv.it_value.tv_usec=20*1000;
    setitimer(ITIMER_REAL,&iv,NULL);
    struct timespec t={2,0}, rem={0,0};
    int nr = nanosleep(&t,&rem);
    memset(&iv,0,sizeof iv); setitimer(ITIMER_REAL,&iv,NULL);
    int eintr_rem = nr==-1 && errno==EINTR && hits>=1 &&
                    rem.tv_sec<2 && (rem.tv_sec>0 || rem.tv_nsec>0);

    // 5) value=0 (interval!=0) disarms setitimer(ITIMER_REAL): no fire, getitimer {0,0}.
    hits=0;
    memset(&sa,0,sizeof sa); sa.sa_handler=h; sigaction(SIGALRM,&sa,NULL);
    memset(&iv,0,sizeof iv); iv.it_interval.tv_usec=10*1000;
    setitimer(ITIMER_REAL,&iv,NULL);
    usleep(40*1000);
    int val0_disarm = hits==0;
    memset(&cur,0,sizeof cur); getitimer(ITIMER_REAL,&cur);
    int val0_zero = cur.it_value.tv_sec==0 && cur.it_value.tv_usec==0;
    memset(&iv,0,sizeof iv); setitimer(ITIMER_REAL,&iv,NULL);

    // 6) timerfd value=0 (interval!=0) is disarmed: gettime {0,0}.
    int fd=timerfd_create(CLOCK_MONOTONIC,0);
    struct itimerspec its; memset(&its,0,sizeof its); its.it_interval.tv_nsec=10*1000*1000;
    timerfd_settime(fd,0,&its,NULL);
    struct itimerspec gc; memset(&gc,0,sizeof gc); timerfd_gettime(fd,&gc);
    int tfd_val0 = gc.it_value.tv_sec==0 && gc.it_value.tv_nsec==0;
    close(fd);

    printf("interact alarm_itimer=%d itimer_alarm=%d alarm_override=%d alarm_clears=%d eintr_rem=%d val0_disarm=%d val0_zero=%d tfd_val0=%d\n",
        alarm_itimer, itimer_alarm, alarm_override, alarm_clears, eintr_rem, val0_disarm, val0_zero, tfd_val0);
    return 0;
}
