// /proc/<pid>/stat field 3 (task run state) CROSS-PROCESS fidelity (#404). A child blocked in pause() is
// interruptible-sleep 'S' on real Linux, NOT 'R'; a busy child is 'R'. hl used to synthesize this field
// from the macOS process status (SRUN), which can't express "all threads asleep in a syscall", so a paused
// child wrongly read 'R' and LTP's TST_PROCESS_STATE_WAIT (pause01/pause02) timed out. This fixture is the
// same shape as that LTP poll: the PARENT reads the CHILD's /proc/<pid>/stat and must observe 'S' for a
// paused child and 'R' for a running one -- byte-identical to native Linux (verdict ok=1 either way).
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// Field 3 of /proc/<pid>/stat = the state char, immediately after the ") " that closes the comm field.
// comm can itself contain spaces/parens, so key off the LAST ')' (Linux's own canonical parse rule).
static char read_state(int pid) {
    char path[64], b[512];
    snprintf(path, sizeof path, "/proc/%d/stat", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return '?';
    int n = (int)read(fd, b, sizeof b - 1);
    close(fd);
    if (n <= 0) return '?';
    b[n] = 0;
    char *rp = strrchr(b, ')');
    if (!rp) return '?';
    char *s = rp + 1;
    while (*s == ' ') s++;
    return *s;
}
// Poll the child's state until it reaches `target` (or ~timeout_ms elapses); return the last state seen.
static char wait_state(int pid, char target, int timeout_ms) {
    char st = '?';
    for (int i = 0; i < timeout_ms / 5; i++) {
        st = read_state(pid);
        if (st == target) return st;
        struct timespec ts = {0, 5 * 1000 * 1000}; // 5ms
        nanosleep(&ts, NULL);
    }
    return st;
}

int main(void) {
    // Test 1: a child parked in pause() must be observed as 'S' (interruptible sleep) by its parent.
    pid_t paused = fork();
    if (paused == 0) { pause(); _exit(0); }
    char s_paused = wait_state(paused, 'S', 3000);
    kill(paused, SIGKILL);
    waitpid(paused, NULL, 0);

    // Test 2: a child spinning in guest code must be observed as 'R' (running/runnable).
    pid_t running = fork();
    if (running == 0) {
        volatile unsigned long x = 0;
        for (;;) x++;
    }
    char s_run = wait_state(running, 'R', 3000);
    kill(running, SIGKILL);
    waitpid(running, NULL, 0);

    int ok = (s_paused == 'S') && (s_run == 'R');
    printf("procstate ok=%d\n", ok);
    return 0;
}
