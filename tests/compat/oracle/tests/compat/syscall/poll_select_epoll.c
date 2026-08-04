// Permanent poll/select/pselect6/ppoll/epoll_create1 guard for the hl matrix. Mirrors the assertions the
// LTP binaries poll02, pselect01, select01, select02, epoll_create1_01 make, plus the completeness edges
// the task calls out (nfds 0/negative -> EINVAL, timeout==0 immediate return, EFAULT on a bad fd_set /
// timeout / pollfd pointer, EBADF for a closed fd in the set, POLLNVAL for a bad fd). Output is a stable,
// fd-number-free transcript so it can be diffed byte-exact against the native (aarch64) / qemu-user (x86)
// oracle. Every reported token is deterministic. Owner: poll/epoll guard.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>
#include <stdlib.h>
#include <sys/select.h>
#include <sys/time.h>
#include <sys/stat.h>
#include <poll.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/syscall.h>

#define T(name, cond) printf("%-26s %s\n", name, (cond) ? "ok" : "FAIL")
static void *BAD;

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    BAD = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    char freg[64], ffifo[64];
    snprintf(freg, sizeof freg, "/tmp/psguard_reg_%d", (int)getpid());
    snprintf(ffifo, sizeof ffifo, "/tmp/psguard_fifo_%d", (int)getpid());

    /* ---- select(2): the three select01 cases (regular file, pipe, FIFO) ---- */
    /* regular file: always ready for read; returns 1, readfds bit set */
    {
        int fd = open(freg, O_CREAT | O_RDWR, 0600);
        fd_set r; FD_ZERO(&r); FD_SET(fd, &r);
        struct timeval to = {0, 100000};
        int ret = select(fd + 1, &r, NULL, NULL, &to);
        T("selreg ret1", ret == 1);
        T("selreg readset", FD_ISSET(fd, &r));
        close(fd); unlink(freg);
    }
    /* system pipe: write a byte -> read end readable AND write end writable -> returns 2 */
    {
        int p[2]; if (pipe(p)) return 2;
        char b = 'x'; if (write(p[1], &b, 1) != 1) return 2;
        fd_set r, w; FD_ZERO(&r); FD_ZERO(&w); FD_SET(p[0], &r); FD_SET(p[1], &w);
        int nfds = (p[0] > p[1] ? p[0] : p[1]) + 1;
        struct timeval to = {0, 100000};
        int ret = select(nfds, &r, &w, NULL, &to);
        T("selpipe ret2", ret == 2);
        T("selpipe readset", FD_ISSET(p[0], &r));
        T("selpipe writeset", FD_ISSET(p[1], &w));
        close(p[0]); close(p[1]);
    }
    /* named pipe (FIFO) opened O_RDWR: write a byte -> read+write ready on same fd -> returns 2 */
    {
        unlink(ffifo);
        if (mkfifo(ffifo, 0600)) return 2;
        int fd = open(ffifo, O_RDWR);
        char b = 'y'; if (write(fd, &b, 1) != 1) return 2;
        fd_set r, w; FD_ZERO(&r); FD_ZERO(&w); FD_SET(fd, &r); FD_SET(fd, &w);
        struct timeval to = {0, 100000};
        int ret = select(fd + 1, &r, &w, NULL, &to);
        T("selfifo ret2", ret == 2);
        T("selfifo readset", FD_ISSET(fd, &r));
        T("selfifo writeset", FD_ISSET(fd, &w));
        close(fd); unlink(ffifo);
    }
    /* select02/pselect01: empty read set + finite timeout -> returns 0 (timed out) */
    {
        int p[2]; if (pipe(p)) return 2;
        fd_set r; FD_ZERO(&r); FD_SET(p[0], &r);
        struct timeval to = {0, 1000};
        errno = 0;
        int ret = select(p[0] + 1, &r, NULL, NULL, &to);
        T("seltimeout ret0", ret == 0);
        /* timeout==0 -> immediate return 0 */
        fd_set r2; FD_ZERO(&r2); FD_SET(p[0], &r2);
        struct timeval z = {0, 0};
        T("selpoll0 ret0", select(p[0] + 1, &r2, NULL, NULL, &z) == 0);
        close(p[0]); close(p[1]);
    }
    /* nfds==0: pure sleep, returns 0 (pselect01 uses nfds 0) */
    {
        fd_set r; FD_ZERO(&r);
        struct timeval z = {0, 0};
        T("selnfds0 ret0", select(0, &r, NULL, NULL, &z) == 0);
    }
    /* negative nfds -> EINVAL */
    {
        fd_set r; FD_ZERO(&r);
        struct timeval z = {0, 0};
        errno = 0;
        int ret = select(-1, &r, NULL, NULL, &z);
        T("selneg einval", ret == -1 && errno == EINVAL);
    }
    /* EBADF: a closed fd present in the set */
    {
        int p[2]; if (pipe(p)) return 2;
        int cfd = dup(p[0]); close(cfd);
        fd_set r; FD_ZERO(&r); FD_SET(cfd, &r);
        struct timeval z = {0, 0};
        errno = 0;
        int ret = select(cfd + 1, &r, NULL, NULL, &z);
        T("selbadf ebadf", ret == -1 && errno == EBADF);
        close(p[0]); close(p[1]);
    }
    /* EFAULT: bad fd_set pointer */
    {
        struct timeval z = {0, 0};
        errno = 0;
        int ret = select(64, (fd_set *)BAD, NULL, NULL, &z);
        T("selbadfdset efault", ret == -1 && errno == EFAULT);
    }
    /* ---- raw pselect6: bad timeout pointer -> EFAULT (libc select derefs a bad timeval itself) ---- */
