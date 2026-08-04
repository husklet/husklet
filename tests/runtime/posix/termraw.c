// termios on a PTY: cfmakeraw, VMIN/VTIME, speed set, tcflush/tcdrain/tcsendbreak succeed and round-trip.
#define _GNU_SOURCE
#include <termios.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    if (m < 0) { printf("termraw openpt=0\n"); return 0; }
    grantpt(m);
    unlockpt(m);
    char *sn = ptsname(m);
    int s = sn ? open(sn, O_RDWR | O_NOCTTY) : -1;
    if (s < 0) { printf("termraw slave=0\n"); return 0; }

    struct termios t;
    tcgetattr(s, &t);
    cfmakeraw(&t);
    t.c_cc[VMIN] = 3;
    t.c_cc[VTIME] = 7;
    cfsetispeed(&t, B9600);
    cfsetospeed(&t, B115200);
    int set = tcsetattr(s, TCSANOW, &t) == 0;

    struct termios r;
    tcgetattr(s, &r);
    int raw_ok = !(r.c_lflag & (ICANON | ECHO | ISIG | IEXTEN)) &&
                 !(r.c_oflag & OPOST) &&
                 !(r.c_iflag & (IXON | ICRNL | BRKINT)) &&
                 r.c_cc[VMIN] == 3 && r.c_cc[VTIME] == 7;
    int speed_ok = cfgetispeed(&r) == B9600 && cfgetospeed(&r) == B115200;

    int flush_ok = tcflush(s, TCIOFLUSH) == 0;
    int drain_ok = tcdrain(s) == 0;
    int brk_ok = tcsendbreak(m, 0) == 0;

    close(s);
    close(m);
    printf("termraw set=%d raw=%d speed=%d flush=%d drain=%d brk=%d\n",
           set, raw_ok, speed_ok, flush_ok, drain_ok, brk_ok);
    return 0;
}
