// MAP_POPULATE prefaults an anonymous mapping so mincore reports every page resident immediately, and the
// pages read back zero. After MADV_DONTNEED the same pages are dropped and mincore reports them not
// resident (the mapping stays valid; a store re-populates on demand). Residency is reported as a count
// relative to the page total, so it is page-size neutral.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MAP_POPULATE
#define MAP_POPULATE 0x8000
#endif

static int resident_count(unsigned char *m, long pages, long ps) {
    unsigned char vec[64];
    if (pages > 64) pages = 64;
    if (mincore(m, ps * pages, vec) != 0) return -1;
    int n = 0;
    for (long i = 0; i < pages; i++) n += vec[i] & 1;
    return n;
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    long pages = 8;
    unsigned char *m = mmap(NULL, ps * pages, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS | MAP_POPULATE, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }

    int all_zero = 1;
    for (long i = 0; i < ps * pages; i++) if (m[i]) { all_zero = 0; break; }
    int res_after_populate = resident_count(m, pages, ps);

    int dn = madvise(m, ps * pages, MADV_DONTNEED) == 0;
    int res_after_dontneed = resident_count(m, pages, ps);

    m[ps * 3 + 5] = 0x1f;                 // re-fault a single page
    int store_ok = m[ps * 3 + 5] == 0x1f;

    munmap(m, ps * pages);
    printf("all_zero=%d populated_all=%d dontneed=%d dropped_all=%d store_ok=%d\n", all_zero,
           res_after_populate == (int)pages, dn, res_after_dontneed == 0, store_ok);
    return 0;
}
