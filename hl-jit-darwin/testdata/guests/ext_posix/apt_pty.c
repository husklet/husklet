// #411 GUARD: apt/dpkg's StartPtyMagic (and any pty owner) does termios + winsize ioctls on the pty
// MASTER *without ever opening the slave* -- it keeps only the master fd. On macOS isatty(master)==1
// (a master is a tty-class char device) yet every termios/winsize ioctl on it ENOTTYs, so hl's old
// isatty-gated retarget ("if (!isatty(fd)) retarget-to-slave") skipped exactly these masters and the
// guest saw ENOTTY -> apt's "Setting TIOCSWINSZ/TCSANOW for master fd N failed" and the debconf frontend
// fallback. ext_posix/pty did NOT catch this because it open()s the slave first, which flips isatty(master)
// to 0 and accidentally re-armed the retarget. THIS guard opens NO slave (exactly like apt), so it fails
// pre-fix and passes post-fix, matching native Linux where a master accepts termios/winsize directly.
#define _DEFAULT_SOURCE
#define _XOPEN_SOURCE 600
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <unistd.h>

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    if (m < 0) { printf("aptpty openpt=0\n"); return 0; }
    unlockpt(m);
    char *sn = ptsname(m);       // resolve the slave name, but DO NOT open it (apt keeps only the master)
    grantpt(m);

    struct termios t;
    int mget = tcgetattr(m, &t) == 0;                    // TCGETS/TCGETS2 on the master
    int mset = mget && tcsetattr(m, TCSANOW, &t) == 0;   // TCSETS/TCSETS2 on the master (apt TCSANOW)
    struct winsize ws = {40, 120, 0, 0};
    int mswin = ioctl(m, TIOCSWINSZ, &ws) == 0;          // apt's failing ioctl
    struct winsize wg = {0, 0, 0, 0};
    int mgwin = ioctl(m, TIOCGWINSZ, &wg) == 0 && wg.ws_row == 40 && wg.ws_col == 120;
    int mdrain = mget && tcsetattr(m, TCSADRAIN, &t) == 0;
    int mflush = mget && tcsetattr(m, TCSAFLUSH, &t) == 0;

    // apt/htop order: the slave is opened AFTER the master's winsize was set (apt sets it, then the forked
    // child opens the slave and ncurses reads the size from it). The slave must inherit the master's size.
    int s = sn ? open(sn, O_RDWR | O_NOCTTY) : -1;
    struct winsize sg = {0, 0, 0, 0};
    int sgwin = s >= 0 && ioctl(s, TIOCGWINSZ, &sg) == 0 && sg.ws_row == 40 && sg.ws_col == 120;

    printf("aptpty ptsname=%d mget=%d mset=%d mswin=%d mgwin=%d mdrain=%d mflush=%d sgwin=%d\n", sn != NULL,
           mget, mset, mswin, mgwin, mdrain, mflush, sgwin);
    return 0;
}
