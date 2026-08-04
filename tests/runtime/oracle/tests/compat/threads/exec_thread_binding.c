#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

struct descriptors {
    int cloexec;
    int survivor;
};

static int inspect(const struct descriptors *descriptors) {
    errno = 0;
    int closed = fcntl(descriptors->cloexec, F_GETFD) == -1 && errno == EBADF;
    int alive = fcntl(descriptors->survivor, F_GETFD) >= 0;
    return closed && alive;
}

static void *inspect_thread(void *opaque) {
    return (void *)(intptr_t)inspect(opaque);
}

static int stage_two(char **argv) {
    struct descriptors descriptors = {
        .cloexec = atoi(argv[2]),
        .survivor = atoi(argv[3]),
    };
    int leader_ok = inspect(&descriptors);
    pthread_t thread;
    if (pthread_create(&thread, NULL, inspect_thread, &descriptors) != 0) return 20;
    void *result = NULL;
    if (pthread_join(thread, &result) != 0) return 21;
    int child_ok = (int)(intptr_t)result;
    int final_close = close(descriptors.survivor) == 0;
    static const char pass[] = "exec_thread_binding leader_ok=1 child_ok=1 final_close=1\n";
    static const char fail[] = "exec_thread_binding leader_ok=0 child_ok=0 final_close=0\n";
    int success = leader_ok && child_ok && final_close;
    const char *output = success ? pass : fail;
    size_t size = success ? sizeof pass - 1 : sizeof fail - 1;
    if (write(STDOUT_FILENO, output, size) != (ssize_t)size) return 22;
    syscall(SYS_exit_group, success ? 0 : 1);
    __builtin_unreachable();
}

static void *execute(void *opaque) {
    struct descriptors *descriptors = opaque;
    char cloexec[24];
    char survivor[24];
    snprintf(cloexec, sizeof cloexec, "%d", descriptors->cloexec);
    snprintf(survivor, sizeof survivor, "%d", descriptors->survivor);
    char *next[] = {"exec-thread-binding", "stage2", cloexec, survivor, NULL};
    execv("/proc/self/exe", next);
    return (void *)(intptr_t)errno;
}

int main(int argc, char **argv) {
    if (argc == 4 && strcmp(argv[1], "stage2") == 0) return stage_two(argv);

    char first[] = "/tmp/hl-exec-binding-a.XXXXXX";
    char second[] = "/tmp/hl-exec-binding-b.XXXXXX";
    struct descriptors descriptors = {
        .cloexec = mkstemp(first),
        .survivor = mkstemp(second),
    };
    if (descriptors.cloexec < 0 || descriptors.survivor < 0) return 10;
    unlink(first);
    unlink(second);
    if (fcntl(descriptors.cloexec, F_SETFD, FD_CLOEXEC) != 0) return 11;

    pthread_t thread;
    if (pthread_create(&thread, NULL, execute, &descriptors) != 0) return 12;
    if (pthread_join(thread, NULL) != 0) return 13;
    return 14;
}
