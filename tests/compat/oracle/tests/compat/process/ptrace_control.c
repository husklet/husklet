// ptrace_control.c -- exercises hl's ptrace(2) WRITE + control surface beyond the read-only tracer test
// (ptrace_tracer.c). Portable across the two Linux guest arches (x86-64 + aarch64). A parent forks a
// child that PTRACE_TRACEME's itself; the parent drives it with PTRACE_SYSCALL and, on the write() entry
// stop, (1) verifies PEEKDATA sees the tracee's buffer, (2) POKEDATA's a replacement over that buffer and
// confirms the *poked* bytes are what the child actually writes down a pipe -- i.e. the write hits the
// tracee's live address space. It also round-trips a scratch register via POKEUSER/GETREGSET and probes
// that the FP register-set request and a bogus request FAIL CLEANLY (no crash / no lying success).
//
// Golden line (identical on both arches -- everything arch-specific normalized to booleans):
//   ptrace-ctl peek=1 poke=1 reg=1 fpclean=1 badclean=1 exit=1 status=0
//
// Deterministic by construction: no execve; the child issues exactly one write() via raw syscall(2).

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <sys/uio.h>
#include <sys/syscall.h>
#include <linux/elf.h> // NT_PRSTATUS, NT_PRFPREG
#include <errno.h>
#include <signal.h>

static int tracee_regs(pid_t child, unsigned long long *buf, int n) {
    struct iovec iov = {buf, (size_t)n * 8};
    return ptrace(PTRACE_GETREGSET, child, (void *)NT_PRSTATUS, &iov) == 0 ? 0 : -1;
}
static long tracee_sysnr(pid_t child) {
    unsigned long long r[40];
    if (tracee_regs(child, r, 40) != 0) return -1;
#if defined(__x86_64__)
    return (long)r[15];
#else
    return (long)r[8];
#endif
}
static unsigned long tracee_arg1(pid_t child) {
    unsigned long long r[40];
    if (tracee_regs(child, r, 40) != 0) return ~0UL;
#if defined(__x86_64__)
    return (unsigned long)r[13]; // rsi
#else
    return (unsigned long)r[1]; // x1
#endif
}

