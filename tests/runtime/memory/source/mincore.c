// mincore(2): residency vector for a mapping. After touching pages they should read as resident.
// The exact vector semantics are OS-specific -> Linux-only, native oracle.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    int n = 8;
    size_t len = ps * n;
    char *m = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    // touch every other page
    for (int i = 0; i < n; i += 2) m[i * ps] = 1;
    unsigned char vec[8] = {0};
    int rc = mincore(m, len, vec);
    int resident = 0;
    for (int i = 0; i < n; i += 2) resident += (vec[i] & 1);
    munmap(m, len);
    printf("mincore rc=%d resident=%d\n", rc, resident);
    return 0;
}
