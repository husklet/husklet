// syscall-compat regression: getresuid/getresgid with a null output pointer must return EFAULT,
// not silently skip the store and report success.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

int main(void) {
    uid_t e, s;
    gid_t ge, gs;
    long r1 = syscall(SYS_getresuid, (void *)0, &e, &s);
    printf("getresuid_null_errno=%d\n", r1 == -1 ? errno : 0);
    long r2 = syscall(SYS_getresgid, (void *)0, &ge, &gs);
    printf("getresgid_null_errno=%d\n", r2 == -1 ? errno : 0);
    return 0;
}
