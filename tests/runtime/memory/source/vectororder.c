#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/uio.h>
#include <unistd.h>

int main(void) {
    int pipefd[2];
    if (pipe(pipefd) != 0) return 2;
    int flags = fcntl(pipefd[0], F_GETFL, 0);
    if (flags < 0 || fcntl(pipefd[0], F_SETFL, flags | O_NONBLOCK) != 0) return 3;

    unsigned char byte = 0x5a;
    struct iovec vectors[2] = {
        {&byte, 1},
        {(void *)(uintptr_t)(UINTPTR_MAX - 7), 16},
    };
    errno = 0;
    ssize_t invalid = writev(pipefd[1], vectors, 2);
    int invalid_ok = invalid == -1 && errno == EFAULT;

    errno = 0;
    ssize_t bad = writev(-1, vectors, 2);
    int bad_ok = bad == -1 && errno == EBADF;

    unsigned char observed = 0;
    errno = 0;
    ssize_t empty = read(pipefd[0], &observed, 1);
    int untouched_ok = empty == -1 && errno == EAGAIN && observed == 0;

    close(pipefd[0]);
    close(pipefd[1]);
    printf("vector-order range=%d badfd=%d untouched=%d\n", invalid_ok, bad_ok, untouched_ok);
    return !(invalid_ok && bad_ok && untouched_ok);
}
