/*
 * Child-exit notification on a Windows host.
 *
 * The POSIX implementation is the self-pipe trick around SIGCHLD: a handler
 * writes one byte to a pipe, the readable end joins the engine's readiness set,
 * and a wait loop drains it and reaps. Two of those three pieces are missing
 * here and they fail differently, which is why this is a refusal rather than a
 * translation:
 *
 *   - there is no SIGCHLD. Windows signals a child's exit by making the child's
 *     process HANDLE become signalled, which is a wait, not an interruption --
 *     there is no asynchronous notification to install a handler for and
 *     therefore nothing to write the byte from. The natural Windows shape is a
 *     wait registration (RegisterWaitForSingleObject) or a job object with a
 *     completion port, and either is a different control flow from a handler.
 *
 *   - the notification is delivered as a DESCRIPTOR, so that it can be waited
 *     on together with everything else. That mixed-handle readiness set is
 *     exactly what this host does not have a single call for -- the refusal
 *     host_poll.h documents at length -- so even a correct wait registration
 *     has nowhere to publish its result today.
 *
 * The engine's own process group already answers "wait for this child" through
 * the typed host seam, using WaitForSingleObject on the process handle, so a
 * caller that needs a child's exit status has a supported path. What this group
 * adds is asynchronous notification, and that is what is refused.
 */

#include "../child.h"

#include <errno.h>
#include <string.h>

int hl_host_child_watch_init(hl_host_child_watch *watch) {
    if (watch != NULL) {
        memset(watch, 0, sizeof(*watch));
        watch->read_descriptor = -1;
        watch->write_descriptor = -1;
        watch->active = 0;
    }
    errno = ENOSYS;
    return -1;
}

/* -1 is not a valid descriptor, so a caller that adds it to a readiness set is
 * refused there rather than waiting on descriptor 0. */
int hl_host_child_watch_descriptor(const hl_host_child_watch *watch) {
    (void)watch;
    return -1;
}

void hl_host_child_watch_notify(const hl_host_child_watch *watch) {
    (void)watch;
}

void hl_host_child_watch_drain(const hl_host_child_watch *watch) {
    (void)watch;
}

void hl_host_child_watch_close(hl_host_child_watch *watch) {
    if (watch != NULL) {
        watch->read_descriptor = -1;
        watch->write_descriptor = -1;
        watch->active = 0;
    }
}

#if defined(HL_NATIVE_TEST_HOOKS)
#include "hl/base.h"

/* The POSIX hosts define these two beside the mechanisms they drive -- the activation-ready pause sits
 * next to the self-pipe the child writes, and the force-terminate probe next to kill(-pid, SIGKILL).
 * Neither mechanism exists here (see the refusal above), but both symbols are named in the native test
 * export manifest, so the Windows artifact must still define them. A hook that merely vanishes is not a
 * compile error: it is `cannot export ...: symbol not defined` at link, and on a host whose loader
 * resolved lazily it would be a MissingBridge at load instead. */
HL_API void hl_c_backend_activation_ready_pause(int paused);
HL_API int32_t hl_c_backend_host_process_force_test(int32_t pid);

HL_API void hl_c_backend_activation_ready_pause(int paused) {
    /* The POSIX arm parks the child between fork and the readiness write so a test can observe the gap.
       There is no such child here -- the launch this would pause is the refusal above -- so there is no
       state worth keeping and nothing that could later read it. */
    (void)paused;
}

HL_API int32_t hl_c_backend_host_process_force_test(int32_t pid) {
    /* The POSIX probe asserts that a force terminate reaches the whole process GROUP. Windows has no
       process group in that sense, so there is no weaker true answer to give -- refuse rather than
       report a success the host never performed. */
    (void)pid;
    return ENOTSUP;
}
#endif
