#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>

static int pin(const char *directory, int descriptor) {
    char path[1024];
    int source;
    if (snprintf(path, sizeof(path), "%s/ambient-%d.lock", directory, descriptor) >= (int)sizeof(path)) return -1;
    source = open(path, O_CREAT | O_RDWR, 0600);
    if (source < 0 || flock(source, LOCK_EX | LOCK_NB) != 0) return -1;
    if (source != descriptor) {
        if (dup2(source, descriptor) != descriptor) return -1;
        if (close(source) != 0) return -1;
    }
    if (fcntl(descriptor, F_SETFD, 0) != 0) return -1;
    return 0;
}

int main(int argc, char **argv) {
    const int descriptors[] = {3, 4, 17};
    if (argc < 3) {
        fprintf(stderr, "usage: ambient-fd-launcher LOCK-DIRECTORY TEST-BINARY [ARG...]\n");
        return 64;
    }
    for (size_t index = 0; index < sizeof(descriptors) / sizeof(*descriptors); ++index) {
        if (pin(argv[1], descriptors[index]) != 0) {
            fprintf(stderr, "cannot pin ambient fd %d: %s\n", descriptors[index], strerror(errno));
            return 71;
        }
    }
    if (setenv("HL_AMBIENT_FD_CHECKPOINT_CHILD", "1", 1) != 0 ||
        setenv("HL_AMBIENT_FD_DIRECTORY", argv[1], 1) != 0) {
        fprintf(stderr, "cannot configure ambient fd child: %s\n", strerror(errno));
        return 71;
    }
    execv(argv[2], &argv[2]);
    fprintf(stderr, "cannot execute ambient fd child: %s\n", strerror(errno));
    return 71;
}
