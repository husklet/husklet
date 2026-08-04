// ^Z (VSUSP) on a pty delivers SIGTSTP to the foreground group -> the whole job stops
// (WIFSTOPPED/WSTOPSIG==SIGTSTP, SIGCHLD si_code==CLD_STOPPED); SIGCONT resumes it
// (WIFCONTINUED, si_code==CLD_CONTINUED). Driven from the master, asserted via waitpid.
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

static char name[128];
static volatile sig_atomic_t chld_stopped, chld_continued;
static void on_chld(int s, siginfo_t *si, void *u){ (void)s;(void)u;
    if (si->si_code == CLD_STOPPED) chld_stopped = 1;
    if (si->si_code == CLD_CONTINUED) chld_continued = 1;
}

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    grantpt(m); unlockpt(m);
    char *sn = ptsname(m);
    strncpy(name, sn ? sn : "", sizeof name - 1);
    int rdy[2]; if (pipe(rdy) < 0) return 1;

    pid_t c = fork();
    if (c == 0) {
        close(rdy[0]);
        setsid();
        int s = open(name, O_RDWR);
        ioctl(s, TIOCSCTTY, 0);
        struct termios t; tcgetattr(s, &t);
        t.c_lflag |= ISIG; t.c_cc[VSUSP] = 26; // ^Z
        tcsetattr(s, TCSANOW, &t);
        struct sigaction sa; memset(&sa,0,sizeof sa);
        sa.sa_sigaction = on_chld; sa.sa_flags = SA_SIGINFO; // deliver CLD_STOPPED/CLD_CONTINUED
        sigaction(SIGCHLD, &sa, 0);
        pid_t w = fork();
        if (w == 0) {
            setpgid(0, 0);
            // stay alive in the foreground group; slave open just to sit on the tty
            int ws = open(name, O_RDWR);
            (void)ws;
            for (;;) pause();
        }
        setpgid(w, w);
        tcsetpgrp(s, w);            // W foreground
        usleep(20000);
        char z = 26; write(m, &z, 1); // inject ^Z on the master
        int st;
        pid_t r1 = waitpid(w, &st, WUNTRACED);
        int stopped = (r1 == w && WIFSTOPPED(st) && WSTOPSIG(st) == SIGTSTP);
        usleep(10000);
        kill(w, SIGCONT);
        pid_t r2 = waitpid(w, &st, WCONTINUED);
        int continued = (r2 == w && WIFCONTINUED(st));
        usleep(10000);
        int cs = chld_stopped, cc = chld_continued;
        kill(w, SIGKILL); waitpid(w, &st, 0);
        unsigned char res = (stopped?1:0)|(continued?2:0)|(cs?4:0)|(cc?8:0);
        write(rdy[1], &res, 1);
        _exit(0);
    }
    close(rdy[1]);
    unsigned char res = 0xff;
    read(rdy[0], &res, 1);
    int st; waitpid(c, &st, 0);
    printf("tstp stopped_sigtstp=%d continued=%d cld_stopped=%d cld_continued=%d\n",
           (res&1)!=0,(res&2)!=0,(res&4)!=0,(res&8)!=0);
    close(m);
    return 0;
}
