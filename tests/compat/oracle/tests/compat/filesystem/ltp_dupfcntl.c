// dup/dup2/dup3/fcntl/read errno + flag semantics — the LTP dup03/dup201/fcntl05/fcntl13/read02 surface,
// distilled to a deterministic self-check. Every line is a fixed string (only booleans/errno-names, never
// raw fds/pids/addresses) so it oracle-diffs hl-vs-native byte-for-byte. A raw dup2(2) syscall is issued
// (glibc dup2 on x86 uses the dup2 syscall; this pins the oldfd==newfd contract that dup3 does NOT share).
//
// The negative cases here are the ones hl historically got wrong because it shares the host descriptor
// table (whose real fd cap is far larger than the guest's emulated RLIMIT_NOFILE) and force-maps guest
// PROT_NONE memory host-writable: dup2 newfd>=cap (EBADF), dup/open past the cap (EMFILE), F_DUPFD floor
// >=cap (EINVAL), F_SETLK bad l_whence / unknown fcntl cmd (EINVAL), and read into a PROT_NONE buffer or a
// directory (EFAULT/EISDIR). To make the fd-cap cases reproducible on BOTH sides (a real host may report a
// huge RLIMIT_NOFILE), we first setrlimit() the soft cap DOWN to a small value, then exhaust against it.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/stat.h>
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
    int w = open("ltp_dupfcntl.tmp", O_RDWR | O_CREAT | O_TRUNC, 0644);
    int fl = fcntl(w, F_GETFL);
    printf("F_GETFL accmode rdwr: %d\n", (fl & O_ACCMODE) == O_RDWR);
    fcntl(w, F_SETFL, fl | O_APPEND | O_NONBLOCK);
    int fl2 = fcntl(w, F_GETFL);
    printf("F_SETFL flags: append=%d nonblock=%d\n", (fl2 & O_APPEND) != 0, (fl2 & O_NONBLOCK) != 0);
    // F_SETFL must NOT change the access mode (RDWR preserved).
    printf("F_SETFL keeps accmode: %d\n", (fl2 & O_ACCMODE) == O_RDWR);

    // F_GETLK on a fresh, unlocked file -> the lock could be placed: l_type becomes F_UNLCK, the other
    // fields (l_whence/l_start/l_len) are left UNCHANGED. (LTP fcntl05.)
    struct flock gl;
    memset(&gl, 0, sizeof gl);
    gl.l_type = F_RDLCK; gl.l_whence = SEEK_CUR; gl.l_start = 0; gl.l_len = 0;
    int glr = fcntl(w, F_GETLK, &gl);
    printf("F_GETLK unlocked: ok=%d unlck=%d whence_kept=%d start0=%d len0=%d\n",
           glr == 0, gl.l_type == F_UNLCK, gl.l_whence == SEEK_CUR, gl.l_start == 0, gl.l_len == 0);

    // F_SETLK with an out-of-range l_whence -> EINVAL (validated before the fd type is consulted). (fcntl13)
    struct flock bw;
    memset(&bw, 0, sizeof bw);
    bw.l_type = F_RDLCK; bw.l_whence = 999; bw.l_start = 0; bw.l_len = 0;
    errno = 0;
    int bwr = fcntl(w, F_SETLK, &bw);
    printf("F_SETLK bad whence: einval=%d\n", bwr < 0 && errno == EINVAL);

    // An unrecognized fcntl command -> EINVAL (NOT forwarded to a diverging host cmd#). (fcntl13 F_BADCMD.)
    errno = 0;
    int bcr = fcntl(w, 999, 0);
    printf("fcntl bad cmd: einval=%d\n", bcr < 0 && errno == EINVAL);

    // fcntl on a closed/invalid fd -> EBADF.
    errno = 0;
    int bad = fcntl(400, F_GETFL);
    printf("fcntl badfd: ret=%d ebadf=%d\n", bad, errno == EBADF);

    // read() error matrix (LTP read02): EBADF on a bad fd, EISDIR on a directory, EFAULT into a PROT_NONE
    // buffer (hl force-maps such pages host-writable, so this only faults if the guest's intent is honoured).
    char rb;
    errno = 0;
    ssize_t re = read(-1, &rb, 1);
    printf("read badfd: ebadf=%d\n", re < 0 && errno == EBADF);

    int dird = open(".", O_RDONLY | O_DIRECTORY);
    errno = 0;
    ssize_t rdd = read(dird, &rb, 1);
    printf("read dir: eisdir=%d\n", rdd < 0 && errno == EISDIR);
    close(dird);

    // fill a readable file, then read it into a PROT_NONE page -> EFAULT.
    if (write(w, "A", 1) == 1) lseek(w, 0, SEEK_SET);
    void *pn = mmap(0, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    errno = 0;
    ssize_t rpn = read(w, pn, 1);
    printf("read PROT_NONE: efault=%d\n", rpn < 0 && errno == EFAULT);
    munmap(pn, 4096);
    close(w);
    unlink("ltp_dupfcntl.tmp");

    // ---- fd-cap (RLIMIT_NOFILE) enforcement: dup2 newfd>=cap -> EBADF, dup/open past the cap -> EMFILE,
    // F_DUPFD floor>=cap -> EINVAL. Lower the soft cap so this reproduces identically on a host with a huge
    // real RLIMIT_NOFILE. Done LAST because it constrains further fd allocation. (LTP dup201/dup03.)
    struct rlimit rl;
    getrlimit(RLIMIT_NOFILE, &rl);
    rl.rlim_cur = 32; // above the handful of fds currently open, below any exhaustion loop
    setrlimit(RLIMIT_NOFILE, &rl);
    int cap = (int)rl.rlim_cur;

    // dup2 with newfd == the (soft) cap -> EBADF (newfd is out of the descriptor range).
    errno = 0;
    int od = raw_dup2(f, cap);
    printf("dup2 newfd>=cap: ebadf=%d\n", od < 0 && errno == EBADF);

    // dup2 with newfd < 0 -> EBADF.
    errno = 0;
    int nd = raw_dup2(f, -1);
    printf("dup2 newfd<0: ebadf=%d\n", nd < 0 && errno == EBADF);

    // F_DUPFD with a floor at/above the cap -> EINVAL.
    errno = 0;
    int hf = fcntl(f, F_DUPFD, cap);
    printf("F_DUPFD floor>=cap: einval=%d\n", hf < 0 && errno == EINVAL);

    // Exhaust the descriptor table with dup(): once the next free fd would reach the cap, EMFILE.
    int got_emfile = 0;
    int held[64];
    int nheld = 0;
    for (int i = 0; i < 60; i++) {
        errno = 0;
        int nf = dup(f);
        if (nf < 0) { got_emfile = (errno == EMFILE); break; }
        if (nheld < (int)(sizeof held / sizeof held[0])) held[nheld++] = nf;
    }
    printf("dup exhaust: emfile=%d\n", got_emfile);
    for (int i = 0; i < nheld; i++) close(held[i]);

    close(f);
    return 0;
}
