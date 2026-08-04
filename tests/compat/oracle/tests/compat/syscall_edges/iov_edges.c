// syscall-compat regression: scatter/gather I/O boundary errnos. writev with iovcnt > IOV_MAX(1024) ->
// EINVAL; writev with a negative iovcnt -> EINVAL; writev with an unmapped iov_base -> EFAULT; readv on a
// bad fd -> EBADF; preadv with a negative offset on a seekable file -> EINVAL; a normal writev/readv
// round-trips the bytes. Arch-neutral. Note: /tmp is RAM(memf)-backed in the engine, so these exercise the
// scatter/gather validation on the RAM-backed-file path (iovcnt bound + per-segment EFAULT), not just host.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/uio.h>
#include <unistd.h>

int main(void) {
    char tmpl[] = "/tmp/iov_XXXXXX";
    int fd = mkstemp(tmpl);
    unlink(tmpl);

    struct iovec iov[2];
    iov[0].iov_base = "ab";
    iov[0].iov_len = 2;
    iov[1].iov_base = "cd";
    iov[1].iov_len = 2;
    ssize_t w = writev(fd, iov, 2);
    printf("writev_bytes=%zd\n", w);

    // iovcnt above IOV_MAX -> EINVAL.
    printf("writev_toobig_errno=%d\n", writev(fd, iov, 2000) == -1 ? errno : 0);
    // negative iovcnt -> EINVAL.
    printf("writev_neg_errno=%d\n", writev(fd, iov, -1) == -1 ? errno : 0);
    // an unmapped iov_base -> EFAULT (the vector count is valid, so the kernel faults on the buffer).
    struct iovec badbase = {(void *)0x1, 8};
    printf("writev_badbase_errno=%d\n", writev(fd, &badbase, 1) == -1 ? errno : 0);
    // readv on a bad fd -> EBADF.
    printf("readv_badfd_errno=%d\n", readv(4096, iov, 2) == -1 ? errno : 0);
    // preadv with a negative offset -> EINVAL.
    char rb[4];
    struct iovec ri = {rb, sizeof(rb)};
    printf("preadv_negoff_errno=%d\n", preadv(fd, &ri, 1, -2) == -1 ? errno : 0);

    // A valid preadv at offset 0 reads the bytes back.
    ssize_t r = preadv(fd, &ri, 1, 0);
    printf("preadv_ok=%d first=%c\n", r == 4, rb[0]);
    return 0;
}
