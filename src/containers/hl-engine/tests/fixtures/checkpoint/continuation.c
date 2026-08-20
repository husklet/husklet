#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int exists(const char *path) { return access(path, F_OK) == 0; }

int main(int argc, char **argv) {
    if (argc != 4) return 2;
    const char *ready = argv[1], *release = argv[2], *result = argv[3];
    int descriptors[2];
    if (pipe(descriptors) != 0) return 3;
    pid_t child = fork();
    if (child < 0) return 4;
    if (child == 0) {
        close(descriptors[0]);
        int marker = open(ready, O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (marker < 0 || write(marker, "R", 1) != 1 || close(marker) != 0) _exit(5);
        while (!exists(release)) {
            if (errno != ENOENT) _exit(6);
            usleep(1000);
        }
        if (write(descriptors[1], "X", 1) != 1) _exit(7);
        close(descriptors[1]);
        _exit(37);
    }
    close(descriptors[1]);
    char byte = 0;
    ssize_t count = read(descriptors[0], &byte, 1);
    char duplicate = 0;
    ssize_t second = read(descriptors[0], &duplicate, 1);
    int status = 0;
    pid_t reaped = waitpid(child, &status, 0);
    errno = 0;
    pid_t duplicate_reap = waitpid(child, &status, WNOHANG);
    int duplicate_errno = errno;
    FILE *output = fopen(result, "w");
    if (!output) return 8;
    fprintf(output, "read=%zd byte=%c second=%zd wait=%d exit=%d duplicate=%d errno=%d\n", count, byte, second,
            reaped == child, WIFEXITED(status) ? WEXITSTATUS(status) : -1, (int)duplicate_reap, duplicate_errno);
    if (fclose(output) != 0) return 9;
    return count == 1 && byte == 'X' && second == 0 && reaped == child && WIFEXITED(status) &&
                   WEXITSTATUS(status) == 37 && duplicate_reap == -1 && duplicate_errno == ECHILD
               ? 0
               : 10;
}
