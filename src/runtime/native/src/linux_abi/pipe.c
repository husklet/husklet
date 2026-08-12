#include "pipe.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>

typedef struct pipe_object {
    const hl_host_services *host;
    hl_host_handle stream;
    uint32_t writing;
} pipe_object;

static int64_t pipe_error(hl_host_result result) {
    if (result.status == HL_STATUS_OK) return (int64_t)result.value;
    switch ((hl_status)result.status) {
    case HL_STATUS_DISCONNECTED: return -HL_LINUX_EPIPE;
    case HL_STATUS_WOULD_BLOCK: return -HL_LINUX_EAGAIN;
    case HL_STATUS_INTERRUPTED: return -HL_LINUX_EINTR;
    case HL_STATUS_OUT_OF_MEMORY: return -HL_LINUX_ENOMEM;
    case HL_STATUS_RESOURCE_LIMIT: return -HL_LINUX_ENFILE;
    case HL_STATUS_PROCESS_LIMIT: return -HL_LINUX_EMFILE;
    case HL_STATUS_INVALID_ARGUMENT: return -HL_LINUX_EINVAL;
    case HL_STATUS_NOT_SUPPORTED: return -HL_LINUX_ENOSYS;
    default: return -HL_LINUX_EIO;
    }
}

static int pipe_services(const hl_host_services *host) {
    return host != NULL && (host->capabilities & HL_HOST_CAP_STREAM) != 0 && host->stream != NULL;
}

static int64_t pipe_read(void *opaque, void *buffer, size_t size) {
    pipe_object *object = opaque;
    if (object->writing) return -HL_LINUX_EBADF;
    return pipe_error(object->host->stream->read(object->host->context, object->stream, (hl_host_bytes){buffer, size}));
}

static int64_t pipe_write(void *opaque, const void *buffer, size_t size) {
    pipe_object *object = opaque;
    if (!object->writing) return -HL_LINUX_EBADF;
    return pipe_error(
        object->host->stream->write(object->host->context, object->stream, (hl_host_const_bytes){buffer, size}));
}

/* fstat on a pipe reports a zero-length FIFO with exactly one link. The link
 * count is not decoration: code that stats a descriptor to decide whether it is
 * a real file reads st_nlink == 0 as a deleted inode. */
static int64_t pipe_status(void *opaque, hl_linux_file_status *status) {
    (void)opaque;
    memset(status, 0, sizeof(*status));
    status->mode = HL_LINUX_S_IFIFO | 0600u;
    status->link_count = 1;
    return 0;
}

static int64_t pipe_set_status_flags(void *opaque, uint32_t flags) {
    pipe_object *object = opaque;
    uint32_t host_flags = (flags & HL_LINUX_O_NONBLOCK) != 0 ? HL_HOST_STREAM_NONBLOCK : 0;
    return pipe_error(object->host->stream->set_status_flags(object->host->context, object->stream, host_flags));
}

/* POLLERR and POLLHUP are OUTPUT-ONLY conditions: poll(2) and epoll(7) report
 * them whether or not the caller asked, because a descriptor whose peer has gone
 * cannot be un-asked about. Deriving the host interest set from the guest's
 * interests -- and then masking the answer by it again -- hid both. A guest
 * waiting POLLIN on the read end of a pipe whose writers have all closed saw a
 * bare POLLIN and had to infer hangup from a zero-length read; a guest waiting
 * POLLOUT on a broken write end saw nothing at all and spun. So the host is
 * always asked for the full set, and the answer keeps ERROR and HANGUP through
 * the final mask. (hl_linux_object_ready applies the same rule one layer up; the
 * bug was that nothing survived to reach it.) */
static uint32_t pipe_readiness(void *opaque, uint32_t interests) {
    pipe_object *object = opaque;
    uint32_t ready = 0;
    const uint32_t unmaskable = HL_LINUX_READY_ERROR | HL_LINUX_READY_HANGUP;
    hl_host_result result = object->host->stream->readiness(object->host->context, object->stream,
                                                            HL_HOST_READY_READ | HL_HOST_READY_WRITE |
                                                                HL_HOST_READY_ERROR | HL_HOST_READY_HANGUP);
    if (result.status != HL_STATUS_OK) return HL_LINUX_READY_ERROR;
    if ((result.value & HL_HOST_READY_READ) != 0) ready |= HL_LINUX_READY_READ;
    if ((result.value & HL_HOST_READY_WRITE) != 0) ready |= HL_LINUX_READY_WRITE;
    if ((result.value & HL_HOST_READY_ERROR) != 0) ready |= HL_LINUX_READY_ERROR;
    if ((result.value & HL_HOST_READY_HANGUP) != 0) ready |= HL_LINUX_READY_HANGUP;
    return ready & (interests | unmaskable);
}

