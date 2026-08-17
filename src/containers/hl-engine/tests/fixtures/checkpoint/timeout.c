#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void ready_after_delay(const char *path) {
    usleep(500000);
    int descriptor = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (descriptor < 0 || write(descriptor, "R", 1) != 1 || close(descriptor) != 0) _exit(20);
    _exit(0);
}

int main(int argc, char **argv) {
    if (argc != 4) return 2;
    const char *mode = argv[1], *ready = argv[2], *result = argv[3];
    pid_t child = fork();
    if (child < 0) return 3;
    if (child == 0) ready_after_delay(ready);
    struct timespec timeout = {.tv_sec = 2, .tv_nsec = 0};
    struct timespec remainder = {.tv_sec = 73, .tv_nsec = 41};
    struct epoll_event event = {.events = 0xdeadbeefU, .data.u64 = 0x123456789abcdef0ULL};
    int result_code;
    errno = 0;
    if (strcmp(mode, "nanosleep") == 0)
        result_code = nanosleep(&timeout, &remainder);
    else if (strcmp(mode, "clock_nanosleep") == 0)
        result_code = clock_nanosleep(CLOCK_MONOTONIC, 0, &timeout, &remainder);
    else if (strcmp(mode, "ppoll") == 0)
        result_code = ppoll(NULL, 0, &timeout, NULL);
    else if (strcmp(mode, "pselect") == 0)
        result_code = pselect(0, NULL, NULL, NULL, &timeout, NULL);
    else if (strcmp(mode, "epoll_pwait") == 0) {
        int epoll = epoll_create1(0);
        if (epoll < 0) return 8;
        result_code = epoll_pwait(epoll, &event, 1, 2000, NULL);
        close(epoll);
    } else if (strcmp(mode, "epoll_pwait2") == 0) {
        int epoll = epoll_create1(0);
        if (epoll < 0) return 8;
        result_code = (int)syscall(SYS_epoll_pwait2, epoll, &event, 1, &timeout, NULL, sizeof(sigset_t));
        close(epoll);
    }
    else
        return 4;
    int saved_errno = errno;
    (void)child;
    FILE *output = fopen(result, "w");
    if (!output) return 6;
    fprintf(output, "result=%d errno=%d rem=%lld.%09ld event=%08x/%016llx\n", result_code, saved_errno,
            (long long)remainder.tv_sec, remainder.tv_nsec, event.events, (unsigned long long)event.data.u64);
    fclose(output);
    return result_code == 0 ? 0 : 7;
}
