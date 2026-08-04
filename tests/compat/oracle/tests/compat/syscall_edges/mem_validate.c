// syscall-compat regression: mprotect/madvise must validate alignment, mapping, and advice like Linux
// instead of fake-succeeding. Uses addr+1 (unaligned to any page size) so it is arch/page-size neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    long pg = sysconf(_SC_PAGESIZE);
    char *m = mmap(0, pg, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

    // mprotect with an unaligned start -> EINVAL.
    long r1 = syscall(SYS_mprotect, (long)(m + 1), (long)(pg - 1), PROT_READ);
    printf("mprotect_unaligned_errno=%d\n", r1 == -1 ? errno : 0);

    // madvise with an advice value Linux does not define -> EINVAL.
    long r3 = syscall(SYS_madvise, (long)m, (long)pg, 99999);
    printf("madvise_badadvice_errno=%d\n", r3 == -1 ? errno : 0);

    // madvise with an unaligned start -> EINVAL.
    long r4 = syscall(SYS_madvise, (long)(m + 1), (long)(pg - 1), 0 /*MADV_NORMAL*/);
    printf("madvise_unaligned_errno=%d\n", r4 == -1 ? errno : 0);

    // mprotect on a page-ALIGNED but UNMAPPED range -> ENOMEM (Linux mm/mprotect.c walks the VMAs and
    // faults a hole). Map a page then unmap it so the address is aligned and provably not mapped.
    char *u = mmap(0, pg, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    munmap(u, pg);
    long r5 = syscall(SYS_mprotect, (long)u, (long)pg, PROT_READ);
    printf("mprotect_unmapped_errno=%d\n", r5 == -1 ? errno : 0); // ENOMEM(12) on Linux
    return 0;
}
