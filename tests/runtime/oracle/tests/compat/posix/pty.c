// pty: exercise the termios + winsize ioctl family on BOTH ends of a pseudo-terminal.
// Reproduces the apt/dpkg (#306), htop TUI (#308) and node raw-mode-backspace symptoms:
//   - master fd: tcgetattr/tcsetattr(TCSANOW) + TIOCSWINSZ/TIOCGWINSZ must succeed (not ENOTTY)
//   - a TIOCSWINSZ on the master is visible via TIOCGWINSZ on the slave (what ncurses reads)
//   - raw mode on the slave (ICANON/ECHO/OPOST cleared, c_cc[VERASE]/VMIN set) round-trips exactly.
// aarch64 glibc drives these via TCGETS2/TCSETS2 (struct termios2 w/ c_ispeed/c_ospeed); x86_64 via TCGETS/TCSETS.
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
    if (m < 0) { printf("pty openpt=0\n"); return 0; }
    grantpt(m);
    unlockpt(m);
    char *sn = ptsname(m);
    int s = sn ? open(sn, O_RDWR | O_NOCTTY) : -1;

    struct termios mt;
    int mget = tcgetattr(m, &mt) == 0;                 // termios on the MASTER (apt: fails w/ ENOTTY)
    int mset = tcsetattr(m, TCSANOW, &mt) == 0;        // apt "Setting in Start via TCSANOW ... failed"

    struct winsize ws = {40, 120, 0, 0};
    int mswin = ioctl(m, TIOCSWINSZ, &ws) == 0;        // apt "Setting TIOCSWINSZ for master fd ... failed"
    struct winsize wg = {0, 0, 0, 0};
    int mgwin = ioctl(m, TIOCGWINSZ, &wg) == 0 && wg.ws_row == 40 && wg.ws_col == 120;
    struct winsize sg = {0, 0, 0, 0};                  // htop/ncurses read the size from the slave
    int sgwin = s >= 0 && ioctl(s, TIOCGWINSZ, &sg) == 0 && sg.ws_row == 40 && sg.ws_col == 120;

    // ALL tcsetattr actions must succeed on the MASTER too (apt uses TCSANOW; drain/flush must not ENOTTY).
    int mdrain = mget && tcsetattr(m, TCSADRAIN, &mt) == 0;
    int mflush = mget && tcsetattr(m, TCSAFLUSH, &mt) == 0;

    // node/readline raw mode on the slave (cfmakeraw-equivalent), then verify the round-trip incl. c_cc.
    int sraw = 0, rawrt = 0;
    if (s >= 0) {
        struct termios t;
        if (tcgetattr(s, &t) == 0) {
            t.c_lflag &= ~(ICANON | ECHO | ISIG | IEXTEN);
            t.c_iflag &= ~(IXON | ICRNL | BRKINT | INPCK | ISTRIP);
            t.c_oflag &= ~OPOST;
            t.c_cc[VMIN] = 1;
            t.c_cc[VTIME] = 0;
            t.c_cc[VERASE] = 0x7f;
            t.c_cc[VINTR] = 3; // ^C
            sraw = tcsetattr(s, TCSANOW, &t) == 0;
            struct termios r;
            if (tcgetattr(s, &r) == 0)
                rawrt = !(r.c_lflag & ICANON) && !(r.c_lflag & ECHO) && !(r.c_oflag & OPOST) &&
                        r.c_cc[VERASE] == 0x7f && r.c_cc[VINTR] == 3 && r.c_cc[VMIN] == 1;
        }
    }

    printf("pty mget=%d mset=%d mdrain=%d mflush=%d mswin=%d mgwin=%d sgwin=%d sraw=%d rawrt=%d\n", mget,
           mset, mdrain, mflush, mswin, mgwin, sgwin, sraw, rawrt);
    return 0;
}
