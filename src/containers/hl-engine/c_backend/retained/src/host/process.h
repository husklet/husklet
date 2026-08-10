#ifndef HL_HOST_PROCESS_H
#define HL_HOST_PROCESS_H

#include <sys/types.h>

// Return a close-on-exec descriptor that becomes persistently readable when pid exits.
// The caller owns the descriptor. Returns -1 with errno set when the host cannot watch pid.
int hl_host_process_open(pid_t pid);

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

#endif
