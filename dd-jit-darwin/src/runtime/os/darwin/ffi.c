// dd-jit-darwin FFI spawn shim — the C side of the typed launch contract (see include/ddjit_api.h).
//
// This TU deliberately references NO engine symbols (only libc: fork/execve/write/dup2/ioctl), so
// it links into the Rust host process safely — the engine itself only ever runs in the spawned child.
// The child is the arch-matching engine binary invoked as `<engine_path> --configfile <path>`. The
// serialized `ddjit_config` is written to a private temp file beside the engine; the engine opens and
// unlinks it at entry. We use a path instead of an inherited fd because some launch environments close all
// non-stdio descriptors across exec despite FD_CLOEXEC being clear. We `fork()` rather than `posix_spawn`
// because the caller may need the child to lead its own process group (pause/kill via killpg) and/or own a
// controlling terminal (interactive PTY) — both require setpgid/setsid/TIOCSCTTY in the child before exec.
#include "../../include/ddjit_api.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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

pid_t ddjit_spawn(const char *engine_path, const uint8_t *config, size_t config_len,
                  int in_fd, int out_fd, int err_fd, uint32_t flags) {
    int dbg = getenv("DD_SPAWN_DEBUG") != NULL;
    char cfgpath[1024];
    char cfgdir[1024];
    snprintf(cfgdir, sizeof cfgdir, "%s", engine_path);
    char *slash = strrchr(cfgdir, '/');
    if (slash) *slash = '\0';
    else snprintf(cfgdir, sizeof cfgdir, ".");
    snprintf(cfgpath, sizeof cfgpath, "%s/.ddjit-config-%ld-XXXXXX", cfgdir, (long)getpid());
    int cfgfd = mkstemp(cfgpath);
    if (cfgfd < 0) return -1;
    int werr = write_all(cfgfd, config, config_len);
    int saved = errno;
    close(cfgfd);
    if (werr != 0) {
        unlink(cfgpath);
        errno = saved;
        return -1;
    }
    if (dbg)
        fprintf(stderr, "[DDSPAWN] engine=%s config_len=%zu configfile=%s io=%d,%d,%d flags=0x%x\n",
                engine_path, config_len, cfgpath, in_fd, out_fd, err_fd, flags);

    pid_t pid = fork();
    if (pid < 0) {
        int e = errno;
        unlink(cfgpath);
        errno = e;
        return -1;
    }

    if (pid == 0) {
        // CHILD — only async-signal-safe calls until execve (fork left just this thread running).

        // Placement: own process group so the caller's killpg reaches the whole container.
        if (flags & DDJIT_SPAWN_SETPGID) setpgid(0, 0);
        // Controlling terminal: become a session leader, then claim the pty slave as our ctty. The caller
        // passes the SAME slave fd as in/out/err, so in_fd names it (0 = "don't steal from another session").
        if (flags & DDJIT_SPAWN_TTY) {
            setsid();
            if (in_fd >= 0) ioctl(in_fd, TIOCSCTTY, 0);
        }

        // Wire the child's stdio. Each of in/out/err, when supplied and not already in place, is dup2'd
        // onto the target descriptor (dup2 clears close-on-exec on the target, so it survives exec).
        if (in_fd >= 0 && in_fd != 0) dup2(in_fd, 0);
        if (out_fd >= 0 && out_fd != 1) dup2(out_fd, 1);
        if (err_fd >= 0 && err_fd != 2) dup2(err_fd, 2);

        char *const argv[] = {(char *)engine_path, (char *)"--configfile", cfgpath, NULL};
        execve(engine_path, argv, environ);
        _exit(127); // exec failed
    }

    return pid;
}
