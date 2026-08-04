// /proc/stat — the system-wide CPU/interrupt/context/boot counters. host_cpu_ticks is live, so assert the
// STRUCTURE exactly: an aggregate "cpu " line with >=10 numeric columns, at least one per-CPU "cpu0" line,
// and the intr/ctxt/btime/processes/procs_running/procs_blocked keys with btime>0 and procs_running>=1.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "pf.h"

int main(void) {
    char b[8192];
    int n = pf_read("/proc/stat", b, sizeof b);
    int cpu_agg = !strncmp(b, "cpu  ", 5);
    // count columns on the aggregate line
    int cols = 0;
    if (cpu_agg) {
        const char *nl = strchr(b, '\n');
        char line[256]; int L = nl ? (int)(nl - b) : 0; if (L > 255) L = 255;
        memcpy(line, b, (size_t)L); line[L] = 0;
        for (char *t = strtok(line + 3, " "); t; t = strtok(NULL, " ")) cols++;
    }
    int has_cpu0 = pf_has(b, "\ncpu0 ");
    char v[64];
    long btime = pf_line_val(b, "btime ", v, sizeof v) ? atol(v) : 0;
    long running = pf_line_val(b, "procs_running ", v, sizeof v) ? atol(v) : 0;
    int has_intr = pf_line_val(b, "intr ", v, sizeof v);
    int has_ctxt = pf_line_val(b, "ctxt ", v, sizeof v);
    int has_procs = pf_line_val(b, "processes ", v, sizeof v);
    int has_blocked = pf_line_val(b, "procs_blocked ", v, sizeof v);
    int ok = n > 0 && cpu_agg && cols >= 10 && has_cpu0 && has_intr && has_ctxt && has_procs &&
             has_blocked && btime > 0 && running >= 1;
    printf("pstat ok=%d\n", ok);
    return 0;
}
