// Two pending signals are read from a signalfd in ascending signal-number order.
#include <sys/signalfd.h>
#include <signal.h>
#include <unistd.h>
#include <stdio.h>
int main(void){
    sigset_t m; sigemptyset(&m); sigaddset(&m, SIGUSR1); sigaddset(&m, SIGUSR2);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int fd = signalfd(-1, &m, SFD_NONBLOCK);
    raise(SIGUSR2); raise(SIGUSR1);
    struct signalfd_siginfo a, b;
    read(fd, &a, sizeof a);
    read(fd, &b, sizeof b);
    // standard signals are delivered lowest-numbered first
    printf("multi first=%d second=%d ordered=%d\n",
           a.ssi_signo, b.ssi_signo, a.ssi_signo < b.ssi_signo);  // 10 12 1
    return 0;
}
