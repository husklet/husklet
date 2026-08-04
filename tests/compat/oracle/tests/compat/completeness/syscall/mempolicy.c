/* NUMA memory-policy syscalls get_mempolicy/set_mempolicy. On any kernel (even single-node) the
   default policy query must succeed or fail with a canonical errno; setting MPOL_DEFAULT is a no-op
   that must be accepted or cleanly rejected. Derived verdict is arch-neutral and host-independent
   (we never print the node mask, only handled/consistency booleans). */
#include "compat.h"
#include <stdio.h>
#ifndef __NR_get_mempolicy
#if defined(__aarch64__)
#define __NR_get_mempolicy 236
#else
#define __NR_get_mempolicy 239
#endif
#endif
#ifndef __NR_set_mempolicy
#if defined(__aarch64__)
#define __NR_set_mempolicy 237
#else
#define __NR_set_mempolicy 238
#endif
#endif
#define MPOL_DEFAULT 0

static int canonical(long rc) {
    return rc == 0 || (rc < 0 && (errno == ENOSYS || errno == EPERM || errno == EINVAL));
}

int main(void) {
    int mode = -1;
    long g = syscall(__NR_get_mempolicy, &mode, (void *)0, 0UL, (void *)0, 0UL);
    int get_ok = canonical(g);
    int mode_sane = (g != 0) ? 1 : (mode >= 0 && mode <= 7);

    long s = syscall(__NR_set_mempolicy, MPOL_DEFAULT, (void *)0, 0UL);
    int set_ok = canonical(s);

    printf("mempolicy get_handled=%d mode_sane=%d set_handled=%d\n", get_ok, mode_sane, set_ok);
    return 0;
}
