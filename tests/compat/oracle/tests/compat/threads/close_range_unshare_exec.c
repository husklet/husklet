#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/eventfd.h>
#include <unistd.h>

#ifndef CLOSE_RANGE_UNSHARE
#define CLOSE_RANGE_UNSHARE (1U << 1)
#endif

struct descriptors {
    int closed;
    int survivor;
};

static int check(const struct descriptors *descriptors) {
    errno = 0;
    int closed = fcntl(descriptors->closed, F_GETFD) == -1 && errno == EBADF;
    int survivor = fcntl(descriptors->survivor, F_GETFD) >= 0;
    return closed && survivor;
}

static void *check_child(void *opaque) {
    return (void *)(intptr_t)check(opaque);
}

static int stage_two(char **argv) {
    struct descriptors descriptors = {
        .closed = atoi(argv[2]),
        .survivor = atoi(argv[3]),
    };
    int leader_ok = check(&descriptors);
    pthread_t thread;
    if (pthread_create(&thread, NULL, check_child, &descriptors) != 0) return 20;
    void *child_result = NULL;
    if (pthread_join(thread, &child_result) != 0) return 21;
    int child_ok = (int)(intptr_t)child_result;
    int final_close = close(descriptors.survivor) == 0;
    static const char pass[] = "close_range_exec leader_ok=1 child_ok=1 final_close=1\n";
    static const char fail[] = "close_range_exec leader_ok=0 child_ok=0 final_close=0\n";
    int success = leader_ok && child_ok && final_close;
    const char *output = success ? pass : fail;
    size_t size = success ? sizeof pass - 1 : sizeof fail - 1;
    if (write(STDOUT_FILENO, output, size) != (ssize_t)size) return 22;
    syscall(SYS_exit_group, success ? 0 : 1);
    __builtin_unreachable();
}

int main(int argc, char **argv) {
    if (argc == 4 && strcmp(argv[1], "stage2") == 0) return stage_two(argv);

    struct descriptors descriptors = {
        .closed = eventfd(0, EFD_NONBLOCK),
        .survivor = eventfd(0, EFD_NONBLOCK),
    };
    if (descriptors.closed < 0 || descriptors.survivor < 0) return 10;

    if (syscall(SYS_close_range, (unsigned)descriptors.closed, (unsigned)descriptors.closed,
                CLOSE_RANGE_UNSHARE) != 0)
        return 11;
    char closed[24];
    char survivor[24];
    snprintf(closed, sizeof closed, "%d", descriptors.closed);
    snprintf(survivor, sizeof survivor, "%d", descriptors.survivor);
    char *next[] = {"close-range-unshare-exec", "stage2", closed, survivor, NULL};
    execv("/proc/self/exe", next);
    return 13;
}
