// pcachex/forkpipe.c -- plain fork WITHOUT execve under the persistent cache (#339). The child
// re-translates from a fresh post-fork arena and exits; before the #339 fork hardening its exit-time
// save persisted the parent's stale reloc table and poisoned the shared cache file (SIGSEGV/hang on the
// next warm run). The child reports through a pipe and the parent prints one line, so output is
// deterministic and golden-checkable across cold and warm runs.
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int fds[2];
    if (pipe(fds) != 0) return 2;
    pid_t p = fork();
    if (p == 0) {
        close(fds[0]);
        volatile unsigned long h = 5381;
        for (int i = 0; i < 300000; i++) h = h * 31 + (unsigned)i;
        unsigned long v = h;
        write(fds[1], &v, sizeof v);
        close(fds[1]);
        _exit(0);
    }
    close(fds[1]);
    unsigned long got = 0;
    ssize_t r = read(fds[0], &got, sizeof got);
    close(fds[0]);
    int st = -1;
    waitpid(p, &st, 0);
    printf("pcache forkpipe r=%d h=%lx exit=%d\n", (int)r, got, WIFEXITED(st) ? WEXITSTATUS(st) : -1);
    return 0;
}
