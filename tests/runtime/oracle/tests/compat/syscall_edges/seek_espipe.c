// syscall-compat regression: seek/positioned-I/O errno contract on non-seekable fds. lseek on a pipe ->
// ESPIPE; pwrite/pread on a pipe -> ESPIPE; pwrite at a negative offset on a regular file -> EINVAL;
// lseek with a bogus whence -> EINVAL; lseek past EOF on a regular file is allowed (returns the offset).
// Arch-neutral: errnos/booleans only.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(void) {
    int pf[2];
    pipe(pf);
    printf("lseek_pipe_errno=%d\n", lseek(pf[0], 0, SEEK_SET) == -1 ? errno : 0);
    char b[4];
    printf("pread_pipe_errno=%d\n", pread(pf[0], b, 4, 0) == -1 ? errno : 0);
    printf("pwrite_pipe_errno=%d\n", pwrite(pf[1], "x", 1, 0) == -1 ? errno : 0);

    char tmpl[] = "/tmp/seek_XXXXXX";
    int fd = mkstemp(tmpl);
    unlink(tmpl);
    printf("pwrite_negoff_errno=%d\n", pwrite(fd, "x", 1, -1) == -1 ? errno : 0);
    printf("lseek_badwhence_errno=%d\n", lseek(fd, 0, 999) == -1 ? errno : 0);
    // Seeking past EOF is legal and returns the new offset.
    off_t o = lseek(fd, 1000, SEEK_SET);
    printf("lseek_pasteof=%d\n", o == 1000);
    return 0;
}
