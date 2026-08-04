#define _GNU_SOURCE
#include <sys/eventfd.h>
#include <sys/wait.h>
#include <unistd.h>
#include <stdio.h>
#include <stdint.h>
#include <errno.h>
#include <string.h>
#include <time.h>
int main(void){
    int efd=eventfd(0,0); // BLOCKING
    pid_t p=fork();
    if(p==0){ struct timespec ts={0,200*1000*1000}; nanosleep(&ts,NULL);
        uint64_t v=5; ssize_t w=write(efd,&v,8);
        _exit(w==8?0:1);
    }
    uint64_t got=0; ssize_t r=read(efd,&got,8); int e=errno;
    int st; waitpid(p,&st,0);
    printf("efd_xproc r=%zd got=%llu errno=%d child=%d\n",r,(unsigned long long)got,r<0?e:0,WIFEXITED(st)?WEXITSTATUS(st):-1);
    return 0;
}
