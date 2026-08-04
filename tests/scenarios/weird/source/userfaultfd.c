#define _GNU_SOURCE
#include <stdio.h>
#include <linux/userfaultfd.h>
#include <sys/syscall.h>
#include <sys/ioctl.h>
#include <unistd.h>
int main(){
  int fd=syscall(SYS_userfaultfd,0);
  if(fd<0){perror("userfaultfd");return 1;}
  struct uffdio_api api={.api=UFFD_API};
  if(ioctl(fd,UFFDIO_API,&api)<0){perror("api");return 1;}
  printf("UFFD=ok\n"); return 0;
}
