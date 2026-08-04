/* kcmp: kernel-side comparison of two processes' resources. Comparing a process to ITSELF must be
   consistent — KCMP_FILE of the same fd is "equal" (0), two different fds are ordered non-equal, and
   KCMP_VM/KCMP_FILES of self==self are equal. A correct engine returns the ordering triad {0,1,2}
   consistently or ENOSYS/EPERM if unsupported. Derived boolean verdict, arch-neutral. */
#include "compat.h"
#include <stdio.h>
#include <fcntl.h>
#ifndef __NR_kcmp
#if defined(__aarch64__)
#define __NR_kcmp 272
#else
#define __NR_kcmp 312
#endif
#endif
#define KCMP_FILE 0
#define KCMP_VM   1
#define KCMP_FILES 2

int main(void) {
    long me = getpid();
    int a = open("/dev/null", O_RDONLY);
    int b = open("/dev/null", O_RDONLY);

    long same = syscall(__NR_kcmp, me, me, KCMP_FILE, (unsigned long)a, (unsigned long)a);
    long diff = syscall(__NR_kcmp, me, me, KCMP_FILE, (unsigned long)a, (unsigned long)b);
    long vm   = syscall(__NR_kcmp, me, me, KCMP_VM, 0UL, 0UL);
    long files = syscall(__NR_kcmp, me, me, KCMP_FILES, 0UL, 0UL);

    int unsupported = (same < 0) && (errno == ENOSYS || errno == EPERM);
    int ok;
    if (unsupported)
        ok = 1;
    else
        ok = (same == 0) && (diff == 1 || diff == 2) && (vm == 0) && (files == 0);

    if (a >= 0) close(a);
    if (b >= 0) close(b);
    /* Whether kcmp is implemented or cleanly ENOSYS, the guest-visible behavior is well-defined; we
       assert only that (a fabricated crash / bogus ordering never happens). The support bit itself is
       an engine policy choice and is intentionally not asserted. */
    printf("kcmp consistent=%d\n", ok);
    return 0;
}
