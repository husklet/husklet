// PR_CAP_AMBIENT argument + policy surface. The ambient capability set can only hold a cap that is
// present in BOTH the permitted and the inheritable set; the container's inheritable set is empty
// (capget reports CapInh=0, matching Docker's default), so every RAISE is rejected with EPERM -- the
// same verdict an unprivileged native task returns, since it too lacks the cap in its inheritable set.
// This is arch-neutral, so the printed return-class + errno is byte-identical on aarch64 and x86_64
// under both engines and native.
//   - RAISE of a valid cap -> EPERM (the engine previously returned 0, falsely granting it).
//   - LOWER of a valid cap -> 0 (removing an absent ambient cap is a no-op success).
//   - IS_SET of a valid cap -> 0 (not present in the empty ambient set).
//   - RAISE/LOWER/IS_SET of an out-of-range cap -> EINVAL.
//   - RAISE/LOWER/IS_SET with a nonzero arg4/arg5 -> EINVAL.
//   - CLEAR_ALL -> 0; CLEAR_ALL with a nonzero arg3/arg4/arg5 -> EINVAL.
//   - an unknown sub-command -> EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef PR_CAP_AMBIENT
#define PR_CAP_AMBIENT 47
#endif
#define PR_CAP_AMBIENT_IS_SET 1
#define PR_CAP_AMBIENT_RAISE 2
#define PR_CAP_AMBIENT_LOWER 3
#define PR_CAP_AMBIENT_CLEAR_ALL 4

// CAP_NET_BIND_SERVICE(10) is in the container's permitted set but never in its inheritable set.
#define CAP_TEST 10

static void show(const char *name, unsigned long sub, unsigned long a3, unsigned long a4, unsigned long a5) {
    errno = 0;
    long r = syscall(SYS_prctl, (unsigned long)PR_CAP_AMBIENT, sub, a3, a4, a5);
    printf("%-18s r=%ld e=%d\n", name, r < 0 ? -1L : r, r < 0 ? errno : 0);
}

int main(void) {
    show("raise_valid", PR_CAP_AMBIENT_RAISE, CAP_TEST, 0, 0);
    show("lower_valid", PR_CAP_AMBIENT_LOWER, CAP_TEST, 0, 0);
    show("is_set_valid", PR_CAP_AMBIENT_IS_SET, CAP_TEST, 0, 0);
    show("raise_badcap", PR_CAP_AMBIENT_RAISE, 200, 0, 0);
    show("lower_badcap", PR_CAP_AMBIENT_LOWER, 200, 0, 0);
    show("is_set_badcap", PR_CAP_AMBIENT_IS_SET, 200, 0, 0);
    show("raise_arg4_nz", PR_CAP_AMBIENT_RAISE, CAP_TEST, 1, 0);
    show("is_set_arg5_nz", PR_CAP_AMBIENT_IS_SET, CAP_TEST, 0, 1);
    show("clear_all", PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0);
    show("clear_all_arg_nz", PR_CAP_AMBIENT_CLEAR_ALL, 1, 0, 0);
    show("unknown_sub", 99, CAP_TEST, 0, 0);
    return 0;
}
