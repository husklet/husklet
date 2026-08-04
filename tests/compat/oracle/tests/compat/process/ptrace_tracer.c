// ext_proc/ptrace_tracer.c -- exercises hl's real ptrace(2) support (bug #238), portable across the two
// Linux guest arches (x86-64 + aarch64). A parent forks a child; the child PTRACE_TRACEME's itself and
// raises SIGSTOP; the parent drives it with PTRACE_SYSCALL and asserts, via PTRACE_GETREGS, that it
// observes the child's syscalls with the CORRECT arch-native syscall number, at both entry and exit,
// with the TRACESYSGOOD 0x80 wait-status bit set. It also verifies PEEKDATA reads the tracee's memory.
//
// Golden line (identical on both arches -- everything arch-specific is normalized to booleans):
//   ptrace ok stopsig=1 sysgood=1 getpid=1 write=1 exit=1 peek=1 status=7
//
// Deterministic by construction: no execve (no glibc syscall storm); the child makes exactly the
// syscalls below via raw syscall(2), so entry/exit stop counts are fixed.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <sys/uio.h>
#include <sys/syscall.h>
#include <linux/elf.h> // NT_PRSTATUS
#include <errno.h>
#include <signal.h>

// PTRACE_GETREGSET(NT_PRSTATUS) is the portable register read (aarch64 has no PTRACE_GETREGS). The image
// is the arch's user_regs_struct: x86-64 orig_rax @ index 15, rdi @ 14; aarch64 x[i] @ index i, x8 @ 8.
static int tracee_regs(pid_t child, unsigned long long *buf, int n) {
    struct iovec iov = {buf, (size_t)n * 8};
    return ptrace(PTRACE_GETREGSET, child, (void *)NT_PRSTATUS, &iov) == 0 ? 0 : -1;
}
static long tracee_sysnr(pid_t child) {
    unsigned long long r[40];
    if (tracee_regs(child, r, 40) != 0) return -1;
#if defined(__x86_64__)
    return (long)r[15]; // orig_rax
#elif defined(__aarch64__)
    return (long)r[8]; // x8
#else
    return -1;
#endif
}
// Second syscall argument (write()'s buffer pointer) from the register image.
static unsigned long tracee_arg1(pid_t child) {
    unsigned long long r[40];
    if (tracee_regs(child, r, 40) != 0) return ~0UL;
#if defined(__x86_64__)
    return (unsigned long)r[13]; // rsi
#elif defined(__aarch64__)
    return (unsigned long)r[1]; // x1
#else
    return ~0UL;
#endif
}

int main(void) {
    // A unique marker string the parent will PEEKDATA out of the child's address space.
    static const char marker[8] = {'d', 'd', 'p', 't', 'r', 'a', 'c', 'e'};

    pid_t child = fork();
    if (child < 0) { printf("ptrace fork-fail\n"); return 1; }

    if (child == 0) {
        // ---- tracee ----
        if (ptrace(PTRACE_TRACEME, 0, 0, 0) != 0) _exit(90);
        raise(SIGSTOP); // parent gets the initial group-stop here, then arms PTRACE_SYSCALL
        // The parent single-steps these via PTRACE_SYSCALL and inspects the register image:
        syscall(SYS_getpid);                 // arch-native __NR_getpid
        syscall(SYS_write, -1, marker, 8);   // arch-native __NR_write (fd -1 -> harmless EBADF)
        _exit(7);
    }

    // ---- tracer ----
    int status = 0, sysgood = 0, saw_getpid = 0, saw_write = 0, saw_exit = 0, peek_ok = 0, stopsig_ok = 0;
    int child_exit = -1, at_entry = 1;

    // 1) initial group-stop (raise(SIGSTOP) -> WIFSTOPPED, WSTOPSIG == SIGSTOP)
    if (waitpid(child, &status, 0) == child && WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) stopsig_ok = 1;
    ptrace(PTRACE_SETOPTIONS, child, 0, (void *)PTRACE_O_TRACESYSGOOD);

    // 2) syscall entry/exit stop loop until the child exits
    for (;;) {
        if (ptrace(PTRACE_SYSCALL, child, 0, 0) != 0) break;
        if (waitpid(child, &status, 0) != child) break;
        if (WIFEXITED(status)) { child_exit = WEXITSTATUS(status); break; }
        if (WIFSIGNALED(status)) { child_exit = 128 + WTERMSIG(status); break; }
        if (!WIFSTOPPED(status)) continue;

        int ss = WSTOPSIG(status);
        // TRACESYSGOOD: a syscall-stop is reported as SIGTRAP|0x80 (== 0x85).
        if (ss == (SIGTRAP | 0x80)) sysgood = 1;
        long nr = tracee_sysnr(child);
        if (nr == SYS_getpid) saw_getpid = 1;
        if (nr == SYS_write) {
            saw_write = 1;
            if (at_entry) {
                // On the write() ENTRY stop, PEEKDATA the marker out of the tracee's address space.
                unsigned long bufaddr = tracee_arg1(child); // write(fd, BUF, len) -> BUF is arg1 (== &marker)
                errno = 0;
                long w = ptrace(PTRACE_PEEKDATA, child, (void *)bufaddr, 0);
                if (!(w == -1 && errno)) {
                    char got[8];
                    memcpy(got, &w, 8);
                    if (memcmp(got, marker, 8) == 0) peek_ok = 1;
                }
            }
        }
        at_entry = !at_entry; // entry and exit alternate for each serviced syscall
    }
    if (child_exit == 7) saw_exit = 1;

    printf("ptrace ok stopsig=%d sysgood=%d getpid=%d write=%d exit=%d peek=%d status=%d\n", stopsig_ok, sysgood,
           saw_getpid, saw_write, saw_exit, peek_ok, child_exit);
    return 0;
}
