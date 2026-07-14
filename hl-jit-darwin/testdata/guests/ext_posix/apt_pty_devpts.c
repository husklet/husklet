// #411 REGRESSION GUARD (registry-first master detection + devpts round-trip). apt/dpkg StartPtyMagic
// sets termios + winsize on the pty MASTER while NO slave is open, then a forked child opens the slave
// and reads the size back. On macOS a master ENOTTYs every termios/winsize ioctl, so hl must recognize
// the master and answer from its per-master g_ptm_* cache (returning success, exactly like Linux). The
// engine's authoritative signal for "this fd is a master" is its devpts registry (stamped at /dev/ptmx
// open), so this guard opens NO slave before the master ops (the master's isatty()==1 on macOS, which is
// why the pre-#411 isatty gate wrongly skipped the retarget and the guest saw ENOTTY). It then opens the
// slave via the devpts path -- TIOCGPTN to get index N, open("/dev/pts/N") (the musl/openpty route, #280)
// -- and verifies the winsize AND termios the master set earlier propagated to it. All fields must be 1,
// == native Linux; pre-fix, the master ops ENOTTY and every m* field is 0.
#define _DEFAULT_SOURCE
#define _XOPEN_SOURCE 600
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <unistd.h>

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    if (m < 0) { printf("aptptsdev openpt=0\n"); return 0; }
    grantpt(m);
    unlockpt(m);

    // ---- master ops with NO slave open (exactly what apt does) ----
    struct termios t;
    int mget = tcgetattr(m, &t) == 0;                       // TCGETS on the master
    t.c_lflag &= ~(ECHO | ICANON);                          // put it in a distinctive (raw-ish) state
    int mset = mget && tcsetattr(m, TCSANOW, &t) == 0;      // TCSETS on the master (apt's TCSANOW)
    struct winsize ws = {51, 133, 0, 0};
    int mswin = ioctl(m, TIOCSWINSZ, &ws) == 0;             // apt's failing ioctl, pre-fix
    struct winsize wg = {0, 0, 0, 0};
    int mgwin = ioctl(m, TIOCGWINSZ, &wg) == 0 && wg.ws_row == 51 && wg.ws_col == 133;

    // ---- open the slave via the devpts route (musl-style: TIOCGPTN -> /dev/pts/N) ----
    int ptn = -1;
    int gotptn = ioctl(m, TIOCGPTN, &ptn) == 0;
    char sname[64];
    snprintf(sname, sizeof sname, "/dev/pts/%d", ptn);
    int s = gotptn ? open(sname, O_RDWR | O_NOCTTY) : -1;

    // ---- the slave must inherit the size AND termios the master set while it was closed ----
    struct winsize sg = {0, 0, 0, 0};
    int sgwin = s >= 0 && ioctl(s, TIOCGWINSZ, &sg) == 0 && sg.ws_row == 51 && sg.ws_col == 133;
    struct termios st;
    int sterm = s >= 0 && tcgetattr(s, &st) == 0 && !(st.c_lflag & ECHO) && !(st.c_lflag & ICANON);

    printf("aptptsdev mget=%d mset=%d mswin=%d mgwin=%d ptn=%d sopen=%d sgwin=%d sterm=%d\n",
           mget, mset, mswin, mgwin, gotptn && ptn >= 0, s >= 0, sgwin, sterm);
    return 0;
}
