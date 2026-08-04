// MADV_DONTNEED over the MIDDLE of a private-anon range must re-zero only the advised pages and leave the
// surrounding pages intact. Probes partial-range madvise bookkeeping (adjacent pages must not be lost) and
// that a subsequent store into the re-zeroed hole lands normally.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    unsigned char *m = mmap(NULL, ps * 4, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    for (int i = 0; i < 4; i++) memset(m + i * ps, 0xd0 + i, ps);

    int rc = madvise(m + ps, ps * 2, MADV_DONTNEED);

    int edge0 = m[0] == 0xd0 && m[ps - 1] == 0xd0;
    int edge3 = m[3 * ps] == 0xd3 && m[4 * ps - 1] == 0xd3;
    int hole_zero = 1;
    for (long i = ps; i < 3 * ps; i++) if (m[i] != 0) { hole_zero = 0; break; }

    m[ps + 7] = 0x42;                 // store into the re-zeroed hole
    int store_lands = m[ps + 7] == 0x42;

    munmap(m, ps * 4);
    printf("rc=%d edge0=%d edge3=%d hole_zero=%d store_lands=%d\n", rc, edge0, edge3, hole_zero, store_lands);
    return 0;
}
