// /proc/<pid>/stat field 3 + /proc/<pid>/status State: CROSS-PROCESS fidelity for a FUTEX_WAIT blocker.
// A child parked in futex(FUTEX_WAIT) is interruptible-sleep 'S' on real Linux, NOT 'R' -- hl used to
// synthesize the state from the macOS process status (SRUN, which can't express "all threads asleep in a
// futex"), so a futex-blocked child wrongly read 'R', hiding blocked threads from monitors/deadlock
// diagnostics. This fixture is the same shape as procstate.c but the child blocks in FUTEX_WAIT instead of
// pause(): the PARENT reads the CHILD's /proc/<pid>/stat AND /proc/<pid>/status and must observe 'S' for
// both -- byte-identical to native Linux (verdict ok=1).
#include <fcntl.h>
#include <linux/futex.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// Field 3 of /proc/<pid>/stat = the state char, right after the ") " that closes the comm field. comm can
// contain spaces/parens, so key off the LAST ')' (Linux's own canonical parse rule).
static char stat_state(int pid) {
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

// /proc/<pid>/status "State:\t<c> (name)" -> the single state char.
static char status_state(int pid) {
    char path[64], b[4096];
    snprintf(path, sizeof path, "/proc/%d/status", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return '?';
    int n = (int)read(fd, b, sizeof b - 1);
    close(fd);
    if (n <= 0) return '?';
    b[n] = 0;
    char *p = strstr(b, "State:");
    if (!p) return '?';
    p += 6;
    while (*p == ' ' || *p == '\t') p++;
    return *p;
}

// Poll the child's stat state until it reaches `target` (or ~timeout_ms elapses); return the last seen.
static char wait_stat(int pid, char target, int timeout_ms) {
    char st = '?';
    for (int i = 0; i < timeout_ms / 5; i++) {
        st = stat_state(pid);
        if (st == target) return st;
        struct timespec ts = {0, 5 * 1000 * 1000}; // 5ms
        nanosleep(&ts, NULL);
    }
    return st;
}

int main(void) {
    // A word the child FUTEX_WAITs on. It stays 0 and is never woken, so the child parks indefinitely --
    // exactly the "blocked in a futex" condition monitors observe.
    static volatile uint32_t word;
    word = 0;

    pid_t child = fork();
    if (child == 0) {
        // FUTEX_WAIT while *word == 0: since nobody ever wakes it, this parks forever (until SIGKILL).
        syscall(SYS_futex, (uint32_t *)&word, FUTEX_WAIT, 0, NULL, NULL, 0);
        _exit(0);
    }

    char s_stat = wait_stat(child, 'S', 3000);
    char s_status = status_state(child);
    kill(child, SIGKILL);
    waitpid(child, NULL, 0);

    int ok = (s_stat == 'S') && (s_status == 'S');
    printf("futexstate ok=%d\n", ok);
    return 0;
}
