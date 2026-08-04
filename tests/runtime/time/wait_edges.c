// nanosleep/clock_nanosleep/ppoll/pselect edge semantics.
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t alarms=0;
static void onalrm(int s){(void)s; alarms++;}

int main(void){
    // 1) clock_nanosleep CLOCK_MONOTONIC relative, full sleep returns 0.
    struct timespec t={0,20*1000*1000},rem={0,0};
    int full = clock_nanosleep(CLOCK_MONOTONIC,0,&t,&rem)==0;

    // 2) EINTR: SIGALRM (no SA_RESTART) interrupts, rem filled and < requested.
    struct sigaction sa; memset(&sa,0,sizeof sa); sa.sa_handler=onalrm; // no SA_RESTART
    sigaction(SIGALRM,&sa,NULL);
    struct itimerspec junk;(void)junk;
    struct timespec big={10,0}, rem2={0,0};
    alarm(1);
    int rc = clock_nanosleep(CLOCK_MONOTONIC,0,&big,&rem2);
    int eintr = rc==EINTR && (rem2.tv_sec>0 || rem2.tv_nsec>0) && rem2.tv_sec<10;

    // 3) clock_nanosleep TIMER_ABSTIME does NOT touch rem (rem left untouched even on EINTR).
    struct timespec now; clock_gettime(CLOCK_MONOTONIC,&now);
    struct timespec abs=now; abs.tv_sec+=10;
    struct timespec sentinel={7,7}; // must stay untouched
    alarm(1);
    int rc2 = clock_nanosleep(CLOCK_MONOTONIC,TIMER_ABSTIME,&abs,&sentinel);
    int abstime_nrem = rc2==EINTR && sentinel.tv_sec==7 && sentinel.tv_nsec==7;

    // 4) clock_nanosleep past absolute deadline returns 0 immediately.
    clock_gettime(CLOCK_MONOTONIC,&now);
    struct timespec past=now; past.tv_sec-=5;
    int pastret = clock_nanosleep(CLOCK_MONOTONIC,TIMER_ABSTIME,&past,NULL)==0;

    // 5) EINVAL on tv_nsec >= 1e9.
    struct timespec bad={0,1000000000L};
    int einval = clock_nanosleep(CLOCK_MONOTONIC,0,&bad,NULL)==EINVAL;

    // 6) ppoll timeout: no fds, times out returning 0.
    struct timespec pt={0,30*1000*1000};
    int ppollto = ppoll(NULL,0,&pt,NULL)==0;

    // 7) ppoll zero timeout returns 0 immediately.
    struct timespec zt={0,0};
    int ppollzero = ppoll(NULL,0,&zt,NULL)==0;

    // 8) pselect timeout returns 0 and does NOT modify the timeout arg (Linux).
    struct timespec ps={0,25*1000*1000}, pscopy=ps;
    int pr = pselect(0,NULL,NULL,NULL,&ps,NULL);
    int pselto = pr==0 && ps.tv_sec==pscopy.tv_sec && ps.tv_nsec==pscopy.tv_nsec;

    // 9) select DOES decrement its timeout on Linux (leftover time written back).
    struct timeval sv={2,0};
    int sr = select(0,NULL,NULL,NULL,&sv);
    // slept full 2s? no fds -> returns 0 after timeout; Linux writes remaining (~0).
    int selmod = sr==0 && sv.tv_sec<2;

    printf("sw full=%d eintr=%d absnrem=%d past=%d einval=%d ppollto=%d ppollzero=%d pselto=%d selmod=%d\n",
        full,eintr,abstime_nrem,pastret,einval,ppollto,ppollzero,pselto,selmod);
    return 0;
}