static hl_host_result pipe_wait_handle(void *opaque) {
    pipe_object *object = opaque;
    return (hl_host_result){.status = HL_STATUS_OK, .value = object->stream};
}

static hl_status pipe_clone(void *opaque, void **child) {
    pipe_object *object = opaque;
    hl_host_result duplicated;
    pipe_object *copy;
    if (child == NULL) return HL_STATUS_INVALID_ARGUMENT;
    duplicated = object->host->stream->duplicate(object->host->context, object->stream);
    if (duplicated.status != HL_STATUS_OK) return (hl_status)duplicated.status;
    copy = malloc(sizeof(*copy));
    if (copy == NULL) {
        (void)object->host->stream->close(object->host->context, duplicated.value);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    *copy = (pipe_object){object->host, duplicated.value, object->writing};
    *child = copy;
    return HL_STATUS_OK;
}

static hl_status pipe_close(void *opaque) {
    pipe_object *object = opaque;
    hl_host_result result = object->host->stream->close(object->host->context, object->stream);
    free(object);
    return (hl_status)result.status;
}

static const hl_linux_object_ops pipe_ops = {
    .fork_while_active_safe = 1,
    .read = pipe_read,
    .write = pipe_write,
    .status = pipe_status,
    .set_status_flags = pipe_set_status_flags,
    .readiness = pipe_readiness,
    .wait_handle = pipe_wait_handle,
    .clone = pipe_clone,
    .close = pipe_close,
};

int64_t hl_linux_pipe_create(hl_linux_abi *linux_abi, uint32_t status_flags, uint32_t descriptor_flags,
                             hl_linux_fd output[2]) {
    hl_host_result pair;
    pipe_object *reader, *writer;
    hl_status status;
    hl_linux_fd installed[2];
    uint32_t host_flags = (status_flags & HL_LINUX_O_NONBLOCK) != 0 ? HL_HOST_STREAM_NONBLOCK : 0;
    if (linux_abi == NULL || output == NULL || (status_flags & ~(uint32_t)HL_LINUX_O_NONBLOCK) != 0 ||
        (descriptor_flags & ~(uint32_t)HL_LINUX_FD_CLOEXEC) != 0)
        return -HL_LINUX_EINVAL;
    if (!pipe_services(linux_abi->host)) return -HL_LINUX_ENOSYS;
    pair = linux_abi->host->stream->pipe_pair(linux_abi->host->context, host_flags);
    if (pair.status != HL_STATUS_OK) return pipe_error(pair);
    reader = malloc(sizeof(*reader));
    writer = malloc(sizeof(*writer));
    if (reader == NULL || writer == NULL) {
        free(reader);
        free(writer);
        (void)linux_abi->host->stream->close(linux_abi->host->context, pair.value);
        (void)linux_abi->host->stream->close(linux_abi->host->context, pair.detail);
        return -HL_LINUX_ENOMEM;
    }
    *reader = (pipe_object){linux_abi->host, pair.value, 0};
    *writer = (pipe_object){linux_abi->host, pair.detail, 1};
    status = hl_linux_object_install(linux_abi, &pipe_ops, reader, HL_LINUX_OBJECT_PIPE,
                                     HL_LINUX_O_RDONLY | status_flags, descriptor_flags, &installed[0]);
    if (status != HL_STATUS_OK) {
        (void)pipe_close(reader);
        (void)pipe_close(writer);
        return status == HL_STATUS_RESOURCE_LIMIT     ? -HL_LINUX_EMFILE
               : status == HL_STATUS_OUT_OF_MEMORY    ? -HL_LINUX_ENOMEM
               : status == HL_STATUS_INVALID_ARGUMENT ? -HL_LINUX_EINVAL
                                                      : -HL_LINUX_EIO;
    }
    status = hl_linux_object_install(linux_abi, &pipe_ops, writer, HL_LINUX_OBJECT_PIPE,
                                     HL_LINUX_O_WRONLY | status_flags, descriptor_flags, &installed[1]);
    if (status != HL_STATUS_OK) {
        (void)hl_linux_fd_close(linux_abi, installed[0], NULL);
        (void)pipe_close(writer);
        return status == HL_STATUS_RESOURCE_LIMIT     ? -HL_LINUX_EMFILE
               : status == HL_STATUS_OUT_OF_MEMORY    ? -HL_LINUX_ENOMEM
               : status == HL_STATUS_INVALID_ARGUMENT ? -HL_LINUX_EINVAL
                                                      : -HL_LINUX_EIO;
    }
    output[0] = installed[0];
    output[1] = installed[1];
    return 0;
}
