// signalfd delivers a blocked signal as a signalfd_siginfo record carrying ssi_signo.
#include <sys/signalfd.h>
#include <signal.h>
#include <unistd.h>
#include <stdio.h>
#include <string.h>
int main(void){
    sigset_t m; sigemptyset(&m); sigaddset(&m, SIGUSR1);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int fd = signalfd(-1, &m, 0);
    raise(SIGUSR1);
    struct signalfd_siginfo si; ssize_t n = read(fd, &si, sizeof si);
    printf("signalfd read=%zd signo=%d is_usr1=%d\n",
           n, si.ssi_signo, si.ssi_signo == (unsigned)SIGUSR1);  // 128 10 1
    return 0;
}
