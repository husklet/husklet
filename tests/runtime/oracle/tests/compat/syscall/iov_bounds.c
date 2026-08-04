// Scatter/gather iovcnt boundary for readv/writev/preadv/pwritev. Linux (fs/read_write.c) accepts
// nr_segs in [0, UIO_MAXIOV] and rejects anything above (or negative, which arrives as a huge
// unsigned long) with EINVAL. nr_segs == 0 -- and, critically, any nr_segs whose segments are ALL
// zero-length -- transfers nothing and returns 0. BSD/macOS instead reject iovcnt 0 with EINVAL, so
// an engine that forwards a collapsed (all-empty) vector straight to the host writev(2) diverges
// from Linux at exactly this boundary. This case pins every rung of the ladder.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>

#define MAXIOV 1024

static struct iovec iov[MAXIOV + 1];
static char sink[MAXIOV + 1];

static void set_empty(int n) {
    for (int i = 0; i < n; i++) {
        iov[i].iov_base = sink;
        iov[i].iov_len = 0;
    }
}

static void report(const char *what, int n, ssize_t rc) {
    printf("%s %d rc=%zd errno=%d\n", what, n, rc, rc < 0 ? errno : 0);
}

int main(void) {
    static const int counts[] = {0, 1, 1023, 1024, 1025, -1};
    int p[2];
    if (pipe(p) != 0) return 1;

    char path[] = "/tmp/hl-iov-bounds-XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 1;
    if (ftruncate(fd, 4096) != 0) return 1;

    // Empty segments (or an empty vector) move no bytes: Linux returns 0 up to and including
    // UIO_MAXIOV, EINVAL above it and for a negative count.
    for (unsigned k = 0; k < sizeof(counts) / sizeof(counts[0]); k++) {
        int n = counts[k];
        set_empty(n > 0 && n <= MAXIOV + 1 ? n : 0);
        errno = 0;
        report("writev empty", n, writev(p[1], iov, n));
        errno = 0;
        report("readv empty", n, readv(p[0], iov, n));
        errno = 0;
        report("pwritev empty", n, pwritev(fd, iov, n, 0));
        errno = 0;
        report("preadv empty", n, preadv(fd, iov, n, 0));
    }

    // A full UIO_MAXIOV vector carrying one byte per segment must actually transfer, and the
    // one-past boundary must still fail before any byte moves.
    static char payload[MAXIOV + 1];
    memset(payload, 'a', sizeof(payload));
    for (int i = 0; i <= MAXIOV; i++) {
        iov[i].iov_base = &payload[i];
        iov[i].iov_len = 1;
    }
    errno = 0;
    report("pwritev full", MAXIOV, pwritev(fd, iov, MAXIOV, 0));
    errno = 0;
    report("pwritev over", MAXIOV + 1, pwritev(fd, iov, MAXIOV + 1, 0));
    static char back[MAXIOV];
    for (int i = 0; i < MAXIOV; i++) {
        iov[i].iov_base = &back[i];
        iov[i].iov_len = 1;
    }
    errno = 0;
    report("preadv full", MAXIOV, preadv(fd, iov, MAXIOV, 0));
    printf("payload intact=%d\n", memcmp(back, payload, MAXIOV) == 0);

    // A vector mixing empty and non-empty segments at the boundary still transfers the non-empty
    // tail; only the count matters for EINVAL.
    set_empty(MAXIOV);
    iov[MAXIOV - 1].iov_base = payload;
    iov[MAXIOV - 1].iov_len = 3;
    errno = 0;
    report("writev mixed", MAXIOV, writev(p[1], iov, MAXIOV));

    // A bad descriptor outranks the count check, on either side of the boundary (Linux resolves
    // the fd before importing the vector).
    set_empty(MAXIOV);
    errno = 0;
    report("writev badfd", MAXIOV, writev(-1, iov, MAXIOV));
    errno = 0;
    report("writev badfd over", MAXIOV + 1, writev(-1, iov, MAXIOV + 1));

    close(p[0]);
    close(p[1]);
    close(fd);
    unlink(path);
    return 0;
}
