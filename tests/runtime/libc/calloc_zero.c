// calloc zeroing (small + large mmap-backed) + realloc(NULL)/shrink semantics. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    size_t n = 4096;
    unsigned char *c = calloc(n, 1);
    if (!c) return 2;
    int zero = 1;
    for (size_t i = 0; i < n; i++)
        if (c[i]) zero = 0;
    free(c);
    // Large calloc (2 MiB) is mmap-backed and must be fully zeroed.
    size_t big = 2u << 20;
    unsigned char *lc = calloc(big, 1);
    if (!lc) return 2;
    int bigzero = 1;
    for (size_t i = 0; i < big; i += 4093)
        if (lc[i]) bigzero = 0;
    lc[big - 1] = 0xAA;
    int wr = lc[big - 1] == 0xAA;
    free(lc);
    // realloc(NULL, n) behaves like malloc.
    char *a = realloc(NULL, 10);
    if (!a) return 2;
    int d_rn = 1;
    strcpy(a, "abc");
    char *shrunk = realloc(a, 4); // shrink preserves prefix
    if (!shrunk) {
        free(a);
        return 2;
    }
    a = shrunk;
    int d_shrink = strcmp(a, "abc") == 0;
    free(a);
    printf("calloc_zero zero=%d bigzero=%d wr=%d rn=%d shrink=%d\n", zero, bigzero, wr, d_rn, d_shrink);
    return 0;
}
