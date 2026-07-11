// CONTRACT for the Chrome peer-procfs env switches DD_HIDE_CHROME_PROCFILES / DD_PROC_CHROME_MODE
// (vfs.c proc_open peer branch). These are load-bearing for Chrome (which polls peer /proc/<pid>/status
// + statm for its memory monitor) but were previously undocumented and had no test gate. This fixture
// pins each switch's observable procfs effect so the behavior is contractual, not "hidden":
//
//   PARENT forks a CHILD that parks in pause(), then reads the CHILD's /proc/<pid>/status:
//     * default (no switch)          -> the file OPENS and reads a NON-EMPTY status body (open=1 nonempty=1)
//     * DD_HIDE_CHROME_PROCFILES=1   -> the peer status/statm are hidden: open fails ENOENT   (open=0 nonempty=0)
//     * DD_PROC_CHROME_MODE=empty    -> the file OPENS but the body is EMPTY (0 bytes)         (open=1 nonempty=0)
//
// These are dd-only env switches (no native form), so cases are golden-checked, not oracle-diffed.
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// Try to open + read the peer's /proc/<pid>/status. Returns:
//   -1  open failed (hidden / ENOENT)
//    0  opened, empty body
//   >0  opened, N bytes read
static int read_peer_status(int pid) {
    char path[64], b[4096];
    snprintf(path, sizeof path, "/proc/%d/status", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    int total = 0, r;
    while (total < (int)sizeof b && (r = (int)read(fd, b + total, sizeof b - total)) > 0)
        total += r;
    close(fd);
    return total;
}

int main(void) {
    pid_t child = fork();
    if (child == 0) {
        pause();
        _exit(0);
    }

    // Poll: in the visible modes the peer registers within a few ms, so open succeeds quickly (n>=0).
    // In the hidden mode open never succeeds, so bail after the window and report open=0.
    int n = -1;
    for (int i = 0; i < 60; i++) {
        n = read_peer_status(child);
        if (n >= 0) break;
        struct timespec ts = {0, 5 * 1000 * 1000}; // 5ms
        nanosleep(&ts, NULL);
    }

    kill(child, SIGKILL);
    waitpid(child, NULL, 0);

    int open_ok = n >= 0;
    int nonempty = n > 0;
    printf("chrome_procswitch open=%d nonempty=%d\n", open_ok, nonempty);
    return 0;
}
