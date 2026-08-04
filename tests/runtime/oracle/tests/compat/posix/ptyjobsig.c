// job-control signal generation: VINTR/VQUIT/VSUSP chars on a pty generate SIGINT/SIGQUIT/SIGTSTP
// to the slave's foreground process group. Child becomes session leader with the slave as ctty.
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

static volatile sig_atomic_t g_got;
static void h(int s) { g_got = s; }

// run one child that waits for signal `deliverchar` and reports which signal it got via exit code.
static int child_case(int m, char *slavename, int deliverchar, int catchsig) {
    pid_t pid = fork();
    if (pid == 0) {
        setsid();                                   // new session, drop any ctty
        int s = open(slavename, O_RDWR);            // session leader + no O_NOCTTY => acquires ctty
        if (s < 0) _exit(10);
        ioctl(s, TIOCSCTTY, 0);
        struct termios t; tcgetattr(s, &t);
        t.c_lflag |= ISIG;
        t.c_cc[VINTR] = 3; t.c_cc[VQUIT] = 28; t.c_cc[VSUSP] = 26;
        tcsetattr(s, TCSANOW, &t);
        signal(catchsig, h);
        // become the foreground group (we are session leader, our pgrp is the fg by default)
        tcsetpgrp(s, getpgrp());
        // signal readiness then wait
        write(s, "R", 1);
        for (int i = 0; i < 500 && !g_got; i++) usleep(2000);
        _exit(g_got == catchsig ? 42 : 11);
    }
    // parent: wait for child readiness "R" on master, then inject the control char
    char rb[8]; int fl = fcntl(m, F_GETFL); fcntl(m, F_SETFL, fl | O_NONBLOCK);
    for (int i = 0; i < 500; i++) { if (read(m, rb, 1) == 1) break; usleep(2000); }
    fcntl(m, F_SETFL, fl);
    usleep(20000);
    char cc = (char)deliverchar;
    write(m, &cc, 1);
    int st = 0;
    waitpid(pid, &st, 0);
    return WIFEXITED(st) ? WEXITSTATUS(st) : -WTERMSIG(st);
}

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    if (m < 0) { printf("jobsig setup=0\n"); return 0; }
    grantpt(m); unlockpt(m);
    char *sn = ptsname(m);
    if (!sn) { printf("jobsig ptsname=0\n"); return 0; }
    char name[128]; strncpy(name, sn, sizeof name - 1);

    int intr = child_case(m, name, 3, SIGINT);    // ^C -> SIGINT
    int quit = child_case(m, name, 28, SIGQUIT);  // ^\ -> SIGQUIT

    printf("jobsig sigint=%d sigquit=%d\n", intr == 42, quit == 42);
    close(m);
    return 0;
}
