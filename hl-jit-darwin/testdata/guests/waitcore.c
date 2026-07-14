// WCOREDUMP encoding in wait4/waitpid status (#401). When a child is terminated by a core-dumping signal
// (SIGQUIT/SIGABRT/SIGSEGV/SIGILL/SIGFPE/SIGBUS/SIGTRAP/SIGSYS/SIGXCPU/SIGXFSZ) while RLIMIT_CORE's soft
// limit is > 0, the parent's wait status must have the 0x80 (WCOREDUMP) bit set -- exactly as Linux does.
// A non-core signal (SIGKILL/SIGTERM/SIGINT/...) or a zero core limit must leave it clear, and a normal
// exit is WIFEXITED with the right code. hl historically dropped the 0x80 bit (LTP waitpid01 TFAIL).
//
// The core limit is set in the PARENT before fork (children inherit it), matching LTP waitpid01's setup.
// Children chdir into a private temp dir so the native oracle's real core file lands there; the parent
// removes it afterward. Output is deterministic (booleans only), so this runs golden AND oracle-diffed.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <dirent.h>
#include <sys/wait.h>
#include <sys/resource.h>
#include <sys/stat.h>

static char g_dir[64];

// Fork a child that kills itself with `sig` (default disposition), reap it, and print the decoded status.
static void run_sig(const char *name, int sig, int expect_core) {
    pid_t p = fork();
    if (p == 0) {
        if (chdir(g_dir) != 0) _exit(99);
        if (sig != SIGKILL) signal(sig, SIG_DFL); // ensure the core/terminate default action
        raise(sig);
        _exit(0); // unreached for fatal signals
    }
    int st = 0;
    if (waitpid(p, &st, 0) != p) { printf("%s WAITFAIL\n", name); return; }
    int core = WIFSIGNALED(st) ? (WCOREDUMP(st) ? 1 : 0) : 0;
    printf("%s signaled=%d term=%d core=%d expect=%d %s\n", name,
           WIFSIGNALED(st) ? 1 : 0, WIFSIGNALED(st) ? WTERMSIG(st) : -1,
           core, expect_core, core == expect_core ? "OK" : "MISMATCH");
    (void)expect_core;
}

static void set_core(rlim_t soft) {
    struct rlimit rl;
    getrlimit(RLIMIT_CORE, &rl);
    rl.rlim_cur = soft;
    if (rl.rlim_max != RLIM_INFINITY && soft > rl.rlim_max) rl.rlim_cur = rl.rlim_max;
    setrlimit(RLIMIT_CORE, &rl);
}

static void cleanup(void) {
    DIR *d = opendir(g_dir);
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (e->d_name[0] == '.') continue;
            char pth[128];
            snprintf(pth, sizeof pth, "%s/%s", g_dir, e->d_name);
            unlink(pth);
        }
        closedir(d);
    }
    rmdir(g_dir);
}

int main(void) {
    strcpy(g_dir, "/tmp/waitcoreXXXXXX");
    if (!mkdtemp(g_dir)) { printf("mkdtemp failed\n"); return 1; }

    // Phase 1: cores disabled (soft limit 0) -> even a core-dumping signal must NOT set WCOREDUMP.
    set_core(0);
    run_sig("quit-nocore", SIGQUIT, 0);

    // Phase 2: cores enabled (one page, like LTP) -> core-dumping signals set WCOREDUMP, others do not.
    set_core((rlim_t)getpagesize());
    run_sig("quit", SIGQUIT, 1);
    run_sig("abrt", SIGABRT, 1);
    run_sig("segv", SIGSEGV, 1);
    run_sig("fpe",  SIGFPE,  1);
    run_sig("ill",  SIGILL,  1);
    run_sig("bus",  SIGBUS,  1);
    run_sig("trap", SIGTRAP, 1);
    run_sig("sys",  SIGSYS,  1);
    run_sig("kill", SIGKILL, 0); // not a core-dumping signal
    run_sig("term", SIGTERM, 0);
    run_sig("int",  SIGINT,  0);

    // Normal exit: WIFEXITED with the exact code, no signal/core bits.
    pid_t p = fork();
    if (p == 0) _exit(7);
    int st = 0;
    waitpid(p, &st, 0);
    printf("exit exited=%d code=%d signaled=%d\n",
           WIFEXITED(st) ? 1 : 0, WIFEXITED(st) ? WEXITSTATUS(st) : -1, WIFSIGNALED(st) ? 1 : 0);

    cleanup();
    printf("waitcore done\n");
    return 0;
}
