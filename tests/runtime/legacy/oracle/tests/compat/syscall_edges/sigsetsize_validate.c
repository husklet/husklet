// syscall-compat regression: rt_sigsuspend(2) and rt_sigtimedwait(2) validate `sigsetsize` FIRST -- both
// kernel entry points open with `if (sigsetsize != sizeof(sigset_t)) return -EINVAL;` -- so a wrong size is
// rejected instantly, before the mask is even copied in.
//
// The engine ignored the argument entirely, so BOTH calls went to sleep on a request Linux refuses: the
// guest hung forever instead of getting EINVAL. Only a raw syscall reaches this (glibc always passes 8).
// Arch-neutral: errnos only. A regression here re-hangs this case rather than failing it, which is exactly
// the signal we want.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

static int ec(long r) {
    return r == -1 ? errno : 0;
}

int main(void) {
    sigset_t set;
    sigemptyset(&set);

    // rt_sigsuspend(mask, sigsetsize): every size but 8 is EINVAL, never a suspend.
    printf("sigsuspend_size4_errno=%d\n", ec(syscall(SYS_rt_sigsuspend, &set, 4)));
    printf("sigsuspend_size16_errno=%d\n", ec(syscall(SYS_rt_sigsuspend, &set, 16)));
    printf("sigsuspend_size0_errno=%d\n", ec(syscall(SYS_rt_sigsuspend, &set, 0)));

    // rt_sigtimedwait(set, info, timeout, sigsetsize): same rule, and it wins over a NULL timeout (which
    // would otherwise mean "wait indefinitely").
    printf("sigtimedwait_size4_errno=%d\n", ec(syscall(SYS_rt_sigtimedwait, &set, (void *)0, (void *)0, 4)));
    printf("sigtimedwait_size16_errno=%d\n",
           ec(syscall(SYS_rt_sigtimedwait, &set, (void *)0, (void *)0, 16)));

    // The size check precedes the pointer copy-in, so a bad size beats a bad mask pointer.
    printf("sigsuspend_badsize_badptr_errno=%d\n", ec(syscall(SYS_rt_sigsuspend, (void *)0x10, 4)));
    printf("sigtimedwait_badsize_badptr_errno=%d\n",
           ec(syscall(SYS_rt_sigtimedwait, (void *)0x10, (void *)0, (void *)0, 4)));

    // Control: the correct size still reaches the real semantics -- an empty set with a zero timeout
    // dequeues nothing and returns EAGAIN rather than blocking.
    struct timespec zero = {0, 0};
    printf("sigtimedwait_size8_errno=%d\n", ec(syscall(SYS_rt_sigtimedwait, &set, (void *)0, &zero, 8)));
    return 0;
}
