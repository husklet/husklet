#ifndef HL_HOST_CHILD_H
#define HL_HOST_CHILD_H

#include <signal.h>

typedef struct hl_host_child_watch {
    int read_descriptor;
    int write_descriptor;
    int active;
    /* The SIGCHLD disposition this watch displaced, so close() can put it back.
     * Absent on a host with no sigaction: struct sigaction is not merely
     * unimplemented there, it is undeclared, and an incomplete member would make
     * this whole structure unusable rather than one field unavailable. Nothing
     * outside the POSIX implementation reads it. */
#if !defined(_WIN32)
    struct sigaction previous;
#endif
} hl_host_child_watch;

int hl_host_child_watch_init(hl_host_child_watch *watch);
int hl_host_child_watch_descriptor(const hl_host_child_watch *watch);
void hl_host_child_watch_notify(const hl_host_child_watch *watch);
void hl_host_child_watch_drain(const hl_host_child_watch *watch);
void hl_host_child_watch_close(hl_host_child_watch *watch);

#endif
