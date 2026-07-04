// dup/dup2/dup3/fcntl flag semantics — the LTP dup03/dup201/fcntl05/fcntl13 surface, distilled to a
// deterministic self-check. Every line is a fixed string (no fds/pids/addresses printed raw) so it can be
// oracle-diffed dd-vs-native on BOTH arches. A raw dup2(2) syscall is issued (glibc dup2 on x86 uses the
// dup2 syscall; this pins the oldfd==newfd contract that dup3 does NOT share).
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

// raw dup2 via the arch syscall where it exists; fall back to libc dup2 on arches without SYS_dup2.
static int raw_dup2(int oldfd, int newfd) {
#ifdef SYS_dup2
    return (int)syscall(SYS_dup2, oldfd, newfd); // glibc sets errno + returns -1 on error
#else
    return dup2(oldfd, newfd);
#endif
}

int main(void) {
    int f = open("/dev/null", O_RDONLY);

    // dup2(oldfd==newfd) with a VALID oldfd -> returns newfd, does NOT close it.
    int r1 = raw_dup2(f, f);
    printf("dup2 same fd: ret==newfd=%d still_open=%d\n", r1 == f, fcntl(f, F_GETFD) >= 0);

    // dup2(oldfd==newfd) with an INVALID oldfd -> EBADF (does not "succeed").
    errno = 0;
    int r2 = raw_dup2(400, 400);
    printf("dup2 same bad fd: ret=%d errno=%s\n", r2, r2 < 0 ? strerror(errno) : "ok");

    // dup2 over an already-open fd -> closes the target, both share the description.
    int a = open("/dev/zero", O_RDONLY);
    int b = open("/dev/null", O_RDONLY);
    int r3 = raw_dup2(a, b);
    char zb = 1;
    read(b, &zb, 1); // b now aliases /dev/zero -> reads a 0 byte
    printf("dup2 over open: ret==b=%d reads_zero=%d\n", r3 == b, zb == 0);
    close(a); close(b);

    // dup() clears FD_CLOEXEC on the copy even if the source had it.
    fcntl(f, F_SETFD, FD_CLOEXEC);
    int d = dup(f);
    printf("dup clears cloexec: %d\n", (fcntl(d, F_GETFD) & FD_CLOEXEC) == 0);
    close(d);
    fcntl(f, F_SETFD, 0);

    // F_DUPFD: lowest free fd >= floor; the copy does NOT get FD_CLOEXEC.
    int df = fcntl(f, F_DUPFD, 100);
    printf("F_DUPFD floor: ge100=%d cloexec=%d\n", df >= 100, (fcntl(df, F_GETFD) & FD_CLOEXEC) != 0);
    close(df);

    // F_DUPFD_CLOEXEC: same floor rule, but the copy DOES get FD_CLOEXEC.
    int dc = fcntl(f, F_DUPFD_CLOEXEC, 100);
    printf("F_DUPFD_CLOEXEC: ge100=%d cloexec=%d\n", dc >= 100, (fcntl(dc, F_GETFD) & FD_CLOEXEC) != 0);
    close(dc);

    // F_GETFD/F_SETFD FD_CLOEXEC round-trip.
    fcntl(f, F_SETFD, FD_CLOEXEC);
    int g1 = (fcntl(f, F_GETFD) & FD_CLOEXEC) != 0;
    fcntl(f, F_SETFD, 0);
    int g2 = (fcntl(f, F_GETFD) & FD_CLOEXEC) != 0;
    printf("F_SETFD roundtrip: set=%d clr=%d\n", g1, g2 == 0);

    // F_GETFL access mode + F_SETFL status flags (O_APPEND/O_NONBLOCK) on a writable fd.
    int w = open("/tmp/ltp_dupfcntl.tmp", O_RDWR | O_CREAT | O_TRUNC, 0644);
    int fl = fcntl(w, F_GETFL);
    printf("F_GETFL accmode rdwr: %d\n", (fl & O_ACCMODE) == O_RDWR);
    fcntl(w, F_SETFL, fl | O_APPEND | O_NONBLOCK);
    int fl2 = fcntl(w, F_GETFL);
    printf("F_SETFL flags: append=%d nonblock=%d\n", (fl2 & O_APPEND) != 0, (fl2 & O_NONBLOCK) != 0);
    // F_SETFL must NOT change the access mode (RDWR preserved).
    printf("F_SETFL keeps accmode: %d\n", (fl2 & O_ACCMODE) == O_RDWR);
    close(w);
    unlink("/tmp/ltp_dupfcntl.tmp");

    // fcntl on a closed/invalid fd -> EBADF.
    errno = 0;
    int bad = fcntl(400, F_GETFL);
    printf("fcntl badfd: ret=%d ebadf=%d\n", bad, errno == EBADF);

    close(f);
    return 0;
}
