// realloc growth chain preserves contents across arena/mmap transitions. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>

int main(void) {
    size_t cap = 8;
    unsigned char *p = malloc(cap);
    for (size_t i = 0; i < cap; i++) p[i] = (unsigned char)i;
    int ok = 1;
    for (int round = 0; round < 16; round++) {
        size_t old = cap; cap *= 2;
        p = realloc(p, cap);
        if (!p) { ok = 0; break; }
        for (size_t i = 0; i < old; i++) if (p[i] != (unsigned char)i) ok = 0;
        for (size_t i = old; i < cap; i++) p[i] = (unsigned char)i;
    }
    int d1 = ok;
    int d2 = cap == (size_t)8 << 16;
    unsigned char *q = realloc(p, 0); // realloc to 0 frees; may return NULL
    (void)q;
    printf("realloc_chain d1=%d d2=%d final=%zu\n", d1, d2, cap);
    return 0;
}
