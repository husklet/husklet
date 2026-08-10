#include "inotify.h"

#include "object.h"

#include <stdlib.h>
#include <string.h>
#include <pthread.h>

#define INOTIFY_EVENT_MASK UINT32_C(0x00000fff)
#define INOTIFY_OPTION_MASK                                                                                            \
    (HL_LINUX_IN_ONLYDIR | HL_LINUX_IN_DONT_FOLLOW | HL_LINUX_IN_EXCL_UNLINK | HL_LINUX_IN_MASK_CREATE |               \
     HL_LINUX_IN_MASK_ADD | HL_LINUX_IN_ONESHOT)

typedef struct inotify_watch {
    int32_t wd;
    uint32_t mask;
    uint64_t token;
    char *path;
    size_t path_size;
} inotify_watch;

typedef struct inotify_object {
    pthread_mutex_t snapshot_lock;
    uint64_t mutation_epoch;
    const hl_linux_inotify_provider_ops *provider;
    void *provider_context;
    inotify_watch *watches;
    uint32_t watch_count;
    uint32_t watch_capacity;
    unsigned char *queue;
    size_t queue_size;
    size_t queue_capacity;
    int32_t next_wd;
    uint32_t nonblocking;
} inotify_object;

static void mutation_begin(inotify_object *object) {
    pthread_mutex_lock(&object->snapshot_lock);
    object->mutation_epoch++;
}

static void mutation_end(inotify_object *object) {
    object->mutation_epoch++;
    pthread_mutex_unlock(&object->snapshot_lock);
}

static int64_t inotify_error(hl_status status) {
    switch (status) {
    case HL_STATUS_OK: return 0;
    case HL_STATUS_NOT_FOUND: return -HL_LINUX_ENOENT;
    case HL_STATUS_NOT_DIRECTORY: return -HL_LINUX_ENOTDIR;
    case HL_STATUS_NAME_TOO_LONG: return -HL_LINUX_ENAMETOOLONG;
    case HL_STATUS_PERMISSION_DENIED: return -HL_LINUX_EACCES;
    case HL_STATUS_IO: return -HL_LINUX_EIO;
    case HL_STATUS_ALREADY_EXISTS: return -HL_LINUX_EEXIST;
    case HL_STATUS_OUT_OF_MEMORY: return -HL_LINUX_ENOMEM;
    case HL_STATUS_INTERRUPTED: return -HL_LINUX_EINTR;
    case HL_STATUS_WOULD_BLOCK: return -HL_LINUX_EAGAIN;
    case HL_STATUS_NOT_SUPPORTED: return -HL_LINUX_ENOSYS;
    default: return -HL_LINUX_EINVAL;
    }
}

static inotify_watch *watch_token(inotify_object *object, uint64_t token) {
    uint32_t index;
    for (index = 0; index < object->watch_count; ++index)
        if (object->watches[index].token == token) return &object->watches[index];
    return NULL;
}

static inotify_watch *watch_id(inotify_object *object, int32_t wd) {
    uint32_t index;
    for (index = 0; index < object->watch_count; ++index)
        if (object->watches[index].wd == wd) return &object->watches[index];
    return NULL;
}

static hl_status queue_event(inotify_object *object, int32_t wd, uint32_t mask, uint32_t cookie, const char *name,
                             size_t name_size) {
    size_t padded = name_size == 0 ? 0 : (name_size + 1u + 3u) & ~(size_t)3u;
    size_t needed = 16u + padded;
    unsigned char *grown;
    if (needed > SIZE_MAX - object->queue_size) return HL_STATUS_OUT_OF_MEMORY;
    if (object->queue_size + needed > object->queue_capacity) {
        size_t capacity = object->queue_capacity == 0 ? 256u : object->queue_capacity;
        while (capacity < object->queue_size + needed) {
            if (capacity > SIZE_MAX / 2u) return HL_STATUS_OUT_OF_MEMORY;
            capacity *= 2u;
        }
        grown = realloc(object->queue, capacity);
        if (grown == NULL) return HL_STATUS_OUT_OF_MEMORY;
        object->queue = grown;
        object->queue_capacity = capacity;
    }
    memcpy(object->queue + object->queue_size, &wd, 4);
    memcpy(object->queue + object->queue_size + 4, &mask, 4);
    memcpy(object->queue + object->queue_size + 8, &cookie, 4);
    {
        uint32_t length = (uint32_t)padded;
        memcpy(object->queue + object->queue_size + 12, &length, 4);
    }
    if (padded != 0) {
        memcpy(object->queue + object->queue_size + 16, name, name_size);
        memset(object->queue + object->queue_size + 16 + name_size, 0, padded - name_size);
    }
    object->queue_size += needed;
    return HL_STATUS_OK;
}

