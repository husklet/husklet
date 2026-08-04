// ptrace_attach.c -- exercises hl's ptrace(2) ATTACH/DETACH + signal-injection control surface (the
// gdb/strace "attach to a running process" path, distinct from the TRACEME self-trace in ptrace_tracer.c).
// Portable across the two Linux guest arches (x86-64 + aarch64). Three probes, all normalized to booleans:
//
//   (1) ATTACH: parent forks a child that spins issuing getpid() syscalls; parent PTRACE_ATTACHes it,
//       waitpid()s the resulting group-stop (WIFSTOPPED, WSTOPSIG==SIGSTOP), reads its registers via
//       GETREGSET, then PTRACE_DETACHes. The child then observes a shared flag and exits 55.
//   (2) ESRCH: PTRACE_ATTACH to a pid that does not exist must fail cleanly with ESRCH (no crash/hang).
//   (3) INJECT: a second child PTRACE_TRACEME's itself and raises SIGSTOP; the parent resumes it with
//       PTRACE_CONT injecting SIGUSR1, whose handler _exit(88)s -- proving CONT-with-signal delivery.
//
// Golden line (identical on both arches):
//   ptrace-att attach=1 regs=1 detach=1 esrch=1 inject=1

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <sys/uio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <linux/elf.h> // NT_PRSTATUS
#include <errno.h>
#include <signal.h>

static void on_usr1(int s) {
    (void)s;
    _exit(88);
}

int main(void) {
    int attach = 0, regs = 0, detach = 0, esrch = 0, inject = 0;

    // ---- (1) ATTACH / GETREGSET / DETACH against a running child ----
    volatile int *flag = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (flag == MAP_FAILED) { printf("ptrace-att mmap-fail\n"); return 1; }
    *flag = 0;
    pid_t child = fork();
    if (child < 0) { printf("ptrace-att fork-fail\n"); return 1; }
    if (child == 0) {
        // Spin hitting syscall boundaries so ATTACH's stop is observed promptly; exit once released.
        while (!*flag) {
            for (volatile int i = 0; i < 2000; i++) {
            }
            syscall(SYS_getpid);
        }
        _exit(55);
    }
    if (ptrace(PTRACE_ATTACH, child, 0, 0) == 0) {
        int status = 0;
        if (waitpid(child, &status, 0) == child && WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) attach = 1;
        unsigned long long r[40];
        struct iovec iov = {r, sizeof r};
        if (ptrace(PTRACE_GETREGSET, child, (void *)NT_PRSTATUS, &iov) == 0 && iov.iov_len >= 8) regs = 1;
        *flag = 1;                          // release the child before detaching
        ptrace(PTRACE_DETACH, child, 0, 0); // resume + drop the trace relationship
        int st = 0;
        if (waitpid(child, &st, 0) == child && WIFEXITED(st) && WEXITSTATUS(st) == 55) detach = 1;
    } else {
        *flag = 1;
        int st = 0;
        waitpid(child, &st, 0);
    }

    // ---- (2) ATTACH to a nonexistent pid -> ESRCH ----
    errno = 0;
    if (ptrace(PTRACE_ATTACH, 0x7ffffff0, 0, 0) == -1 && errno == ESRCH) esrch = 1;

    // ---- (3) CONT with signal injection ----
    pid_t c2 = fork();
    if (c2 == 0) {
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_handler = on_usr1;
        sigaction(SIGUSR1, &sa, NULL);
        if (ptrace(PTRACE_TRACEME, 0, 0, 0) != 0) _exit(90);
        raise(SIGSTOP);         // initial stop -> parent injects SIGUSR1 on resume
        for (volatile int i = 0; i < 100000000; i++) {
        }
        _exit(1); // reached only if the injected signal never arrived
    }
    int s2 = 0;
    if (waitpid(c2, &s2, 0) == c2 && WIFSTOPPED(s2)) {
        ptrace(PTRACE_CONT, c2, 0, (void *)(long)SIGUSR1); // resume delivering SIGUSR1
        if (waitpid(c2, &s2, 0) == c2 && WIFEXITED(s2) && WEXITSTATUS(s2) == 88) inject = 1;
    }
    if (!inject) {
        kill(c2, SIGKILL);
        waitpid(c2, &s2, 0);
    }

    printf("ptrace-att attach=%d regs=%d detach=%d esrch=%d inject=%d\n", attach, regs, detach, esrch, inject);
    return 0;
}
