// signalfd mask update: a second signalfd(fd, ...) on an existing signalfd replaces its mask exactly.
// Narrowing the mask drops the signals it removed -- the fd stops reporting them, and a signal that
// was raised but is no longer in the fd's mask stays pending on the thread instead of being consumed.
#include <sys/signalfd.h>
#include <signal.h>
#include <unistd.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>

int main(void) {
    sigset_t both;
    sigemptyset(&both);
    sigaddset(&both, SIGUSR1);
    sigaddset(&both, SIGUSR2);
    sigprocmask(SIG_BLOCK, &both, NULL);

    int fd = signalfd(-1, &both, SFD_NONBLOCK);

    // Narrow the SAME signalfd to SIGUSR1 only.
    sigset_t just_usr1;
    sigemptyset(&just_usr1);
    sigaddset(&just_usr1, SIGUSR1);
    int fd2 = signalfd(fd, &just_usr1, SFD_NONBLOCK);
    printf("update same_fd=%d\n", fd2 == fd);

    raise(SIGUSR1);
    raise(SIGUSR2);

    struct signalfd_siginfo si;
    ssize_t a = read(fd, &si, sizeof si);
    int first = a == (ssize_t)sizeof si ? (int)si.ssi_signo : -1;

    errno = 0;
    ssize_t b = read(fd, &si, sizeof si);
    int drained = (b < 0 && errno == EAGAIN);

    // SIGUSR2 was dropped from the fd's mask, so it is never delivered through the fd and remains
    // pending on the thread.
    sigset_t pending;
    sigpending(&pending);
    printf("read_signo=%d only_one=%d usr2_still_pending=%d\n",
           first, drained, sigismember(&pending, SIGUSR2));
    return 0;
}
