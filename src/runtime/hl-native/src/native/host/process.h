#ifndef HL_HOST_PROCESS_H
#define HL_HOST_PROCESS_H

#include "system.h"

#include <sys/types.h>
#include <unistd.h>

// Return a close-on-exec descriptor that becomes persistently readable when pid exits.
// The caller owns the descriptor. Returns -1 with errno set when the host cannot watch pid.
int hl_host_process_open(pid_t pid);

/* Mint a stable liveness capability for exactly one macOS process incarnation.
 * `expected_birth` is zero when the caller has not yet observed the process and
 * otherwise fences the result to that birth timestamp. The returned close-on-
 * exec descriptor is owned by the caller and becomes readable on NOTE_EXIT.
 * Other hosts use their native process-handle primitive at the call site. */
#if defined(__APPLE__)
int hl_host_process_identity_open(pid_t pid, uint64_t expected_birth, uint64_t expected_generation,
                                  uint64_t *actual_birth, uint64_t *actual_generation);
int hl_host_process_peer_identity_open(int socket_descriptor, uint64_t claimed_pid, uint64_t *actual_pid,
                                       uint64_t *actual_birth, uint64_t *actual_generation);
#endif

#if defined(_WIN32)
/*
 * Guest fork(2) and its reaping half, for the one host that has no POSIX
 * spelling of either.
 *
 * These are the bridge the ABI layer's <sys/wait.h> replacement could not build
 * for itself: that layer holds a pid_t, and on Windows a pid names no waitable
 * object -- you cannot wait on one without first opening it, and the layer owns
 * no handle table. The backend does, so the table lives there and these four
 * entry points are what the layer calls instead of fork/waitpid/kill/getppid.
 *
 * They are errno-shaped rather than hl_host_result-shaped on purpose. Every
 * caller is a POSIX call being emulated, the errno values are part of what is
 * being emulated (ECHILD in particular is load-bearing to a shell), and routing
 * them through the typed result only to translate back would lose the
 * distinctions the callers act on.
 */

/* Clone this process. 0 in the child, the child's pid in the parent, -1/errno on
 * failure. Only the calling thread is carried into the child. */
int hl_host_windows_fork(void);

/* Reap a child of this process. Returns the reaped pid and stores a LINUX status
 * word, 0 for WNOHANG with nothing to report, -1/errno otherwise. */
int hl_host_windows_waitpid(pid_t pid, int *status, int options);

/* Signal 0 (liveness) and SIGKILL are real; every other signal is ENOSYS. */
int hl_host_windows_kill(pid_t pid, int signo);

/* The creating process id, or -1. Not maintained across the parent's death. */
int hl_host_windows_parent_pid(void);

/* Drop every inherited child record. Called by hl_host_windows_fork on the child
 * side; exposed so a child that arrives by some other route can do the same. */
void hl_host_windows_fork_child_reset(void);
#endif

static inline pid_t hl_host_process_clone_current(void) {
#if defined(_WIN32)
    return hl_host_windows_fork();
#else
    return fork();
#endif
}

#endif
