/*
 * Local descriptor transport is unavailable on a Windows host.
 *
 * The POSIX checkpoint broker attaches descriptors with SCM_RIGHTS. Windows has
 * no ancillary-data channel and requires a different DuplicateHandle protocol.
 *
 * DuplicateHandle requires an explicit peer process handle and a different
 * protocol, so these raw-descriptor helpers refuse explicitly.
 */

#include "../fork_wire.h"

#include <errno.h>

int hl_fork_wire_send_descriptors(int socket, const void *buffer, size_t size, const int *descriptors,
                                  int descriptor_count) {
    (void)socket;
    (void)buffer;
    (void)size;
    (void)descriptors;
    (void)descriptor_count;
    errno = ENOSYS;
    return -1;
}

int hl_fork_wire_receive_descriptors(int socket, void *buffer, size_t size, int *descriptors, int *descriptor_count) {
    (void)socket;
    (void)buffer;
    (void)size;
    (void)descriptors;
    if (descriptor_count != NULL) *descriptor_count = 0;
    errno = ENOSYS;
    return -1;
}

int hl_fork_wire_send(int socket, const void *buffer, size_t size) {
    (void)socket;
    (void)buffer;
    (void)size;
    errno = ENOSYS;
    return -1;
}

int hl_fork_wire_receive(int socket, void *buffer, size_t size) {
    (void)socket;
    (void)buffer;
    (void)size;
    errno = ENOSYS;
    return -1;
}
