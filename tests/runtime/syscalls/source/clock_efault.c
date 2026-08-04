// CANDIDATE ENGINE BUG: clock_gettime with a bad user pointer faults the task instead of returning EFAULT.
// Native aarch64 Linux: syscall(SYS_clock_gettime, CLOCK_REALTIME, (void*)-1) -> -1/EFAULT(14).
// Engine: the guest is killed by SIGSEGV (probe child reports sig=11 => printed -11).
// Note: sibling bad-pointer syscalls (gettimeofday, times, read, pipe2, getrusage, newfstatat) all
// correctly return EFAULT under the same engine, so the fault-handling gap is clock_gettime-specific.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
int main(void){
    pid_t p=fork();
    if(p==0){ long r=syscall(SYS_clock_gettime,0,(long)-1,0,0,0); _exit(r==-1?(errno&0x7f):0); }
    int st=0; waitpid(p,&st,0);
    if(WIFSIGNALED(st)) printf("clock_gettime=-%d (CRASH)\n", WTERMSIG(st)); // engine: -11
    else printf("clock_gettime=%d\n", WEXITSTATUS(st));                     // native: 14
    return 0;
}
