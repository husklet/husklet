// #139/#222 regression guard: a fork -> execve CHILD that FAULTS, plus a main-thread handled fault under
// CRASHDBG. A compiler DRIVER (gcc/clang) forks and execve's sub-processes (cc1/as/ld/collect2); such an
// exec'd child may take a guest CPU fault -- one it HANDLES via a registered SIGSEGV handler (cc1's fatal-
// signal handler, glibc stack-overflow detection) or an unhandled one the kernel turns into SIGSEGV. hl
// must re-establish the faulting thread's signal/Mach-exception state across fork + in-process execve and
// deliver both fates exactly as Linux.
//
//   (no arg)     driver: fork; child execve(self, MODE); parent waitpid; report the child's fate.
//   handled      exec'd child: install a SIGSEGV/SIGBUS handler, deref NULL, siglongjmp out, _exit(42)
//   unhandled    exec'd child: deref NULL with NO handler -> killed by SIGSEGV (parent sees WIFSIGNALED 11)
//   mainhandled  NO fork/exec: take a handled fault on the MAIN thread (exercises the aarch64 CRASHDBG Mach
//                exception port's guest-fault delivery, whose handler runs on a dedicated exc_thread) and
//                print a deterministic line -> golden. Pre-fix this was a spurious [MACH] abort under CRASHDBG.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <setjmp.h>
#include <unistd.h>
#include <sys/wait.h>

static sigjmp_buf jb;
static volatile sig_atomic_t caught;
static void onsegv(int sig) { (void)sig; caught = 1; siglongjmp(jb, 1); }

static void install_handler(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = onsegv;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
}

// Fault, catch it, return 42; or return 8 if the handler never ran.
static int fault_and_catch(void) {
    install_handler();
    if (sigsetjmp(jb, 1) == 0) {
        volatile int *p = (int *)0x0;
        *p = 123;   // fault
        return 7;   // never reached
    }
    return caught ? 42 : 8;
}

static int run_child(const char *mode) {
    if (!strcmp(mode, "handled")) return fault_and_catch();
    // unhandled: crash for real
    volatile int *p = (int *)0x0;
    *p = 123;
    return 9; // never reached
}

int main(int argc, char **argv) {
    if (argc == 2) {
        if (!strcmp(argv[1], "mainhandled")) {
            int r = fault_and_catch();
            printf(r == 42 ? "mainhandled ok\n" : "mainhandled BAD %d\n", r);
            return 0;
        }
        return run_child(argv[1]);
    }

    const char *modes[] = {"handled", "unhandled"};
    for (int m = 0; m < 2; m++) {
        pid_t pid = fork();
        if (pid < 0) { printf("fork_fail\n"); return 1; }
        if (pid == 0) {
            execl(argv[0], argv[0], modes[m], (char *)NULL);
            _exit(97); // exec failed
        }
        int st = 0;
        if (waitpid(pid, &st, 0) != pid) { printf("wait_fail\n"); return 1; }
        if (WIFEXITED(st))
            printf("%s: exited %d\n", modes[m], WEXITSTATUS(st));
        else if (WIFSIGNALED(st))
            printf("%s: signal %d\n", modes[m], WTERMSIG(st));
        else
            printf("%s: unknown\n", modes[m]);
        fflush(stdout);
    }
    return 0;
}
