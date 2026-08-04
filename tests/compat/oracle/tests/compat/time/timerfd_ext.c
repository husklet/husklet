// timerfd extended semantics vs the native oracle (normalized 0/1 verdicts, byte-identical on both
// Linux engines). Covers surface the base timerfdx case omits:
//   * TFD_NONBLOCK -> unexpired read gives EAGAIN; the expiration count resets to 0 after a read.
//   * timerfd_settime old_value returns the previously armed setting.
//   * TFD_CLOEXEC sets FD_CLOEXEC (and its absence leaves it clear).
//   * poll() reports POLLIN once armed and expired; an unexpired long timer times out (0).
//   * re-arm after disarm fires again; CLOCK_REALTIME and CLOCK_BOOTTIME one-shots fire.
//   * gettime after a periodic read reports the interval as the remaining time.
//   * a timerfd armed before fork() delivers an independent expiration to parent and child.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/timerfd.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int rd(int fd, uint64_t *o){ return read(fd,o,8)==8; }

int main(void){
    struct itimerspec its, old, cur;

    // 1) TFD_NONBLOCK: unexpired read -> EAGAIN.
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    memset(&its,0,sizeof its); its.it_value.tv_sec=100;
    timerfd_settime(fd,0,&its,NULL);
    uint64_t n=12345; errno=0;
    int eagain = read(fd,&n,8)==-1 && errno==EAGAIN;
    close(fd);

    // 2) count reset after read: periodic, let several fire, read once (>=3), then the accumulated count
    // must be gone -- the next read either blocks-as-EAGAIN or reports only ticks accrued SINCE the read.
    // (Requiring a bare EAGAIN asserted that the second read lands inside one 10ms period, which is a
    // property of the host's scheduling, not of timerfd: one descheduled slice past the interval makes a
    // fresh expiry legitimately readable. The invariant under test is the reset, so test the reset.)
    fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    memset(&its,0,sizeof its);
    its.it_value.tv_nsec=10*1000*1000; its.it_interval.tv_nsec=10*1000*1000;
    timerfd_settime(fd,0,&its,NULL);
    usleep(45*1000); // >=4 periods elapse, so the >=3 floor keeps margin on a slow host
    n=0; int firstpos = rd(fd,&n) && n>=3;
    errno=0; uint64_t n2=0;
    ssize_t again = read(fd,&n2,8);
    int consumed = (again==-1 && errno==EAGAIN) || (again==8 && n2<n);
    close(fd);

    // 3) timerfd_settime returns the previous setting in old_value.
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    memset(&its,0,sizeof its); its.it_value.tv_sec=50; its.it_interval.tv_sec=9;
    timerfd_settime(fd,0,&its,NULL);
    memset(&its,0,sizeof its); its.it_value.tv_sec=30;
    memset(&old,0,sizeof old);
    timerfd_settime(fd,0,&its,&old);
    int oldval = old.it_value.tv_sec>0 && old.it_value.tv_sec<=50 && old.it_interval.tv_sec==9;
    close(fd);

    // 4) TFD_CLOEXEC sets FD_CLOEXEC; default leaves it clear.
    fd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
    int cloexec = (fcntl(fd,F_GETFD)&FD_CLOEXEC)!=0;
    close(fd);
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    int nocloexec = (fcntl(fd,F_GETFD)&FD_CLOEXEC)==0;
    close(fd);

    // 5) poll POLLIN fires after expiry.
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    memset(&its,0,sizeof its); its.it_value.tv_nsec=30*1000*1000;
    timerfd_settime(fd,0,&its,NULL);
    struct pollfd pfd={fd,POLLIN,0};
    int pollin = poll(&pfd,1,2000)==1 && (pfd.revents&POLLIN);
    n=0; rd(fd,&n);
    close(fd);

    // 6) poll on an unexpired long timer times out (0).
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    memset(&its,0,sizeof its); its.it_value.tv_sec=100;
    timerfd_settime(fd,0,&its,NULL);
    struct pollfd pfd2={fd,POLLIN,0};
    int pollto = poll(&pfd2,1,50)==0;
    close(fd);

    // 7) re-arm after disarm works.
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    memset(&its,0,sizeof its); its.it_value.tv_sec=100;
    timerfd_settime(fd,0,&its,NULL);
    memset(&its,0,sizeof its);
    timerfd_settime(fd,0,&its,NULL);
    memset(&its,0,sizeof its); its.it_value.tv_nsec=25*1000*1000;
    timerfd_settime(fd,0,&its,NULL);
    n=0; int rearm = rd(fd,&n) && n==1;
    close(fd);

    // 8) CLOCK_REALTIME + CLOCK_BOOTTIME one-shots fire.
    fd = timerfd_create(CLOCK_REALTIME, 0);
    memset(&its,0,sizeof its); its.it_value.tv_nsec=25*1000*1000;
    timerfd_settime(fd,0,&its,NULL);
    n=0; int rt = rd(fd,&n) && n==1;
    close(fd);
    int bfd = timerfd_create(CLOCK_BOOTTIME, 0);
    int boot=1;
    if (bfd>=0){ memset(&its,0,sizeof its); its.it_value.tv_nsec=25*1000*1000;
        timerfd_settime(bfd,0,&its,NULL); n=0; boot=rd(bfd,&n)&&n==1; close(bfd); }

    // 9) gettime after a periodic read reports interval as remaining (0<rem<=interval).
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    memset(&its,0,sizeof its); its.it_value.tv_nsec=10*1000*1000; its.it_interval.tv_sec=5;
    timerfd_settime(fd,0,&its,NULL);
    n=0; rd(fd,&n);
    memset(&cur,0,sizeof cur); timerfd_gettime(fd,&cur);
    int gtafter = cur.it_interval.tv_sec==5 && (cur.it_value.tv_sec>0 || cur.it_value.tv_nsec>0) && cur.it_value.tv_sec<=5;
    close(fd);

    // 10) a timerfd armed before fork() delivers an independent expiration to parent and child.
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    memset(&its,0,sizeof its); its.it_value.tv_nsec=40*1000*1000; its.it_interval.tv_nsec=40*1000*1000;
    timerfd_settime(fd,0,&its,NULL);
    pid_t pid=fork();
    if(pid==0){ uint64_t c=0; _exit((read(fd,&c,8)==8 && c>=1)?0:1); }
    uint64_t pc=0; int prd=read(fd,&pc,8)==8 && pc>=1;
    int st=0; waitpid(pid,&st,0);
    int fork_ok = prd && WIFEXITED(st) && WEXITSTATUS(st)==0;
    close(fd);

    printf("tfd eagain=%d firstpos=%d consumed=%d oldval=%d cloexec=%d nocloexec=%d pollin=%d pollto=%d rearm=%d rt=%d boot=%d gtafter=%d fork=%d\n",
        eagain,firstpos,consumed,oldval,cloexec,nocloexec,pollin,pollto,rearm,rt,boot,gtafter,fork_ok);
    return 0;
}
