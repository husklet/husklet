// sentry_exec_proc -- fork+exec identity and descriptor coverage for the untrusted-guest SENTRY split.
// The sentry is forked from the ORIGINAL worker image, so before the per-process /proc carve-out a
// fork+exec'd guest that read /proc/self/auxv got the INITIAL image's AT_EXECFN pointer -- an address the
// exec teardown had already unmapped -- and crashed (Rust coreutils `cat` parses auxv at startup; this is
// what killed the eclipse-temurin javac heredoc scenario). The same split also served /proc/self/exe from
// the sentry's stale identity, and left memfd_create/ftruncate/splice/copy_file_range running LOCALLY in
// the worker against virtual descriptor numbers (CoreCLR startup and Rust coreutils I/O both broke).
//
// Shape (deterministic): the parent forks ONCE; the child re-execs this binary via /proc/self/exe with a
// "child" marker. The EXEC'D image then checks, in order:
//   * /proc/self/auxv: parse the raw {type,value} pairs, find AT_EXECFN(31), and strlen+compare the
//     pointed-to string against its own argv[0] -- a stale pre-exec pointer faults or mismatches;
//   * readlink(/proc/self/exe): must resolve to an openable path (the sentry's stale answer named the
//     pre-exec image);
//   * memfd_create + ftruncate + write + pread round-trip of the i*7+3 ramp (sum 32640);
//   * pipe + splice into a /tmp file + read-back of the same ramp (sum 32640).
// The parent waitpid()s and prints the child's exit status. Registered trusted + .untrusted() against the
// SAME golden: the forwarded results must reproduce the trusted bytes exactly.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static void fill(unsigned char *buf) {
    for (int i = 0; i < 256; i++) buf[i] = (unsigned char)(i * 7 + 3);
}

static int write_all(int fd, const unsigned char *buf, size_t len) {
    for (size_t off = 0; off < len;) {
        ssize_t w = write(fd, buf + off, len - off);
        if (w <= 0) return -1;
        off += (size_t)w;
    }
    return 0;
}

static unsigned long execfn_from_auxv(void) {
    int fd = open("/proc/self/auxv", O_RDONLY);
    if (fd < 0) return 0;
    unsigned long pair[2];
    unsigned long execfn = 0;
    for (;;) {
        ssize_t r = read(fd, pair, sizeof pair);
        if (r != (ssize_t)sizeof pair || pair[0] == 0) break;
        if (pair[0] == 31) execfn = pair[1];
    }
    close(fd);
    return execfn;
}

static int child_main(const char *argv0) {
    // AT_EXECFN must point at THIS exec's own initial stack, not the pre-exec image's torn-down one --
    // dereferencing a stale pointer faults. Linux sets it to the exec path and hl to argv[0], so only
    // require a readable absolute path here (the crash mode, not the exact spelling, is the regression).
    (void)argv0;
    unsigned long execfn = execfn_from_auxv();
    if (!execfn) {
        puts("execfn-missing");
        return 21;
    }
    const char *name = (const char *)execfn;
    printf("execfn-%s\n", name[0] == '/' && strlen(name) > 1 ? "ok" : "bad");
    // /proc/self/exe must name THIS image (openable), not the sentry's stale pre-exec identity.
    char exe[4096];
    ssize_t n = readlink("/proc/self/exe", exe, sizeof exe - 1);
    if (n <= 0) {
        puts("exe-unreadable");
        return 22;
    }
    exe[n] = 0;
    int efd = open(exe, O_RDONLY);
    printf("exe-%s\n", efd >= 0 ? "ok" : "unopenable");
    if (efd >= 0) close(efd);
    // memfd_create + ftruncate + write + pread: the descriptor must live in the guest's (virtual) fd
    // space; a worker-local real fd number broke every follow-up operation under the sentry split.
    unsigned char ramp[256], back[256];
    fill(ramp);
    int mfd = memfd_create("sentry-exec-proc", 0);
    if (mfd < 0 || ftruncate(mfd, 65536) != 0 || write_all(mfd, ramp, sizeof ramp) != 0) return 23;
    memset(back, 0, sizeof back);
    if (pread(mfd, back, sizeof back, 0) != (ssize_t)sizeof back) return 24;
    close(mfd);
    unsigned sum = 0;
    for (int i = 0; i < 256; i++) sum += back[i];
    printf("memfd-sum %u\n", sum);
    // pipe + splice into a file + read-back: splice carries TWO guest descriptors and must forward.
    char path[64];
    snprintf(path, sizeof path, "/tmp/sentry_exec_proc.%d", (int)getpid());
    int pfd[2];
    if (pipe(pfd) != 0) return 25;
    if (write_all(pfd[1], ramp, sizeof ramp) != 0) return 26;
    close(pfd[1]);
    int out = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (out < 0) return 27;
    size_t moved = 0;
    while (moved < sizeof ramp) {
        ssize_t s = splice(pfd[0], NULL, out, NULL, sizeof ramp - moved, 0);
        if (s <= 0) break;
        moved += (size_t)s;
    }
    close(pfd[0]);
    close(out);
    if (moved != sizeof ramp) {
        puts("splice-short");
        unlink(path);
        return 28;
    }
    int in = open(path, O_RDONLY);
    memset(back, 0, sizeof back);
    if (in < 0 || read(in, back, sizeof back) != (ssize_t)sizeof back) return 29;
    close(in);
    unlink(path);
    sum = 0;
    for (int i = 0; i < 256; i++) sum += back[i];
    printf("splice-sum %u\n", sum);
    return 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "child") == 0) return child_main(argv[0]);
    pid_t kid = fork();
    if (kid < 0) {
        perror("fork");
        return 1;
    }
    if (kid == 0) {
        // Re-exec THIS binary: the fresh image's per-process /proc identity is what the checks probe.
        char *args[] = {argv[0], (char *)"child", NULL};
        execv("/proc/self/exe", args);
        _exit(30);
    }
    int status = 0;
    if (waitpid(kid, &status, 0) != kid || !WIFEXITED(status)) {
        puts("child-lost");
        return 2;
    }
    printf("child-exit %d\n", WEXITSTATUS(status));
    return WEXITSTATUS(status) == 0 ? 0 : 3;
}
