// A queued realtime signal carries its sival_int through to signalfd's ssi_int payload.
#include <sys/signalfd.h>
#include <signal.h>
#include <unistd.h>
#include <stdio.h>
int main(void){
    int rt = SIGRTMIN + 1;
    sigset_t m; sigemptyset(&m); sigaddset(&m, rt);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int fd = signalfd(-1, &m, 0);
    union sigval sv; sv.sival_int = 4242;
    sigqueue(getpid(), rt, sv);
    struct signalfd_siginfo si; read(fd, &si, sizeof si);
    printf("rt is_rt=%d int=%d code_si_queue=%d\n",
           si.ssi_signo == (unsigned)rt, si.ssi_int, si.ssi_code == SI_QUEUE);  // 1 4242 1
    return 0;
}
