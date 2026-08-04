// PR_SET_CHILD_SUBREAPER + orphan reparenting: a grandchild orphaned by its immediate parent is
// reparented to the nearest ancestor marked as a subreaper (us), which then reaps it. Deterministic
// because we control the reaper -- no reliance on init/pid-1 identity. Double-fork zombie discipline.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int set = prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == 0;
    int isreaper = 0;
    prctl(PR_GET_CHILD_SUBREAPER, &isreaper, 0, 0, 0);

    pid_t child = fork();
    if (child == 0) {
        // create a grandchild that outlives us, then exit immediately -> grandchild orphaned
        pid_t g = fork();
        if (g == 0) {
            // give the middle process time to exit so we are reparented before we exit
            usleep(60 * 1000);
            _exit(77);              // grandchild's real exit code
        }
        _exit(0);                   // middle process dies now, orphaning the grandchild
    }
    // reap the middle child
    int st = 0;
    waitpid(child, &st, 0);
    int mid_ok = WIFEXITED(st) && WEXITSTATUS(st) == 0;

    // As the subreaper, a wait() with no explicit pid must eventually harvest the reparented grandchild.
    int gcode = -1, harvested = 0;
    for (;;) {
        int gs = 0;
        pid_t r = waitpid(-1, &gs, 0);
        if (r <= 0) break;          // ECHILD -> nothing left
        if (WIFEXITED(gs)) { gcode = WEXITSTATUS(gs); harvested = 1; }
    }
    printf("subreaper set=%d isreaper=%d mid_ok=%d harvested=%d gcode=%d\n",
           set, isreaper ? 1 : 0, mid_ok, harvested, gcode); // 1 1 1 1 77
    return 0;
}
