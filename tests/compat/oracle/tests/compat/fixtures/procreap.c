// Fork/wait/exit-code + process-group-teardown matrix. Regression test for the LTP SAFE_FORK class
// (#394): a parent that reaps a child's private process group with kill(-pgid, SIGKILL) must survive
// and report exit codes byte-exactly. The old engine folded every kill(a0<=0) into "signal myself", so
// the parent SIGKILLed itself (exit 255) after the child's group teardown. Deterministic (no pids in
// output) -> .oracle() diffs stdout+exit against native.
#include <stdio.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

static int reap(pid_t p) { // -> WEXITSTATUS, or 1000+signo if killed by a signal
    int st = 0;
    if (waitpid(p, &st, 0) != p) return -1;
    if (WIFSIGNALED(st)) return 1000 + WTERMSIG(st);
    return WIFEXITED(st) ? WEXITSTATUS(st) : -2;
}

int main(void) {
    // 1. child exit(0)
    pid_t a = fork();
    if (a == 0) _exit(0);
    printf("exit0 code=%d\n", reap(a));

    // 2. child exit(42)
    pid_t b = fork();
    if (b == 0) _exit(42);
    printf("exit42 code=%d\n", reap(b));

    // 3. child killed by SIGKILL
    pid_t k = fork();
    if (k == 0) { raise(SIGKILL); _exit(0); }
    printf("sigkill code=%d\n", reap(k)); // 1000+9

    // 4. multiple children, distinct codes, reaped in order
    pid_t m[4];
    for (int i = 0; i < 4; i++) { m[i] = fork(); if (m[i] == 0) _exit(i * 10); }
    int sum = 0;
    for (int i = 0; i < 4; i++) sum += reap(m[i]);
    printf("multi sum=%d\n", sum); // 0+10+20+30

    // 5. WNOHANG poll until the child is reaped
    pid_t w = fork();
    if (w == 0) { usleep(50 * 1000); _exit(7); }
    int st = 0, polls = 0;
    while (waitpid(w, &st, WNOHANG) == 0) { usleep(5 * 1000); polls++; }
    printf("wnohang code=%d polled=%d\n", WEXITSTATUS(st), polls > 0);

    // 6. nested fork: grandchild
    pid_t n = fork();
    if (n == 0) {
        pid_t g = fork();
        if (g == 0) _exit(5);
        _exit(reap(g)); // parent-of-grandchild forwards its code
    }
    printf("nested code=%d\n", reap(n)); // 5

    // 7. process-group teardown (the #394 root cause): the child makes its OWN process group and blocks;
    //    the parent SIGKILLs that whole group and must SURVIVE (not kill itself), then reap WIFSIGNALED.
    pid_t c = fork();
    if (c == 0) { setpgid(0, 0); pause(); _exit(0); }
    usleep(30 * 1000);          // let the child establish its group + block in pause()
    kill(-c, SIGKILL);          // signal the child's private group -- must NOT hit the parent
    printf("pgkill code=%d survived=1\n", reap(c)); // 1000+9

    printf("procreap done\n");
    return 0;
}
