#include "compat.h"
#include <stdio.h>
#include <errno.h>
#include <sched.h>
/* unshare(unknown flags) -> EINVAL; setns(-1, ...) -> EBADF. hl used to fake-succeed both. */
int main(void){
  long u=syscall(SYS_unshare, (long)0xdeadbeefU);
  int unshare_einval=(u==-1 && errno==EINVAL);
  long s=syscall(SYS_setns, -1, CLONE_NEWNET);
  int setns_ebadf=(s==-1 && errno==EBADF);
  printf("unshare_setns unshare_einval=%d setns_ebadf=%d\n", unshare_einval, setns_ebadf); return 0; }
