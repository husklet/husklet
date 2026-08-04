// syscall-compat regression: a self-referential symlink must terminate path resolution with ELOOP, not
// loop forever. Native Linux caps symlink traversal (40) and returns ELOOP. Arch-neutral: errno only.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>
#include <stdlib.h>

int main(void) {
    char dir[] = "/tmp/eloop_XXXXXX";
    mkdtemp(dir);
    char loop[256];
    snprintf(loop, sizeof(loop), "%s/loop", dir);
    symlink(loop, loop);
    struct stat st;
    int r = stat(loop, &st);
    printf("eloop_errno=%d\n", r == -1 ? errno : 0);
    unlink(loop); rmdir(dir);
    return 0;
}
