#include "compat.h"
#include <stdio.h>
#include <errno.h>
/* pidfd_open with an unknown flag -> EINVAL; pidfd_send_signal with nonzero flags -> EINVAL. */
int main(void){
  long o=syscall(__NR_pidfd_open, getpid(), 0x40000000 /*not PIDFD_NONBLOCK*/);
  int open_einval=(o==-1 && errno==EINVAL);
  if(o>=0) close((int)o);
  long fd=syscall(__NR_pidfd_open, getpid(), 0);
  long sg = fd>=0 ? syscall(__NR_pidfd_send_signal, (int)fd, 0, (void*)0, (unsigned)0x80000000u /*reserved bit*/) : -1;
  int send_einval=(fd>=0 && sg==-1 && errno==EINVAL);
  if(fd>=0) close((int)fd);
  printf("pidfd_flags open_einval=%d send_einval=%d\n", open_einval, send_einval); return 0; }
