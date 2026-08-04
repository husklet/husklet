// int3 (#BP -> SIGTRAP) and UD1/UD2 (#UD -> SIGILL) must reach the guest's signal handler, not terminate the
// process. On Apple Silicon a JIT'd host BRK/UDF raises a Mach exception the x86 engine did not catch, so
// the guest handler was silently skipped and hl exited 133/132. This exercises: (1) int3 delivering SIGTRAP
// and recovering via siglongjmp, (2) ud2 delivering SIGILL and RESUMING after the handler advances RIP past
// the 2-byte 0F 0B. Oracle-diffed vs qemu (stdout + exit).
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <ucontext.h>

static volatile int trap_seen = 0;
static volatile int ill_seen = 0;
static volatile int ud1_seen = 0;
static sigjmp_buf jb;

static void on_trap(int s, siginfo_t *si, void *uc) {
    (void)s;
    (void)si;
    (void)uc;
    trap_seen = 1;
    siglongjmp(jb, 1);
}

static void on_ill(int s, siginfo_t *si, void *ucv) {
    (void)s;
    (void)si;
    ill_seen = 1;
    ucontext_t *uc = (ucontext_t *)ucv;
    unsigned char *pc = (unsigned char *)uc->uc_mcontext.gregs[REG_RIP];
    if (pc[0] == 0x67 && pc[1] == 0x0f && pc[2] == 0xb9) {
        ud1_seen = 1;
        uc->uc_mcontext.gregs[REG_RIP] += 5; // 67 0F B9 /r disp8: Chrome's addr32 UD1 probe
    } else {
        uc->uc_mcontext.gregs[REG_RIP] += 2; // 0F 0B: UD2
    }
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_flags = SA_SIGINFO;

    sa.sa_sigaction = on_trap;
    sigaction(SIGTRAP, &sa, NULL);
    if (sigsetjmp(jb, 1) == 0)
        __asm__ volatile("int3");
    printf("int3 trap=%d\n", trap_seen);

    sa.sa_sigaction = on_ill;
    sigaction(SIGILL, &sa, NULL);
    __asm__ volatile("ud2");
    printf("ud2 ill=%d resumed=1\n", ill_seen);

    __asm__ volatile(".byte 0x67, 0x0f, 0xb9, 0x40, 0x0b");
    printf("ud1 ill=%d resumed=1\n", ud1_seen);
    return 0;
}
