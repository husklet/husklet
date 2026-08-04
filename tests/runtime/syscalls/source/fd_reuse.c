// #224 fd-close-table guard: every emulated fd kind (inotify / timerfd / eventfd / memfd) MUST shed its
// per-fd emulation table on close(2), so a RECYCLED fd number is never misrouted to the dead emulation.
// Each probe creates an emulated fd as the lowest free descriptor N, closes it, reopens a PLAIN file which
// the kernel hands back the SAME number N, and asserts N now behaves as an ordinary file -- not the ghost
// it was. That is exactly real-Linux behaviour (a reused fd is just a file), so the line is oracle-diffable.
// The historical bug: fd_reset_emul cleared g_inotify_owner but NOT g_inotify[fd], so a recycled inotify
// instance number still routed read() into the inotify drain (empty/EAGAIN) instead of reading the file.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/eventfd.h>
#include <sys/timerfd.h>
#include <unistd.h>

#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002U
#endif
#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#define F_GET_SEALS 1034
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
#endif

static const char *DATA = "/tmp/hl_fdreuse_data";
static const char *WDIR = "/tmp/hl_fdreuse_dir";
#define WANT "PLAINDATA"
#define WLEN 9

// Close emulated fd N, reopen DATA (kernel returns the lowest free fd == N), assert the number was reused
// AND that read() now returns the plain file's bytes -- not inotify events / a timerfd count / an eventfd
// counter. O_NONBLOCK is belt-and-suspenders: a regular file ignores it, but a *stale* timer/inotify read
// path would then return EAGAIN rather than block if the fix were ever reverted.
static int read_reuse(int n) {
    close(n);
    int f = open(DATA, O_RDONLY | O_NONBLOCK);
    if (f < 0) return 0;
    char buf[64];
    int r = (int)read(f, buf, sizeof buf);
    int ok = (f == n) && r == WLEN && memcmp(buf, WANT, WLEN) == 0;
    close(f);
    return ok;
}

int main(void) {
    int d = open(DATA, O_CREAT | O_TRUNC | O_WRONLY, 0644);
    if (d >= 0) {
        if (write(d, WANT, WLEN) != WLEN) { /* ignore */ }
        close(d);
    }
    mkdir(WDIR, 0755);

    // inotify instance (the #224 bug) -- a DIRECTORY watch also exercises the watch-fd/snapshot teardown.
    int in = inotify_init1(0);
    inotify_add_watch(in, WDIR, IN_CREATE | IN_DELETE);
    int inotify_ok = read_reuse(in);

    // timerfd -- a 100s deadline means a stale read would see no expiration.
    int tf = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec its = {{0, 0}, {100, 0}};
    timerfd_settime(tf, 0, &its, NULL);
    int timerfd_ok = read_reuse(tf);

    // eventfd -- a non-zero initial counter would make a stale read return 8 bytes of counter, not the file.
    int ef = eventfd(7, 0);
    int eventfd_ok = read_reuse(ef);

    // memfd + F_SEAL_WRITE -- the reused PLAIN fd must NOT report the stale F_SEAL_WRITE. Real Linux on a
    // sealing-capable fs (tmpfs) returns 0 seals; dd returns EINVAL (-1); either proves "not stale". A stale
    // g_memfd_seal[n] would surface F_SEAL_WRITE (0x8) on the recycled number -> caught here on both arches.
    int mf = memfd_create("dd224", MFD_ALLOW_SEALING);
    int memfd_ok = 0;
    if (mf >= 0) {
        if (ftruncate(mf, 8) == 0) { /* ok */ }
        fcntl(mf, F_ADD_SEALS, F_SEAL_WRITE);
        int n = mf;
        close(n);
        int f = open(DATA, O_RDONLY);
        int seals = fcntl(f, F_GET_SEALS);
        memfd_ok = (f == n) && !(seals > 0 && (seals & F_SEAL_WRITE));
        close(f);
    }

    printf("fdreuse inotify=%d timerfd=%d eventfd=%d memfd=%d\n",
           inotify_ok, timerfd_ok, eventfd_ok, memfd_ok);
    unlink(DATA);
    rmdir(WDIR);
    return 0;
}
