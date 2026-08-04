// The stale-/proc/self-identity hunter. After fork(2) the child is a NEW process: its /proc/self must
// report the CHILD's pid, not the parent's cached identity. Cross-check three ways:
//   * in the child, /proc/self/stat field 1 and /proc/self/status "Pid:" both equal the child's getpid()
//     and differ from the parent pid it inherited;
//   * from the parent, reading the child's /proc/<child>/stat gives field 1 == child pid and field 4
//     (ppid) == the parent's own pid.
// An engine that synthesizes /proc/self from a process-global identity (the recent bug class) makes the
// forked child read the parent's pid and fails. Derived + deterministic.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>
#include "pf.h"

static long stat_field(const char *path, int idx /*1-based*/) {
    char b[4096];
    if (pf_read(path, b, sizeof b) <= 0) return -1;
    int nf = 0;
    long val = -1;
    for (char *t = strtok(b, " \n"); t; t = strtok(NULL, " \n")) {
        if (++nf == idx) { val = atol(t); break; }
    }
    return val;
}

int main(void) {
    pid_t parent = getpid();
    int pfd[2];
    if (pipe(pfd) != 0) { printf("forkself ok=0\n"); return 0; }
    pid_t child = fork();
    if (child == 0) {
        char b[8192], v[64];
        long stat_pid = stat_field("/proc/self/stat", 1);
        pf_read("/proc/self/status", b, sizeof b);
        long status_pid = pf_line_val(b, "Pid:", v, sizeof v) ? atol(v) : -1;
        int child_ok = stat_pid == getpid() && status_pid == getpid() && getpid() != parent;
        char one = child_ok ? '1' : '0';
        write(pfd[1], &one, 1);
        _exit(0);
    }
    char cr = '0';
    read(pfd[0], &cr, 1);
    char path[64];
    snprintf(path, sizeof path, "/proc/%d/stat", child);
    long peer_pid = stat_field(path, 1);
    long peer_ppid = stat_field(path, 4);
    int parent_view_ok = peer_pid == child && peer_ppid == parent;
    waitpid(child, NULL, 0);
    int ok = cr == '1' && parent_view_ok;
    printf("forkself ok=%d\n", ok);
    return 0;
}
