// x86/glibc fixture: a STATIC-PIE glibc guest that walks the classic libc surface — heap
// (malloc/realloc/free), stdio formatting (snprintf round-trip), string/mem ops, qsort with a
// comparator through a function pointer, strtod, and a clock_gettime sanity read — then prints
// "glibc ok". Everything checked is deterministic; the golden only matches on full success, so a
// broken glibc startup (auxv/TLS/ifunc resolution) or any of the above mislowering fails the case.
// Build (see build.sh): x86_64-linux-gnu-gcc -O2 -static-pie
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static int cmp_int(const void *a, const void *b) {
    return *(const int *)a - *(const int *)b;
}

int main(void) {
    int bad = 0;

    // heap: grow a buffer through realloc and checksum it.
    unsigned char *p = malloc(64);
    if (!p) return 1;
    for (int i = 0; i < 64; i++)
        p[i] = (unsigned char)(i * 3);
    p = realloc(p, 4096);
    if (!p) return 1;
    unsigned sum = 0;
    for (int i = 0; i < 64; i++)
        sum += p[i];
    bad |= (sum != 6048);
    free(p);

    // stdio: snprintf/ sscanf round-trip.
    char buf[64];
    snprintf(buf, sizeof buf, "%d/%x/%s", 12345, 0xbeef, "tail");
    bad |= (strcmp(buf, "12345/beef/tail") != 0);
    int d = 0, x = 0;
    bad |= (sscanf(buf, "%d/%x/", &d, &x) != 2 || d != 12345 || x != 0xbeef);

    // qsort through a function pointer.
    int v[8] = {7, 1, 6, 2, 5, 3, 4, 0};
    qsort(v, 8, sizeof v[0], cmp_int);
    for (int i = 0; i < 8; i++)
        bad |= (v[i] != i);

    // strtod parses and the value survives arithmetic.
    double dv = strtod("2.5e2", NULL);
    bad |= (dv != 250.0);

    // clock_gettime returns a sane (non-negative, post-2000) wall time via the vdso/syscall path.
    struct timespec ts;
    bad |= (clock_gettime(CLOCK_REALTIME, &ts) != 0 || ts.tv_sec < 946684800);

    if (bad)
        return 1;
    puts("glibc ok");
    return 0;
}
