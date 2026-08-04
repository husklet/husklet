// Controlling-process (session leader) death sends SIGHUP -- and, on exit, ONLY SIGHUP -- to the
// controlling terminal's FOREGROUND process group (the classic "close the terminal -> children get
// SIGHUP"). Linux pairs the SIGCONT with the SIGHUP only on the TIOCNOTTY path (tty_jobctrl.c passes
// on_exit=1 from do_exit, which skips the SIGCONT), so fg_sigcont=0 here is the native verdict.
// Deterministic via a grandparent pipe: the foreground worker reports the signals it received.
//
// Ordering is enforced with two pipes, not with sleeps: the worker acks once BOTH handlers are armed,
// and only then does the leader hand it the terminal and die. Without the ack the leader could die
// first, killing the worker by default SIGHUP disposition before it observes anything.
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
static volatile sig_atomic_t got_hup, got_cont;
static void on_hup(int s){ (void)s; got_hup=1; }
static void on_cont(int s){ (void)s; got_cont=1; }

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    grantpt(m); unlockpt(m);
    char *sn = ptsname(m);
    strncpy(name, sn ? sn : "", sizeof name - 1);

    int out[2]; if (pipe(out) < 0) return 1;  // worker -> us: the signals it saw
    int rdy[2]; if (pipe(rdy) < 0) return 1;  // leader -> worker: you are the foreground group
    int ack[2]; if (pipe(ack) < 0) return 1;  // worker -> leader: my handlers are armed

    pid_t c = fork();
    if (c == 0) {
        // Keep both rdy/ack ends until the worker is forked; each side drops its unused end after.
        close(out[0]);
        setsid();
        int s = open(name, O_RDWR);   // acquire ctty (session leader, no O_NOCTTY)
        ioctl(s, TIOCSCTTY, 0);
        pid_t w = fork();
        if (w == 0) {
            close(rdy[1]); close(ack[0]);
            setpgid(0, 0);
            struct sigaction sa; memset(&sa,0,sizeof sa);
            sa.sa_handler = on_hup;  sigaction(SIGHUP, &sa, 0);
            sa.sa_handler = on_cont; sigaction(SIGCONT, &sa, 0);
            char a='A'; if (write(ack[1], &a, 1) != 1) _exit(2); // handlers armed: the leader may proceed
            char r; if (read(rdy[0], &r, 1) != 1) _exit(3);      // wait until we are the foreground group
            for (int i=0;i<3000 && !got_hup;i++) usleep(1000);
            // fg_sigcont is a NEGATIVE assertion, so give it a settle window: tty_jobctrl.c sends SIGCONT
            // after the SIGHUP only on the TIOCNOTTY path, never when the controlling process EXITS.
            usleep(50*1000);
            unsigned char res = (got_hup?1:0)|(got_cont?2:0);
            if (write(out[1], &res, 1) != 1) _exit(4);
            _exit(0);
        }
        // The leader keeps rdy[1]/ack[0]; the worker owns the other ends.
        close(rdy[0]); close(ack[1]);
        char a; if (read(ack[0], &a, 1) != 1) _exit(5);
        setpgid(w, w);
        tcsetpgrp(s, w);                   // W is foreground
        char go='G'; if (write(rdy[1], &go, 1) != 1) _exit(6);
        usleep(30000);
        _exit(0);                          // controlling-process death -> SIGHUP to W's group
    }
    close(out[1]); close(rdy[0]); close(rdy[1]); close(ack[0]); close(ack[1]);
    unsigned char res = 0;
    // A worker that died before reporting must FAIL, not inherit a sentinel that reads as success.
    ssize_t got = read(out[0], &res, 1);
    int st; waitpid(c, &st, 0);
    printf("slhup fg_sighup=%d fg_sigcont=%d\n", got==1 && (res&1)!=0, got==1 && (res&2)!=0);
    close(m);
    return 0;
}
