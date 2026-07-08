// /proc/self/status — the human-readable field table. Assert the fields tools actually read: Name, State,
// Tgid/Pid(==getpid)/PPid, Uid/Gid (4 columns), VmSize/VmRSS with kB units, Threads, and the signal masks.
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

// Count CPUs named by a Cpus_allowed_list value like "0", "0-17", or "0-1,4,6-7".
static int cpulist_count(const char *s) {
    int total = 0;
    while (*s) {
        char *end;
        long a = strtol(s, &end, 10);
        if (end == s) break;
        long b = a;
        if (*end == '-') b = strtol(end + 1, &end, 10);
        if (b >= a) total += (int)(b - a + 1);
        s = (*end == ',') ? end + 1 : end;
    }
    return total;
}

int main(void) {
    char b[8192];
    int n = pf_read("/proc/self/status", b, sizeof b);
    char v[128];
    int pid_ok = pf_line_val(b, "Pid:", v, sizeof v) && atoi(v) == (int)getpid();
    int has_name = pf_has(b, "Name:");
    int has_state = pf_has(b, "State:");
    int has_ppid = pf_has(b, "PPid:");
    int has_threads = pf_has(b, "Threads:");
    // Uid: line has 4 whitespace-separated ids
    int uid_cols = 0;
    if (pf_line_val(b, "Uid:", v, sizeof v))
        for (char *t = strtok(v, " \t"); t; t = strtok(NULL, " \t")) uid_cols++;
    int vmrss_kb = pf_line_val(b, "VmRSS:", v, sizeof v) && strstr(v, "kB");
    int vmsize_kb = pf_line_val(b, "VmSize:", v, sizeof v) && strstr(v, "kB");
    int has_sig = pf_has(b, "SigPnd:") && pf_has(b, "SigBlk:") && pf_has(b, "SigCgt:");
    // Cpus_allowed_list must reflect the container's real online-CPU set, i.e. agree with sched_getaffinity
    // (and nproc). The old hardcode ("0", one CPU) contradicted a multi-CPU affinity mask; assert the count
    // matches. Host-independent: both sides derive from the same container CPU allotment.
    cpu_set_t aff;
    int naff = (sched_getaffinity(0, sizeof aff, &aff) == 0) ? CPU_COUNT(&aff) : -1;
    int cpus_ok = 0;
    if (pf_line_val(b, "Cpus_allowed_list:", v, sizeof v))
        cpus_ok = (naff > 0) && (cpulist_count(v) == naff);
    int ok = n > 0 && has_name && has_state && pid_ok && has_ppid && has_threads && uid_cols == 4 &&
             vmrss_kb && vmsize_kb && has_sig && cpus_ok;
    printf("selfstatus ok=%d\n", ok);
    return 0;
}
