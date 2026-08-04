// signals-compat regression: a load from a file-backed mmap region that lies BEYOND the file's last page
// raises SIGBUS (not SIGSEGV) with si_code == BUS_ADRERR, exactly as Linux does. The engine used to rewrite
// every host SIGBUS to SIGSEGV (a macOS-only disambiguation that has no place on a Linux host, where the
// kernel already raises SIGBUS only for genuine bus errors), so a guest SIGBUS handler never ran and the
// disposition/si_code were wrong.
//
// Two observations, each in its own child so state is clean and the raw termination is deterministic:
//   - handler installed: the fault is delivered as SIGBUS with si_code BUS_ADRERR; the handler records it
//     and _exit()s with a distinctive code,
//   - default disposition: the process is terminated by SIGBUS (observed via the child wait status).
// The mapping is two pages over a one-page file, and the probe touches the first byte of the second (unbacked)
// page. Arch-neutral output; verified on both the aarch64 and x86_64 guest file-mmap fault paths.
#define _GNU_SOURCE
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile int caught_sig, caught_code;

static void handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    caught_sig = sig;
    caught_code = si->si_code;
    // Report through the exit code (async-signal-safe): 100 = SIGBUS+BUS_ADRERR (correct), 101 otherwise.
    _exit((sig == SIGBUS && si->si_code == BUS_ADRERR) ? 100 : 101);
}

// Map two pages over a one-page file; return the mapping (m[4096] is the unbacked past-EOF byte).
static char *map_past_eof(void) {
    char tmpl[] = "/tmp/sbe_XXXXXX";
    int fd = mkstemp(tmpl);
    if (fd < 0) return NULL;
    unlink(tmpl);
    if (ftruncate(fd, 4096) != 0) return NULL;
    char *m = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    return m == MAP_FAILED ? NULL : m;
}

static void touch_past_eof(int with_handler) {
    if (with_handler) {
        struct sigaction sa = {0};
        sa.sa_sigaction = handler;
        sa.sa_flags = SA_SIGINFO;
        sigaction(SIGBUS, &sa, NULL);
    } else {
        signal(SIGBUS, SIG_DFL);
    }
    char *m = map_past_eof();
    if (!m) _exit(2);
    volatile char v = m[4096];
    (void)v;
    _exit(3); // reached only if no signal fired
}

int main(void) {
    // 1) handler-observed disposition + si_code (child exits 100 iff SIGBUS/BUS_ADRERR)
    pid_t p1 = fork();
    if (p1 == 0) touch_past_eof(1);
    int s1 = 0;
    waitpid(p1, &s1, 0);
    printf("handler: is_sigbus_adrerr=%d\n", WIFEXITED(s1) && WEXITSTATUS(s1) == 100);

    // 2) default disposition terminates by SIGBUS
    pid_t p2 = fork();
    if (p2 == 0) touch_past_eof(0);
    int s2 = 0;
    waitpid(p2, &s2, 0);
    printf("default: is_sigbus=%d\n", WIFSIGNALED(s2) && WTERMSIG(s2) == SIGBUS);
    return 0;
}
