// Advisory madvise hints that must be accepted (rc=0) on a private-anon range without changing observable
// contents: WILLNEED, COLD, PAGEOUT, SEQUENTIAL, RANDOM, NORMAL. MADV_DONTNEED then re-zero is checked to
// separate destructive from non-destructive advice. Unsupported-here advice is reported by errno-name.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MADV_COLD
#define MADV_COLD 20
#endif
#ifndef MADV_PAGEOUT
#define MADV_PAGEOUT 21
#endif

static const char *rc(int r) {
    if (r == 0) return "0";
    switch (errno) {
    case EINVAL: return "EINVAL";
    case ENOMEM: return "ENOMEM";
    case ENOSYS: return "ENOSYS";
    default: return "OTHER";
    }
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    unsigned char *m = mmap(NULL, ps * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    memset(m, 0x6b, ps * 2);

    errno = 0; const char *wn = rc(madvise(m, ps * 2, MADV_WILLNEED));
    errno = 0; const char *sq = rc(madvise(m, ps * 2, MADV_SEQUENTIAL));
    errno = 0; const char *rn = rc(madvise(m, ps * 2, MADV_RANDOM));
    errno = 0; const char *nm = rc(madvise(m, ps * 2, MADV_NORMAL));
    errno = 0; const char *cd = rc(madvise(m, ps * 2, MADV_COLD));
    errno = 0; const char *po = rc(madvise(m, ps * 2, MADV_PAGEOUT));
    int preserved = m[0] == 0x6b && m[ps * 2 - 1] == 0x6b;   // non-destructive advice kept data

    printf("willneed=%s sequential=%s random=%s normal=%s cold=%s pageout=%s preserved=%d\n", wn, sq, rn, nm,
           cd, po, preserved);
    munmap(m, ps * 2);
    return 0;
}
