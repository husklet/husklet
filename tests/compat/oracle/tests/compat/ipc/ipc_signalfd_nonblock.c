// SFD_NONBLOCK signalfd with no pending signal returns EAGAIN rather than blocking.
#include <sys/signalfd.h>
#include <signal.h>
#include <unistd.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
int main(void){
    sigset_t m; sigemptyset(&m); sigaddset(&m, SIGUSR1);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int fd = signalfd(-1, &m, SFD_NONBLOCK);
    struct signalfd_siginfo si;
    errno = 0;
    ssize_t n = read(fd, &si, sizeof si); int e = errno;
    printf("nonblock read=%zd errno=%s\n", n, strerror(e));   // -1 EAGAIN
    return 0;
}
