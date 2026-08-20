/* A terminal-shaped checkpoint fixture: an init waits for a child blocked in
 * sleep, matching an interactive shell running `sleep 1000`. */
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
        struct timespec interval = {.tv_nsec = 1000000};
        while (access(argv[1], F_OK) != 0) {
            if (errno != ENOENT) return 5;
            if (nanosleep(&interval, NULL) != 0 && errno != EINTR) return 6;
        }
        dprintf(descriptor, "CHILD-RESTORED\n");
        while (access(argv[2], F_OK) != 0) {
            if (errno != ENOENT) return 7;
            if (nanosleep(&interval, NULL) != 0 && errno != EINTR) return 8;
        }
        dprintf(descriptor, "CHILD-FINAL\n");
        return 0;
    }
    dprintf(descriptor, "READY\n");
    int status;
    while (waitpid(child, &status, 0) < 0)
        if (errno != EINTR) return 9;
    dprintf(descriptor, "PARENT-FINAL\n");
    return WIFEXITED(status) ? WEXITSTATUS(status) : 10;
}
