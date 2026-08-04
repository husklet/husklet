/* seccomp(2) BPF-filter ENFORCEMENT: install a classic-BPF program that denies getpid with EPERM
   (SECCOMP_RET_ERRNO) and allows every other syscall (SECCOMP_RET_ALLOW), then confirm the kernel/engine
   actually runs the filter: getpid must fail -1/EPERM while an allowed syscall (getuid) still succeeds.
   Native aarch64 (the oracle) enforces this; hl used to accept the install as a no-op and still service
   getpid (fail-open). Deterministic verdict (errno, not the raw pid/uid) so JIT stdout == native. */
#include "compat.h"
#include <stdio.h>
#include <sys/prctl.h>
#include <linux/seccomp.h>
#include <linux/filter.h>
#include <linux/audit.h>
#include <stddef.h>

int main(void) {
    struct sock_filter f[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_AARCH64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_THREAD),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (EPERM & SECCOMP_RET_DATA)),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = {.len = sizeof f / sizeof f[0], .filter = f};
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        printf("seccomp nnp_fail=1\n");
        return 0;
    }
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog) < 0) {
        printf("seccomp install_fail=1 errno=%d\n", errno);
        return 0;
    }
    errno = 0;
    long pid = syscall(__NR_getpid);
    int errno_block = (pid == -1 && errno == EPERM);
    long uid = syscall(__NR_getuid);
    int allow_pass = (uid >= 0);
    printf("seccomp errno_block=%d allow_pass=%d\n", errno_block, allow_pass);
    return 0;
}
