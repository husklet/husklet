// Peer /proc/<pid> identity: after fork+prctl(PR_SET_NAME), a PARENT reading the child's /proc/<pid>/{stat,
// status,comm} must see the CHILD's task name and pid lineage, NOT the engine binary. An engine that answers
// a peer's comm from the host process name reports "hl-engine-linux" for every child (the recent peer-identity
// bug class). Deterministic: the child sets a fixed name and hand-shakes with the parent over a pipe so the
// read happens while the child is live. All facts agree native==engine, so the golden is environment-stable.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#include "pf.h"

static const char *NAME = "peerprobe9"; // <=15 bytes (TASK_COMM_LEN-1), distinct from any binary name

// parse the parenthesized comm out of a /proc/<pid>/stat line: "pid (comm) state ppid ..."
static void stat_comm(const char *line, char *out, size_t n) {
    const char *o = strchr(line, '('), *c = strrchr(line, ')');
    out[0] = 0;
    if (o && c && c > o) {
        size_t L = (size_t)(c - o - 1);
        if (L > n - 1) L = n - 1;
        memcpy(out, o + 1, L);
        out[L] = 0;
    }
}
static long stat_ppid(const char *line) {
    const char *c = strrchr(line, ')');
    if (!c) return -1;
    const char *q = c + 2;
    while (*q && *q != ' ') q++; // skip state
    return atol(q);
}

int main(void) {
    pid_t parent = getpid();
    int pfd[2], done[2];
    if (pipe(pfd) || pipe(done)) { printf("peerid ok=0\n"); return 0; }
    pid_t child = fork();
    if (child == 0) {
        prctl(PR_SET_NAME, (unsigned long)NAME, 0, 0, 0);
        char one = '1';
        write(pfd[1], &one, 1); // tell parent the name is set
        char c;
        read(done[0], &c, 1); // stay alive until parent has read our /proc
        _exit(0);
    }
    char r = 0;
    read(pfd[0], &r, 1);

    char p[64], b[8192], v[128];
    // stat: field1 pid, comm, field4 ppid
    snprintf(p, sizeof p, "/proc/%d/stat", child);
    pf_read(p, b, sizeof b);
    long s_pid = atol(b);
    char s_comm[64];
    stat_comm(b, s_comm, sizeof s_comm);
    long s_ppid = stat_ppid(b);
    // status: Pid/PPid/Name
    snprintf(p, sizeof p, "/proc/%d/status", child);
    pf_read(p, b, sizeof b);
    long st_pid = pf_line_val(b, "Pid:", v, sizeof v) ? atol(v) : -1;
    long st_ppid = pf_line_val(b, "PPid:", v, sizeof v) ? atol(v) : -1;
    char st_name[64] = "";
    pf_line_val(b, "Name:", st_name, sizeof st_name);
    // comm file
    char c_comm[64] = "";
    snprintf(p, sizeof p, "/proc/%d/comm", child);
    if (pf_read(p, c_comm, sizeof c_comm) > 0) {
        char *nl = strchr(c_comm, '\n');
        if (nl) *nl = 0;
    }

    int pid_ok = (s_pid == child) && (st_pid == child);
    int ppid_ok = (s_ppid == parent) && (st_ppid == parent);
    int comm_is_child = !strcmp(s_comm, NAME) && !strcmp(st_name, NAME) && !strcmp(c_comm, NAME);
    int comm_consistent = !strcmp(s_comm, st_name) && !strcmp(s_comm, c_comm);
    int comm_not_engine = !strstr(s_comm, "hl-engine") && !strstr(st_name, "hl-engine");

    // peer cgroup line format, maps, fd dir, cwd link
    snprintf(p, sizeof p, "/proc/%d/cgroup", child);
    pf_read(p, b, sizeof b);
    int cgroup_fmt = strncmp(b, "0::/", 4) == 0;
    snprintf(p, sizeof p, "/proc/%d/maps", child);
    int maps_ok = pf_read(p, b, sizeof b) > 0;
    struct stat ss;
    snprintf(p, sizeof p, "/proc/%d/fd", child);
    int fd_dir = stat(p, &ss) == 0 && S_ISDIR(ss.st_mode);
    char lk[256];
    snprintf(p, sizeof p, "/proc/%d/cwd", child);
    int cwd_link = readlink(p, lk, sizeof lk) > 0;

    printf("pid_ok=%d\n", pid_ok);
    printf("ppid_ok=%d\n", ppid_ok);
    printf("comm_is_child=%d\n", comm_is_child);
    printf("comm_consistent=%d\n", comm_consistent);
    printf("comm_not_engine=%d\n", comm_not_engine);
    printf("cgroup_fmt=%d\n", cgroup_fmt);
    printf("maps_ok=%d\n", maps_ok);
    printf("fd_dir=%d\n", fd_dir);
    printf("cwd_link=%d\n", cwd_link);

    char one = '1';
    write(done[1], &one, 1);
    waitpid(child, NULL, 0);
    // a reaped pid disappears from /proc
    snprintf(p, sizeof p, "/proc/%d/stat", child);
    printf("reaped_enoent=%d\n", pf_read(p, b, sizeof b) < 0);
    return 0;
}
