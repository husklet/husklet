#include "eventfd.h"

#include <stdlib.h>
#include <string.h>

#define HL_EVENTFD_SUBSCRIPTIONS 64u

typedef struct eventfd_subscription {
    void *observer;
    uint64_t token;
    hl_host_handle handle;
} eventfd_subscription;

typedef struct eventfd_object {
    const hl_host_services *host;
    hl_host_handle counter;
    hl_host_handle lock;
    eventfd_subscription subscriptions[HL_EVENTFD_SUBSCRIPTIONS];
} eventfd_object;

static int64_t eventfd_error(hl_status status) {
    switch (status) {
    case HL_STATUS_OK: return 0;
    case HL_STATUS_WOULD_BLOCK: return -HL_LINUX_EAGAIN;
    case HL_STATUS_PERMISSION_DENIED: return -HL_LINUX_EACCES;
    case HL_STATUS_OUT_OF_MEMORY: return -HL_LINUX_ENOMEM;
    case HL_STATUS_RESOURCE_LIMIT: return -HL_LINUX_ENFILE;
    case HL_STATUS_PROCESS_LIMIT: return -HL_LINUX_EMFILE;
    case HL_STATUS_NOT_SUPPORTED: return -HL_LINUX_ENOSYS;
    case HL_STATUS_INVALID_ARGUMENT: return -HL_LINUX_EINVAL;
    default: return -HL_LINUX_EIO;
    }
}

static int eventfd_services(const hl_host_services *host) {
    return host != NULL &&
           (host->capabilities & (HL_HOST_CAP_COUNTER | HL_HOST_CAP_SYNC)) ==
               (HL_HOST_CAP_COUNTER | HL_HOST_CAP_SYNC) &&
           host->counter != NULL && host->sync != NULL;
}

/* The kernel's two size rules are deliberately ASYMMETRIC (fs/eventfd.c):
 *
 *     eventfd_read  rejects count <  8, and transfers exactly one 8-byte
 *                   counter into a larger buffer, returning 8. Bytes past the
 *                   counter are left untouched.
 *     eventfd_write rejects count != 8. A 9- or 16-byte write is EINVAL and
 *                   must not reach the counter.
 *
 * Reading is the side that differs, because the kernel copies a fixed-size
 * object out rather than filling the buffer. Requiring an exact 8 on read turns
 * the ordinary `read(efd, &u64_in_a_wider_struct, sizeof struct)` idiom into
 * EINVAL, which is a failure no caller anticipates -- the counter is never
 * consumed and the descriptor looks permanently broken. */
static int64_t eventfd_read(void *opaque, void *buffer, size_t size) {
    eventfd_object *object = opaque;
    hl_host_result result;
    if (size < sizeof(uint64_t)) return -HL_LINUX_EINVAL;
    result = object->host->counter->read(object->host->context, object->counter);
    if (result.status != HL_STATUS_OK) return eventfd_error((hl_status)result.status);
    memcpy(buffer, &result.value, sizeof(result.value));
    return (int64_t)sizeof(result.value);
}

static int64_t eventfd_write(void *opaque, const void *buffer, size_t size) {
    eventfd_object *object = opaque;
    hl_host_result result;
    uint64_t value;
    if (size != sizeof(value)) return -HL_LINUX_EINVAL;
    memcpy(&value, buffer, sizeof(value));
    if (value == UINT64_MAX) return -HL_LINUX_EINVAL;
    result = object->host->counter->write(object->host->context, object->counter, value);
    return result.status == HL_STATUS_OK ? (int64_t)sizeof(value) : eventfd_error((hl_status)result.status);
}

/* An eventfd is backed by an anonymous inode, and the kernel builds every one of
 * those with S_IFREG | S_IRUSR | S_IWUSR and a link count of one. So fstat on an
 * eventfd reports a zero-length REGULAR file -- not a FIFO, which is what a
 * pipe-backed emulation reports and what code testing S_ISFIFO would wrongly
 * conclude. Reporting no type bits at all is worse still: S_ISREG, S_ISFIFO and
 * S_ISCHR are then all false and the descriptor matches no file type. */
