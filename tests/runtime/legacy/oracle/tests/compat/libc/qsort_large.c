// qsort a 512-element reversed array + bsearch hit/miss. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>

static int cmp(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

int main(void) {
    enum { N = 512 };
    int v[N];
    for (int i = 0; i < N; i++) v[i] = N - i; // 512..1
    qsort(v, N, sizeof v[0], cmp);
    int sorted = 1; for (int i = 1; i < N; i++) if (v[i - 1] > v[i]) sorted = 0;
    int d1 = sorted && v[0] == 1 && v[N - 1] == N;
    int key = 300;
    int *hit = bsearch(&key, v, N, sizeof v[0], cmp);
    int d2 = hit && *hit == 300;
    int miss = 9999;
    int d3 = bsearch(&miss, v, N, sizeof v[0], cmp) == NULL;
    printf("qsort_large d1=%d d2=%d d3=%d\n", d1, d2, d3);
    return 0;
}
