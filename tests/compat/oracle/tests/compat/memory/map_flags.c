// mmap MAP_* placement/behavior flags must be honored (or cleanly refused), not silently dropped. On a
// Linux host these bits carry real kernel semantics: MAP_HUGETLB with an unsupported huge-page size must
// fail EINVAL (a flag that is stripped to an ordinary mapping fake-succeeds -- a JIT/DB believes it got
// huge pages it did not), while MAP_NORESERVE (overcommit a mapping larger than RAM+swap), MAP_STACK
// (pthread stack hint), MAP_POPULATE (prefault), and MAP_LOCKED (small wire) all succeed and read back
// zero. Facts are page-size and RAM neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MAP_HUGETLB
#define MAP_HUGETLB 0x40000
#endif
#ifndef MAP_STACK
#define MAP_STACK 0x20000
#endif
#ifndef MAP_POPULATE
#define MAP_POPULATE 0x8000
#endif
#ifndef MAP_LOCKED
#define MAP_LOCKED 0x2000
#endif
#ifndef MAP_NORESERVE
#define MAP_NORESERVE 0x4000
#endif

static int maps_and_zero(int flags, size_t len) {
    void *m = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | flags, -1, 0);
    if (m == MAP_FAILED) return 0;
    int ok = ((volatile unsigned char *)m)[0] == 0 && ((volatile unsigned char *)m)[len - 1] == 0;
    munmap(m, len);
    return ok;
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    size_t len = (size_t)ps * 8;

    // MAP_HUGETLB with an absurd 16 GiB huge-page size (log2 = 34) is never configured -> EINVAL on both
    // native and engine (before forwarding, the engine stripped the flag and fake-succeeded).
    errno = 0;
    void *huge = mmap(NULL, (size_t)2 * 1024 * 1024, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | (34 << 26), -1, 0);
    int hugetlb_einval = huge == MAP_FAILED && errno == EINVAL;
    if (huge != MAP_FAILED) munmap(huge, (size_t)2 * 1024 * 1024);

    // A 16 GiB MAP_NORESERVE mapping (larger than RAM+swap) succeeds under overcommit and touches sparsely.
    size_t big = (size_t)16 * 1024 * 1024 * 1024;
    void *nr = mmap(NULL, big, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    int noreserve = 0;
    if (nr != MAP_FAILED) {
        ((volatile unsigned char *)nr)[big - ps] = 7;
        noreserve = ((volatile unsigned char *)nr)[big - ps] == 7;
        munmap(nr, big);
    }

    int stack = maps_and_zero(MAP_STACK, len);
    int populate = maps_and_zero(MAP_POPULATE, len);
    int locked = maps_and_zero(MAP_LOCKED, len);

    printf("map_flags hugetlb_einval=%d noreserve=%d stack=%d populate=%d locked=%d\n", hugetlb_einval,
           noreserve, stack, populate, locked);
    return hugetlb_einval && noreserve && stack && populate && locked ? 0 : 1;
}
