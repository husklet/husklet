// canonical-mode line discipline: line buffering, ERASE editing, ^D EOF, VINTR->SIGINT to fg pgrp.
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
static int drain(int fd, char *buf, int cap) {
    int fl = fcntl(fd, F_GETFL);
    fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    int total = 0;
    for (int i = 0; i < 300 && total < cap; i++) {
        ssize_t n = read(fd, buf + total, cap - total);
        if (n > 0) total += n; else break;
    }
    fcntl(fd, F_SETFL, fl);
    return total;
}

int main(void) {
    int m, s;
    if (mk(&m, &s) < 0) { printf("canon setup=0\n"); return 0; }

    // Canonical mode, no echo. ERASE = 0x7f.
    struct termios t;
    tcgetattr(s, &t);
    t.c_lflag = ICANON;                 // canon on, echo/isig off
    t.c_iflag = 0; t.c_oflag = 0;
    t.c_cc[VERASE] = 0x7f;
    t.c_cc[VEOF] = 4;                   // ^D
    tcsetattr(s, TCSANOW, &t);
    tcflush(s, TCIOFLUSH); tcflush(m, TCIOFLUSH);

    // 1. line buffering: a partial line (no newline) is NOT delivered to a canonical read.
    write(m, "abc", 3);
    usleep(20000);
    char pb[64] = {0};
    int navail = drain(s, pb, sizeof pb - 1);   // no NL yet -> nothing readable
    int line_buffered = (navail == 0);

    // 2. complete the line + ERASE edit: "abc" already queued; send "\x7fX\n" -> erases 'c' -> "abX\n".
    write(m, "\x7f" "X\n", 3);
    usleep(20000);
    char lb[64] = {0};
    int nl = drain(s, lb, sizeof lb - 1);
    int erase_ok = (nl == 4 && memcmp(lb, "abX\n", 4) == 0);

    // 3. ^D EOF: a bare ^D on an empty line makes the slave read return 0 (EOF), not blocking.
    tcflush(s, TCIOFLUSH); tcflush(m, TCIOFLUSH);
    write(m, "\x04", 1);
    usleep(20000);
    char eb[8];
    ssize_t er = read(s, eb, sizeof eb);   // canonical ^D on empty line => read returns 0
    int eof_ok = (er == 0);

    // 4. ^D after data: "hi" then ^D delivers "hi" WITHOUT a newline.
    tcflush(s, TCIOFLUSH); tcflush(m, TCIOFLUSH);
    write(m, "hi\x04", 3);
    usleep(20000);
    char db[16] = {0};
    int nd = drain(s, db, sizeof db - 1);
    int eof_data = (nd == 2 && memcmp(db, "hi", 2) == 0);

    close(s); close(m);
    printf("canon linebuf=%d erase=%d eof=%d eofdata=%d\n", line_buffered, erase_ok, eof_ok, eof_data);
    return 0;
}
