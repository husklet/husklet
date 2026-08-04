// POSIX per-process timer extended semantics vs the native oracle (normalized 0/1 verdicts):
//   * timer_getoverrun reports the coalesced missed-expiration count (>0, bounded) after a
//     signal-blocked periodic timer piles up several expirations behind one deliverable instance.
//   * SIGEV_SIGNAL delivery carries si_value.sival_int and si_code SI_TIMER.
//   * a fired one-shot's timer_gettime reads back {0,0}.
//   * two independent timers each deliver.
//   * timer_settime old_value returns the previous remaining/interval; timer_gettime decreases.
//   * a bad clockid to timer_create -> EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static timer_t g_tid;
static volatile sig_atomic_t got=0; static volatile long ov=-2; static volatile int sival=0, scode=0;
static void h(int s,siginfo_t*si,void*u){ (void)s;(void)u;
    if(got==0){ ov=timer_getoverrun(g_tid); sival=si->si_value.sival_int; scode=si->si_code; }
    got++; }

int main(void){
    struct sigaction sa; memset(&sa,0,sizeof sa); sa.sa_flags=SA_SIGINFO; sa.sa_sigaction=h;
    sigaction(SIGRTMIN+3,&sa,NULL);

    struct sigevent sev; memset(&sev,0,sizeof sev);
    sev.sigev_notify=SIGEV_SIGNAL; sev.sigev_signo=SIGRTMIN+3; sev.sigev_value.sival_int=0x1234;
    timer_create(CLOCK_MONOTONIC,&sev,&g_tid);

    // Block the signal, arm a fast periodic timer, let many periods elapse: several expirations
    // coalesce behind one pending instance. On unblock the handler runs and reports the overrun.
    sigset_t set; sigemptyset(&set); sigaddset(&set,SIGRTMIN+3);
    sigprocmask(SIG_BLOCK,&set,NULL);
    struct itimerspec its; memset(&its,0,sizeof its);
    its.it_value.tv_nsec=5*1000*1000; its.it_interval.tv_nsec=5*1000*1000;
    timer_settime(g_tid,0,&its,NULL);
    usleep(120*1000);
    sigprocmask(SIG_UNBLOCK,&set,NULL);
    usleep(10*1000);
    memset(&its,0,sizeof its); timer_settime(g_tid,0,&its,NULL); // disarm

    int fired = got>=1;
    int overrun_positive = ov>0;
    int overrun_bounded = ov>0 && ov<100000;   // sane, not garbage/negative
    int payload = sival==0x1234 && scode==SI_TIMER;
    // fired one-shot semantics: after disarm gettime reads {0,0}.
    struct itimerspec g; memset(&g,0xff,sizeof g); timer_gettime(g_tid,&g);
    int post_zero = g.it_value.tv_sec==0 && g.it_value.tv_nsec==0 && g.it_interval.tv_sec==0;
    timer_delete(g_tid);

    // two independent one-shot timers both deliver.
    got=0;
    timer_t a,b;
    sev.sigev_value.sival_int=1; timer_create(CLOCK_MONOTONIC,&sev,&a);
    sev.sigev_value.sival_int=2; timer_create(CLOCK_MONOTONIC,&sev,&b);
    memset(&its,0,sizeof its); its.it_value.tv_nsec=15*1000*1000; timer_settime(a,0,&its,NULL);
    memset(&its,0,sizeof its); its.it_value.tv_nsec=30*1000*1000; timer_settime(b,0,&its,NULL);
    for(int i=0;i<12 && got<2;i++){ struct timespec w={0,10*1000*1000},r; while(nanosleep(&w,&r)==-1&&errno==EINTR)w=r; }
    int two_fire = got>=2;
    timer_delete(a); timer_delete(b);

    // timer_settime old_value + gettime decreasing on a SIGEV_NONE timer.
    timer_t t2; memset(&sev,0,sizeof sev); sev.sigev_notify=SIGEV_NONE;
    timer_create(CLOCK_MONOTONIC,&sev,&t2);
    memset(&its,0,sizeof its); its.it_value.tv_sec=40; its.it_interval.tv_sec=8;
    timer_settime(t2,0,&its,NULL);
    struct itimerspec old2; memset(&old2,0,sizeof old2);
    struct itimerspec re; memset(&re,0,sizeof re); re.it_value.tv_sec=20;
    timer_settime(t2,0,&re,&old2);
    int settime_old = old2.it_value.tv_sec>0 && old2.it_value.tv_sec<=40 && old2.it_interval.tv_sec==8;
    struct itimerspec gg; memset(&gg,0,sizeof gg); timer_gettime(t2,&gg);
    int gettime_ok = gg.it_value.tv_sec>0 && gg.it_value.tv_sec<=20;
    timer_delete(t2);

    // bad clockid -> EINVAL.
    timer_t tbad; errno=0;
    int badclk = timer_create(99,&sev,&tbad)==-1 && errno==EINVAL;

    printf("ptimer fired=%d overrun_positive=%d overrun_bounded=%d payload=%d post_zero=%d two_fire=%d settime_old=%d gettime_ok=%d badclk=%d\n",
        fired,overrun_positive,overrun_bounded,payload,post_zero,two_fire,settime_old,gettime_ok,badclk);
    return 0;
}
