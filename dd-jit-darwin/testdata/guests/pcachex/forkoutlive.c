// pcachex/forkoutlive.c -- fork child that OUTLIVES the parent, no execve (#339 crash repro shape).
// The parent exits immediately after forking; the child sleeps, re-translates a compute slice from its
// fresh post-fork arena, and exits LAST -- so before the #339 fork-save bar, the child's poisoned save
// was the final rename and every later warm run of this binary crashed/hung on load. The parent's line
// is the only stdout (the child stays silent), keeping the observable output deterministic.
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(void) {
    pid_t p = fork();
    if (p == 0) {
        usleep(300000); // outlive the parent -> the child's exit-time save (if any) is the last writer
        volatile unsigned long h = 5381;
        for (int i = 0; i < 400000; i++) h = h * 31 + (unsigned)i;
        (void)h;
        _exit(0);
    }
    printf("pcache forkoutlive forked=%d\n", p > 0 ? 1 : 0);
    return 0;
}