static hl_status inotify_pump(inotify_object *object) {
    hl_linux_inotify_provider_event events[32];
    uint32_t count = 0;
    uint32_t index;
    hl_status status;
    mutation_begin(object);
    status = object->provider->drain(object->provider_context, events, 32, &count);
    if (status == HL_STATUS_WOULD_BLOCK) {
        mutation_end(object);
        return HL_STATUS_OK;
    }
    if (status != HL_STATUS_OK) {
        mutation_end(object);
        return status;
    }
    for (index = 0; index < count; ++index) {
        inotify_watch *watch = watch_token(object, events[index].token);
        uint32_t mask;
        if (watch == NULL && (events[index].mask & HL_LINUX_IN_Q_OVERFLOW) == 0) continue;
        mask = events[index].mask;
        if (watch != NULL && (mask & (INOTIFY_EVENT_MASK | HL_LINUX_IN_ISDIR)) != 0 &&
            (mask & (watch->mask & INOTIFY_EVENT_MASK)) == 0)
            continue;
        status = queue_event(object, watch == NULL ? -1 : watch->wd, mask, events[index].cookie, events[index].name,
                             events[index].name_size);
        if (status != HL_STATUS_OK) {
            mutation_end(object);
            return status;
        }
        if (watch != NULL && ((watch->mask & HL_LINUX_IN_ONESHOT) != 0 || (mask & HL_LINUX_IN_IGNORED) != 0)) {
            int32_t wd = watch->wd;
            uint64_t token = watch->token;
            uint32_t position = (uint32_t)(watch - object->watches);
            if ((mask & HL_LINUX_IN_IGNORED) == 0) {
                (void)object->provider->remove(object->provider_context, token);
                status = queue_event(object, wd, HL_LINUX_IN_IGNORED, 0, NULL, 0);
                if (status != HL_STATUS_OK) {
                    mutation_end(object);
                    return status;
                }
            }
            free(watch->path);
            object->watches[position] = object->watches[--object->watch_count];
        }
    }
    mutation_end(object);
    return HL_STATUS_OK;
}

static int64_t inotify_read(void *opaque, void *buffer, size_t size) {
    inotify_object *object = opaque;
    size_t offset = 0;
    hl_status status = inotify_pump(object);
    if (status != HL_STATUS_OK) return inotify_error(status);
    while (object->queue_size == 0 && object->nonblocking == 0) {
        status = object->provider->wait(object->provider_context);
        if (status != HL_STATUS_OK) return inotify_error(status);
        status = inotify_pump(object);
        if (status != HL_STATUS_OK) return inotify_error(status);
    }
    if (object->queue_size == 0) return -HL_LINUX_EAGAIN;
    if (size < 16) return -HL_LINUX_EINVAL;
    mutation_begin(object);
    while (offset + 16 <= object->queue_size) {
        uint32_t length;
        size_t record;
        memcpy(&length, object->queue + offset + 12, 4);
        record = 16u + length;
        /* The record must fit BOTH in what remains of the queue and in the
         * guest's buffer. Bounding only by the guest buffer let a malformed
         * queue push `offset` past queue_size, which then read out of bounds
         * and underflowed `queue_size -= offset` to ~SIZE_MAX. */
        if (record > object->queue_size - offset) break;
        if (record > size - offset) break;
        offset += record;
    }
    if (offset == 0) {
        mutation_end(object);
        return -HL_LINUX_EINVAL;
    }
    memcpy(buffer, object->queue, offset);
    object->queue_size -= offset;
    if (object->queue_size != 0) memmove(object->queue, object->queue + offset, object->queue_size);
    mutation_end(object);
    return (int64_t)offset;
}

