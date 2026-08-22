#include <stdio.h>
#include <stdlib.h>

static int cmp(const void *a, const void *b) {
    long x = *(const long *)a, y = *(const long *)b;
    return (x > y) - (x < y);
}

int main(void) {
    static long v[100000];
    for (int i = 0; i < 100000; i++)
        v[i] = 99999 - i;
    qsort(v, 100000, sizeof(long), cmp);
    long s = 0;
    for (int i = 0; i < 100000; i++)
        s += v[i];
    printf("QS %ld %ld %ld\n", v[0], v[99999], s);
    return 0;
}
