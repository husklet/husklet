// forkserver equivalence probe (#369): one static guest with subcommands covering everything a
// fork-server-spawned engine must get right to be indistinguishable from a cold-spawned one:
// argv, environment, cwd, tty detection on 0/1/2, stdin plumbing, exit codes, fatal signals, and
// asynchronously delivered (forwarded) signals. Golden-checked cold-vs-forkserver by
// hl-tests/tests/forkserver.rs on BOTH Linux engines.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <stdint.h>
#include <sys/eventfd.h>

// Exercise the eventfd + pipe syscalls the way the perf event/pipe workloads do. This runs in the "id"
// command, so it also fires during the fork-server PREWARM run (which invokes the prewarm binary with a
// bare argv -> "id"). Two regressions this pins: (1) the prewarm parent ran guests without binding the
// per-guest host-service tables, so an eventfd guest NULL-dereferenced g_eventfd_count and SIGSEGV'd the
// resident server; (2) the warm runner (inherited prewarmed arena + pristine restore) mis-ran a fresh
// pipe/eventfd. On any failure the "id" command returns non-42 so the golden identity/warm gate catches it.
static int io_selfcheck(void) {
    int efd = eventfd(0, EFD_CLOEXEC);
    if (efd < 0) return -1;
    uint64_t v = 1;
    if (write(efd, &v, sizeof v) != (ssize_t)sizeof v || read(efd, &v, sizeof v) != (ssize_t)sizeof v || v != 1) {
        close(efd);
        return -1;
    }
    if (close(efd) != 0) return -1;
    int fds[2];
    if (pipe(fds) != 0) return -1;
    uint64_t token = UINT64_C(0x0123456789abcdef), got = 0;
    if (write(fds[1], &token, sizeof token) != (ssize_t)sizeof token ||
        read(fds[0], &got, sizeof got) != (ssize_t)sizeof got || got != token) {
        close(fds[0]);
        close(fds[1]);
        return -1;
    }
    return (close(fds[0]) != 0 || close(fds[1]) != 0) ? -1 : 0;
}

static void on_term(int s) {
    (void)s;
    const char msg[] = "TERM\n";
    ssize_t r = write(1, msg, sizeof msg - 1);
    (void)r;
    _exit(7);
}

int main(int argc, char **argv) {
    const char *cmd = argc > 1 ? argv[1] : "id";
    if (strcmp(cmd, "id") == 0) {
        if (io_selfcheck() != 0) return 1; // eventfd/pipe must work cold, served, and warm (see io_selfcheck)
        printf("argc=%d\n", argc);
        for (int i = 0; i < argc; i++)
            printf("argv[%d]=%s\n", i, argv[i]);
        const char *e = getenv("FSRV_TEST_ENV");
        printf("env=%s\n", e ? e : "(unset)");
        char cwd[4096];
        printf("cwd=%s\n", getcwd(cwd, sizeof cwd) ? cwd : "(none)");
        printf("tty=%d%d%d\n", isatty(0), isatty(1), isatty(2));
        // /proc/self/exe must be the SAME absolute, canonical path cold and through the fork-server
        // (incl. the warm prewarm path) -- #378: the forkserver warm runner used to leak a non-canonical
        // g_exe_path on aarch64. Go's os.Executable / the JVM / execv("/proc/self/exe") depend on this.
        char exe[4096];
        ssize_t el = readlink("/proc/self/exe", exe, sizeof exe - 1);
        if (el < 0) el = 0;
        exe[el] = 0;
        printf("exe=%s\n", exe);
        return 42;
    }
    if (strcmp(cmd, "exit") == 0) return argc > 2 ? atoi(argv[2]) : 0;
    if (strcmp(cmd, "stdin") == 0) { // echo stdin back, prefixed (stdio fd passing)
        char b[4096];
        ssize_t n;
        while ((n = read(0, b, sizeof b)) > 0) {
            if (write(1, "<", 1) < 0 || write(1, b, (size_t)n) < 0) return 1;
        }
        return 0;
    }
    if (strcmp(cmd, "segv") == 0) { // die by SIGSEGV: the caller must observe the same fatal signal
        *(volatile int *)0 = 1;
        return 99; // unreachable
    }
    if (strcmp(cmd, "waitterm") == 0) { // forwarded-signal delivery (client ^C/kill -> runner -> guest)
        signal(SIGTERM, on_term);
        if (puts("ready") < 0) return 1;
        fflush(stdout);
        for (;;)
            pause();
    }
    fprintf(stderr, "usage: probe id|exit N|stdin|segv|waitterm\n");
    return 2;
}