static uint32_t inotify_ready(void *opaque, uint32_t interests) {
    inotify_object *object = opaque;
    uint32_t ready;
    if ((interests & HL_LINUX_READY_READ) == 0) return 0;
    if (object->queue_size == 0) (void)inotify_pump(object);
    pthread_mutex_lock(&object->snapshot_lock);
    ready = object->queue_size != 0 || object->provider->readiness(object->provider_context) != 0;
    pthread_mutex_unlock(&object->snapshot_lock);
    return ready != 0 ? HL_LINUX_READY_READ : 0;
}

static hl_host_result inotify_wait_handle(void *opaque) {
    inotify_object *object = opaque;
    hl_host_result result;
    pthread_mutex_lock(&object->snapshot_lock);
    result = object->provider->wait_handle(object->provider_context);
    pthread_mutex_unlock(&object->snapshot_lock);
    return result;
}

static hl_status inotify_subscribe(void *opaque, void (*notify)(void *, uint64_t), void *observer, uint64_t token) {
    inotify_object *object = opaque;
    hl_status status;
    if (object->provider->subscribe == NULL) return HL_STATUS_NOT_SUPPORTED;
    pthread_mutex_lock(&object->snapshot_lock);
    status = object->provider->subscribe(object->provider_context, notify, observer, token);
    pthread_mutex_unlock(&object->snapshot_lock);
    return status;
}

static void inotify_unsubscribe(void *opaque, void *observer, uint64_t token) {
    inotify_object *object = opaque;
    if (object->provider->unsubscribe != NULL) {
        pthread_mutex_lock(&object->snapshot_lock);
        object->provider->unsubscribe(object->provider_context, observer, token);
        pthread_mutex_unlock(&object->snapshot_lock);
    }
}

