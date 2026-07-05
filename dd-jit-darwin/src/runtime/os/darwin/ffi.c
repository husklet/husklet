// dd-jit-darwin FFI spawn shim — the C side of the typed launch contract (see include/ddjit_api.h).
//
// This TU deliberately references NO engine symbols (only libc: fork/execve/pipe/write/dup2/ioctl), so
// it links into the Rust host process safely — the engine itself only ever runs in the spawned child.
// The child is the arch-matching engine binary invoked as `<engine_path> --configfd <read-end>`; we
// write the serialized `ddjit_config` buffer to the write end and close it, so the engine reads exactly
// one config and hits EOF. We `fork()` rather than `posix_spawn` because the caller may need the child to
// lead its own process group (pause/kill via killpg) and/or own a controlling terminal (interactive PTY)
// — both require setpgid/setsid/TIOCSCTTY in the child before exec, which posix_spawn cannot express.
#include "../../include/ddjit_api.h"

#include <errno.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <unistd.h>

extern char **environ;

static int write_all(int fd, const uint8_t *p, size_t n) {
    while (n) {
        ssize_t w = write(fd, p, n);
        if (w < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        p += (size_t)w;
        n -= (size_t)w;
    }
    return 0;
}

// Async-signal-safe unsigned->decimal: snprintf may take a lock, so it is unsafe between fork and exec.
// Writes the digits of `v` into `buf` (assumed large enough) and NUL-terminates.
static void u32_to_str(unsigned v, char *buf) {
    char tmp[16];
    int i = 0;
    do {
        tmp[i++] = (char)('0' + (v % 10));
        v /= 10;
    } while (v);
    int j = 0;
    while (i) buf[j++] = tmp[--i];
    buf[j] = '\0';
}

pid_t ddjit_spawn(const char *engine_path, const uint8_t *config, size_t config_len,
                  int in_fd, int out_fd, int err_fd, uint32_t flags) {
    int pfd[2];
    if (pipe(pfd) != 0) return -1;

    pid_t pid = fork();
    if (pid < 0) {
        int e = errno;
        close(pfd[0]);
        close(pfd[1]);
        errno = e;
        return -1;
    }

    if (pid == 0) {
        // CHILD — only async-signal-safe calls until execve (fork left just this thread running).
        close(pfd[1]); // never hold the write end, else the engine never sees config EOF

        // Placement: own process group so the caller's killpg reaches the whole container.
        if (flags & DDJIT_SPAWN_SETPGID) setpgid(0, 0);
        // Controlling terminal: become a session leader, then claim the pty slave as our ctty. The caller
        // passes the SAME slave fd as in/out/err, so in_fd names it (0 = "don't steal from another session").
        if (flags & DDJIT_SPAWN_TTY) {
            setsid();
            if (in_fd >= 0) ioctl(in_fd, TIOCSCTTY, 0);
        }

        // Move the config read end above fd 2 so wiring stdio below can't clobber it (a fresh pipe fd is
        // normally >= 3 already; this only fires if the caller closed some of 0/1/2).
        int cfd = pfd[0];
        while (cfd <= 2) {
            int n = fcntl(cfd, F_DUPFD, 3);
            if (n < 0) _exit(127);
            cfd = n;
        }

        // Wire the child's stdio. Each of in/out/err, when supplied and not already in place, is dup2'd
        // onto the target descriptor (dup2 clears close-on-exec on the target, so it survives exec).
        if (in_fd >= 0 && in_fd != 0) dup2(in_fd, 0);
        if (out_fd >= 0 && out_fd != 1) dup2(out_fd, 1);
        if (err_fd >= 0 && err_fd != 2) dup2(err_fd, 2);

        // Tell the engine which fd carries the config (built without snprintf — see u32_to_str).
        char fdbuf[16];
        u32_to_str((unsigned)cfd, fdbuf);
        char *const argv[] = {(char *)engine_path, (char *)"--configfd", fdbuf, NULL};
        execve(engine_path, argv, environ);
        _exit(127); // exec failed
    }

    // PARENT — the caller owns the in/out/err fds (it closes its own copies); we touch only the pipe.
    close(pfd[0]); // the parent never reads the config
    int werr = write_all(pfd[1], config, config_len);
    close(pfd[1]); // EOF to the engine so it reads exactly one buffer
    // If the write failed the child is running with a truncated/absent config and will fail its own
    // launch; we still return the pid so the caller can reap it and observe the failure exit.
    (void)werr;
    return pid;
}
