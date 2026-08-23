#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

#define ATTEMPTS 70

int main(void) {
    int notify[2];
    if (pipe2(notify, O_NONBLOCK | O_CLOEXEC) != 0) return 10;

    pid_t children[ATTEMPTS];
    int created = 0;
    int failure = 0;
    for (int i = 0; i < ATTEMPTS; i++) {
        pid_t child = fork();
        if (child == 0) {
            char marker = 1;
            (void)write(notify[1], &marker, 1);
            for (;;)
                pause();
        }
        if (child < 0) {
            failure = errno;
            break;
        }
        children[created++] = child;
    }

    int markers = 0;
    for (int rounds = 0; rounds < 10000 && markers < created; rounds++) {
        char bytes[64];
        ssize_t count = read(notify[0], bytes, sizeof bytes);
        if (count > 0)
            markers += (int)count;
        else
            usleep(100);
    }

    int reaped = 0;
    for (int i = 0; i < created; i++) {
        kill(children[i], SIGKILL);
        int status;
        if (waitpid(children[i], &status, 0) == children[i]) reaped++;
    }
    close(notify[0]);
    close(notify[1]);

    int bounded = created == 63 && failure == EAGAIN;
    int children_ran = markers == created;
    printf("sentry_fork_exhaustion bounded=%d created=%d markers=%d reaped=%d\n", bounded, created, markers, reaped);
    return bounded && children_ran && reaped == created ? 0 : 1;
}
