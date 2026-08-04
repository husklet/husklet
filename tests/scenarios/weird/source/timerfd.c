#include <stdio.h>
#include <sys/timerfd.h>
#include <unistd.h>
#include <stdint.h>
int main(){
  int fd=timerfd_create(CLOCK_MONOTONIC,0);
  if(fd<0){perror("timerfd");return 1;}
  struct itimerspec its={{0,0},{0,50000000}};
  timerfd_settime(fd,0,&its,0);
  uint64_t e; read(fd,&e,8);
  printf("TIMERFD=%llu\n",(unsigned long long)e); return 0;
}
