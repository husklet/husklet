// SA_RESTART semantics per syscall class: a signal during read(2) on a pipe restarts when the
// handler is SA_RESTART and returns EINTR otherwise, while nanosleep(2) is never restarted and
// always reports EINTR with a remaining time; the handler must have run in every case.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t hits;
static void onalrm(int s) { (void)s; hits++; }

static void arm(int restart) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = onalrm;
    sa.sa_flags = restart ? SA_RESTART : 0;
    sigaction(SIGALRM, &sa, NULL);
}

// deliver SIGALRM after ~30ms from a child, then let it write to the pipe at ~60ms
static pid_t poke(int wfd, int payload) {
    pid_t p = fork();
    if (p == 0) {
        struct timespec a = {0, 30000000};
        nanosleep(&a, NULL);
        kill(getppid(), SIGALRM);
        if (payload) {
            struct timespec b = {0, 30000000};
            nanosleep(&b, NULL);
            (void)!write(wfd, "Z", 1);
        }
        _exit(0);
    }
    return p;
}

int main(void) {
    int fd[2];
    if (pipe(fd)) return 1;
    char c;
    int st;

    arm(1);
    hits = 0;
    pid_t p1 = poke(fd[1], 1);
    ssize_t r1 = read(fd[0], &c, 1);
    int e1 = (r1 == -1) ? errno : 0;
    int h1 = hits;
    waitpid(p1, &st, 0);

    arm(0);
    hits = 0;
    pid_t p2 = poke(fd[1], 1);
    ssize_t r2 = read(fd[0], &c, 1);
    int e2 = (r2 == -1) ? errno : 0;
    int h2 = hits;
    waitpid(p2, &st, 0);
    // drain the byte the child eventually wrote
    struct timespec settle = {0, 80000000};
    nanosleep(&settle, NULL);
    char drain;
    ssize_t d = read(fd[0], &drain, 1);

    arm(1);
    hits = 0;
    pid_t p3 = poke(fd[1], 0);
    struct timespec req = {5, 0}, rem = {0, 0};
    int n = nanosleep(&req, &rem);
    int en = (n == -1) ? errno : 0;
    int remsane = (rem.tv_sec >= 4 && rem.tv_sec <= 5);
    int h3 = hits;
    waitpid(p3, &st, 0);
    printf("r1=%zd e1=%d h1=%d r2=%zd e2=%d h2=%d d=%zd n=%d en=%d remsane=%d h3=%d\n",
           r1, e1, h1, r2, e2, h2, d, n, en, remsane, h3);
    return 0;
}
