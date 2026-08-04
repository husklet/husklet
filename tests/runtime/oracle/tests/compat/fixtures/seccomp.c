#define _GNU_SOURCE

#include <errno.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EACCES),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {
        .len = (unsigned short)(sizeof instructions / sizeof instructions[0]),
        .filter = instructions,
    };
    long install;
    long result;
    int blocked;

    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) return 1;
    install = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program);
    if (install != 0) return 2;
    errno = 0;
    result = syscall(SYS_getpid);
    blocked = result == -1 && errno == EACCES;
    printf("seccomp install=%ld blocked=%d errno=%d\n", install, blocked, errno);
    return blocked ? 0 : 3;
}
