// #420 REGRESSION GUARD (devpts fork-teardown: /dev/pts/N must resolve while the pair is open ANYWHERE).
// apt's SetupSlavePtyMagic runs in the forked CHILD: it closes the inherited pty MASTER fd, then opens the
// slave BY NAME ("/dev/pts/N"), expecting the still-live slave (the PARENT keeps the master open). Under hl
// the child's master close ran pts_on_close() which freed devpts index N in the child's (COW-private) table,
// so the child's later open("/dev/pts/N") saw pts_master_fd(N)==-1 and returned ENOENT -> apt printed
//   "E: Can not write log (Is /dev/pts mounted?)".
// Real Linux/devpts (native oracle here): the pty is alive as long as ANY process holds the master, so the
// child opens the slave fine and its writes reach the parent's master. This guard reproduces exactly that:
//   parent posix_openpt+grantpt+unlockpt (index N) -> fork -> child closes master, opens /dev/pts/N, writes
//   a byte -> parent (still holding the master) reads that byte back.
// PASS (native + fixed hl, both arches): childopen=1 roundtrip=1.  FAIL (pre-#420 hl): childopen=0 roundtrip=0.
#define _DEFAULT_SOURCE
#define _XOPEN_SOURCE 600
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

int main(void) {
    int m = posix_openpt(O_RDWR | O_NOCTTY);
    if (m < 0) { printf("ptsfork openpt=0\n"); return 0; }
    grantpt(m);
    unlockpt(m);
    int n = -1;
    if (ioctl(m, TIOCGPTN, &n) != 0 || n < 0) { printf("ptsfork ptn=0\n"); return 0; }

    int verdict[2]; // child -> parent: {sopen, errno}
    if (pipe(verdict) != 0) { printf("ptsfork pipe=0\n"); return 0; }

    pid_t pid = fork();
    if (pid < 0) { printf("ptsfork fork=0\n"); return 0; }

    if (pid == 0) {
        // CHILD: exactly what apt's SetupSlavePtyMagic does -- drop the master, open the slave by name.
        close(m);
        close(verdict[0]);
        char name[64];
        snprintf(name, sizeof name, "/dev/pts/%d", n);
        int s = open(name, O_RDWR | O_NOCTTY);
        int res[2] = {s >= 0, s >= 0 ? 0 : errno};
        if (s >= 0) { char z = 'Z'; if (write(s, &z, 1) != 1) res[0] = 2; }
        (void)!write(verdict[1], res, sizeof res); // parent consumes it
        if (s >= 0) close(s);
        _exit(0);
    }

    // PARENT: keep the master open the whole time; consume the child's verdict, then read the byte it wrote.
    close(verdict[1]);
    int res[2] = {0, 0};
    int childopen = read(verdict[0], res, sizeof res) == (ssize_t)sizeof res && res[0] == 1;
    int roundtrip = 0;
    if (childopen) {
        struct pollfd pfd = {.fd = m, .events = POLLIN};
        if (poll(&pfd, 1, 2000) == 1 && (pfd.revents & POLLIN)) {
            char buf = 0;
            roundtrip = read(m, &buf, 1) == 1 && buf == 'Z';
        }
    }
    waitpid(pid, NULL, 0);
    close(verdict[0]);
    close(m);
    printf("ptsfork childopen=%d roundtrip=%d\n", childopen, roundtrip);
    return 0;
}