static hl_status inotify_clone(void *opaque, void **out_context) {
    inotify_object *source = opaque;
    inotify_object *copy;
    uint32_t index;
    hl_status status;
    copy = calloc(1, sizeof(*copy));
    if (copy == NULL) return HL_STATUS_OUT_OF_MEMORY;
    if (pthread_mutex_init(&copy->snapshot_lock, NULL) != 0) {
        free(copy);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    pthread_mutex_lock(&source->snapshot_lock);
    copy->provider = source->provider;
    status = source->provider->clone(source->provider_context, &copy->provider_context);
    if (status != HL_STATUS_OK) {
        pthread_mutex_unlock(&source->snapshot_lock);
        pthread_mutex_destroy(&copy->snapshot_lock);
        free(copy);
        return status;
    }
    copy->next_wd = source->next_wd;
    copy->nonblocking = source->nonblocking;
    copy->mutation_epoch = 0;
    if (source->watch_capacity != 0) {
        inotify_watch *watches = calloc(source->watch_capacity, sizeof(*watches));
        if (watches == NULL) goto no_memory_locked;
        copy->watches = watches;
        copy->watch_capacity = source->watch_capacity;
        for (index = 0; index < source->watch_count; ++index) {
            copy->watches[index] = source->watches[index];
            copy->watches[index].path = malloc(source->watches[index].path_size + 1u);
            if (copy->watches[index].path == NULL) goto no_memory_locked;
            memcpy(copy->watches[index].path, source->watches[index].path, source->watches[index].path_size + 1u);
            copy->watch_count++;
        }
    }
    if (source->queue_size != 0) {
        copy->queue = malloc(source->queue_size);
        if (copy->queue == NULL) goto no_memory_locked;
        memcpy(copy->queue, source->queue, source->queue_size);
        copy->queue_size = copy->queue_capacity = source->queue_size;
    }
    pthread_mutex_unlock(&source->snapshot_lock);
    *out_context = copy;
    return HL_STATUS_OK;
no_memory_locked:
    pthread_mutex_unlock(&source->snapshot_lock);
    if (copy->watches != NULL) {
        for (index = 0; index < copy->watch_count && index < copy->watch_capacity; ++index)
            free(copy->watches[index].path);
    }
    free(copy->watches);
    (void)copy->provider->close(copy->provider_context);
    pthread_mutex_destroy(&copy->snapshot_lock);
    free(copy);
    return HL_STATUS_OUT_OF_MEMORY;
}

static hl_status inotify_close(void *opaque) {
    inotify_object *object = opaque;
    uint32_t index;
    hl_status status;
    for (index = 0; index < object->watch_count; ++index)
        free(object->watches[index].path);
    free(object->watches);
    free(object->queue);
    status = object->provider->close(object->provider_context);
    pthread_mutex_destroy(&object->snapshot_lock);
    free(object);
    return status;
}

static const hl_linux_object_ops inotify_ops = {.read = inotify_read,
                                                .fork_while_active_safe = 1,
                                                .readiness = inotify_ready,
                                                .wait_handle = inotify_wait_handle,
                                                .subscribe = inotify_subscribe,
                                                .unsubscribe = inotify_unsubscribe,
                                                .clone = inotify_clone,
                                                .close = inotify_close};

int64_t hl_linux_inotify_create(hl_linux_abi *linux_abi, const hl_linux_inotify_provider_ops *provider,
                                void *provider_context, uint32_t descriptor_flags, uint32_t status_flags) {
    inotify_object *object;
    hl_linux_fd fd;
    hl_status status;
    if (linux_abi == NULL || provider == NULL || provider->add == NULL || provider->modify == NULL ||
        provider->remove == NULL || provider->drain == NULL || provider->wait == NULL ||
        provider->wait_handle == NULL || provider->readiness == NULL || provider->clone == NULL ||
        provider->close == NULL)
        return -HL_LINUX_EINVAL;
    object = calloc(1, sizeof(*object));
    if (object == NULL) return -HL_LINUX_ENOMEM;
    if (pthread_mutex_init(&object->snapshot_lock, NULL) != 0) {
        free(object);
        return -HL_LINUX_ENOMEM;
    }
    object->provider = provider;
    object->provider_context = provider_context;
    object->next_wd = 1;
    object->nonblocking = (status_flags & 00004000u) != 0;
    status = hl_linux_object_install(linux_abi, &inotify_ops, object, HL_LINUX_OBJECT_INOTIFY, status_flags,
                                     descriptor_flags, &fd);
    if (status != HL_STATUS_OK) {
        (void)provider->close(provider_context);
        pthread_mutex_destroy(&object->snapshot_lock);
        free(object);
        return inotify_error(status);
    }
    return (int64_t)fd;
}

int64_t hl_linux_inotify_create_at(hl_linux_abi *linux_abi, hl_linux_fd requested,
                                   const hl_linux_inotify_provider_ops *provider, void *provider_context,
                                   uint32_t descriptor_flags, uint32_t status_flags) {
    inotify_object *object;
    hl_status status;
    if (linux_abi == NULL || provider == NULL || provider->add == NULL || provider->modify == NULL ||
        provider->remove == NULL || provider->drain == NULL || provider->wait == NULL ||
        provider->wait_handle == NULL || provider->readiness == NULL || provider->clone == NULL ||
        provider->close == NULL)
        return -HL_LINUX_EINVAL;
    object = calloc(1, sizeof(*object));
    if (object == NULL) return -HL_LINUX_ENOMEM;
    if (pthread_mutex_init(&object->snapshot_lock, NULL) != 0) {
        free(object);
        return -HL_LINUX_ENOMEM;
    }
    object->provider = provider;
    object->provider_context = provider_context;
    object->next_wd = 1;
    object->nonblocking = (status_flags & 00004000u) != 0;
    status = hl_linux_object_install_at(linux_abi, requested, &inotify_ops, object, HL_LINUX_OBJECT_INOTIFY,
                                        status_flags, descriptor_flags);
    if (status != HL_STATUS_OK) {
        (void)provider->close(provider_context);
        pthread_mutex_destroy(&object->snapshot_lock);
        free(object);
        return inotify_error(status);
    }
    return (int64_t)requested;
}

int64_t hl_linux_inotify_add(hl_linux_abi *linux_abi, hl_linux_fd fd, const char *path, size_t path_size,
                             uint32_t mask) {
    hl_linux_object_pin pin;
    inotify_object *object;
    inotify_watch *watch = NULL;
    uint32_t index;
    uint32_t events = mask & INOTIFY_EVENT_MASK;
    hl_status status;
    if (path == NULL || path_size == 0 || events == 0 || (mask & ~(INOTIFY_EVENT_MASK | INOTIFY_OPTION_MASK)) != 0)
        return -HL_LINUX_EINVAL;
    status = hl_linux_object_pin_fd(linux_abi, fd, &pin);
    if (status != HL_STATUS_OK) return -HL_LINUX_EBADF;
    if (pin.ops != &inotify_ops) {
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    object = pin.context;
    mutation_begin(object);
    for (index = 0; index < object->watch_count; ++index)
        if (object->watches[index].path_size == path_size && !memcmp(object->watches[index].path, path, path_size)) {
            watch = &object->watches[index];
            break;
        }
    if (watch != NULL) {
        uint32_t previous;
        if ((mask & HL_LINUX_IN_MASK_CREATE) != 0) {
            mutation_end(object);
            hl_linux_object_unpin(&pin);
            return -HL_LINUX_EEXIST;
        }
        previous = watch->mask;
        watch->mask = (mask & HL_LINUX_IN_MASK_ADD) != 0 ? watch->mask | (mask & ~UINT32_C(0x20000000))
                                                         : mask & ~UINT32_C(0x10000000);
        status = object->provider->modify(object->provider_context, watch->token, watch->mask);
        if (status != HL_STATUS_OK) watch->mask = previous;
        index = (uint32_t)watch->wd;
    } else {
        char *saved;
        inotify_watch *grown;
        if (object->watch_count == object->watch_capacity) {
            uint32_t capacity = object->watch_capacity == 0 ? 8u : object->watch_capacity * 2u;
            grown = realloc(object->watches, (size_t)capacity * sizeof(*grown));
            if (grown == NULL) {
                mutation_end(object);
                hl_linux_object_unpin(&pin);
                return -HL_LINUX_ENOMEM;
            }
            object->watches = grown;
            object->watch_capacity = capacity;
        }
        saved = malloc(path_size + 1u);
        if (saved == NULL) {
            mutation_end(object);
            hl_linux_object_unpin(&pin);
            return -HL_LINUX_ENOMEM;
        }
        memcpy(saved, path, path_size);
        saved[path_size] = 0;
        if (object->next_wd <= 0 || object->next_wd == INT32_MAX) {
            free(saved);
            mutation_end(object);
            hl_linux_object_unpin(&pin);
            return -HL_LINUX_ENOSYS;
        }
        watch = &object->watches[object->watch_count];
        *watch = (inotify_watch){object->next_wd++, mask & ~UINT32_C(0x10000000),
                                 (uint64_t)(uint32_t)object->next_wd - 1u, saved, path_size};
        status = object->provider->add(object->provider_context, path, path_size, watch->token, watch->mask);
        index = (uint32_t)watch->wd;
        if (status == HL_STATUS_OK)
            object->watch_count++;
        else
            free(saved);
    }
    mutation_end(object);
    hl_linux_object_unpin(&pin);
    return status == HL_STATUS_OK ? (int64_t)index : inotify_error(status);
}

int64_t hl_linux_inotify_remove(hl_linux_abi *linux_abi, hl_linux_fd fd, int32_t wd) {
    hl_linux_object_pin pin;
    inotify_object *object;
    inotify_watch *watch;
    uint32_t index;
    hl_status status = hl_linux_object_pin_fd(linux_abi, fd, &pin);
    if (status != HL_STATUS_OK) return -HL_LINUX_EBADF;
    if (pin.ops != &inotify_ops) {
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    object = pin.context;
    mutation_begin(object);
    watch = watch_id(object, wd);
    if (watch == NULL) {
        mutation_end(object);
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    status = object->provider->remove(object->provider_context, watch->token);
    if (status == HL_STATUS_OK || status == HL_STATUS_NOT_FOUND) {
        (void)queue_event(object, wd, HL_LINUX_IN_IGNORED, 0, NULL, 0);
        index = (uint32_t)(watch - object->watches);
        free(watch->path);
        object->watches[index] = object->watches[--object->watch_count];
        status = HL_STATUS_OK;
    }
    mutation_end(object);
    hl_linux_object_unpin(&pin);
    return inotify_error(status);
}

#define INOTIFY_IMAGE_MAGIC UINT64_C(0x484c494e4f544659)
#define INOTIFY_IMAGE_VERSION UINT32_C(1)

typedef struct inotify_image_header {
    uint64_t magic;
    uint32_t version;
    uint32_t watch_count;
    int32_t next_wd;
    uint32_t nonblocking;
    uint64_t queue_size;
} inotify_image_header;

typedef struct inotify_image_watch {
    int32_t wd;
    uint32_t mask;
    uint64_t token;
    uint64_t path_size;
} inotify_image_watch;

hl_status hl_linux_inotify_export(hl_linux_abi *linux_abi, hl_linux_fd fd, void *buffer, size_t capacity,
                                  size_t *out_size) {
    hl_linux_object_pin pin;
    inotify_object *object;
    inotify_image_header header;
    unsigned char *cursor = buffer;
    size_t needed = sizeof(header);
    uint32_t index;
    hl_status status;
    if (linux_abi == NULL || out_size == NULL) return HL_STATUS_INVALID_ARGUMENT;
    status = hl_linux_object_pin_fd(linux_abi, fd, &pin);
    if (status != HL_STATUS_OK) return status;
    if (pin.ops != &inotify_ops) {
        hl_linux_object_unpin(&pin);
        return HL_STATUS_INVALID_ARGUMENT;
    }
    object = pin.context;
    status = inotify_pump(object);
    if (status != HL_STATUS_OK) {
        hl_linux_object_unpin(&pin);
        return status;
    }
    pthread_mutex_lock(&object->snapshot_lock);
    for (index = 0; index < object->watch_count; ++index) {
        size_t path_size = object->watches[index].path_size;
        if (path_size > SIZE_MAX - sizeof(inotify_image_watch) ||
            needed > SIZE_MAX - sizeof(inotify_image_watch) - path_size) {
            pthread_mutex_unlock(&object->snapshot_lock);
            hl_linux_object_unpin(&pin);
            return HL_STATUS_OUT_OF_MEMORY;
        }
        needed += sizeof(inotify_image_watch) + path_size;
    }
    if (object->queue_size > SIZE_MAX - needed) {
        pthread_mutex_unlock(&object->snapshot_lock);
        hl_linux_object_unpin(&pin);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    needed += object->queue_size;
    *out_size = needed;
    if (buffer == NULL || capacity < needed) {
        pthread_mutex_unlock(&object->snapshot_lock);
        hl_linux_object_unpin(&pin);
        return buffer == NULL ? HL_STATUS_OK : HL_STATUS_OUT_OF_MEMORY;
    }
    header = (inotify_image_header){INOTIFY_IMAGE_MAGIC, INOTIFY_IMAGE_VERSION, object->watch_count,
                                    object->next_wd,     object->nonblocking,   object->queue_size};
    memcpy(cursor, &header, sizeof(header));
    cursor += sizeof(header);
    for (index = 0; index < object->watch_count; ++index) {
        inotify_watch *watch = &object->watches[index];
        inotify_image_watch saved = {watch->wd, watch->mask, watch->token, watch->path_size};
        memcpy(cursor, &saved, sizeof(saved));
        cursor += sizeof(saved);
        memcpy(cursor, watch->path, watch->path_size);
        cursor += watch->path_size;
    }
    memcpy(cursor, object->queue, object->queue_size);
    pthread_mutex_unlock(&object->snapshot_lock);
    hl_linux_object_unpin(&pin);
    return HL_STATUS_OK;
}

int64_t hl_linux_inotify_import_at(hl_linux_abi *linux_abi, hl_linux_fd requested,
                                   const hl_linux_inotify_provider_ops *provider, void *provider_context,
                                   uint32_t descriptor_flags, uint32_t status_flags, const void *buffer, size_t size) {
    const unsigned char *cursor;
    const unsigned char *end;
    inotify_image_header header;
    inotify_object *object = NULL;
    uint32_t index;
    hl_status status = HL_STATUS_INVALID_ARGUMENT;
    if (linux_abi == NULL || provider == NULL || buffer == NULL || size < sizeof(header) || provider->add == NULL ||
        provider->modify == NULL || provider->remove == NULL || provider->drain == NULL || provider->wait == NULL ||
        provider->wait_handle == NULL || provider->readiness == NULL || provider->clone == NULL ||
        provider->close == NULL)
        goto fail_provider;
    cursor = buffer;
    end = cursor + size;
    memcpy(&header, cursor, sizeof(header));
    cursor += sizeof(header);
    if (header.magic != INOTIFY_IMAGE_MAGIC || header.version != INOTIFY_IMAGE_VERSION || header.next_wd <= 0 ||
        header.queue_size > (uint64_t)(end - cursor) || header.watch_count > UINT32_C(1048576))
        goto fail_provider;
    object = calloc(1, sizeof(*object));
    if (object == NULL) {
        status = HL_STATUS_OUT_OF_MEMORY;
        goto fail_provider;
    }
    if (pthread_mutex_init(&object->snapshot_lock, NULL) != 0) {
        status = HL_STATUS_OUT_OF_MEMORY;
        free(object);
        object = NULL;
        goto fail_provider;
    }
    object->provider = provider;
    object->provider_context = provider_context;
    object->next_wd = header.next_wd;
    object->nonblocking = (status_flags & HL_LINUX_O_NONBLOCK) != 0;
    if (header.watch_count != 0) {
        inotify_watch *watches = calloc(header.watch_count, sizeof(*watches));
        if (watches == NULL) {
            status = HL_STATUS_OUT_OF_MEMORY;
            goto fail_object;
        }
        object->watches = watches;
        object->watch_capacity = header.watch_count;
    }
    for (index = 0; index < header.watch_count; ++index) {
        inotify_image_watch saved;
        inotify_watch watch = {0};
        if ((size_t)(end - cursor) < sizeof(saved)) goto fail_object;
        memcpy(&saved, cursor, sizeof(saved));
        cursor += sizeof(saved);
        if (saved.wd <= 0 || saved.path_size == 0 || saved.path_size > (uint64_t)(end - cursor) ||
            saved.path_size > SIZE_MAX - 1u)
            goto fail_object;
        watch.path = malloc((size_t)saved.path_size + 1u);
        if (watch.path == NULL) {
            status = HL_STATUS_OUT_OF_MEMORY;
            goto fail_object;
        }
        memcpy(watch.path, cursor, (size_t)saved.path_size);
        watch.path[saved.path_size] = 0;
        cursor += saved.path_size;
        watch.wd = saved.wd;
        watch.mask = saved.mask;
        watch.token = saved.token;
        watch.path_size = (size_t)saved.path_size;
        status = provider->add(provider_context, watch.path, watch.path_size, watch.token, watch.mask);
        if (status != HL_STATUS_OK) {
            free(watch.path);
            goto fail_object;
        }
        object->watches[object->watch_count] = watch;
        object->watch_count++;
    }
    if (header.queue_size != (uint64_t)(end - cursor)) goto fail_object;
    if (header.queue_size != 0) {
        object->queue = malloc((size_t)header.queue_size);
        if (object->queue == NULL) {
            status = HL_STATUS_OUT_OF_MEMORY;
            goto fail_object;
        }
        memcpy(object->queue, cursor, (size_t)header.queue_size);
        object->queue_size = object->queue_capacity = (size_t)header.queue_size;
        /* Validate the RECORD FRAMING, not just the total size. The queue is a
         * run of 16-byte headers (wd, mask, cookie, len) each followed by `len`
         * name bytes, and inotify_read() walks it trusting `len`. A restored
         * image is untrusted input -- the image digest is an unkeyed FNV-1a, so
         * it detects bit-rot but is trivially recomputed after a deliberate
         * edit -- so a record claiming a `len` past the end of the queue would
         * let that walk run off the allocation. Require every record to fit and
         * the records to tile the queue EXACTLY; reject the image otherwise. */
        {
            size_t walk = 0;
            while (walk != object->queue_size) {
                uint32_t name_len;
                if (object->queue_size - walk < 16u) goto fail_object;
                memcpy(&name_len, object->queue + walk + 12, 4);
                if ((size_t)name_len > object->queue_size - walk - 16u) goto fail_object;
                walk += 16u + (size_t)name_len;
            }
        }
    }
    status = hl_linux_object_install_at(linux_abi, requested, &inotify_ops, object, HL_LINUX_OBJECT_INOTIFY,
                                        status_flags, descriptor_flags);
    if (status != HL_STATUS_OK) goto fail_object;
    return (int64_t)requested;
fail_object:
    if (object != NULL) {
        if (object->watches != NULL) {
            for (index = 0; index < object->watch_count && index < object->watch_capacity; ++index) {
                (void)provider->remove(provider_context, object->watches[index].token);
                free(object->watches[index].path);
            }
        }
        free(object->watches);
        free(object->queue);
        pthread_mutex_destroy(&object->snapshot_lock);
        free(object);
    }
fail_provider:
    if (provider != NULL && provider->close != NULL && provider_context != NULL)
        (void)provider->close(provider_context);
    return inotify_error(status);
}
