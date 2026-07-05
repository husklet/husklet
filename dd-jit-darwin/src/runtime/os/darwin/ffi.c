// dd-jit-darwin FFI spawn shim — the C side of the typed launch contract (see include/ddjit_api.h).
//
// This TU deliberately references NO engine symbols (only libc: posix_spawn/pipe/write), so it links
// into the Rust host process safely — the engine itself only ever runs in the spawned child. The child
// is the arch-matching engine binary invoked as `<engine_path> --configfd <read-end>`; we write the
// serialized `ddjit_config` buffer to the write end and close it, so the engine reads exactly one
// config and hits EOF. The child's guest-exit `_exit()` reaps it, so the returned pid is the whole
// container's lifetime.
#include "../../include/ddjit_api.h"

#include <errno.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
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

pid_t ddjit_spawn(const char *engine_path, const uint8_t *config, size_t config_len) {
    int pfd[2];
    if (pipe(pfd) != 0) return -1;

    // The child inherits the read end; we keep the write end. Number the read end deterministically so
    // the engine is told exactly which fd to read (`--configfd <read-end>`).
    char fdbuf[16];
    snprintf(fdbuf, sizeof fdbuf, "%d", pfd[0]);

    // The write end must NOT leak into the child (else the engine never sees EOF).
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_addclose(&fa, pfd[1]);

    char *const argv[] = {(char *)engine_path, (char *)"--configfd", fdbuf, NULL};
    pid_t pid = -1;
    int rc = posix_spawn(&pid, engine_path, &fa, NULL, argv, environ);
    posix_spawn_file_actions_destroy(&fa);
    close(pfd[0]); // the parent's copy of the read end is done with

    if (rc != 0) {
        close(pfd[1]);
        errno = rc;
        return -1;
    }

    // Stream the config, then close so the engine reads one buffer and EOFs.
    int werr = write_all(pfd[1], config, config_len);
    close(pfd[1]);
    if (werr != 0) {
        // The child is already running with a truncated/absent config; it will fail its own launch. We
        // still return the pid so the caller can reap it and see the failure exit.
    }
    return pid;
}
