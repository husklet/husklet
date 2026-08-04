// syscall-compat regression: RLIMIT_FSIZE enforcement across the file-size-extending operations that must
// raise SIGXFSZ, not just plain write(2). With a soft limit of one page each offending operation runs in a
// child so the raw SIGXFSZ termination is observed deterministically via the child's wait status:
//   - write past the limit through an UNLINKED-while-open fd (the RAM-backed scratch path that used to skip
//     the size gate entirely),
//   - ftruncate/truncate growing a file past the limit,
//   - fallocate reserving past the limit (Linux gates it even with FALLOC_FL_KEEP_SIZE).
// truncate on a MISSING path must still be ENOENT (no signal): the limit is checked only after the file is
// known to exist. Arch-neutral: signal numbers / booleans printed.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

static void set_limit(void) {
    struct rlimit rl = {4096, 4096};
    setrlimit(RLIMIT_FSIZE, &rl);
    signal(SIGXFSZ, SIG_DFL); // default action terminates on overflow
}

// Run `body` in a child; print whether it was killed by SIGXFSZ. `body` returns an exit code used only when
// no signal fires (which would itself be the bug the expected output rejects).
static void probe(const char *tag, int (*body)(void)) {
    pid_t pid = fork();
    if (pid == 0)
        _exit(body());
    int status = 0;
    waitpid(pid, &status, 0);
    int sigxfsz = WIFSIGNALED(status) && WTERMSIG(status) == SIGXFSZ;
    printf("%s: sigxfsz=%d exit=%d\n", tag, sigxfsz, WIFEXITED(status) ? WEXITSTATUS(status) : -1);
}

static int body_write_unlinked(void) {
    char tmpl[] = "/tmp/fso_w_XXXXXX";
    int fd = mkstemp(tmpl);
    unlink(tmpl); // RAM-backed scratch fd: an unlinked-while-open regular file
    set_limit();
    char buf[4096];
    memset(buf, 'x', sizeof buf);
    if (write(fd, buf, 4096) != 4096) return 10;
    write(fd, buf, 1); // over the limit -> SIGXFSZ
    return 11;         // reached only if no signal fired
}

static int body_ftruncate(void) {
    char tmpl[] = "/tmp/fso_ft_XXXXXX";
    int fd = mkstemp(tmpl);
    unlink(tmpl);
    set_limit();
    ftruncate(fd, 8192); // grow past the limit -> SIGXFSZ
    return 20;
}

static int body_truncate(void) {
    char path[] = "/tmp/fso_tr_file";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return 30;
    close(fd);
    set_limit();
    truncate(path, 8192); // grow past the limit -> SIGXFSZ
    unlink(path);
    return 31;
}

static int body_fallocate(void) {
    char tmpl[] = "/tmp/fso_fa_XXXXXX";
    int fd = mkstemp(tmpl);
    unlink(tmpl);
    set_limit();
    fallocate(fd, 0, 0, 8192); // reserve past the limit -> SIGXFSZ
    return 40;
}

int main(void) {
    probe("write_unlinked", body_write_unlinked);
    probe("ftruncate", body_ftruncate);
    probe("truncate", body_truncate);
    probe("fallocate", body_fallocate);

    // truncate on a missing path is ENOENT, never SIGXFSZ (the limit is only enforced once the target
    // resolves). Run in-process: no signal should fire, and errno must be ENOENT.
    struct rlimit rl = {4096, 4096};
    setrlimit(RLIMIT_FSIZE, &rl);
    signal(SIGXFSZ, SIG_DFL);
    int r = truncate("/tmp/fso_missing_zzz", 8192);
    printf("truncate_missing: rc=%d enoent=%d\n", r, r < 0 && errno == ENOENT);
    return 0;
}
