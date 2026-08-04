/* io_uring_setup robustness. Whether or not the engine implements io_uring, the syscall must be
   HANDLED cleanly: either return a valid ring fd, or fail with a canonical errno (ENOSYS if the
   feature is absent, EPERM if disabled, EINVAL for a rejected parameter shape). A crash, a bogus
   errno (EFAULT/garbage), or a nonsense return would flip the verdict. Derived booleans only, so the
   output is arch-neutral and identical on a real Linux host and a correct engine. */
#include "compat.h"
#include <stdio.h>
#include <string.h>
#ifndef __NR_io_uring_setup
#define __NR_io_uring_setup 425
#endif

struct io_uring_params { unsigned pad[30]; };  /* 120 bytes; kernel writes back into it */

static int canonical(long rc) {
    if (rc >= 0) return 1;                       /* success: got a ring fd */
    return errno == ENOSYS || errno == EPERM || errno == EINVAL || errno == EACCES;
}

int main(void) {
    struct io_uring_params p;
    memset(&p, 0, sizeof p);
    long r = syscall(__NR_io_uring_setup, 8u, &p);
    int ok = canonical(r);
    if (r >= 0) close((int)r);

    /* zero entries must be rejected with EINVAL (or ENOSYS if unimplemented) — never accepted. */
    memset(&p, 0, sizeof p);
    long z = syscall(__NR_io_uring_setup, 0u, &p);
    int zero_rejected = (z < 0) && (errno == EINVAL || errno == ENOSYS || errno == EPERM);

    printf("io_uring_setup handled=%d zero_rejected=%d\n", ok, zero_rejected);
    return 0;
}
