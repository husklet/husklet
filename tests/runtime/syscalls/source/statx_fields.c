// syscall-compat coverage: statx(2) returns coherent metadata. For a freshly written 7-byte regular file the
// mask must report SIZE/TYPE/MODE, the size must be 7, the type must be S_IFREG, and the link count 1. statx
// on a missing path -> ENOENT. Arch-neutral: only derived facts / errnos are printed (no inode/time values).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <linux/stat.h>
#ifndef __NR_statx
#define __NR_statx 291
#endif

int main(void) {
    char tmpl[] = "/tmp/statx_XXXXXX";
    int fd = mkstemp(tmpl);
    write(fd, "1234567", 7);
    close(fd);

    struct statx sx;
    long r = syscall(__NR_statx, AT_FDCWD, tmpl, 0, STATX_ALL, &sx);
    printf("statx_ok=%d\n", r == 0);
    printf("has_size=%d has_type=%d has_mode=%d\n", (sx.stx_mask & STATX_SIZE) != 0,
           (sx.stx_mask & STATX_TYPE) != 0, (sx.stx_mask & STATX_MODE) != 0);
    printf("size=%llu isreg=%d nlink=%d\n", (unsigned long long)sx.stx_size,
           (sx.stx_mode & S_IFMT) == S_IFREG, sx.stx_nlink == 1);

    unlink(tmpl);
    struct statx sx2;
    printf("missing_errno=%d\n", syscall(__NR_statx, AT_FDCWD, tmpl, 0, STATX_BASIC_STATS, &sx2) == -1 ? errno : 0);
    return 0;
}