#ifdef __NR_pselect6
    {
        fd_set r; FD_ZERO(&r);
        errno = 0;
        long ret = syscall(__NR_pselect6, 0, &r, NULL, NULL, (void *)BAD, NULL);
        T("pselbadtmo efault", ret == -1 && errno == EFAULT);
        /* NULL timeout with nfds==0 and no fds would block forever; skip. valid all-zero timeout: */
        struct timespec ts = {0, 0};
        errno = 0;
        T("psel6 tmo0 ret0", syscall(__NR_pselect6, 0, &r, NULL, NULL, &ts, NULL) == 0);
    }
#endif
    /* ---- poll(2)/ppoll ---- */
    {
        int p[2]; if (pipe(p)) return 2;
        struct pollfd pf = {.fd = p[0], .events = POLLIN};
        errno = 0;
        T("poll empty tmo0", poll(&pf, 1, 0) == 0);          /* nothing ready, immediate */
        struct pollfd pw = {.fd = p[1], .events = POLLOUT};
        int rw = poll(&pw, 1, 0);
        T("poll writable", rw == 1 && (pw.revents & POLLOUT));
        struct pollfd pb = {.fd = 999, .events = POLLIN};
        int rb = poll(&pb, 1, 0);
        T("poll badfd nval", rb == 1 && (pb.revents & POLLNVAL));
        struct pollfd pn = {.fd = -1, .events = POLLIN};
        int rn = poll(&pn, 1, 0);
        T("poll negfd ignored", rn == 0 && pn.revents == 0);
        errno = 0;
        T("poll badptr efault", poll((struct pollfd *)BAD, 1, 0) == -1 && errno == EFAULT);
        close(p[0]); close(p[1]);
    }

    /* ---- epoll_create1: CLOEXEC round-trip + bad-flag EINVAL (epoll_create1_01) ---- */
    {
        int e0 = epoll_create1(0);
        T("epc1 nocloexec", e0 >= 0 && (fcntl(e0, F_GETFD) & FD_CLOEXEC) == 0);
        if (e0 >= 0) close(e0);
        int e1 = epoll_create1(EPOLL_CLOEXEC);
        T("epc1 cloexec", e1 >= 0 && (fcntl(e1, F_GETFD) & FD_CLOEXEC) == FD_CLOEXEC);
        if (e1 >= 0) close(e1);
        errno = 0;
        int eb = epoll_create1(0x12345678);
        T("epc1 badflag einval", eb == -1 && errno == EINVAL);
        if (eb >= 0) close(eb);
    }

    printf("POLLSEL_DONE\n");
    return 0;
}
