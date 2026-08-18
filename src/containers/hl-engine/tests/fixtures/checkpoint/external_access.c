#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int wait_readable(const char *path) {
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        if (access(path, R_OK) == 0) return 0;
        struct timespec delay = {0, 1000000};
        nanosleep(&delay, NULL);
    }
    return -1;
}

static int exists(const char *path) {
    errno = 0;
    int result = access(path, F_OK);
    return result == 0 ? 1 : result == -1 && errno == ENOENT ? 0 : -1;
}

static void path(char *output, size_t capacity, const char *directory, const char *name) {
    snprintf(output, capacity, "%s/%s", directory, name);
}

int main(int argc, char **argv) {
    if (argc != 2) return 10;
    char output[1024], release[1024], mutate[1024];
    char inherited_create[1024], inherited_delete[1024], restored_create[1024], restored_delete[1024];
    path(output, sizeof output, argv[1], "output");
    path(release, sizeof release, argv[1], "release");
    path(mutate, sizeof mutate, argv[1], "mutate");
    path(inherited_create, sizeof inherited_create, argv[1], "inherited-create");
    path(inherited_delete, sizeof inherited_delete, argv[1], "inherited-delete");
    path(restored_create, sizeof restored_create, argv[1], "restored-create");
    path(restored_delete, sizeof restored_delete, argv[1], "restored-delete");

    int log = open(output, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (log < 0 || exists(inherited_create) != 0 || exists(inherited_delete) != 1) return 11;
    dprintf(log, "READY\n");
    if (wait_readable(release) != 0) return 12;
    if (exists(inherited_create) != 1 || exists(inherited_delete) != 0) return 13;
    if (exists(restored_create) != 0 || exists(restored_delete) != 1) return 14;
    dprintf(log, "RESTORED-CACHED\n");
    if (wait_readable(mutate) != 0) return 15;
    if (exists(restored_create) != 1 || exists(restored_delete) != 0) return 16;
    dprintf(log, "DONE external-access-ok\n");
    return 0;
}
