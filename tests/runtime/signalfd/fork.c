#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/signalfd.h>
#include <sys/wait.h>
#include <unistd.h>

int signalfd_fork(void) {
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    if (sigprocmask(SIG_BLOCK, &mask, 0) != 0) return 20;
    int fd = signalfd(-1, &mask, SFD_NONBLOCK);
    if (fd < 0) return 21;
    pid_t child = fork();
    if (child < 0) return 22;
    if (child == 0) {
        struct signalfd_siginfo info;
        if (raise(SIGUSR1) != 0) _exit(30);
        ssize_t bytes = read(fd, &info, sizeof(info));
        _exit(bytes == 128 && info.ssi_signo == SIGUSR1 ? 0 : 31);
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child) return 23;
    struct signalfd_siginfo info;
    errno = 0;
    ssize_t parent = read(fd, &info, sizeof(info));
    int isolated = parent == -1 && errno == EAGAIN;
    int closed = close(fd) == 0;
    printf("signalfd_fork child=%d isolated=%d closed=%d\n",
        WIFEXITED(status) && WEXITSTATUS(status) == 0, isolated, closed);
    return 0;
}
