#include "abi.h"

#if defined(__aarch64__)
#define GUEST_OPENAT 56
#define GUEST_CLOSE 57
#define GUEST_UNSHARE 97
#define GUEST_SETNS 268
#elif defined(__x86_64__)
#define GUEST_OPENAT 257
#define GUEST_CLOSE 3
#define GUEST_UNSHARE 272
#define GUEST_SETNS 308
#endif

#define AT_FDCWD (-100)
#define CLONE_NEWUTS 0x04000000
#define CLONE_NEWUSER 0x10000000
#define CLONE_NEWNET 0x40000000
#define EBADF 9
#define EPERM 1
#define ENOSYS 38
#define EINVAL 22

void _start(void) {
    static const char path[] = "/proc/self/ns/uts";
    static const char message[] = "namespace-ok\n";
    long descriptor = guest_call(GUEST_OPENAT, AT_FDCWD, (long)path, 0);
    if (descriptor < 0) guest_exit(-descriptor);
    if (guest_call(GUEST_UNSHARE, 0, 0, 0) != 0) guest_exit(2);
    if (guest_call(GUEST_UNSHARE, 1, 0, 0) != -EINVAL) guest_exit(3);
    if (guest_call(GUEST_UNSHARE, CLONE_NEWUSER, 0, 0) != -ENOSYS) guest_exit(8);
    if (guest_call(GUEST_SETNS, -1, CLONE_NEWUTS, 0) != -EBADF) guest_exit(4);
    if (guest_call(GUEST_SETNS, descriptor, CLONE_NEWNET, 0) != -EINVAL) guest_exit(5);
    if (guest_call(GUEST_SETNS, descriptor, CLONE_NEWUTS, 0) != -EPERM) guest_exit(6);
    if (guest_call(GUEST_CLOSE, descriptor, 0, 0) != 0) guest_exit(7);
    guest_write(message, sizeof(message) - 1);
    guest_exit(0);
}
