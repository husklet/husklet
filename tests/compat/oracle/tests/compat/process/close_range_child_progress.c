#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int channel[2];
    if (pipe(channel) != 0) return 2;
    unsigned first = (unsigned)(channel[0] > channel[1] ? channel[0] : channel[1]) + 1;
    pid_t child = fork();
    if (child < 0) return 3;
    if (child == 0) {
        close(channel[0]);
        long result = syscall(SYS_close_range, first, UINT_MAX, 0);
        char byte = result == 0 ? 'x' : 'e';
        if (write(channel[1], &byte, 1) != 1) _exit(4);
        _exit(result == 0 ? 0 : 5);
    }
    close(channel[1]);
    struct pollfd ready = {.fd = channel[0], .events = POLLIN};
    int fast = poll(&ready, 1, 1000) == 1;
    int status = 0;
    if (!fast) kill(child, SIGKILL);
    waitpid(child, &status, 0);
    int exited = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    printf("close-range-child-progress fast=%d exited=%d\n", fast, exited);
    return fast && exited ? 0 : 1;
}
