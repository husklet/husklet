// MAP_FIXED over an existing mapping must split it: the middle page adopts the new protection
// and contents while the outer pages keep the old ones, mprotect on the middle page does not
// leak to its neighbours, and unmapping a sub-range leaves the flanks addressable.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    char *base = mmap(NULL, ps * 5, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    memset(base, 'A', ps * 5);
    char *mid = mmap(base + ps * 2, ps, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    int placed = (mid == base + ps * 2);
    int midzero = (base[ps * 2] == 0);
    int leftA = (base[ps] == 'A');
    int rightA = (base[ps * 3] == 'A');
    base[ps * 2] = 'B';

    int mp = mprotect(base + ps * 2, ps, PROT_READ);
    unsigned char vec[5];
    int mc = mincore(base, ps * 5, vec);
    int resident = (vec[1] & 1) && (vec[2] & 1) && (vec[3] & 1);

    int mu = munmap(base + ps * 2, ps);
    // flanks must still be usable after the hole punch
    base[ps] = 'L';
    base[ps * 3] = 'R';
    int flanks = (base[ps] == 'L' && base[ps * 3] == 'R');
    // remapping into the hole succeeds
    char *again = mmap(base + ps * 2, ps, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    int refilled = (again == base + ps * 2) && (again[0] == 0);
    int mu2 = munmap(base, ps * 5);
    printf("placed=%d midzero=%d leftA=%d rightA=%d mp=%d mc=%d resident=%d mu=%d flanks=%d refilled=%d mu2=%d\n",
           placed, midzero, leftA, rightA, mp, mc, resident, mu, flanks, refilled, mu2);
    return 0;
}
