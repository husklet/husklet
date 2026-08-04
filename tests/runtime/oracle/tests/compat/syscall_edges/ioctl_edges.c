// syscall-compat regression: ioctl dispatch by fd type. FIONREAD on a pipe reports the buffered byte count;
// FIONBIO toggles non-blocking (a drained non-blocking pipe read -> EAGAIN); TIOCGWINSZ on a non-tty ->
// ENOTTY; FIONREAD on a bad fd -> EBADF. Arch-neutral: errnos/values printed.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <unistd.h>

int main(void) {
    int pf[2];
    pipe(pf);
    write(pf[1], "hello", 5);
    int avail = -1;
    ioctl(pf[0], FIONREAD, &avail);
    printf("fionread=%d\n", avail);

    // FIONBIO sets non-blocking; after draining, a read -> EAGAIN.
    int on = 1;
    ioctl(pf[0], FIONBIO, &on);
    char buf[8];
    read(pf[0], buf, 5);
    printf("nonblock_drained_errno=%d\n", read(pf[0], buf, 5) == -1 ? errno : 0);

    // TIOCGWINSZ on a pipe (non-tty) -> ENOTTY.
    struct winsize ws;
    printf("winsz_nontty_errno=%d\n", ioctl(pf[0], TIOCGWINSZ, &ws) == -1 ? errno : 0);

    // FIONREAD on a bad fd -> EBADF.
    printf("fionread_badfd_errno=%d\n", ioctl(4096, FIONREAD, &avail) == -1 ? errno : 0);
    return 0;
}
