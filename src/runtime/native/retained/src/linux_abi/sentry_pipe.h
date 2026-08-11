#ifndef HL_LINUX_ABI_SENTRY_PIPE_H
#define HL_LINUX_ABI_SENTRY_PIPE_H

#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stddef.h>
#include <time.h>
#include <unistd.h>

static inline ssize_t hl_sentry_pipe_write(int fd, const void *data, size_t size) {
    sigset_t blocked;
    sigset_t previous;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGPIPE);
    int mask_error = pthread_sigmask(SIG_BLOCK, &blocked, &previous);
    if (mask_error != 0) {
        errno = mask_error;
        return -1;
    }

    sigset_t pending;
    sigpending(&pending);
    int already_pending = sigismember(&pending, SIGPIPE);
    ssize_t result;
    do
        result = write(fd, data, size);
    while (result < 0 && errno == EINTR);
    int saved = errno;

    if (result < 0 && saved == EPIPE && !already_pending) {
        int signal_number;
        while (sigwait(&blocked, &signal_number) == EINTR) {}
    }
    pthread_sigmask(SIG_SETMASK, &previous, NULL);
    errno = saved;
    return result;
}

#endif
