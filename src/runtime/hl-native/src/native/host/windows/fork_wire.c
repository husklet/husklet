/*
 * The forkserver client transport on a Windows host.
 *
 * The POSIX implementation of this group is one thing end to end: a connected
 * AF_UNIX SOCK_STREAM socket, over which a launch request travels as ordinary
 * bytes and the client's three standard streams travel as SCM_RIGHTS ancillary
 * data. The descriptor passing is not an optimisation -- it is the mechanism.
 * A forkserver worker becomes the client's process by inheriting the client's
 * stdin, stdout and stderr, and there is no version of this protocol that omits
 * that step and still works.
 *
 * Windows has neither half in a form this signature can use:
 *
 *   - AF_UNIX sockets do exist here (Windows 10 1803 and later), but SCM_RIGHTS
 *     does not, on any transport. There is no ancillary-data channel in Winsock
 *     at all.
 *   - the operation that DOES pass a handle between processes is DuplicateHandle,
 *     and it is shaped differently in a way that matters: it is initiated by a
 *     process that holds a handle to the OTHER process, not sent alongside a
 *     message on a connection. So the transfer is not something the client can
 *     attach to its request; it is something the server must reach out and
 *     perform, which means the server needs the client's pid and the right to
 *     open it, and the protocol needs a round trip that does not exist today.
 *
 * That is a protocol change rather than a port, so every entry point here is a
 * REFUSAL. The failure is loud and immediate at open(), which is the right
 * place: a client that cannot reach a forkserver falls back to launching the
 * guest itself, and that fallback is the path this host actually runs today.
 *
 * The four hl_fork_wire_* transport helpers take a raw int socket and are the
 * server side's as much as the client's; they refuse for the same reason and
 * additionally because that int names no host object here.
 */

#include "../fork_wire.h"

#include <errno.h>

int hl_host_fork_client_open(hl_host_fork_client *client, const char *path) {
    (void)path;
    if (client != NULL) client->private_value = 0;
    errno = ENOSYS;
    return -1;
}

int hl_host_fork_client_send_launch(hl_host_fork_client *client, const void *buffer, size_t size) {
    (void)client;
    (void)buffer;
    (void)size;
    errno = ENOSYS;
    return -1;
}

int hl_host_fork_client_receive(hl_host_fork_client *client, void *buffer, size_t size) {
    (void)client;
    (void)buffer;
    (void)size;
    errno = ENOSYS;
    return -1;
}

void hl_host_fork_client_close(hl_host_fork_client *client) {
    if (client != NULL) client->private_value = 0;
}

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
