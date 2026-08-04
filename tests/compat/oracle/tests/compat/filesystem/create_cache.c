#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char path[128];
    struct stat status;
    snprintf(path, sizeof path, "/tmp/hl-create-cache-%ld", (long)getpid());
    unlink(path);
    errno = 0;
    int missing = stat(path, &status) == -1 && errno == ENOENT;
    int old = open(path, O_CREAT | O_EXCL | O_RDWR, 0600);
    int created = old >= 0 && write(old, "old", 3) == 3 && stat(path, &status) == 0 && status.st_size == 3;
    int removed = old >= 0 && unlink(path) == 0;
    errno = 0;
    removed = removed && stat(path, &status) == -1 && errno == ENOENT;
    int fresh = open(path, O_CREAT | O_EXCL | O_RDWR, 0600);
    int recreated = fresh >= 0 && write(fresh, "new", 3) == 3 && stat(path, &status) == 0 && status.st_size == 3;
    char before[4] = {0};
    char after[4] = {0};
    int independent = old >= 0 && fresh >= 0 && pread(old, before, 3, 0) == 3 && pread(fresh, after, 3, 0) == 3 &&
                      strcmp(before, "old") == 0 && strcmp(after, "new") == 0;
    printf("create-cache missing=%d created=%d removed=%d recreated=%d independent=%d\n", missing, created, removed,
           recreated, independent);
    if (old >= 0) close(old);
    if (fresh >= 0) close(fresh);
    unlink(path);
    return missing && created && removed && recreated && independent ? 0 : 1;
}