static int64_t eventfd_status(void *opaque, hl_linux_file_status *status) {
    (void)opaque;
    memset(status, 0, sizeof(*status));
    status->mode = HL_LINUX_S_IFREG | 0600u;
    status->link_count = 1;
    return 0;
}

static int64_t eventfd_set_status_flags(void *opaque, uint32_t flags) {
    eventfd_object *object = opaque;
    hl_host_result current = object->host->counter->get_flags(object->host->context, object->counter);
    uint32_t host_flags;
    hl_host_result result;
    if (current.status != HL_STATUS_OK) return eventfd_error((hl_status)current.status);
    host_flags = (uint32_t)current.value & HL_HOST_COUNTER_SEMAPHORE;
    if ((flags & HL_LINUX_O_NONBLOCK) != 0) host_flags |= HL_HOST_COUNTER_NONBLOCK;
    result = object->host->counter->set_flags(object->host->context, object->counter, host_flags);
    return eventfd_error((hl_status)result.status);
}

static uint32_t eventfd_readiness(void *opaque, uint32_t interests) {
    eventfd_object *object = opaque;
    hl_host_result result = object->host->counter->readiness(object->host->context, object->counter,
                                                             interests & HL_LINUX_READY_READ ? HL_HOST_READY_READ : 0);
    uint32_t readiness = HL_LINUX_READY_WRITE;
    if (result.status != HL_STATUS_OK) return HL_LINUX_READY_ERROR;
    if ((result.value & HL_HOST_READY_READ) != 0) readiness |= HL_LINUX_READY_READ;
    return readiness & interests;
}

static hl_status eventfd_subscribe(void *opaque, void (*notify)(void *, uint64_t), void *observer, uint64_t token) {
    eventfd_object *object = opaque;
    uint32_t index;
    hl_host_result result;
    (void)object->host->sync->mutex_lock(object->host->context, object->lock);
    for (index = 0; index < HL_EVENTFD_SUBSCRIPTIONS && object->subscriptions[index].handle != 0; ++index) {}
    if (index == HL_EVENTFD_SUBSCRIPTIONS) {
        (void)object->host->sync->mutex_unlock(object->host->context, object->lock);
        return HL_STATUS_RESOURCE_LIMIT;
    }
    result = object->host->counter->subscribe(object->host->context, object->counter, notify, observer, token);
    if (result.status == HL_STATUS_OK)
        object->subscriptions[index] = (eventfd_subscription){observer, token, result.value};
    (void)object->host->sync->mutex_unlock(object->host->context, object->lock);
    return (hl_status)result.status;
}

static void eventfd_unsubscribe(void *opaque, void *observer, uint64_t token) {
    eventfd_object *object = opaque;
    uint32_t index;
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    (void)object->host->sync->mutex_lock(object->host->context, object->lock);
    for (index = 0; index < HL_EVENTFD_SUBSCRIPTIONS; ++index)
        if (object->subscriptions[index].handle != 0 && object->subscriptions[index].observer == observer &&
            object->subscriptions[index].token == token) {
            handle = object->subscriptions[index].handle;
            object->subscriptions[index] = (eventfd_subscription){0};
            break;
        }
    (void)object->host->sync->mutex_unlock(object->host->context, object->lock);
    if (handle != HL_HOST_HANDLE_INVALID) (void)object->host->counter->unsubscribe(object->host->context, handle);
}

