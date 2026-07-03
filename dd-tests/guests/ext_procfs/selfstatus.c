// /proc/self/status — the human-readable field table. Assert the fields tools actually read: Name, State,
// Tgid/Pid(==getpid)/PPid, Uid/Gid (4 columns), VmSize/VmRSS with kB units, Threads, and the signal masks.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

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
    int ok = n > 0 && has_name && has_state && pid_ok && has_ppid && has_threads && uid_cols == 4 &&
             vmrss_kb && vmsize_kb && has_sig;
    printf("selfstatus ok=%d\n", ok);
    return 0;
}
