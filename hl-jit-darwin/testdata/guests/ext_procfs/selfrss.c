// Self memory-footprint fidelity: top/htop/ps read a process's resident-set size from /proc/<pid>/status
// (VmRSS), /proc/<pid>/statm (field 2 = resident pages) and /proc/<pid>/stat (field 24 = rss pages). A
// real Linux process ALWAYS has a non-zero VmRSS (it has faulted at least its own code/stack). hl computed
// the SELF pid's RSS from the guest's tracked anon charge, which is 0 for a process that has only its
// static image resident -- so top/htop/ps showed this process at RES=0, a hl-only divergence from real
// docker (a PEER pid already reported a live resident size; only self read 0). This probe reads all three
// resident figures and asserts each is > 0 (the invariant that FAILS on the pre-fix engine). We do NOT
// assert exact cross-file equality: VmRSS / statm-resident / stat-rss are sampled at slightly different
// instants and in the running process's own page-size units, so their reconstructed magnitudes can drift
// by a page or two even on real Linux -- the load-bearing, non-flaky property is simply non-zero. Host-
// independent verdict: ok=1 on real Linux and a correct hl; the pre-fix engine printed ok=0 (rss=0). Run
// bare on both Linux engines so the /proc synth fires.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    // /proc/self/status VmRSS (kB)
    long vmrss = -1;
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) { printf("selfrss ok=0 nostatus\n"); return 0; }
    char line[256];
    while (fgets(line, sizeof line, f))
        if (!strncmp(line, "VmRSS:", 6)) { vmrss = strtol(line + 6, NULL, 10); break; }
    fclose(f);
    if (vmrss <= 0) { printf("selfrss ok=0 vmrss=%ld\n", vmrss); return 0; }

    // /proc/self/statm field 2 = resident pages
    long sm_size = -1, sm_res = -1;
    f = fopen("/proc/self/statm", "r");
    if (!f || fscanf(f, "%ld %ld", &sm_size, &sm_res) != 2) { printf("selfrss ok=0 nostatm\n"); if (f) fclose(f); return 0; }
    fclose(f);
    if (sm_res <= 0) { printf("selfrss ok=0 statm_res=%ld\n", sm_res); return 0; }

    // /proc/self/stat field 24 = rss pages (fields are space-separated after the "(comm)" token)
    long st_rss = -1;
    f = fopen("/proc/self/stat", "r");
    if (!f) { printf("selfrss ok=0 nostat\n"); return 0; }
    static char sbuf[4096];
    size_t nr = fread(sbuf, 1, sizeof sbuf - 1, f);
    fclose(f);
    sbuf[nr] = 0;
    char *rp = strrchr(sbuf, ')'); // last ')' ends the comm field; field 3 (state) begins at rp+2
    if (rp) {
        char *p = rp + 2;
        int field = 3; // p is at the start of field 3; walk token-by-token to field 24 (rss, in pages)
        while (*p && field < 24) {
            while (*p && *p != ' ') p++; // skip the current field's token
            while (*p == ' ') p++;       // skip the separating space(s)
            field++;
        }
        if (field == 24) st_rss = strtol(p, NULL, 10);
    }
    if (st_rss <= 0) { printf("selfrss ok=0 stat_rss=%ld\n", st_rss); return 0; }

    (void)sm_size;
    printf("selfrss ok=1\n");
    return 0;
}
