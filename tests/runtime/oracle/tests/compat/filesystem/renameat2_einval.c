// renameat2(2) flag validation is a fixed-ABI errno contract checked before any path work:
// RENAME_NOREPLACE|RENAME_EXCHANGE together is EINVAL (mutually exclusive), and an unknown flag
// bit is EINVAL. A plain rename of a file created in the same /tmp directory still succeeds. All
// on a single tmp directory (no mapped-volume path), so the flag-validation classes are the same
// on every backend independent of host VFS.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE (1 << 0)
#endif
#ifndef RENAME_EXCHANGE
#define RENAME_EXCHANGE (1 << 1)
#endif

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_rn2_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);
    close(openat(dfd, "a", O_CREAT | O_RDWR, 0644));

    errno = 0;
    int both = (int)syscall(SYS_renameat2, dfd, "a", dfd, "b", RENAME_NOREPLACE | RENAME_EXCHANGE) == -1 ? errno : 0;
    errno = 0;
    int badflag = (int)syscall(SYS_renameat2, dfd, "a", dfd, "b", 0x40) == -1 ? errno : 0;
    // Plain rename still works.
    int plain = (int)syscall(SYS_renameat2, dfd, "a", dfd, "b", 0) == 0;
    int landed = faccessat(dfd, "b", F_OK, 0) == 0 && faccessat(dfd, "a", F_OK, 0) == -1;

    unlinkat(dfd, "b", 0);
    close(dfd);
    rmdir(dir);
    printf("renameat2-einval both=%d badflag=%d plain=%d landed=%d\n", both, badflag, plain, landed);
    return 0;
}