static hl_status eventfd_clone(void *opaque, void **child) {
    eventfd_object *object = opaque;
    eventfd_object *copy;
    hl_host_result counter = object->host->counter->duplicate(object->host->context, object->counter);
    hl_host_result lock;
    if (counter.status != HL_STATUS_OK) return (hl_status)counter.status;
    lock = object->host->sync->mutex_create(object->host->context);
    if (lock.status != HL_STATUS_OK) {
        (void)object->host->counter->close(object->host->context, counter.value);
        return (hl_status)lock.status;
    }
    copy = calloc(1, sizeof(*copy));
    if (copy == NULL) {
        (void)object->host->sync->mutex_close(object->host->context, lock.value);
        (void)object->host->counter->close(object->host->context, counter.value);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    copy->host = object->host;
    copy->counter = counter.value;
    copy->lock = lock.value;
    *child = copy;
    return HL_STATUS_OK;
}

static hl_status eventfd_close(void *opaque) {
    eventfd_object *object = opaque;
    uint32_t index;
    hl_status status = HL_STATUS_OK;
    for (index = 0; index < HL_EVENTFD_SUBSCRIPTIONS; ++index)
        if (object->subscriptions[index].handle != 0) {
            hl_host_result result =
                object->host->counter->unsubscribe(object->host->context, object->subscriptions[index].handle);
            if (status == HL_STATUS_OK && result.status != HL_STATUS_OK) status = (hl_status)result.status;
        }
    {
        hl_host_result result = object->host->counter->close(object->host->context, object->counter);
        if (status == HL_STATUS_OK && result.status != HL_STATUS_OK) status = (hl_status)result.status;
    }
    (void)object->host->sync->mutex_close(object->host->context, object->lock);
    free(object);
    return status;
}

static const hl_linux_object_ops eventfd_ops = {
    .fork_while_active_safe = 1,
    .read = eventfd_read,
    .write = eventfd_write,
    .status = eventfd_status,
    .set_status_flags = eventfd_set_status_flags,
    .readiness = eventfd_readiness,
    .subscribe = eventfd_subscribe,
    .unsubscribe = eventfd_unsubscribe,
    .clone = eventfd_clone,
    .close = eventfd_close,
};

static int64_t eventfd_install(hl_linux_abi *linux_abi, hl_linux_fd requested, hl_host_handle counter, uint32_t flags,
                               uint32_t descriptor_flags) {
    eventfd_object *object;
    hl_host_result lock;
    hl_status status;
    if (!eventfd_services(linux_abi == NULL ? NULL : linux_abi->host)) return -HL_LINUX_ENOSYS;
    lock = linux_abi->host->sync->mutex_create(linux_abi->host->context);
    if (lock.status != HL_STATUS_OK) return eventfd_error((hl_status)lock.status);
    object = calloc(1, sizeof(*object));
    if (object == NULL) {
        (void)linux_abi->host->sync->mutex_close(linux_abi->host->context, lock.value);
        return -HL_LINUX_ENOMEM;
    }
    object->host = linux_abi->host;
    object->counter = counter;
    object->lock = lock.value;
    if (requested == UINT32_MAX) {
        hl_linux_fd fd;
        status = hl_linux_object_install(
            linux_abi, &eventfd_ops, object, HL_LINUX_OBJECT_EVENTFD,
            HL_LINUX_O_RDWR | ((flags & HL_LINUX_EVENTFD_NONBLOCK) ? HL_LINUX_O_NONBLOCK : 0), descriptor_flags, &fd);
        if (status == HL_STATUS_OK) return (int64_t)fd;
    } else {
        status = hl_linux_object_install_at(
            linux_abi, requested, &eventfd_ops, object, HL_LINUX_OBJECT_EVENTFD,
            HL_LINUX_O_RDWR | ((flags & HL_LINUX_EVENTFD_NONBLOCK) ? HL_LINUX_O_NONBLOCK : 0), descriptor_flags);
        if (status == HL_STATUS_OK) return (int64_t)requested;
    }
    (void)eventfd_close(object);
    return eventfd_error(status);
}

static int64_t eventfd_create_common(hl_linux_abi *linux_abi, hl_linux_fd requested, uint64_t initial, uint32_t flags,
                                     uint32_t descriptor_flags) {
    uint32_t host_flags = 0;
    hl_host_result counter;
    if ((flags & ~(uint32_t)(HL_LINUX_EVENTFD_SEMAPHORE | HL_LINUX_EVENTFD_NONBLOCK)) != 0 || initial == UINT64_MAX)
        return -HL_LINUX_EINVAL;
    if (!eventfd_services(linux_abi == NULL ? NULL : linux_abi->host)) return -HL_LINUX_ENOSYS;
    if (flags & HL_LINUX_EVENTFD_SEMAPHORE) host_flags |= HL_HOST_COUNTER_SEMAPHORE;
    if (flags & HL_LINUX_EVENTFD_NONBLOCK) host_flags |= HL_HOST_COUNTER_NONBLOCK;
    counter = linux_abi->host->counter->create(linux_abi->host->context, initial, host_flags);
    if (counter.status != HL_STATUS_OK) return eventfd_error((hl_status)counter.status);
    return eventfd_install(linux_abi, requested, counter.value, flags, descriptor_flags);
}

int64_t hl_linux_eventfd_create(hl_linux_abi *linux_abi, uint64_t initial, uint32_t flags, uint32_t descriptor_flags) {
    return eventfd_create_common(linux_abi, UINT32_MAX, initial, flags, descriptor_flags);
}

int64_t hl_linux_eventfd_create_at(hl_linux_abi *linux_abi, hl_linux_fd requested, uint64_t initial, uint32_t flags,
                                   uint32_t descriptor_flags) {
    return eventfd_create_common(linux_abi, requested, initial, flags, descriptor_flags);
}

hl_status hl_linux_eventfd_wait_handle(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_host_handle *handle) {
    hl_linux_object_pin pin;
    hl_status status;
    if (handle == NULL) return HL_STATUS_INVALID_ARGUMENT;
    status = hl_linux_object_pin_fd(linux_abi, fd, &pin);
    if (status != HL_STATUS_OK) return status;
    if (pin.ops != &eventfd_ops)
        status = HL_STATUS_INVALID_ARGUMENT;
    else
        *handle = ((eventfd_object *)pin.context)->counter;
    hl_linux_object_unpin(&pin);
    return status;
}

int64_t hl_linux_eventfd_send(hl_linux_abi *linux_abi, hl_host_handle endpoint, hl_linux_fd fd,
                              hl_host_const_bytes payload, uint32_t rights) {
    hl_linux_object_pin pin;
    hl_host_transfer_attachment attachment;
    hl_host_result result;
    hl_status status = hl_linux_object_pin_fd(linux_abi, fd, &pin);
    if (status != HL_STATUS_OK) return eventfd_error(status);
    if (pin.ops != &eventfd_ops) {
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    attachment =
        (hl_host_transfer_attachment){((eventfd_object *)pin.context)->counter, HL_HOST_TRANSFER_KIND_COUNTER, rights};
    result = linux_abi->host->transfer->send(linux_abi->host->context, endpoint, payload, &attachment, 1);
    hl_linux_object_unpin(&pin);
    return result.status == HL_STATUS_OK ? (int64_t)result.value : eventfd_error((hl_status)result.status);
}

int64_t hl_linux_eventfd_receive(hl_linux_abi *linux_abi, hl_host_handle endpoint, hl_host_bytes payload,
                                 uint32_t descriptor_flags, hl_linux_fd *fd) {
    hl_host_transfer_attachment attachment;
    hl_host_result result;
    int64_t installed;
    if (fd == NULL) return -HL_LINUX_EINVAL;
    result = linux_abi->host->transfer->receive(linux_abi->host->context, endpoint, payload, &attachment, 1);
    if (result.status != HL_STATUS_OK) return eventfd_error((hl_status)result.status);
    if (attachment.kind != HL_HOST_TRANSFER_KIND_COUNTER) {
        (void)linux_abi->host->counter->close(linux_abi->host->context, attachment.object);
        return -HL_LINUX_EINVAL;
    }
    {
        hl_host_result flags = linux_abi->host->counter->get_flags(linux_abi->host->context, attachment.object);
        uint32_t object_flags = 0;
        if (flags.status != HL_STATUS_OK) {
            (void)linux_abi->host->counter->close(linux_abi->host->context, attachment.object);
            return eventfd_error((hl_status)flags.status);
        }
        if (flags.value & HL_HOST_COUNTER_SEMAPHORE) object_flags |= HL_LINUX_EVENTFD_SEMAPHORE;
        if (flags.value & HL_HOST_COUNTER_NONBLOCK) object_flags |= HL_LINUX_EVENTFD_NONBLOCK;
        installed = eventfd_install(linux_abi, UINT32_MAX, attachment.object, object_flags, descriptor_flags);
    }
    if (installed < 0) return installed;
    *fd = (hl_linux_fd)installed;
    return (int64_t)result.value;
}
