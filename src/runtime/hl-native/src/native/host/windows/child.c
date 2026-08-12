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
