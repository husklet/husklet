#define _GNU_SOURCE
#include <stdio.h>
#include <linux/seccomp.h>
#include <linux/filter.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>
int main(){
  struct sock_filter f[]={ BPF_STMT(BPF_RET|BPF_K, SECCOMP_RET_ALLOW) };
  struct sock_fprog prog={.len=1,.filter=f};
  prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0);
  if(syscall(SYS_seccomp,SECCOMP_SET_MODE_FILTER,0,&prog)<0){perror("seccomp");return 1;}
  printf("SECCOMP=installed\n"); return 0;
}
