/* io_uring entry-point robustness + async-runtime fallback contract.
   Docker's default seccomp profile blocks io_uring with EPERM. This probe asserts all three entry
   points are HANDLED cleanly -- either a valid result on an unrestricted native kernel, a bad-fd
   result after native dispatch, or the container policy result -- AND that synchronous fallback
   I/O moves bytes intact. Derived booleans only, so the output is arch-neutral and identical on a
   real Linux host and the engine. Companion to io_uring_setup.c, which covers setup shape. */
#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#ifndef __NR_io_uring_setup
#define __NR_io_uring_setup 425
#endif
#ifndef __NR_io_uring_enter
#define __NR_io_uring_enter 426
#endif
#ifndef __NR_io_uring_register
#define __NR_io_uring_register 427
#endif

struct io_uring_params { unsigned pad[30]; };  /* 120 bytes; kernel writes back into it */

/* ENOSYS is deliberately excluded: the engine exposes Linux's io_uring API under Docker's default
   policy, so it must report EPERM instead of claiming the syscall does not exist. */
static int handled(long rc) {
    if (rc >= 0) return 1;
    return errno == EBADF || errno == EINVAL || errno == EPERM || errno == EACCES || errno == EOPNOTSUPP;
}

int main(void) {
    struct io_uring_params p;
    memset(&p, 0, sizeof p);
    long s = syscall(__NR_io_uring_setup, 8u, &p);
    int setup_ok = handled(s);
    if (s >= 0) close((int)s);

    /* Drive io_uring_enter and io_uring_register against a guaranteed-bad ring fd, so the errno is
       deterministic across supported policies: EBADF where io_uring is admitted, EPERM where the
       default container profile blocks it. */
    long e = syscall(__NR_io_uring_enter, -1, 0u, 0u, 0u, (void *)0, (unsigned long)0);
    int enter_ok = handled(e);

    long r = syscall(__NR_io_uring_register, -1, 1u /* IORING_UNREGISTER_BUFFERS */, (void *)0, 0u);
    int register_ok = handled(r);

    /* The fallback every async runtime takes when io_uring is unavailable: plain synchronous I/O.
       Prove it transfers bytes intact. memfd-backed so it needs no writable working directory. */
    int fallback_ok = 0;
    long fd = syscall(SYS_memfd_create, "uring_fb", 0u);
    if (fd >= 0) {
        static const char msg[] = "async-runtime-fallback";
        char buf[sizeof msg];
        memset(buf, 0, sizeof buf);
        if (pwrite((int)fd, msg, sizeof msg, 0) == (ssize_t)sizeof msg &&
            pread((int)fd, buf, sizeof msg, 0) == (ssize_t)sizeof msg &&
            memcmp(buf, msg, sizeof msg) == 0)
            fallback_ok = 1;
        close((int)fd);
    }

    printf("io_uring setup=%d enter=%d register=%d fallback=%d\n",
           setup_ok, enter_ok, register_ok, fallback_ok);
    return 0;
}
