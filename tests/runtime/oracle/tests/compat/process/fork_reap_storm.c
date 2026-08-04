// Fork storm + full reap discipline: launch 40 children with distinct exit codes, reap each with a
// WNOHANG poll loop, verify the exit-code sum, that every child was reaped exactly once, and that a
// final wait() reports ECHILD (no leaked zombies). Also folds in a double-fork so an intermediate
// child's own child is reaped by the intermediate before it exits (no grandchild zombie).
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

#define N 40

int main(void) {
    pid_t pids[N];
    for (int i = 0; i < N; i++) {
        pid_t p = fork();
        if (p == 0) {
            if (i == 7) {
                // double fork: reap our own child before exiting
                pid_t g = fork();
                if (g == 0) _exit(0);
                int gs = 0; waitpid(g, &gs, 0);
            }
            _exit(i % 100);
        }
        pids[i] = p;
    }

    int reaped = 0, sum = 0, expected = 0;
    for (int i = 0; i < N; i++) expected += i % 100;
    for (int i = 0; i < N; i++) {
        int st = 0;
        // WNOHANG poll until this specific child is reaped
        pid_t r;
        while ((r = waitpid(pids[i], &st, WNOHANG)) == 0) usleep(1000);
        if (r == pids[i] && WIFEXITED(st)) { reaped++; sum += WEXITSTATUS(st); }
    }

    errno = 0;
    int echild = wait(NULL) == -1 && errno == ECHILD;

    printf("fork_reap_storm reaped=%d sum_ok=%d echild=%d\n",
           reaped, sum == expected, echild);
    return 0;
}
