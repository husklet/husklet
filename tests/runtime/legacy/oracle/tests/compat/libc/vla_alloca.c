// Variable-length arrays + alloca stack allocation. Portable verdicts.
#include <stdio.h>
#include <alloca.h>
#include <string.h>

static int sum_vla(int n) {
    int a[n];
    for (int i = 0; i < n; i++) a[i] = i + 1;
    int s = 0; for (int i = 0; i < n; i++) s += a[i];
    return s;
}

int main(void) {
    int d1 = sum_vla(100) == 5050;
    char *p = alloca(64);
    memset(p, 'q', 64);
    int d2 = p[0] == 'q' && p[63] == 'q';
    // 2D VLA
    int r = 3, c = 4;
    int m[r][c];
    for (int i = 0; i < r; i++) for (int j = 0; j < c; j++) m[i][j] = i * c + j;
    int d3 = m[2][3] == 11 && m[0][0] == 0;
    printf("vla_alloca d1=%d d2=%d d3=%d\n", d1, d2, d3);
    return 0;
}
