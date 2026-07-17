// exec-fd-inheritance + epoll: the EXACT Chrome renderer-launch bootstrap that Wall 7 blocks on.
//
// Chrome's PlatformChannel: the browser creates a socketpair, dup2()s one end to a known fd number,
// clears FD_CLOEXEC, and execve()s the renderer, passing that fd number on the command line
// (--mojo-platform-channel-handle=<fd>). The renderer then arms epoll (EPOLLIN) on THAT
// exec-inherited fd and blocks in epoll_wait for the "invitation" the browser writes later.
//
// This gate reproduces that permutation OFFLINE with no live Chrome. The distinguishing feature vs the
// existing zygote-inbound (fork-inherit) / scm-recv-epoll (SCM_RIGHTS) gates is EXECVE: exec resets
// process state, so the socket fd must survive across exec at its specific number (FD_CLOEXEC clear),
// and an epoll arm on that exec-inherited fd must deliver a CROSS-PROCESS wake when the parent writes.
//
// Protocol per trigger mode (level-triggered and edge-triggered, both exercised):
//   child (post-exec): epoll_ctl ADD the inherited fd, write "R" (armed), then two rounds of
//                      epoll_wait -> read the invitation -> ack. A re-arm round proves the fd stays
//                      armed. Finally writes "PONG" (reverse direction) and exits 0 iff every step held.
//   parent: waits for "R", sleeps so the child is blocked IN epoll_wait, writes the invitation (the
//           real cross-process wake), collects the ack, then epoll_waits on its own end for "PONG".
// A SIGALRM watchdog in BOTH processes guarantees a hard _exit on any hang (never blocks the harness).
// Deterministic golden: on a correct engine every field is 1/0; a lost exec-inherited-fd epoll wake
// (the Wall 7 hypothesis) makes the child time out -> child!=0 / pong=0 -> the gate FAILS.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

#define CHILD_FD 42       // Chrome passes the platform-channel fd at a fixed number; mirror that here
#define WAIT_MS 2500      // per-round epoll_wait budget (comfortably > the parent's arm delay)

static void die_alarm(int sig) { (void)sig; _exit(7); } // watchdog: any hang -> hard exit, never block

