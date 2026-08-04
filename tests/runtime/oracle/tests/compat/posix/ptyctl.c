// misc pty control: TIOCPKT packet mode, TIOCGSID/tcgetsid, TIOCEXCL, TIOCOUTQ, /dev/tty reach.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>
#include <signal.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <errno.h>

static int drain(int fd, char *buf, int cap) {
    int fl = fcntl(fd, F_GETFL); fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    int total = 0;
    for (int i = 0; i < 200 && total < cap; i++) {
        ssize_t n = read(fd, buf + total, cap - total);
        if (n > 0) total += n; else break;
    }
    fcntl(fd, F_SETFL, fl);
    return total;
}

int main(void) {
    // child owns a controlling tty so TIOCGSID/dev-tty are meaningful.
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    if (m < 0) { printf("ttyctl setup=0\n"); return 0; }
    grantpt(m); unlockpt(m);
    char *sn = ptsname(m);
    char name[128]; strncpy(name, sn ? sn : "", sizeof name - 1);

    // --- TIOCPKT packet mode on the master (independent of ctty) ---
    int pkt = 1;
    int pkton = ioctl(m, TIOCPKT, &pkt) == 0;
    int s = open(name, O_RDWR | O_NOCTTY);
    struct termios rt; tcgetattr(s, &rt); cfmakeraw(&rt); tcsetattr(s, TCSANOW, &rt);
    tcflush(m, TCIOFLUSH);
    write(s, "P", 1);
    usleep(20000);
    unsigned char pb[16] = {0};
    int np = drain(m, (char *)pb, sizeof pb - 1);
    // packet mode: slave data reaches the master framed as a TIOCPKT_DATA(0x00) control byte immediately
    // followed by the data byte(s). Scan for the {0x00,'P'} data packet (a preceding status byte, e.g. a
    // stop/start notification, may lead -- its exact value/coalescing is timing-dependent, so we don't pin it).
    int pkt_data = 0;
    for (int i = 0; i + 1 < np; i++)
        if (pb[i] == 0x00 && pb[i + 1] == 'P') pkt_data = 1;
    close(s);

    // --- TIOCGSID / tcgetsid on a controlling slave (in a child session) ---
    int pctl[2]; pipe(pctl);
    pid_t pid = fork();
    if (pid == 0) {
        setsid();
        int cs = open(name, O_RDWR);   // acquire ctty
        ioctl(cs, TIOCSCTTY, 0);
        pid_t want = getsid(0);
        pid_t got = -1;
        int r = ioctl(cs, TIOCGSID, &got);
        int gsid_ok = (r == 0 && got == want);
        // /dev/tty must reach the controlling terminal
        int dt = open("/dev/tty", O_RDWR);
        int devtty_ok = (dt >= 0 && isatty(dt));
        // tcgetsid wrapper agreement
        int tcg_ok = (tcgetsid(cs) == want);
        int res = (gsid_ok ? 1 : 0) | (devtty_ok ? 2 : 0) | (tcg_ok ? 4 : 0);
        write(pctl[1], &res, sizeof res);
        _exit(0);
    }
    close(pctl[1]);
    int childres = 0;
    read(pctl[0], &childres, sizeof childres);
    int st; waitpid(pid, &st, 0);

    printf("ttyctl pkton=%d pkt_data=%d gsid=%d devtty=%d tcgetsid=%d\n",
           pkton, pkt_data, (childres & 1) != 0, (childres & 2) != 0, (childres & 4) != 0);
    close(m);
    return 0;
}
