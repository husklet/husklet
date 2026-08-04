// posix_memalign / aligned_alloc / memalign alignment guarantees. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>
#include <malloc.h>
#include <stdint.h>

static int aligned(void *p, size_t a) { return p != NULL && ((uintptr_t)p % a) == 0; }

int main(void) {
    void *p = NULL;
    int r = posix_memalign(&p, 256, 1000);
    int d1 = r == 0 && aligned(p, 256);
    free(p);
    void *q = aligned_alloc(64, 128); // size multiple of alignment
    int d2 = aligned(q, 64);
    free(q);
    void *m = memalign(128, 500);
    int d3 = aligned(m, 128);
    free(m);
    // Non-power-of-two alignment must be rejected by posix_memalign.
    void *bad = NULL;
    int r2 = posix_memalign(&bad, 3, 16);
    int d4 = r2 != 0;
    printf("align_alloc d1=%d d2=%d d3=%d d4=%d\n", d1, d2, d3, d4);
    return 0;
}