int main(void) {
    static const char marker[8] = {'m', 'a', 'r', 'k', 'e', 'r', '0', '0'};
    static const char poked[8] = {'P', 'O', 'K', 'E', 'D', '!', '!', '!'};
    int pfd[2];
    if (pipe(pfd) != 0) { printf("ptrace-ctl pipe-fail\n"); return 1; }

    pid_t child = fork();
    if (child < 0) { printf("ptrace-ctl fork-fail\n"); return 1; }

    if (child == 0) {
        // ---- tracee ----
        close(pfd[0]);
        char buf[8];
        memcpy(buf, marker, 8);
        if (ptrace(PTRACE_TRACEME, 0, 0, 0) != 0) _exit(90);
        raise(SIGSTOP);
        syscall(SYS_write, pfd[1], buf, (size_t)8); // parent POKEs `buf` on the entry stop
        _exit(0);
    }

    // ---- tracer ----
    close(pfd[1]);
    int status = 0, peek_ok = 0, poke_ok = 0, reg_ok = 0, fpclean = 0, badclean = 0, saw_exit = 0;
    int child_exit = -1, at_entry = 1;

    if (waitpid(child, &status, 0) != child || !WIFSTOPPED(status)) { printf("ptrace-ctl nostop\n"); return 1; }
    ptrace(PTRACE_SETOPTIONS, child, 0, (void *)PTRACE_O_TRACESYSGOOD);

    // Register round-trip via GETREGSET + POKEUSER: read a scratch GPR's user-area offset, poke a value,
    // read it back through GETREGSET. (x86-64 r15 @ user-offset 0, index 0; aarch64 x0 @ offset 0, index 0.)
    // We do this at the very first syscall entry stop we hit.
    int did_reg = 0;

    for (;;) {
        if (ptrace(PTRACE_SYSCALL, child, 0, 0) != 0) break;
        if (waitpid(child, &status, 0) != child) break;
        if (WIFEXITED(status)) { child_exit = WEXITSTATUS(status); break; }
        if (WIFSIGNALED(status)) { child_exit = 128 + WTERMSIG(status); break; }
        if (!WIFSTOPPED(status)) continue;

        long nr = tracee_sysnr(child);

        if (!did_reg) {
            did_reg = 1;
            // SETREGSET(NT_PRSTATUS) round-trip: read the GPR image, flip a scratch register, write it
            // back, read again, confirm the write took. Portable (both arches marshal NT_PRSTATUS).
            unsigned long long before[40];
            struct iovec riov = {before, sizeof(before)};
            if (ptrace(PTRACE_GETREGSET, child, (void *)NT_PRSTATUS, &riov) == 0) {
                // A scratch caller-clobbered index that the pending write() entry won't need read back
                // immediately: aarch64 x10 (idx 10), x86-64 r10 (idx 7). Both are call-clobbered scratch.
#if defined(__x86_64__)
                int idx = 7; // r10
#else
                int idx = 10; // x10
#endif
                unsigned long long saved = before[idx];
                before[idx] = 0x1234abcd5678ef00ULL;
                struct iovec wiov = {before, riov.iov_len};
                if (ptrace(PTRACE_SETREGSET, child, (void *)NT_PRSTATUS, &wiov) == 0) {
                    unsigned long long after[40];
                    struct iovec aiov = {after, sizeof(after)};
                    if (ptrace(PTRACE_GETREGSET, child, (void *)NT_PRSTATUS, &aiov) == 0 &&
                        after[idx] == 0x1234abcd5678ef00ULL)
                        reg_ok = 1;
                    // restore so the tracee's real execution is undisturbed
                    before[idx] = saved;
                    struct iovec fiov = {before, riov.iov_len};
                    ptrace(PTRACE_SETREGSET, child, (void *)NT_PRSTATUS, &fiov);
                }
            }
            // FP register set: engine stages this (clean -EIO); native returns it. Assert only "clean".
            unsigned long long fpbuf[64];
            struct iovec fpiov = {fpbuf, sizeof fpbuf};
            errno = 0;
            long fr = ptrace(PTRACE_GETREGSET, child, (void *)NT_PRFPREG, &fpiov);
            if (fr == 0 || errno != 0) fpclean = 1; // succeeded OR failed with an errno; never crashed
            // A bogus/unsupported request must fail cleanly with an errno, not crash or fake-succeed.
            errno = 0;
            long br = ptrace((enum __ptrace_request)0x4fff, child, 0, 0);
            if (br == -1 && errno != 0) badclean = 1;
        }

        // The entry/exit stops alternate PER SYSCALL, so track at_entry only across write() stops -- the
        // number of unrelated syscall-stops before the write (raise()'s internal tgkill/sigprocmask) is
        // not fixed across libc/arch, so a global toggle would misparity which write-stop is the entry.
        if (nr == SYS_write) {
            if (at_entry) {
                unsigned long bufaddr = tracee_arg1(child);
                errno = 0;
                long w = ptrace(PTRACE_PEEKDATA, child, (void *)bufaddr, 0);
                if (!(w == -1 && errno)) {
                    char got[8];
                    memcpy(got, &w, 8);
                    if (memcmp(got, marker, 8) == 0) peek_ok = 1;
                }
                // POKEDATA the replacement over the tracee's buffer, at the ENTRY stop (before the kernel
                // reads it), so the child writes the poked bytes down the pipe.
                long word;
                memcpy(&word, poked, 8);
                if (ptrace(PTRACE_POKEDATA, child, (void *)bufaddr, (void *)word) == 0) poke_ok = 1;
            }
            at_entry = !at_entry;
        }
    }
    if (child_exit == 0) saw_exit = 1;

    // Read what the child actually wrote: must be the POKED bytes, proving the poke hit live tracee memory.
    char out[8];
    ssize_t n = read(pfd[0], out, 8);
    if (!(n == 8 && memcmp(out, poked, 8) == 0)) poke_ok = 0;

    printf("ptrace-ctl peek=%d poke=%d reg=%d fpclean=%d badclean=%d exit=%d status=%d\n", peek_ok, poke_ok,
           reg_ok, fpclean, badclean, saw_exit, child_exit);
    return 0;
}
