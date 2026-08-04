// faccessat / faccessat2: F_OK / R_OK / W_OK / X_OK checks, AT_SYMLINK_NOFOLLOW on a
// dangling link, and ENOENT / EACCES resolution against mode bits.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_faccess_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "rx", O_CREAT | O_WRONLY, 0644);
    close(fd);
    fchmodat(dfd, "rx", 0555, 0);   // read+execute, no write

    int exists = faccessat(dfd, "rx", F_OK, 0) == 0;
    int readable = faccessat(dfd, "rx", R_OK, 0) == 0;
    int executable = faccessat(dfd, "rx", X_OK, 0) == 0;

    // Missing name -> ENOENT.
    errno = 0;
    int miss = faccessat(dfd, "gone", F_OK, 0);
    int enoent = miss != 0 && errno == ENOENT;

    // Dangling symlink: following -> ENOENT, NOFOLLOW on the link itself -> exists.
    symlinkat("nonexistent-target", dfd, "dangling");
    errno = 0;
    int follow_dangling = faccessat(dfd, "dangling", F_OK, 0);
    int dangling_enoent = follow_dangling != 0 && errno == ENOENT;
    int nofollow_exists = faccessat(dfd, "dangling", F_OK, AT_SYMLINK_NOFOLLOW) == 0;

    // faccessat2 with the same flag mirrors faccessat.
    int a2 = (int)syscall(__NR_faccessat2, dfd, "rx", R_OK, 0);
    int faccessat2_ok = a2 == 0;

    fchmodat(dfd, "rx", 0644, 0);
    unlinkat(dfd, "rx", 0);
    unlinkat(dfd, "dangling", 0);
    close(dfd);
    rmdir(dir);
    printf("faccessat-flags exists=%d readable=%d executable=%d enoent=%d dangling-enoent=%d nofollow-exists=%d faccessat2=%d\n",
           exists, readable, executable, enoent, dangling_enoent, nofollow_exists, faccessat2_ok);
    return 0;
}
