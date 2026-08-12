#include "fork_wire.h"

#include <errno.h>
#include <stdint.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/types.h>
#include <unistd.h>

#define HL_FORK_WIRE_MAX_DESCRIPTORS 8

static int hl_fork_client_descriptor(const hl_host_fork_client *client) {
    if (client == NULL || client->private_value == 0 || client->private_value > (uintptr_t)INT32_MAX) {
        errno = EINVAL;
        return -1;
    }
    return (int)(client->private_value - 1);
}

int hl_host_fork_client_open(hl_host_fork_client *client, const char *path) {
    struct sockaddr_un address;
    int descriptor;
    size_t length;
    if (client == NULL || path == NULL || client->private_value != 0) {
        errno = EINVAL;
        return -1;
    }
    length = strlen(path);
    if (length == 0 || length >= sizeof address.sun_path) {
        errno = ENAMETOOLONG;
        return -1;
    }
    descriptor = socket(AF_UNIX, SOCK_STREAM, 0);
    if (descriptor < 0) return -1;
    memset(&address, 0, sizeof address);
    address.sun_family = AF_UNIX;
    memcpy(address.sun_path, path, length + 1);
    if (connect(descriptor, (struct sockaddr *)&address, sizeof address) != 0) {
        int saved_error = errno;
        (void)close(descriptor);
        errno = saved_error;
        return -1;
    }
    client->private_value = (uintptr_t)descriptor + 1;
    return 0;
}

int hl_host_fork_client_send_launch(hl_host_fork_client *client, const void *buffer, size_t size) {
    int descriptor = hl_fork_client_descriptor(client);
    int streams[3] = {STDIN_FILENO, STDOUT_FILENO, STDERR_FILENO};
    if (descriptor < 0) return -1;
    return hl_fork_wire_send_descriptors(descriptor, buffer, size, streams, 3);
}

int hl_host_fork_client_receive(hl_host_fork_client *client, void *buffer, size_t size) {
    int descriptor = hl_fork_client_descriptor(client);
    if (descriptor < 0) return -1;
    return hl_fork_wire_receive(descriptor, buffer, size);
}

void hl_host_fork_client_close(hl_host_fork_client *client) {
    int descriptor;
    if (client == NULL || client->private_value == 0) return;
    descriptor = hl_fork_client_descriptor(client);
    client->private_value = 0;
    if (descriptor >= 0) (void)close(descriptor);
}

int hl_fork_wire_send_descriptors(int socket, const void *buffer, size_t size, const int *descriptors,
                                  int descriptor_count) {
    struct iovec vector = {.iov_base = (void *)buffer, .iov_len = size};
    char control[CMSG_SPACE(sizeof(int) * HL_FORK_WIRE_MAX_DESCRIPTORS)] = {0};
    struct msghdr message = {0};
    ssize_t result;
    if (buffer == NULL || size == 0 || descriptor_count < 0 || descriptor_count > HL_FORK_WIRE_MAX_DESCRIPTORS ||
        (descriptor_count > 0 && descriptors == NULL)) {
        errno = EINVAL;
        return -1;
    }
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    if (descriptor_count > 0) {
        struct cmsghdr *header;
        message.msg_control = control;
        message.msg_controllen = (socklen_t)CMSG_SPACE(sizeof(int) * (size_t)descriptor_count);
        header = CMSG_FIRSTHDR(&message);
        header->cmsg_level = SOL_SOCKET;
        header->cmsg_type = SCM_RIGHTS;
        header->cmsg_len = (socklen_t)CMSG_LEN(sizeof(int) * (size_t)descriptor_count);
        memcpy(CMSG_DATA(header), descriptors, sizeof(int) * (size_t)descriptor_count);
    }
    do {
        result = sendmsg(socket, &message, 0);
    } while (result < 0 && errno == EINTR);
    if (result <= 0) return -1;
    if ((size_t)result < size) return hl_fork_wire_send(socket, (const char *)buffer + result, size - (size_t)result);
    return 0;
}

int hl_fork_wire_receive_descriptors(int socket, void *buffer, size_t size, int *descriptors, int *descriptor_count) {
    struct iovec vector = {.iov_base = buffer, .iov_len = size};
    char control[CMSG_SPACE(sizeof(int) * HL_FORK_WIRE_MAX_DESCRIPTORS)];
    struct msghdr message = {0};
    ssize_t result;
    if (buffer == NULL || size == 0 || descriptor_count == NULL || descriptors == NULL) {
        errno = EINVAL;
        return -1;
    }
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    message.msg_control = control;
    message.msg_controllen = sizeof control;
    do {
        result = recvmsg(socket, &message, 0);
    } while (result < 0 && errno == EINTR);
    if (result < 0) return -1;
    *descriptor_count = 0;
    if ((message.msg_flags & MSG_CTRUNC) != 0) {
        /* The kernel may already have installed the rights that fit. Close every complete one visible
           in the truncated buffer before rejecting the message. */
        for (struct cmsghdr *header = CMSG_FIRSTHDR(&message); header != NULL; header = CMSG_NXTHDR(&message, header)) {
            if (header->cmsg_level == SOL_SOCKET && header->cmsg_type == SCM_RIGHTS &&
                header->cmsg_len >= CMSG_LEN(0)) {
                size_t bytes = header->cmsg_len - CMSG_LEN(0);
                int *rights = (int *)CMSG_DATA(header);
                for (size_t index = 0; index < bytes / sizeof(int); index++)
                    (void)close(rights[index]);
            }
        }
        errno = EMSGSIZE;
        return -1;
    }
    for (struct cmsghdr *header = CMSG_FIRSTHDR(&message); header != NULL; header = CMSG_NXTHDR(&message, header)) {
        if (header->cmsg_level == SOL_SOCKET && header->cmsg_type == SCM_RIGHTS) {
            if (header->cmsg_len < CMSG_LEN(0)) goto invalid_rights;
            size_t bytes = header->cmsg_len - CMSG_LEN(0);
            int count = (int)(bytes / sizeof(int));
            int *rights = (int *)CMSG_DATA(header);
            if (bytes % sizeof(int) != 0 || count > HL_FORK_WIRE_MAX_DESCRIPTORS - *descriptor_count) {
                for (int index = 0; index < count; index++)
                    (void)close(rights[index]);
                goto invalid_rights;
            }
            memcpy(descriptors + *descriptor_count, rights, sizeof(int) * (size_t)count);
            *descriptor_count += count;
        }
    }
    return (int)result;

invalid_rights:
    while (*descriptor_count > 0)
        (void)close(descriptors[--*descriptor_count]);
    errno = EPROTO;
    return -1;
}

int hl_fork_wire_send(int socket, const void *buffer, size_t size) {
    const char *cursor = (const char *)buffer;
    if (buffer == NULL && size != 0) {
        errno = EINVAL;
        return -1;
    }
    while (size > 0) {
        ssize_t result = send(socket, cursor, size, 0);
        if (result < 0 && errno == EINTR) continue;
        if (result <= 0) return -1;
        cursor += result;
        size -= (size_t)result;
    }
    return 0;
}

int hl_fork_wire_receive(int socket, void *buffer, size_t size) {
    char *cursor = (char *)buffer;
    if (buffer == NULL && size != 0) {
        errno = EINVAL;
        return -1;
    }
    while (size > 0) {
        ssize_t result = recv(socket, cursor, size, 0);
        if (result < 0 && errno == EINTR) continue;
        if (result <= 0) return -1;
        cursor += result;
        size -= (size_t)result;
    }
    return 0;
}
