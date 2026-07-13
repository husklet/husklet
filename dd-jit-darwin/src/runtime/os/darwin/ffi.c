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

// Build the environment for the spawned engine, GUARANTEEING OBJC_DISABLE_INITIALIZE_FORK_SAFETY is set.
//
// The engine implements a guest fork() as a real host fork() of a multithreaded process that touches
// ObjC/CoreFoundation/Foundation/IOSurface on the host-GPU (--gui) path. libobjc's initialize-fork-safety
// guard aborts a fork child if an ObjC +initialize was in progress at fork() — unless libobjc read
// OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES. libobjc reads it EXACTLY ONCE, at the engine's dyld load, from
// the environ we hand execve(). Rather than rely on each caller (the Rust launcher, the daemon, tests)
// remembering to put it in the environment, guarantee it for EVERY spawn here, at the one execve() that
// starts an engine. The primary, load-independent fix is the engine's own startup prewarm
// (vfs.c dd_gpu_prewarm_fork_safety, which forces those +initialize's to completion before any guest
// thread/fork); this is defense-in-depth.
//
// Built as a private envp copy so we neither mutate this host process's environ (other host threads may be
// reading it) nor call the non-async-signal-safe setenv() in the fork child. An explicit caller-provided
// value (YES, or NO to debug fork hygiene) is left untouched. Returns `environ` unchanged when the var is
// already present, else a malloc'd array the caller frees in the PARENT after fork (the forked child
// inherits a COW copy for execve; only the pointer array is owned, its strings are borrowed from
// `environ` + one static literal). NULL is never returned; on allocation failure we fall back to `environ`.
static char **ddjit_child_env(void) {
    static const char *KEY = "OBJC_DISABLE_INITIALIZE_FORK_SAFETY=";
    size_t keylen = strlen(KEY);
    size_t n = 0;
    for (char **e = environ; *e; e++) {
        if (strncmp(*e, KEY, keylen) == 0) return environ; // caller already set it (any value) -> honor it
        n++;
    }
    char **copy = (char **)malloc((n + 2) * sizeof(char *));
    if (!copy) return environ; // OOM: the prewarm still closes the race; don't fail the spawn over this
    for (size_t i = 0; i < n; i++) copy[i] = environ[i];
    copy[n] = (char *)"OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES";
    copy[n + 1] = NULL;
    return copy;
}

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
    // Build the engine's environ (with OBJC_DISABLE_INITIALIZE_FORK_SAFETY guaranteed) BEFORE fork, so the
    // child only has to execve() with it — no malloc/setenv in the async-signal-only fork child.
    char **child_env = ddjit_child_env();
    int env_owned = (child_env != environ); // ddjit_child_env malloc'd it -> the parent must free it
    pid_t pid = fork();
    if (pid < 0) {
        int e = errno;
        unlink(cfgpath);
        if (env_owned) free(child_env); // parent-only: reclaim the array the child never inherited
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
        execve(engine_path, argv, child_env);
        _exit(127); // exec failed
    }

    // PARENT: the child has its own COW copy of the array for execve, so free our copy (never in the
    // child, where free() is not async-signal-safe). Fixes a one-array-per-launch leak in a long-lived
    // parent (daemon) whenever OBJC_DISABLE_INITIALIZE_FORK_SAFETY was not already in the environment.
    if (env_owned) free(child_env);
    return pid;
}
