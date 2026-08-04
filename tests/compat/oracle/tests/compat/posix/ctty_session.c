// controlling-tty acquisition/loss + session/setsid/setpgid rules
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

// child A: session leader opens slave WITHOUT O_NOCTTY -> acquires ctty; tcgetsid==sid; /dev/tty reaches it
static int childA(void) {
    int r = 0;
    setsid();
    // before acquiring a ctty, /dev/tty must fail ENXIO
    int pre = open("/dev/tty", O_RDWR);
    int pre_enxio = (pre < 0 && errno == ENXIO);
    if (pre >= 0) close(pre);
    int s = open(name, O_RDWR); // session leader, no O_NOCTTY => acquires ctty
    if (s < 0) return 100;
    pid_t sid = getsid(0);
    pid_t got = -1;
    int gsid = (ioctl(s, TIOCGSID, &got) == 0 && got == sid);
    int tcg = (tcgetsid(s) == sid);
    int dt = open("/dev/tty", O_RDWR);
    int devtty = (dt >= 0 && isatty(dt));
    if (dt >= 0) close(dt);
    close(s);
    r = (pre_enxio?1:0)|(gsid?2:0)|(tcg?4:0)|(devtty?8:0);
    return r;
}

// child B: NON-session-leader open of slave does NOT acquire a ctty (/dev/tty stays ENXIO)
static int childB(void) {
    setsid();
    // create a child in a subgroup so B is not able to become ctty owner? Actually B IS session leader here.
    // To be a non-session-leader, fork once more and open in the grandchild.
    int pipefd[2]; if (pipe(pipefd) < 0) return 100;
    pid_t g = fork();
    if (g == 0) {
        // grandchild: NOT a session leader (parent is)
        int s = open(name, O_RDWR); // no O_NOCTTY, but not session leader => no ctty acquired
        int leaderness = (getsid(0) == getpid()); // should be 0: not leader
        int dt = open("/dev/tty", O_RDWR);
        int devtty_fail = (dt < 0 && errno == ENXIO);
        if (dt >= 0) close(dt);
        if (s >= 0) close(s);
        int res = (!leaderness?1:0)|(devtty_fail?2:0);
        write(pipefd[1], &res, sizeof res);
        _exit(0);
    }
    close(pipefd[1]);
    int res = 0; read(pipefd[0], &res, sizeof res);
    int st; waitpid(g, &st, 0);
    return res;
}

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    grantpt(m); unlockpt(m);
    char *sn = ptsname(m);
    strncpy(name, sn ? sn : "", sizeof name - 1);

    pid_t a = fork();
    if (a == 0) _exit(childA());
    int sa; waitpid(a, &sa, 0);
    int ra = WIFEXITED(sa) ? WEXITSTATUS(sa) : -1;

    pid_t b = fork();
    if (b == 0) _exit(childB());
    int sb; waitpid(b, &sb, 0);
    int rb = WIFEXITED(sb) ? WEXITSTATUS(sb) : -1;

    // setsid EPERM when already a group leader
    pid_t c = fork();
    if (c == 0) {
        setpgid(0, 0);            // become group leader
        int e = (setsid() == -1 && errno == EPERM);
        _exit(e ? 0 : 1);
    }
    int sc; waitpid(c, &sc, 0);
    int setsid_eperm = WIFEXITED(sc) && WEXITSTATUS(sc) == 0;

    // setpgid cannot move a session leader (EPERM)
    pid_t d = fork();
    if (d == 0) {
        setsid();
        int e = (setpgid(0, getppid()) == -1); // move into another group -> should fail
        _exit(e ? 0 : 1);
    }
    int sd; waitpid(d, &sd, 0);
    int setpgid_leader = WIFEXITED(sd) && WEXITSTATUS(sd) == 0;

    printf("ctty A_pre_enxio=%d A_gsid=%d A_tcgetsid=%d A_devtty=%d B_notleader=%d B_devtty_enxio=%d setsid_eperm=%d setpgid_leader_fails=%d\n",
           (ra&1)!=0,(ra&2)!=0,(ra&4)!=0,(ra&8)!=0,(rb&1)!=0,(rb&2)!=0,setsid_eperm,setpgid_leader);
    close(m);
    return 0;
}
