#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static uint64_t monotonic_ns(void) {
    struct timespec value;
    syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &value);
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec;
}

int main(void) {
    uint64_t started = monotonic_ns();
    pid_t child = vfork();
    if (child == 0) {
        struct timespec delay = {0, 100000000};
        syscall(SYS_nanosleep, &delay, NULL);
        syscall(SYS_exit, 23);
    }
    uint64_t elapsed = monotonic_ns() - started;
    int status = 0;
    pid_t reaped = waitpid(child, &status, 0);
    int suspended = elapsed >= UINT64_C(80000000);
    int exited = reaped == child && WIFEXITED(status) && WEXITSTATUS(status) == 23;
    printf("vfork-parent suspended=%d exited=%d\n", suspended, exited);
    return suspended && exited ? 0 : 1;
}
