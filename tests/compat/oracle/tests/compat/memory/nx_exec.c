// memory-compat isolation: NX / execute-permission enforcement. A guest that jumps into a page which is
// mapped WITHOUT PROT_EXEC must fault -- Linux delivers SIGSEGV with si_code SEGV_ACCERR (permission), and
// with no handler installed the process is terminated by SIGSEGV. This probes the W^X boundary from the
// data side: a page holding a valid return stub but mapped PROT_READ (no exec) or PROT_NONE must not run.
// Arch-neutral output: only whether the access faulted (killed / ACCERR), not any arch detail.
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#if defined(__aarch64__)
static const unsigned char RET_STUB[] = {0xC0, 0x03, 0x5F, 0xD6}; // ret
#elif defined(__x86_64__)
static const unsigned char RET_STUB[] = {0xC3}; // ret
#else
#error unsupported arch
#endif

static void handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    _exit((sig == SIGSEGV && si->si_code == SEGV_ACCERR) ? 100 : 101);
}

// Map a page RWX so the stub is written and translatable, then downgrade to `prot` (which lacks EXEC) and
// call it. Each observation runs in its own child so the raw wait status is deterministic.
static void child_exec(int prot, int with_handler) {
    long ps = sysconf(_SC_PAGESIZE);
    unsigned char *p = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) _exit(10);
    memcpy(p, RET_STUB, sizeof RET_STUB);
    if (mprotect(p, ps, prot) != 0) _exit(11);
    if (with_handler) {
        struct sigaction sa = {0};
        sa.sa_sigaction = handler;
        sa.sa_flags = SA_SIGINFO;
        sigaction(SIGSEGV, &sa, NULL);
    }
    ((void (*)(void))p)();
    _exit(42); // reached only if the non-exec page (wrongly) executed
}

static int run(int prot, int with_handler) {
    pid_t pid = fork();
    if (pid == 0) child_exec(prot, with_handler);
    int st = 0;
    waitpid(pid, &st, 0);
    return st;
}

int main(void) {
    struct { const char *name; int prot; } cases[] = {
        {"prot_read", PROT_READ},
        {"prot_none", PROT_NONE},
    };
    for (unsigned i = 0; i < sizeof cases / sizeof cases[0]; i++) {
        int sd = run(cases[i].prot, 0);
        int sh = run(cases[i].prot, 1);
        printf("%s killed_segv=%d handler_accerr=%d\n", cases[i].name,
               WIFSIGNALED(sd) && WTERMSIG(sd) == SIGSEGV,
               WIFEXITED(sh) && WEXITSTATUS(sh) == 100);
    }
    return 0;
}
