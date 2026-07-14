// The *at() family with an explicit dirfd: fchmodat / fchownat / faccessat / fstatat / unlinkat.
// Portable POSIX -> golden verdict on every engine.
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_atdir_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    // create a file relative to dfd
    int fd = openat(dfd, "f", O_CREAT | O_RDWR | O_TRUNC, 0600);
    write(fd, "x", 1);
    close(fd);

    int chm = fchmodat(dfd, "f", 0640, 0) == 0;
    struct stat st;
    fstatat(dfd, "f", &st, 0);
    int mode_ok = (st.st_mode & 0777) == 0640;
    // chown to our own uid (no-op, must succeed for an unprivileged caller)
    int cho = fchownat(dfd, "f", getuid(), getgid(), 0) == 0;
    int acc_r = faccessat(dfd, "f", R_OK | W_OK, 0) == 0;
    int acc_miss = faccessat(dfd, "nope", F_OK, 0) != 0;
    int unl = unlinkat(dfd, "f", 0) == 0;
    int gone = faccessat(dfd, "f", F_OK, 0) != 0;
    close(dfd);
    rmdir(dir);
    printf("atflags chmod=%d mode=%d chown=%d acc=%d accmiss=%d unlink=%d gone=%d\n",
           chm, mode_ok, cho, acc_r, acc_miss, unl, gone);
    return 0;
}
