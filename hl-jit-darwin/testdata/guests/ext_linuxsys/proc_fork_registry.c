#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int proc_status_exists(pid_t pid) {
    char path[64];
    snprintf(path, sizeof path, "/proc/%d/status", (int)pid);
    return access(path, R_OK) == 0;
}

int main(void) {
    int ready[2], release[2];
    if (pipe(ready) != 0 || pipe(release) != 0) {
        printf("pipe errno=%d\n", errno);
        return 1;
    }

    pid_t self = getpid();
    pid_t child = fork();
    if (child < 0) {
        printf("fork errno=%d\n", errno);
        return 1;
    }
    if (child == 0) {
        close(ready[0]);
        close(release[1]);
        char b = 'x';
        if (write(ready[1], &b, 1) != 1) _exit(2);
        if (read(release[0], &b, 1) < 0) _exit(3);
        _exit(0);
    }

    close(ready[1]);
    close(release[0]);
    char b = 0;
    int got_ready = read(ready[0], &b, 1) == 1;
    int child_live = got_ready && proc_status_exists(child);
    int self_before = proc_status_exists(self);
    if (write(release[1], "x", 1) != 1) {
        printf("release errno=%d\n", errno);
        return 1;
    }
    int st = 0;
    int waited = waitpid(child, &st, 0) == child;
    int self_after = proc_status_exists(self);

    printf("proc_fork_registry child=%d self_before=%d self_after=%d exit=%d\n", child_live, self_before, self_after,
           waited && WIFEXITED(st) && WEXITSTATUS(st) == 0);
    return child_live && self_before && self_after && waited && WIFEXITED(st) && WEXITSTATUS(st) == 0 ? 0 : 1;
}
