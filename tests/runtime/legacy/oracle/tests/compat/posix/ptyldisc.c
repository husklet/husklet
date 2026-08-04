// line-discipline data path through a pty master/slave pair (deterministic, single process).
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <errno.h>

static int mk(int *mp, int *sp) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    if (m < 0) return -1;
    grantpt(m); unlockpt(m);
    char *sn = ptsname(m);
    int s = sn ? open(sn, O_RDWR | O_NOCTTY) : -1;
    if (s < 0) return -1;
    *mp = m; *sp = s;
    return 0;
}

// drain readable bytes from fd into buf up to cap, nonblocking; return count.
static int drain(int fd, char *buf, int cap) {
    int fl = fcntl(fd, F_GETFL);
    fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    int total = 0;
    for (int i = 0; i < 200 && total < cap; i++) {
        ssize_t n = read(fd, buf + total, cap - total);
        if (n > 0) total += n;
        else break;
    }
    fcntl(fd, F_SETFL, fl);
    return total;
}

int main(void) {
    int m, s;
    if (mk(&m, &s) < 0) { printf("ldisc setup=0\n"); return 0; }

    // 1. RAW mode: write master -> slave reads byte-exact, no echo back to master, no translation.
    struct termios t;
    tcgetattr(s, &t);
    cfmakeraw(&t);
    t.c_cc[VMIN] = 0; t.c_cc[VTIME] = 0;
    tcsetattr(s, TCSANOW, &t);
    tcflush(s, TCIOFLUSH);
    write(m, "ab\r\n", 4);            // master input -> slave
    char sb[64] = {0};
    int sn = drain(s, sb, sizeof sb - 1);
    // raw: no ICRNL, so \r stays \r. no echo.
    int raw_slave_exact = (sn == 4 && memcmp(sb, "ab\r\n", 4) == 0);
    char mb[64] = {0};
    int mecho = drain(m, mb, sizeof mb - 1);   // raw+noecho: nothing echoed to master
    int raw_no_echo = (mecho == 0);

    // 2. slave->master OPOST: raw (OPOST off) => \n passes through unchanged.
    write(s, "x\n", 2);
    char o1[64] = {0};
    int n1 = drain(m, o1, sizeof o1 - 1);
    int raw_out_exact = (n1 == 2 && memcmp(o1, "x\n", 2) == 0);

    // 3. OPOST + ONLCR: slave writes "\n" -> master reads "\r\n".
    tcgetattr(s, &t);
    t.c_oflag |= (OPOST | ONLCR);
    tcsetattr(s, TCSANOW, &t);
    tcflush(m, TCIOFLUSH);
    write(s, "y\n", 2);
    char o2[64] = {0};
    int n2 = drain(m, o2, sizeof o2 - 1);
    int onlcr = (n2 == 3 && memcmp(o2, "y\r\n", 3) == 0);

    // 4. ICRNL: master sends "\r" -> slave reads "\n" (input CR->NL translation), raw off.
    tcgetattr(s, &t);
    t.c_lflag &= ~(ICANON | ECHO | ISIG);
    t.c_iflag = ICRNL;   // only ICRNL on the input side
    t.c_oflag = 0;
    tcsetattr(s, TCSANOW, &t);
    tcflush(s, TCIOFLUSH);
    write(m, "\r", 1);
    char o3[8] = {0};
    int n3 = drain(s, o3, sizeof o3 - 1);
    int icrnl = (n3 == 1 && o3[0] == '\n');

    // 5. ECHO: input to slave is echoed back to master. noicanon so bytes flow immediately.
    tcgetattr(s, &t);
    t.c_lflag = ECHO;            // echo on, canon off
    t.c_iflag = 0; t.c_oflag = 0;
    tcsetattr(s, TCSANOW, &t);
    tcflush(m, TCIOFLUSH); tcflush(s, TCIOFLUSH);
    write(m, "Z", 1);
    char e1[16] = {0};
    // slave must read the byte AND master must see the echo
    char es[8] = {0};
    drain(s, es, sizeof es - 1);
    int necho = drain(m, e1, sizeof e1 - 1);
    int echo_on = (necho >= 1 && e1[0] == 'Z');

    // 6. FIONREAD on slave: pending input bytes count.
    tcgetattr(s, &t);
    cfmakeraw(&t); t.c_cc[VMIN] = 0; t.c_cc[VTIME] = 0;
    tcsetattr(s, TCSANOW, &t);
    tcflush(s, TCIOFLUSH); tcflush(m, TCIOFLUSH);
    write(m, "12345", 5);
    usleep(20000);
    int avail = -1;
    int fret = ioctl(s, FIONREAD, &avail);
    int fionread_ok = (fret == 0 && avail == 5);

    printf("ldisc raw_slave=%d raw_noecho=%d raw_out=%d onlcr=%d icrnl=%d echo=%d fionread=%d(av=%d)\n",
           raw_slave_exact, raw_no_echo, raw_out_exact, onlcr, icrnl, echo_on, fionread_ok, avail);
    close(s); close(m);
    return 0;
}
