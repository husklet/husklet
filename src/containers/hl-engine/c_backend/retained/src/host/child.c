#define _POSIX_C_SOURCE 200809L
#include "child.h"

#include <fcntl.h>
#include <stddef.h>
#include <unistd.h>

static hl_host_child_watch *volatile hl_child_watch;

void hl_host_child_watch_notify(const hl_host_child_watch *watch) {
    unsigned char byte = 1;
    if (watch != NULL && watch->write_descriptor >= 0) {
        ssize_t ignored = write(watch->write_descriptor, &byte, 1);
        (void)ignored;
    }
}

static void hl_child_signal(int signal_number) {
    (void)signal_number;
    hl_host_child_watch_notify(hl_child_watch);
}

int hl_host_child_watch_init(hl_host_child_watch *watch) {
    int descriptors[2];
    struct sigaction action;
    if (watch == NULL || hl_child_watch != NULL || pipe(descriptors) != 0) return -1;
    watch->read_descriptor = descriptors[0];
    watch->write_descriptor = descriptors[1];
    watch->active = 0;
    (void)fcntl(watch->read_descriptor, F_SETFL, O_NONBLOCK);
    (void)fcntl(watch->write_descriptor, F_SETFL, O_NONBLOCK);
    (void)fcntl(watch->read_descriptor, F_SETFD, FD_CLOEXEC);
    (void)fcntl(watch->write_descriptor, F_SETFD, FD_CLOEXEC);
    action.sa_handler = hl_child_signal;
    sigemptyset(&action.sa_mask);
    action.sa_flags = 0;
    hl_child_watch = watch;
    if (sigaction(SIGCHLD, &action, &watch->previous) != 0) {
        hl_child_watch = NULL;
        close(watch->read_descriptor);
        close(watch->write_descriptor);
        watch->read_descriptor = watch->write_descriptor = -1;
        return -1;
    }
    watch->active = 1;
    return 0;
}

int hl_host_child_watch_descriptor(const hl_host_child_watch *watch) {
    return watch != NULL && watch->active ? watch->read_descriptor : -1;
}

void hl_host_child_watch_drain(const hl_host_child_watch *watch) {
    unsigned char bytes[64];
    if (watch == NULL || !watch->active) return;
    while (read(watch->read_descriptor, bytes, sizeof bytes) > 0) {}
}

void hl_host_child_watch_close(hl_host_child_watch *watch) {
    if (watch == NULL || !watch->active) return;
    if (hl_child_watch == watch) {
        (void)sigaction(SIGCHLD, &watch->previous, NULL);
        hl_child_watch = NULL;
    }
    close(watch->read_descriptor);
    close(watch->write_descriptor);
    watch->read_descriptor = watch->write_descriptor = -1;
    watch->active = 0;
}
