// fcntl(2) differential probe: prints arch-stable derived booleans/errnos for
// F_GETFL/F_SETFL masking, F_GETFD/F_SETFD, F_DUPFD(_CLOEXEC), F_GETOWN/SETOWN,
// F_SETSIG/GETSIG, O_ASYNC/SIGIO delivery, F_SETLEASE/GETLEASE, F_NOTIFY.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/eventfd.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t g_sig = 0;
static void onsig(int s) { g_sig = s; }

int main(void) {
    // ---- F_GETFL / F_SETFL masking on a regular file ----
    char path[] = "/tmp/fcntlprobe.XXXXXX";
    int fd = mkstemp(path);
    unlink(path);
    // Reopen with O_DIRECT is fs-dependent; instead toggle via F_SETFL.
    int fl0 = fcntl(fd, F_GETFL);
    printf("file.accmode.rdwr=%d\n", (fl0 & O_ACCMODE) == O_RDWR);

    // set O_NONBLOCK, verify readback; clear, verify gone
    fcntl(fd, F_SETFL, fl0 | O_NONBLOCK);
    printf("file.nonblock.set=%d\n", (fcntl(fd, F_GETFL) & O_NONBLOCK) != 0);
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) & ~O_NONBLOCK);
    printf("file.nonblock.clr=%d\n", (fcntl(fd, F_GETFL) & O_NONBLOCK) == 0);

    // toggle O_APPEND
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_APPEND);
    printf("file.append.set=%d\n", (fcntl(fd, F_GETFL) & O_APPEND) != 0);
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) & ~O_APPEND);
    printf("file.append.clr=%d\n", (fcntl(fd, F_GETFL) & O_APPEND) == 0);

    // O_DIRECT via F_SETFL (tmpfs may reject; report round-trip consistency:
    // whatever F_SETFL accepted must be reflected by F_GETFL)
    int r = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_DIRECT);
    int has_direct = (fcntl(fd, F_GETFL) & O_DIRECT) != 0;
    // consistent = SETFL rc>=0 <=> GETFL now shows it (both fail on tmpfs EINVAL)
    printf("file.direct.consistent=%d\n", (r >= 0) == has_direct);
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) & ~O_DIRECT);

    // access mode / creation flags must NOT change via F_SETFL: try to flip to
    // O_RDONLY + O_SYNC + O_CREAT; kernel silently ignores accmode+creation,
    // only status flags stick. Verify accmode unchanged.
    fcntl(fd, F_SETFL, O_RDONLY | O_CREAT | O_SYNC | O_APPEND);
    int fl2 = fcntl(fd, F_GETFL);
    printf("file.accmode.unchanged=%d\n", (fl2 & O_ACCMODE) == O_RDWR);
    printf("file.creat.notset=%d\n", (fl2 & O_CREAT) == 0);
    // O_SYNC is not changeable via F_SETFL on Linux; must still be clear
    printf("file.sync.notset=%d\n", (fl2 & O_SYNC) == 0);
    fcntl(fd, F_SETFL, O_RDWR); // restore clean

    // ---- F_GETFD / F_SETFD (FD_CLOEXEC) ----
    printf("file.cloexec.default=%d\n", (fcntl(fd, F_GETFD) & FD_CLOEXEC) == 0);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    printf("file.cloexec.set=%d\n", (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0);
    fcntl(fd, F_SETFD, 0);
    printf("file.cloexec.clr=%d\n", (fcntl(fd, F_GETFD) & FD_CLOEXEC) == 0);

    // O_CLOEXEC at open == F_GETFD FD_CLOEXEC set
    int cf = open("/dev/null", O_RDONLY | O_CLOEXEC);
    printf("open.cloexec.getfd=%d\n", (fcntl(cf, F_GETFD) & FD_CLOEXEC) != 0);
    close(cf);

    // ---- F_DUPFD / F_DUPFD_CLOEXEC ----
    int d1 = fcntl(fd, F_DUPFD, 100);
    printf("dupfd.min=%d\n", d1 >= 100);
    printf("dupfd.nocloexec=%d\n", (fcntl(d1, F_GETFD) & FD_CLOEXEC) == 0);
    int d2 = fcntl(fd, F_DUPFD_CLOEXEC, 100);
    printf("dupfd_cloexec.min=%d\n", d2 >= 100);
    printf("dupfd_cloexec.cloexec=%d\n", (fcntl(d2, F_GETFD) & FD_CLOEXEC) != 0);
    // shared offset: dup'd fd shares OFD offset
    lseek(fd, 7, SEEK_SET);
    printf("dupfd.shared_offset=%d\n", lseek(d1, 0, SEEK_CUR) == 7);
    // F_DUPFD negative floor -> EINVAL
    errno = 0; fcntl(fd, F_DUPFD, -1);
    printf("dupfd.neg.einval=%d\n", errno == EINVAL);
    close(d1); close(d2);

    // ---- F_GETOWN / F_SETOWN ----
    errno = 0;
    int so = fcntl(fd, F_SETOWN, getpid());
    int go = fcntl(fd, F_GETOWN);
    printf("setown.ok=%d\n", so == 0);
    printf("getown.roundtrip=%d\n", go == getpid());

    // ---- F_SETSIG / F_GETSIG ----
    printf("getsig.default=%d\n", fcntl(fd, F_GETSIG) == 0);
    fcntl(fd, F_SETSIG, SIGUSR1);
    printf("setsig.roundtrip=%d\n", fcntl(fd, F_GETSIG) == SIGUSR1);
    fcntl(fd, F_SETSIG, 0);

    // ---- O_ASYNC / SIGIO delivery on a socketpair ----
    {
        int sv[2];
        socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
        signal(SIGIO, onsig);
        fcntl(sv[0], F_SETOWN, getpid());
        int sfl = fcntl(sv[0], F_GETFL);
        fcntl(sv[0], F_SETFL, sfl | O_ASYNC);
        g_sig = 0;
        write(sv[1], "x", 1);
        // give async signal a chance without hanging
        for (int i = 0; i < 100 && g_sig == 0; i++) {
            struct timespec ts = {0, 5000000};
            nanosleep(&ts, NULL);
        }
        printf("async.sigio.delivered=%d\n", g_sig == SIGIO);
        // with F_SETSIG custom signal
        signal(SIGUSR2, onsig);
        fcntl(sv[0], F_SETSIG, SIGUSR2);
        g_sig = 0;
        char buf[8]; (void)read(sv[0], buf, sizeof buf); // drain
        write(sv[1], "yy", 2);
        for (int i = 0; i < 100 && g_sig == 0; i++) {
            struct timespec ts = {0, 5000000};
            nanosleep(&ts, NULL);
        }
        printf("async.customsig.delivered=%d\n", g_sig == SIGUSR2);
        close(sv[0]); close(sv[1]);
    }

    // ---- F_SETLEASE / F_GETLEASE ----
    {
        // read lease on our O_RDONLY fd to /dev/null-like: use a fresh regular file
        char lp[] = "/tmp/fcntllease.XXXXXX";
        int lfd = mkstemp(lp);
        unlink(lp);
        errno = 0;
        int sl = fcntl(lfd, F_SETLEASE, F_RDLCK);
        int slerr = errno;
        int gl = fcntl(lfd, F_GETLEASE);
        // On native unprivileged, a read lease on an fd we own & opened RDWR ->
        // EAGAIN (writer = the fd itself). Report the errno class + get result.
        printf("setlease.rc=%d\n", sl == 0 ? 1 : 0);
        printf("setlease.errno_eagain=%d\n", sl < 0 && slerr == EAGAIN);
        printf("getlease.unlck_when_none=%d\n", gl == F_UNLCK);
        close(lfd);
    }

    // ---- F_NOTIFY (dnotify) on a directory ----
    {
        int dfd = open("/tmp", O_RDONLY | O_DIRECTORY);
        errno = 0;
        int nr = fcntl(dfd, F_NOTIFY, DN_CREATE | DN_MULTISHOT);
        printf("notify.rc0=%d\n", nr == 0);
        close(dfd);
    }

    // ---- F_GET_RW_HINT / F_SET_RW_HINT (write life-time hints) ----
    {
        unsigned long long h = ~0ull;
        int gr = fcntl(fd, 1035 /*F_GET_RW_HINT*/, &h);
        printf("get_rw_hint.ok=%d\n", gr == 0);
        // round-trip: set NOT_SET(0) then read it back
        unsigned long long w = 0;
        int sr = fcntl(fd, 1036 /*F_SET_RW_HINT*/, &w);
        unsigned long long r2 = ~0ull;
        int gr2 = fcntl(fd, 1035, &r2);
        printf("rw_hint.roundtrip=%d\n", sr == 0 && gr2 == 0 && r2 == 0);
    }

    // ---- unknown command -> EINVAL ----
    errno = 0;
    fcntl(fd, 999);
    printf("badcmd.einval=%d\n", errno == EINVAL);

    close(fd);
    return 0;
}
