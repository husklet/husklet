// /proc/self/statm is the seven-field page-count summary (size resident shared text lib data dt) that
// ps and lightweight monitors read instead of the heavier status file. Assert the field COUNT is exactly
// 7, all are non-negative integers, total size >= resident (RSS can never exceed the VM size), resident
// is at least one page (a running process has faulted its own image) and the text field is > 0. Catches a
// short, stubbed or self-inconsistent statm. Structural + self-consistent, oracle-neutral.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "pf.h"

int main(void) {
    char b[512];
    int n = pf_read("/proc/self/statm", b, sizeof b);
    long f[8];
    int nf = 0;
    for (char *t = strtok(b, " \n"); t && nf < 8; t = strtok(NULL, " \n")) f[nf++] = atol(t);
    int all_nonneg = 1;
    for (int i = 0; i < nf; i++) if (f[i] < 0) all_nonneg = 0;
    int ok = n > 0 && nf == 7 && all_nonneg && f[0] >= f[1] && f[1] >= 1 && f[3] > 0;
    printf("selfstatm ok=%d\n", ok);
    return 0;
}
