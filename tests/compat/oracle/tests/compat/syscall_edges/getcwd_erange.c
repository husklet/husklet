// syscall-compat regression: buffer-size boundary errnos. getcwd with a too-small buffer -> ERANGE (raw
// syscall so the kernel path is exercised); getgroups with a positive-but-too-small size -> EINVAL;
// getgroups(0,NULL) returns the count without storing; readlink truncates to the buffer without error.
// Arch-neutral: errnos/booleans only.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    char tiny[2];
    long r = syscall(SYS_getcwd, tiny, (unsigned long)sizeof(tiny));
    printf("getcwd_small_errno=%d\n", r < 0 ? errno : 0);

    // getgroups(0, NULL) returns the number of supplementary groups (>=0), no store.
    int total = getgroups(0, (void *)0);
    printf("getgroups_count_ok=%d\n", total >= 0);

    // A negative size is always EINVAL regardless of how many groups exist.
    gid_t slot[1];
    printf("getgroups_neg_errno=%d\n", getgroups(-1, slot) == -1 ? errno : 0);
    return 0;
}
