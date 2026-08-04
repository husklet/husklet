// mremap flag validation is a fixed-ABI contract independent of host page tables: MREMAP_FIXED
// without MREMAP_MAYMOVE is EINVAL, an unknown flag bit is EINVAL, and a zero new_size is EINVAL.
// mlock2 with an unknown flag is EINVAL, and mincore rejects an unaligned start address with
// EINVAL and an unmapped range with ENOMEM. All are errno classes the engine must emulate to
// Linux, never values it can read from the host kernel.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    char *m = mmap(0, ps * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("mmap_failed\n"); return 1; }

    errno = 0;
    int fixed = mremap(m, ps, ps, MREMAP_FIXED, m) == MAP_FAILED ? errno : 0;
    errno = 0;
    int badflag = mremap(m, ps, ps, 0x40) == MAP_FAILED ? errno : 0;
    errno = 0;
    int newsz0 = mremap(m, ps, 0, 0) == MAP_FAILED ? errno : 0;

    errno = 0;
    int mlock2_bad = (int)syscall(SYS_mlock2, m, ps, 0x4) == -1 ? errno : 0;

    unsigned char vec[2];
    errno = 0;
    int mincore_unalign = mincore(m + 1, ps, vec) == -1 ? errno : 0;
    munmap(m, ps * 2);
    errno = 0;
    int mincore_unmapped = mincore(m, ps, vec) == -1 ? errno : 0;

    printf("mremap-einval fixed=%d badflag=%d newsz0=%d mlock2=%d mincore_unalign=%d mincore_unmapped=%d\n",
           fixed, badflag, newsz0, mlock2_bad, mincore_unalign, mincore_unmapped);
    return 0;
}
