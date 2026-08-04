// syscall-compat regression: POSIX advisory record locks via fcntl F_SETLK/F_GETLK on a regular file.
// A write lock is acquired, F_GETLK from the same owner reports the region UNLOCKED (F_UNLCK) because the
// process would not conflict with itself, and unlocking succeeds. F_SETPIPE_SZ/F_GETPIPE_SZ round-trip a
// pipe capacity. Arch-neutral: errnos, F_UNLCK/booleans printed, never raw addresses.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(void) {
    char tmpl[] = "/tmp/fcntl_lock_XXXXXX";
    int fd = mkstemp(tmpl);
    unlink(tmpl);
    ftruncate(fd, 4096);

    struct flock fl = {0};
    fl.l_type = F_WRLCK;
    fl.l_whence = SEEK_SET;
    fl.l_start = 0;
    fl.l_len = 100;
    int r = fcntl(fd, F_SETLK, &fl);
    printf("setlk_ok=%d\n", r == 0);

    struct flock q = {0};
    q.l_type = F_WRLCK;
    q.l_whence = SEEK_SET;
    q.l_start = 0;
    q.l_len = 100;
    fcntl(fd, F_GETLK, &q);
    // Same owner: no conflict, kernel reports F_UNLCK.
    printf("getlk_unlck=%d\n", q.l_type == F_UNLCK);

    fl.l_type = F_UNLCK;
    printf("unlck_ok=%d\n", fcntl(fd, F_SETLK, &fl) == 0);

    int pf[2];
    pipe(pf);
    int got = fcntl(pf[0], F_SETPIPE_SZ, 65536);
    int cur = fcntl(pf[0], F_GETPIPE_SZ);
    // Kernel may round up, but the capacity must be at least what we asked and match the read-back.
    printf("pipesz_ge=%d pipesz_match=%d\n", got >= 65536, got == cur);
    return 0;
}
