// Background process-group access to the controlling terminal generates job-control stop signals:
//  - a background read() raises SIGTTIN and stops the whole group;
//  - with SIGTTIN ignored the read returns EIO instead of stopping;
//  - a background write() with TOSTOP set raises SIGTTOU and stops the group.
// Driven from a session leader that owns the ctty; the background child runs in its own group and
// its stop/exit is asserted by the leader via waitpid(WUNTRACED).
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

static char name[128];

//  op 0: read()  -> expect SIGTTIN stop
//  op 1: read() with SIGTTIN ignored -> expect EIO
//  op 2: write() with TOSTOP -> expect SIGTTOU stop
static int bg_case(int slave, int op) {
    pid_t bg = fork();
    if (bg == 0) {
        setpgid(0, 0); // background group; the leader's group stays foreground
        if (op == 1) signal(SIGTTIN, SIG_IGN);
        char b[4];
        long r; int e;
        errno = 0;
        if (op == 0 || op == 1) { r = read(slave, b, 1); e = errno; }
        else { r = write(slave, "x", 1); e = errno; }
        _exit((r < 0 && e == EIO) ? 77 : (r >= 0 ? 66 : 55));
    }
    setpgid(bg, bg);
    int st;
    waitpid(bg, &st, WUNTRACED);
    if (WIFSTOPPED(st)) {
        int sig = WSTOPSIG(st);
        kill(bg, SIGKILL);
        waitpid(bg, &st, 0);
        return 1000 + sig;
    }
    return WIFEXITED(st) ? WEXITSTATUS(st) : -1;
}

static void leader(int reportfd) {
    setsid();
    int s = open(name, O_RDWR);
    if (s < 0) { int v[3] = {0,0,0}; write(reportfd, v, sizeof v); _exit(0); }
    ioctl(s, TIOCSCTTY, 0);
    struct termios t; tcgetattr(s, &t);
    t.c_lflag |= TOSTOP;
    tcsetattr(s, TCSANOW, &t);
    tcsetpgrp(s, getpgrp());
    int v[3] = { bg_case(s, 0), bg_case(s, 1), bg_case(s, 2) };
    write(reportfd, v, sizeof v);
    _exit(0);
}

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    grantpt(m); unlockpt(m);
    char *sn = ptsname(m);
    strncpy(name, sn ? sn : "", sizeof name - 1);
    int p[2]; if (pipe(p) < 0) return 1;
    pid_t sl = fork();
    if (sl == 0) { close(p[0]); leader(p[1]); }
    close(p[1]);
    int v[3] = {0,0,0};
    read(p[0], v, sizeof v);
    int st; waitpid(sl, &st, 0);
    printf("bgtty read_sigttin=%d ign_eio=%d write_sigttou=%d\n",
           v[0] == (1000 + SIGTTIN), v[1] == 77, v[2] == (1000 + SIGTTOU));
    close(m);
    return 0;
}
