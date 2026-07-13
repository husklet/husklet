#include "compat.h"
#include <stdio.h>
#include <errno.h>
#include <sys/types.h>
/* getresuid/getresgid with a NULL output pointer must fault (EFAULT), like Linux. */
int main(void){
  uid_t e,s;
  long a=syscall(SYS_getresuid, (void*)0, &e, &s);
  int uid_efault=(a==-1 && errno==EFAULT);
  gid_t ge,gs;
  long b=syscall(SYS_getresgid, (void*)0, &ge, &gs);
  int gid_efault=(b==-1 && errno==EFAULT);
  printf("getres_null uid_efault=%d gid_efault=%d\n", uid_efault, gid_efault); return 0; }
