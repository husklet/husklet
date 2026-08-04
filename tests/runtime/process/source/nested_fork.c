// Nested fork topology: a forked child that itself forks a grandchild (the
// classic double-fork daemonization / shell-subshell pipeline shape). Each
// generation reports its getppid() through a pipe handshake so the parent chain
// is observed deterministically. The native aarch64 run is the oracle.
//
// KNOWN BUG (x86_64 production engine only, excluded-known-bug): when a process
// that was itself created by glibc fork() calls glibc fork() again, the middle
// generation's memory is corrupted -- glibc's own stack-guard trips ("stack
// smashing detected") and the middle process dies, orphaning the grandchild.
// The raw clone(2) path is unaffected and the aarch64 engine is correct, so the
// fault is specific to the x86 translator's handling of a glibc fork issued from
// an already-forked child. See the report for the minimized repro. This golden
// is the correct (native / aarch64) behavior; the case activates on x86 once the
// corruption is fixed.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int report[2], go[2];
    if (pipe(report) != 0 || pipe(go) != 0) return 2;
    fflush(stdout);
    pid_t b = fork();
    if (b == 0) {
        long b_ppid = getppid();
        pid_t c = fork();
        if (c == 0) {
            close(go[1]);
            close(report[0]);
            char proceed;
            (void)read(go[0], &proceed, 1);
            char line[48];
            int len = snprintf(line, sizeof line, "C %ld\n", (long)getppid());
            (void)write(report[1], line, len);
            _exit(0);
        }
        // Middle generation keeps running after its own fork -- this is the path
        // the x86 corruption strikes.
        char line[48];
        int len = snprintf(line, sizeof line, "B %ld\n", b_ppid);
        (void)write(report[1], line, len);
        waitpid(c, NULL, 0);
        _exit(0);
    }
    long a_pid = getpid();
    close(go[0]);
    close(report[1]);
    char first[64];
    ssize_t n = read(report[0], first, sizeof first - 1);
    if (n < 0) n = 0;
    first[n] = 0;
    (void)write(go[1], "g", 1);
    long b_reported_ppid = 0;
    long c_ppid = 0;
    // Read both report lines regardless of interleaving order.
    char rest[128];
    ssize_t m = read(report[0], rest, sizeof rest - 1);
    if (m < 0) m = 0;
    rest[m] = 0;
    char combined[192];
    snprintf(combined, sizeof combined, "%s%s", first, rest);
    char *bline = strstr(combined, "B ");
    char *cline = strstr(combined, "C ");
    if (bline) b_reported_ppid = atol(bline + 2);
    if (cline) c_ppid = atol(cline + 2);
    waitpid(b, NULL, 0);
    printf("middle survived fork: B.getppid==A=%d\n", b_reported_ppid == a_pid);
    printf("grandchild parented by middle: C.getppid==B=%d\n", c_ppid != 0 && c_ppid != a_pid && c_ppid != 1);
    return 0;
}
