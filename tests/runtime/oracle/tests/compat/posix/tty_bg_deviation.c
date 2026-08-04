// EXCLUDED-KNOWN-BUG repro. Two background-tty job-control deviations from native (golden below is the
// native aarch64 oracle: both booleans 1). Skipped by the matrix (excluded-known-bug) until fixed.
//
//  (1) tcset_sigttou (BOTH engines wrong -> 0): a background-group tcsetattr must raise SIGTTOU and stop
//      the group. The engine blocks SIGTTOU around tcsetattr/tcsetpgrp/tcflush (fs.c tty_ctl_block) to
//      dodge a shell give_terminal_to handoff race, so a genuine background tcsetattr silently SUCCEEDS
//      instead of stopping. A well-behaved shell already blocks SIGTTOU itself (and that block is mirrored
//      onto the host by rt_sigprocmask, signal.c case 135), so scoping the engine block to callers that did
//      NOT block it would match native for both the shell path and this background path.
//  (2) blk_eio (x86_64 engine wrong -> stops instead of EIO): with SIGTTIN BLOCKED (not ignored), a
//      background read of the ctty must return EIO. The aarch64 engine does; the x86_64 engine stops the
//      process (SIGTTIN delivered) -- the guest's blocked SIGTTIN is not reflected onto the host thread at
//      read time on the x86 target, so the host kernel generates the stop instead of EIO. aarch64 correct.
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

//  op 0: tcsetattr() from bg -> expect SIGTTOU stop
//  op 1: read() with SIGTTIN blocked -> expect EIO
static int bg_case(int slave, int op) {
    pid_t bg = fork();
    if (bg == 0) {
        setpgid(0, 0);
        if (op == 1) { sigset_t s; sigemptyset(&s); sigaddset(&s, SIGTTIN); sigprocmask(SIG_BLOCK,&s,0); }
        long r; int e; errno = 0;
        if (op == 0) { struct termios t; tcgetattr(slave,&t); r = tcsetattr(slave, TCSANOW, &t); e = errno; }
        else { char b[4]; r = read(slave, b, 1); e = errno; }
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
    if (s < 0) { int v[2] = {0,0}; write(reportfd, v, sizeof v); _exit(0); }
    ioctl(s, TIOCSCTTY, 0);
    tcsetpgrp(s, getpgrp());
    int v[2] = { bg_case(s, 0), bg_case(s, 1) };
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
    int v[2] = {0,0};
    read(p[0], v, sizeof v);
    int st; waitpid(sl, &st, 0);
    printf("bgdev tcset_sigttou=%d blk_eio=%d\n",
           v[0] == (1000 + SIGTTOU), v[1] == 77);
    close(m);
    return 0;
}
