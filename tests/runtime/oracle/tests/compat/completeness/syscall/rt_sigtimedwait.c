#define _GNU_SOURCE
#include "compat.h"
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long long elapsed_ns(struct timespec start, struct timespec end) {
  return (long long)(end.tv_sec-start.tv_sec)*1000000000LL + end.tv_nsec-start.tv_nsec;
}

int main(void){
  sigset_t set; sigemptyset(&set); sigaddset(&set, SIGUSR1);
  sigprocmask(SIG_BLOCK, &set, 0);
  raise(SIGUSR1);
  struct timespec to={2,0}; siginfo_t info; memset(&info,0,sizeof info);
  long r=syscall(SYS_rt_sigtimedwait, &set, &info, &to, _NSIG/8);
  int pending = r==SIGUSR1 && info.si_signo==SIGUSR1;

  struct timespec zero={0,0}; errno=0;
  r=syscall(SYS_rt_sigtimedwait, &set, &info, &zero, _NSIG/8);
  int zero_poll = r==-1 && errno==EAGAIN;

  pid_t parent=getpid(), child=fork();
  if(child==0){ usleep(50000); kill(parent,SIGUSR1); _exit(0); }
  struct timespec start,end, generous={1,0}; memset(&info,0,sizeof info);
  clock_gettime(CLOCK_MONOTONIC,&start);
  r=syscall(SYS_rt_sigtimedwait, &set, &info, &generous, _NSIG/8);
  clock_gettime(CLOCK_MONOTONIC,&end);
  int delayed=r==SIGUSR1 && info.si_signo==SIGUSR1 && elapsed_ns(start,end)<800000000LL;
  waitpid(child,0,0);

  struct timespec short_to={0,20000000};
  clock_gettime(CLOCK_MONOTONIC,&start); errno=0;
  r=syscall(SYS_rt_sigtimedwait, &set, &info, &short_to, _NSIG/8);
  clock_gettime(CLOCK_MONOTONIC,&end);
  long long elapsed=elapsed_ns(start,end);
  int timeout=r==-1 && errno==EAGAIN && elapsed>=10000000LL && elapsed<800000000LL;
  printf("rt_sigtimedwait pending=%d zero=%d delayed=%d timeout=%d\n",pending,zero_poll,delayed,timeout);
  return !(pending&&zero_poll&&delayed&&timeout); }
