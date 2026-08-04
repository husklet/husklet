#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int transfer(int fd, void *data, size_t size, int writing) {
    size_t offset = 0;
    while (offset < size) {
        ssize_t count = writing ? write(fd, (char *)data + offset, size - offset)
                                : read(fd, (char *)data + offset, size - offset);
        if (count <= 0) return -1;
        offset += (size_t)count;
    }
    return 0;
}

int main(void) {
    char dir[128], path[192], resolved[4096];
    int ready[2], advance[2];
    snprintf(dir, sizeof dir, "/tmp/hl_sibling_generation_%d", (int)getpid());
    snprintf(path, sizeof path, "%s/recreated", dir);
    if (mkdir(dir, 0755) != 0 || pipe(ready) != 0 || pipe(advance) != 0) return 1;
    if (realpath(path, resolved) != NULL) return 2;

    pid_t child = fork();
    if (child == 0) {
        close(ready[0]);
        close(advance[1]);
        for (unsigned round = 0; round < 100; ++round) {
            int fd = open(path, O_CREAT | O_RDWR | O_TRUNC | O_CLOEXEC, 0600);
            if (fd < 0 || write(fd, &round, sizeof(round)) != sizeof(round) || close(fd) != 0) _exit(20);
            char event = 'c';
            if (transfer(ready[1], &event, 1, 1) != 0 || transfer(advance[0], &event, 1, 0) != 0) _exit(21);
            if (unlink(path) != 0) _exit(22);
            event = 'd';
            if (transfer(ready[1], &event, 1, 1) != 0 || transfer(advance[0], &event, 1, 0) != 0) _exit(23);
        }
        _exit(0);
    }

    close(ready[1]);
    close(advance[0]);
    int coherent = child > 0;
    for (unsigned round = 0; coherent && round < 100; ++round) {
        char event = 0;
        unsigned value = 0;
        if (transfer(ready[0], &event, 1, 0) != 0 || event != 'c' || realpath(path, resolved) == NULL)
            coherent = 0;
        int fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0 || read(fd, &value, sizeof(value)) != sizeof(value) || value != round) coherent = 0;
        if (fd >= 0) close(fd);
        if (transfer(advance[1], &event, 1, 1) != 0 || transfer(ready[0], &event, 1, 0) != 0 || event != 'd' ||
            realpath(path, resolved) != NULL)
            coherent = 0;
        if (transfer(advance[1], &event, 1, 1) != 0) coherent = 0;
    }
    close(ready[0]);
    close(advance[1]);
    int status = 0;
    coherent = coherent && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    unlink(path);
    rmdir(dir);
    printf("sibling-generation rounds=100 coherent=%d\n", coherent);
    return coherent ? 0 : 3;
}
