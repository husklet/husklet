#include "compat.h"
#include <stdio.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/socket.h>
/* SO_PASSCRED on a closed fd -> EBADF; SO_PEERCRED on a non-socket -> ENOTSOCK. hl used to fake-succeed. */
int main(void){
  int on=1;
  long r1=syscall(SYS_setsockopt, -1, SOL_SOCKET, SO_PASSCRED, &on, (socklen_t)sizeof on);
  int badfd_ebadf=(r1==-1 && errno==EBADF);
  int fd=open("/dev/null", O_RDONLY);
  struct ucred uc; socklen_t ul=sizeof uc;
  long r2=syscall(SYS_getsockopt, fd, SOL_SOCKET, SO_PEERCRED, &uc, &ul);
  int notsock=(r2==-1 && errno==ENOTSOCK);
  printf("passcred_badfd ebadf=%d notsock=%d\n", badfd_ebadf, notsock); return 0; }
