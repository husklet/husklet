/* A tree whose child only moves when the capture kicks it.
 *
 * The sibling `sleep_tree.c` child polls every millisecond, so it reaches a checkpoint safepoint on its
 * own within a millisecond of the trigger generation being bumped -- which makes it useless for proving
 * anything about whether the coordinator FOUND it, because it would dump itself either way. This child
 * blocks in one long nanosleep, exactly as an interactive `sleep 1000` does, so it reaches a safepoint
 * only when it is interrupted. A capture that never enumerates it therefore never captures it, and the
 * difference between "found" and "not found" is observable in the manifest instead of being a race.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    char output[1024];
    if (snprintf(output, sizeof output, "%s.output", argv[1]) >= (int)sizeof output) return 2;
    int descriptor = open(output, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (descriptor < 0) return 3;
    pid_t child = fork();
    if (child < 0) return 4;
    if (child == 0) {
        dprintf(descriptor, "CHILD-READY\n");
        struct timespec interval = {.tv_sec = 1000};
        while (nanosleep(&interval, NULL) != 0)
            if (errno != EINTR) return 5;
        return 0;
    }
    dprintf(descriptor, "READY\n");
    int status;
    while (waitpid(child, &status, 0) < 0)
        if (errno != EINTR) return 9;
    dprintf(descriptor, "PARENT-FINAL\n");
    return WIFEXITED(status) ? WEXITSTATUS(status) : 10;
}
