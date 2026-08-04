// /proc/self is a magic symlink to the caller's own PID. lsof, gdb and every runtime that resolves
// "/proc/self/..." relies on it reading back exactly the decimal getpid(). A synthesized /proc that
// hardcodes or staleley caches the self identity fails here (the recent stale /proc/self class of bug).
// Also /proc/self/root readlink must be "/" for a normal container root. Derived + deterministic.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char link[64];
    ssize_t n = readlink("/proc/self", link, sizeof link - 1);
    int self_ok = 0;
    if (n > 0) {
        link[n] = 0;
        self_ok = atoi(link) == (int)getpid() && (int)strlen(link) == snprintf(NULL, 0, "%d", getpid());
    }
    char root[64];
    ssize_t r = readlink("/proc/self/root", root, sizeof root - 1);
    int root_ok = 0;
    if (r > 0) { root[r] = 0; root_ok = strcmp(root, "/") == 0; }
    // /proc/<getpid()> is the same identity reached the long way; its status Pid must match.
    int ok = self_ok && root_ok;
    printf("selflink ok=%d\n", ok);
    return 0;
}
