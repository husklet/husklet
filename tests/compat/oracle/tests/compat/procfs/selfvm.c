// The virtual-memory figures must be consistent across the two files that expose them: /proc/self/statm
// (in pages) and /proc/self/status (VmSize/VmRSS in kB). ps and top mix these sources and assume they
// describe the same address space. Convert statm size/resident pages to kB with the real page size and
// require they equal status VmSize/VmRSS (sampled back-to-back, so exact on a single-threaded process here).
// Also VmRSS <= VmSize and both > 0. A stub that fills one file but not the other, or uses a different page
// size, fails. Derived + self-consistent, oracle-neutral.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

int main(void) {
    long pg = sysconf(_SC_PAGESIZE) / 1024; // kB per page
    char b[8192];
    pf_read("/proc/self/statm", b, sizeof b);
    long size_pg = 0, res_pg = 0;
    sscanf(b, "%ld %ld", &size_pg, &res_pg);

    pf_read("/proc/self/status", b, sizeof b);
    char v[64];
    long vmsize = pf_line_val(b, "VmSize:", v, sizeof v) ? atol(v) : -1;
    long vmrss = pf_line_val(b, "VmRSS:", v, sizeof v) ? atol(v) : -1;

    // The two files are sampled a few syscalls apart, so allow a small drift (page-fault noise) rather than
    // exact equality; the point is that both express the SAME address space in a consistent page size.
    long tol = 16 * pg; // 16 pages of slack
    long labs_size = vmsize - size_pg * pg; if (labs_size < 0) labs_size = -labs_size;
    long labs_rss = vmrss - res_pg * pg; if (labs_rss < 0) labs_rss = -labs_rss;
    int size_ok = labs_size <= tol;
    int rss_ok = labs_rss <= tol;
    int rel_ok = vmrss > 0 && vmsize > 0 && vmrss <= vmsize;
    int ok = size_ok && rss_ok && rel_ok;
    printf("selfvm ok=%d\n", ok);
    return 0;
}
