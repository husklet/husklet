// pcachex/selfexec.c -- fork+execve storm against /proc/self/exe (the go-build shape, #339).
// Parent forks N children; each child execve's this same binary with argv[1]="child <i>"; a child does a
// deterministic compute slice and exits with a code derived from its index. The parent reaps everything
// and prints ONE deterministic line (children write nothing), so the case is golden-checkable no matter
// how the children interleave. Under HL_JIT_PCACHE=1 every child exercises the engine's in-process execve
// cache reload (proc.c case 221) and the fork-child save bar; concurrent matrix runs exercise the
// same-key load/save races the #339 hardening exists for.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc > 1 && strncmp(argv[1], "child", 5) == 0) {
        int idx = atoi(argv[1] + 5);
        volatile unsigned long h = 5381;
        for (int i = 0; i < 100000; i++) h = h * 31 + (unsigned)i;
        (void)h;
        _exit(10 + idx); // deterministic, index-derived
    }
    const int n = 6;
    int live = 0;
    for (int i = 0; i < n; i++) {
        pid_t p = fork();
        if (p == 0) {
            char tag[32];
            snprintf(tag, sizeof tag, "child%d", i);
            char *na[] = {argv[0], tag, NULL};
            execv(argv[0], na); // argv[0] is the absolute guest path (matrix + lifecycle lane pass it)
            _exit(99);
        }
        if (p > 0) live++;
    }
    long sum = 0;
    int reaped = 0, bad = 0;
    for (;;) {
        int st;
        pid_t r = wait(&st);
        if (r < 0) break;
        reaped++;
        if (WIFEXITED(st)) sum += WEXITSTATUS(st);
        else bad++;
    }
    // sum of 10..10+n-1 = 10n + n(n-1)/2 = 75 for n=6
    printf("pcache selfexec reaped=%d sum=%ld bad=%d\n", reaped, sum, bad);
    return (reaped == live && sum == 75 && bad == 0) ? 0 : 1;
}