static void sleep_ms(int ms) {
    struct timespec ts = {ms / 1000, (long)(ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

// read exactly n bytes (blocking); returns 0 on success, -1 on EOF/short/error.
static int read_n(int fd, void *buf, size_t n) {
    size_t got = 0;
    while (got < n) {
        ssize_t r = read(fd, (char *)buf + got, n - got);
        if (r <= 0) return -1;
        got += (size_t)r;
    }
    return 0;
}

// ---- CHILD (post-exec): epoll-arm the inherited fd and prove cross-process wakes ----
// argv: [self, "child", "<fdnum>", "lt"|"et"]. exit 0 == every assertion held.
static int child_main(int fd, int et) {
    signal(SIGALRM, die_alarm);
    alarm(6); // fresh timer in the exec'd image; fires if a wake is ever lost

    // The fd must have survived exec at its number with FD_CLOEXEC clear; if it hadn't, it would be
    // closed now and every op below would EBADF -> a clean failure (never a false pass).
    if (fcntl(fd, F_GETFD) < 0) return 10;

    int ep = epoll_create1(0);
    if (ep < 0) return 11;
    struct epoll_event ev = {0};
    ev.events = EPOLLIN | (et ? EPOLLET : 0);
    ev.data.fd = fd;
    if (epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ev) != 0) return 12;

    if (write(fd, "R", 1) != 1) return 13; // tell the parent we are armed

    // Two invitation rounds over the SAME persistent epoll registration (round 2 proves the arm
    // survives / re-fires — the renderer reads more than one message off its primary channel).
    const char *want[2] = {"GO1", "GO2"};
    for (int round = 0; round < 2; round++) {
        struct epoll_event out[4];
        int n = epoll_wait(ep, out, 4, WAIT_MS);
        if (n != 1) return 20 + round;            // 0 == the lost cross-process wake (Wall 7 shape)
        if (out[0].data.fd != fd) return 30 + round;
        if (!(out[0].events & EPOLLIN)) return 40 + round;
        char b[4] = {0};
        if (read_n(fd, b, 3) != 0) return 50 + round;
        if (memcmp(b, want[round], 3) != 0) return 60 + round;
        char ack[2] = {'A', (char)('1' + round)};
        if (write(fd, ack, 2) != 2) return 70 + round; // serialize rounds (ET-safe: no coalescing)
    }

    // Reverse direction: data must also flow child -> parent through the exec-inherited fd.
    if (write(fd, "PONG", 4) != 4) return 80;
    close(ep);
    return 0;
}

// ---- PARENT: drive one trigger mode; returns via *out_child / *out_pong ----
// Builds a fresh socketpair, dup2()s one end to CHILD_FD, clears FD_CLOEXEC, execve()s self.
static void run_mode(const char *self, const char *mode, int *out_child, int *out_pong) {
    *out_child = -1;
    *out_pong = 0;

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return;

    pid_t pid = fork();
    if (pid == 0) {
        // child pre-exec: place the parent-facing end at the fixed fd number, CLOEXEC clear, then exec.
        close(sv[0]);
        if (sv[1] != CHILD_FD) {
            if (dup2(sv[1], CHILD_FD) < 0) _exit(90);
            close(sv[1]);
        }
        int fl = fcntl(CHILD_FD, F_GETFD);
        fcntl(CHILD_FD, F_SETFD, fl & ~FD_CLOEXEC); // must survive execve
        char fdbuf[16];
        snprintf(fdbuf, sizeof fdbuf, "%d", CHILD_FD);
        char *na[] = {(char *)self, (char *)"child", fdbuf, (char *)mode, NULL};
        execve(self, na, environ);
        _exit(91); // exec failed
    }
    close(sv[1]);
    int p = sv[0];

    signal(SIGALRM, die_alarm);
    alarm(10); // ultimate parent guard (child's 6s fires first on a real hang)

    // 1) wait for "armed", then let the child settle INTO epoll_wait so the write is a live wake.
    char r;
    if (read_n(p, &r, 1) == 0 && r == 'R') {
        for (int round = 0; round < 2; round++) {
            sleep_ms(120);                             // child is now blocked in epoll_wait
            const char *inv = round == 0 ? "GO1" : "GO2";
            if (write(p, inv, 3) != 3) break;
            char ack[2];
            if (read_n(p, ack, 2) != 0) break;         // A1 / A2
            if (ack[0] != 'A' || ack[1] != (char)('1' + round)) break;
        }
        // 2) reverse direction, received via the PARENT's OWN epoll on its end.
        int ep = epoll_create1(0);
        if (ep >= 0) {
            struct epoll_event ev = {0};
            ev.events = EPOLLIN;
            ev.data.fd = p;
            if (epoll_ctl(ep, EPOLL_CTL_ADD, p, &ev) == 0) {
                struct epoll_event out[4];
                if (epoll_wait(ep, out, 4, WAIT_MS) == 1) {
                    char pong[4] = {0};
                    if (read_n(p, pong, 4) == 0 && memcmp(pong, "PONG", 4) == 0) *out_pong = 1;
                }
            }
            close(ep);
        }
    }
    alarm(0);
    close(p);
    int st = 0;
    waitpid(pid, &st, 0);
    *out_child = WIFEXITED(st) ? WEXITSTATUS(st) : (128 + (WIFSIGNALED(st) ? WTERMSIG(st) : 0));
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc >= 4 && strcmp(argv[1], "child") == 0)
        return child_main(atoi(argv[2]), strcmp(argv[3], "et") == 0);

    const char *self = "/proc/self/exe"; // canonical re-exec target inside the rootfs (matches procexe)
    int c_lt, p_lt, c_et, p_et;
    run_mode(self, "lt", &c_lt, &p_lt);
    run_mode(self, "et", &c_et, &p_et);
    printf("execfd mode=lt child=%d pong=%d\n", c_lt, p_lt);
    printf("execfd mode=et child=%d pong=%d\n", c_et, p_et);
    return 0;
}
