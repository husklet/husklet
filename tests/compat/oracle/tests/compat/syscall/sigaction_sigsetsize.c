// CANDIDATE ENGINE BUG: rt_sigaction does not validate the sigsetsize argument.
// Native aarch64 Linux: syscall(SYS_rt_sigaction, SIGUSR1, &sa, NULL, 4) -> -1/EINVAL(22) (kernel requires
// sigsetsize == 8). Engine: returns 0 (success). The engine DOES correctly reject SIGKILL/SIGSTOP handlers
// and out-of-range signals with EINVAL, and rt_sigprocmask DOES validate its size -- so this size-validation
// gap is specific to rt_sigaction.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
int main(void){
    struct sigaction sa; memset(&sa,0,sizeof(sa)); sa.sa_handler=SIG_IGN;
    long r=syscall(SYS_rt_sigaction, SIGUSR1, &sa, (void*)0, 4);
    printf("badsize_errno=%d\n", r==-1?errno:0); // native 22, engine 0
    return 0;
}
