/* memfd_secret robustness. This very recent syscall (447) returns a fd backed by memory unmapped
   from the kernel's direct map, or fails ENOSYS where unavailable (common: it is gated behind a boot
   flag). A correct engine returns a usable fd or a canonical errno, never a crash or a bogus errno.
   A bad flags value must be rejected. Derived boolean verdict, arch-neutral. */
#include "compat.h"
#include <stdio.h>
#ifndef __NR_memfd_secret
#define __NR_memfd_secret 447
#endif

int main(void) {
    long fd = syscall(__NR_memfd_secret, 0u);
    int handled = (fd >= 0) || (errno == ENOSYS || errno == EPERM || errno == EOPNOTSUPP);
    if (fd >= 0) close((int)fd);

    long bad = syscall(__NR_memfd_secret, 0xFFFFFFFFu);
    int bad_rejected = (bad < 0) && (errno == EINVAL || errno == ENOSYS || errno == EPERM ||
                                     errno == EOPNOTSUPP);
    if (bad >= 0) close((int)bad);

    printf("memfd_secret handled=%d bad_flags_rejected=%d\n", handled, bad_rejected);
    return 0;
}
