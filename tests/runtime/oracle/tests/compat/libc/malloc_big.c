// malloc/calloc/realloc/free large + arena growth; usable_size, malloc_trim, mallopt. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <malloc.h>

int main(void) {
    // Force mmap threshold small so a big block uses mmap; still deterministic API contract.
    int mo = mallopt(M_MMAP_THRESHOLD, 65536);
    size_t big = 1u << 20; // 1 MiB
    unsigned char *p = malloc(big);
    int d1 = p != NULL;
    memset(p, 0xAB, big);
    int d2 = p[0] == 0xAB && p[big - 1] == 0xAB;
    int d3 = malloc_usable_size(p) >= big;
    p = realloc(p, big * 2);
    int d4 = p != NULL && p[big - 1] == 0xAB; // preserved on grow
    free(p);
    // Many small allocations grow the main arena via brk.
    void *v[512]; int ok = 1;
    for (int i = 0; i < 512; i++) { v[i] = malloc(128); if (!v[i]) ok = 0; memset(v[i], i & 0xff, 128); }
    for (int i = 0; i < 512; i++) { unsigned char *b = v[i]; if (b[0] != (i & 0xff)) ok = 0; }
    for (int i = 0; i < 512; i++) free(v[i]);
    malloc_trim(0); // must not crash; return value not asserted
    printf("malloc_big mo=%d d1=%d d2=%d d3=%d d4=%d small=%d\n", mo, d1, d2, d3, d4, ok);
    return 0;
}
