// brk/sbrk growth and shrink. sbrk(0) reports the break; growing by a page yields a writable, zero region
// whose old-break address is usable; shrinking returns the break to its origin. All facts are derived
// (writability, byte contents, monotonic direction), never raw break addresses.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    void *base = sbrk(0);
    if (base == (void *)-1) { printf("sbrk unsupported\n"); return 2; }

    void *grown = sbrk(ps);
    int grew = grown == base;                 // sbrk returns the PREVIOUS break
    void *after = sbrk(0);
    int moved_up = (char *)after - (char *)base == ps;

    unsigned char *region = grown;
    int fresh_zero = 1;
    for (long i = 0; i < ps; i++) if (region[i]) { fresh_zero = 0; break; }
    memset(region, 0xab, ps);
    int writable = region[0] == 0xab && region[ps - 1] == 0xab;

    void *shrunk = sbrk(-ps);
    int shrank = shrunk == after;
    void *end = sbrk(0);
    int back_to_base = end == base;

    printf("grew=%d moved_up=%d fresh_zero=%d writable=%d shrank=%d back_to_base=%d\n", grew, moved_up,
           fresh_zero, writable, shrank, back_to_base);
    return 0;
}
