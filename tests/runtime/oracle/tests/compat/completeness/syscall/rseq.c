/* rseq (restartable sequences) registration validation. glibc auto-registers an rseq area at startup
   on capable kernels, so a fresh registration of a DIFFERENT area must fail EBUSY, a mismatched
   signature/size must fail EINVAL, and the whole syscall is ENOSYS where unsupported. A correct
   engine returns those canonical errnos rather than crashing or corrupting the thread's rseq area.
   Derived boolean verdict, arch-neutral. */
#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#ifndef __NR_rseq
#define __NR_rseq 293  /* stable across x86_64 (334) is legacy; 293 is the aarch64 no... */
#endif
/* rseq shares no cross-arch number; resolve per arch. */
#undef __NR_rseq
#if defined(__aarch64__)
#define __NR_rseq 293
#else
#define __NR_rseq 334
#endif

struct rseq_area { uint32_t cpu_id_start, cpu_id; uint64_t rseq_cs; uint32_t flags; } __attribute__((aligned(32)));

int main(void) {
    struct rseq_area area;
    memset(&area, 0, sizeof area);
    /* Wrong length must be rejected EINVAL (registration requires the exact ABI size), or ENOSYS. */
    long badlen = syscall(__NR_rseq, &area, 7u, 0u, 0x53053053u);
    int badlen_rejected = (badlen < 0) && (errno == EINVAL || errno == ENOSYS);

    /* Registering a second, different area while glibc already holds one must fail EBUSY (or EINVAL
       for the signature), or ENOSYS. It must not silently succeed. */
    long dup = syscall(__NR_rseq, &area, (unsigned)sizeof area, 0u, 0x53053053u);
    int dup_rejected = (dup < 0) && (errno == EBUSY || errno == EINVAL || errno == EPERM ||
                                     errno == ENOSYS);

    printf("rseq badlen_rejected=%d dup_rejected=%d\n", badlen_rejected, dup_rejected);
    return 0;
}
