#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int path_join(char output[PATH_MAX], const char *directory, const char *leaf) {
    int count = snprintf(output, PATH_MAX, "%s/%s", directory, leaf);
    return count > 0 && count < PATH_MAX ? 0 : -1;
}

static int wait_marker(const char *path) {
    struct timespec delay = {.tv_sec = 0, .tv_nsec = 5000000};
    for (int attempt = 0; attempt < 6000; ++attempt) {
        if (access(path, F_OK) == 0) return 0;
        if (errno != ENOENT) return -1;
        nanosleep(&delay, NULL);
    }
    errno = ETIMEDOUT;
    return -1;
}

static int append(int descriptor, const char *text) {
    size_t size = strlen(text);
    return write(descriptor, text, size) == (ssize_t)size && fsync(descriptor) == 0 ? 0 : -1;
}

int main(int argc, char **argv) {
    char output[PATH_MAX], cycle1[PATH_MAX], cycle2[PATH_MAX], finish[PATH_MAX];
    if (argc != 2 || path_join(output, argv[1], "output") != 0 || path_join(cycle1, argv[1], "cycle1") != 0 ||
        path_join(cycle2, argv[1], "cycle2") != 0 || path_join(finish, argv[1], "finish") != 0)
        return 10;
    int descriptor = open(output, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (descriptor != 3) return descriptor < 0 ? 11 : 12;
    if (append(descriptor, "BOOT fd=3\n") != 0 || wait_marker(cycle1) != 0 ||
        append(descriptor, "CYCLE 1 fd=3\n") != 0 || wait_marker(cycle2) != 0 ||
        append(descriptor, "DONE ambient-fd-ok fd=3\n") != 0 || wait_marker(finish) != 0 || close(descriptor) != 0)
        return 13;
    return 0;
}
