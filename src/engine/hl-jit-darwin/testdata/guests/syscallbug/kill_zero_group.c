// kill(0, sig): Linux sends the signal to EVERY process in the caller's process group -- not just the
// caller. hl historically routed kill(0) to the caller only, so a job-control shell / supervisor that
// signals its group left sibling children unsignalled. This probe forks a child that stays in the parent's
// process group, both install a SIGUSR1 handler, then the parent kill(0, SIGUSR1)s the group; the child
// must observe the signal too. Oracle-diffed vs native aarch64 (native always signals the group).
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t got;
static void onusr1(int s) {
    (void)s;
    got = 1;
}

int main(void) {
    int rp[2], wp[2]; // rp: child->parent "ready"; wp: child->parent verdict
    if (pipe(rp) || pipe(wp)) {
        printf("kill_zero pipe_fail\n");
        return 1;
    }
    signal(SIGUSR1, onusr1); // installed BEFORE fork so the child inherits it too
    pid_t c = fork();
    if (c == 0) {
        signal(SIGUSR1, onusr1);          // (redundant; the child inherited it) keep the child in our pgroup
        if (write(rp[1], "r", 1) < 0) {}  // signal readiness -- handler is armed
        for (int i = 0; i < 1000 && !got; i++) {
            struct timespec ts = {0, 1000000}; // 1ms; ~1s ceiling so a missed signal can never hang the test
            nanosleep(&ts, NULL);
        }
        char v = got ? '1' : '0';
        if (write(wp[1], &v, 1) < 0) {}
        _exit(0);
    }
    char b = 0;
    int ready = (read(rp[0], &b, 1) == 1 && b == 'r'); // child's handler is armed before we signal
    int kill_ok = (kill(0, SIGUSR1) == 0);             // signal our whole process group (parent + child)
    char cv = 0;
    int child_got = (read(wp[0], &cv, 1) == 1 && cv == '1');
    waitpid(c, NULL, 0);
    printf("kill_zero ready=%d kill_ok=%d child_got=%d\n", ready, kill_ok, child_got); // 1 1 1
    return 0;
}
