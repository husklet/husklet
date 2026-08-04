#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <unistd.h>
int main(void){
  int fds[2]; if(pipe(fds)){printf("ioctl fionread=-1 nonblock=0\n");return 0;}
  write(fds[1], "hello", 5);
  int avail=0; long r=syscall(SYS_ioctl, fds[0], FIONREAD, &avail);
  int on=1; long nb=syscall(SYS_ioctl, fds[0], FIONBIO, &on);
  char path[96]; snprintf(path,sizeof path,"/tmp/hl-ioctl-fio-%d",(int)getpid());
  int fd=open(path,O_CREAT|O_TRUNC|O_RDWR,0600); write(fd,"abcdef",6); lseek(fd,3,SEEK_SET);
  int regular=-1, ro=0, clo=0, unclo=0;
  if(fd>=0){ ro=ioctl(fd,FIONREAD,&regular)==0 && regular==3;
    int enable=1; ioctl(fd,FIONBIO,&enable); int fl=fcntl(fd,F_GETFL); int rnb=(fl&O_NONBLOCK)!=0;
    ioctl(fd,FIOCLEX); clo=(fcntl(fd,F_GETFD)&FD_CLOEXEC)!=0;
    ioctl(fd,FIONCLEX); unclo=(fcntl(fd,F_GETFD)&FD_CLOEXEC)==0;
    ro=ro&&rnb; close(fd); unlink(path); }
  printf("ioctl fionread=%d nb_ok=%d regular=%d cloexec=%d clear=%d\n", r==0?avail:-1, nb==0, ro, clo, unclo);
  return 0; }
