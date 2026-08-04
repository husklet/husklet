// Replicates the "htop shows every core at identical usage" bug (user-reported). htop/top compute
// per-core busy% as the DELTA of each cpuN line in /proc/stat between two samples. hl's /proc/stat
// used to emit every cpuN line as the aggregate host ticks split EVENLY (aggregate/ncpu), so every
// core's delta was identical -> all meters move in lockstep at the same %. A correct kernel reports
// each core's OWN jiffies, so a core running a busy loop shows a large user-delta while idle cores
// don't -> the deltas differ.
//
// This test pegs one core with a ~250ms busy loop between two /proc/stat reads and checks whether the
// per-core DELTA tuples are all identical. Verdict is a normalized 0/1 so it is byte-identical to the
// native oracle:
//   multicore   = 1 iff >=2 cpuN lines (host is multi-core; same on hl and the oracle)
//   deltas_differ = 1 iff the per-core (user,nice,system,idle) deltas are NOT all identical
// Real Linux + fixed hl: deltas_differ=1 (the busy core diverges from the idle ones).
// Buggy hl (aggregate/ncpu): deltas_differ=0 (every core advances by the same split share).
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <time.h>

#define MAXC 512

// Read the per-cpu lines of /proc/stat into tup[] as "user-nice-system-idle" strings; returns count.
static int read_percpu(char tup[][64], int max) {
    FILE *f = fopen("/proc/stat", "r");
    if (!f) return -1;
    char line[1024];
    int n = 0;
    while (fgets(line, sizeof line, f)) {
        // per-cpu lines are "cpuN " with a digit right after "cpu"; the aggregate "cpu " is skipped.
        if (strncmp(line, "cpu", 3) != 0 || line[3] < '0' || line[3] > '9') continue;
        const char *p = line;
        while (*p && *p != ' ') p++; // skip the "cpuN" token
        long user, nice, sys, idle;
        if (sscanf(p, " %ld %ld %ld %ld", &user, &nice, &sys, &idle) == 4 && n < max) {
            snprintf(tup[n], 64, "%ld-%ld-%ld-%ld", user, nice, sys, idle);
            n++;
        }
    }
    fclose(f);
    return n;
}

// Subtract field-wise a-b of two "u-n-s-i" strings into a canonical delta string.
static void delta(const char *a, const char *b, char *out, size_t outn) {
    long a0, a1, a2, a3, b0, b1, b2, b3;
    sscanf(a, "%ld-%ld-%ld-%ld", &a0, &a1, &a2, &a3);
    sscanf(b, "%ld-%ld-%ld-%ld", &b0, &b1, &b2, &b3);
    snprintf(out, outn, "%ld-%ld-%ld-%ld", a0 - b0, a1 - b1, a2 - b2, a3 - b3);
}

int main(void) {
    char before[MAXC][64], after[MAXC][64];
    int n0 = read_percpu(before, MAXC);
    // Peg a core for ~250ms so a correct kernel shows that core's user-delta diverging from idle cores.
    struct timespec t0, now;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    volatile unsigned long x = 0;
    for (;;) {
        for (int i = 0; i < 200000; i++) x += i;
        clock_gettime(CLOCK_MONOTONIC, &now);
        double ms = (now.tv_sec - t0.tv_sec) * 1000.0 + (now.tv_nsec - t0.tv_nsec) / 1e6;
        if (ms >= 250.0) break;
    }
    int n1 = read_percpu(after, MAXC);
    (void)x;

    int multicore = (n0 >= 2 && n1 == n0) ? 1 : 0;
    int deltas_differ = 0;
    if (multicore) {
        char d[MAXC][64];
        for (int i = 0; i < n0; i++) delta(after[i], before[i], d[i], sizeof d[0]);
        for (int i = 1; i < n0 && !deltas_differ; i++)
            if (strcmp(d[i], d[0]) != 0) deltas_differ = 1;
    }
    printf("procstat multicore=%d deltas_differ=%d\n", multicore, deltas_differ);
    return 0;
}
