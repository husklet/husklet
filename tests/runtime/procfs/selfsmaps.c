// /proc/self/smaps is the per-mapping memory detail jemalloc, the JVM's NMT and heap profilers parse. Each
// region is a maps-style header line followed by "Key:  N kB" detail rows. Assert the invariants a correct
// smaps holds, not host addresses: at least one region carries a "Size:" and an "Rss:" line both in kB with
// Rss <= Size; "Pss:" is present; and the executable's own [heap] or an anonymous writable region shows a
// non-zero Rss (the process's live memory is represented). A stub smaps missing the detail rows fails.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    FILE *f = fopen("/proc/self/smaps", "r");
    if (!f) { printf("selfsmaps ok=0\n"); return 0; }
    // force a resident anonymous allocation so at least one region has Rss>0.
    volatile char *p = malloc(256 * 1024);
    for (int i = 0; i < 256 * 1024; i += 4096) p[i] = 1;

    char line[512];
    long cur_size = -1;
    int paired = 0;    // a region with both Size and Rss, Rss<=Size
    int has_pss = 0;
    int any_rss_pos = 0;
    while (fgets(line, sizeof line, f)) {
        long v;
        if (sscanf(line, "Size: %ld kB", &v) == 1) cur_size = v;
        else if (sscanf(line, "Rss: %ld kB", &v) == 1) {
            if (cur_size >= 0 && v <= cur_size) paired = 1;
            if (v > 0) any_rss_pos = 1;
        } else if (!strncmp(line, "Pss:", 4)) has_pss = 1;
    }
    fclose(f);
    int ok = paired && has_pss && any_rss_pos;
    printf("selfsmaps ok=%d\n", ok);
    return 0;
}
