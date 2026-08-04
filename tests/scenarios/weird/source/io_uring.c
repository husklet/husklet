#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <linux/io_uring.h>
#include <unistd.h>
int main(){
  struct io_uring_params p; memset(&p,0,sizeof p);
  int fd=syscall(SYS_io_uring_setup,8,&p);
  if(fd<0){perror("io_uring_setup");return 1;}
  printf("IOURING=ok sq=%u\n",p.sq_entries); return 0;
}
