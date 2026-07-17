// posix_fadvise(2) advisory hints on a real file: each advice returns 0 (or a positive errno it
// echoes). Linux-form (glibc posix_fadvise64) -> diffed against native oracle.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/tmp/hl_fadvise_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    char blk[8192];
    for (int i = 0; i < (int)sizeof blk; i++) blk[i] = (char)i;
    for (int i = 0; i < 16; i++) write(fd, blk, sizeof blk);

    int seq = posix_fadvise(fd, 0, 0, POSIX_FADV_SEQUENTIAL);
    int wn  = posix_fadvise(fd, 0, 65536, POSIX_FADV_WILLNEED);
    int rnd = posix_fadvise(fd, 0, 0, POSIX_FADV_RANDOM);
    int dn  = posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED);
    close(fd);
    unlink(path);
    printf("fadvise seq=%d willneed=%d random=%d dontneed=%d\n", seq, wn, rnd, dn);
    return 0;
}
