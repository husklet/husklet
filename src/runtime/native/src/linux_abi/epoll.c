#include "epoll.h"
#include "../core/provider/files.h"

#include <stdlib.h>
#include <string.h>
#include <pthread.h>

enum { HL_LINUX_OBJECT_EPOLL = 0x65706f6cu };

static int64_t epoll_error(hl_status status) {
    switch (status) {
    case HL_STATUS_NOT_FOUND: return -HL_LINUX_EBADF;
    case HL_STATUS_OUT_OF_MEMORY: return -HL_LINUX_ENOMEM;
    case HL_STATUS_ALREADY_EXISTS: return -HL_LINUX_EEXIST;
    case HL_STATUS_INTERRUPTED: return -HL_LINUX_EINTR;
    case HL_STATUS_NOT_SUPPORTED: return -HL_LINUX_ENOSYS;
    case HL_STATUS_WOULD_BLOCK: return -HL_LINUX_EAGAIN;
    case HL_STATUS_OK: return 0;
    default: return -HL_LINUX_EINVAL;
    }
}

typedef struct hl_linux_epoll_watch {
    hl_linux_fd fd;
    hl_linux_ofd ofd;
    uint32_t descriptor_generation;
    uint32_t ofd_generation;
    uint32_t interests;
    uint32_t previous;
    uint32_t disabled;
    uint32_t subscribed;
    hl_host_handle wait_handle;
    hl_host_handle provider_handle;
    uint64_t data;
    uint64_t token;
} hl_linux_epoll_watch;

enum { EPOLL_UNSUBSCRIBED = 0, EPOLL_CALLBACK = 1, EPOLL_HOST = 2, EPOLL_STALE = 3 };

typedef struct hl_linux_epoll {
    pthread_mutex_t lock;
    hl_linux_abi *linux_abi;
    hl_host_handle wake;
    hl_linux_epoll_watch *watches;
    uint32_t count;
    uint32_t capacity;
    uint64_t next_token;
} hl_linux_epoll;

static void epoll_notify(void *opaque, uint64_t token) {
    hl_linux_epoll *epoll = opaque;
    (void)token;
    (void)epoll->linux_abi->host->event->wake(epoll->linux_abi->host->context, epoll->wake);
}

static void epoll_unsubscribe(hl_linux_epoll *epoll, hl_linux_epoll_watch *watch) {
    hl_linux_object_pin target;
    if (watch->subscribed == EPOLL_HOST) {
        (void)epoll->linux_abi->host->event->control(epoll->linux_abi->host->context, epoll->wake, HL_HOST_EVENT_DELETE,
                                                     watch->wait_handle, watch->token, HL_HOST_READY_READ);
        watch->subscribed = EPOLL_STALE;
        return;
    }
    if (watch->subscribed != EPOLL_CALLBACK) return;
    if (hl_provider_files_is_handle(watch->provider_handle)) {
        hl_provider_files_unsubscribe(watch->provider_handle, epoll, watch->token);
        watch->subscribed = EPOLL_STALE;
        return;
    }
    if (hl_linux_object_pin_ofd(epoll->linux_abi, watch->ofd, watch->ofd_generation, &target) == HL_STATUS_OK) {
        if (target.ops->unsubscribe != NULL) target.ops->unsubscribe(target.context, epoll, watch->token);
        hl_linux_object_unpin(&target);
    }
    watch->subscribed = EPOLL_STALE;
}

static hl_status epoll_close(void *opaque) {
    hl_linux_epoll *epoll = opaque;
    uint32_t index;
    for (index = 0; index < epoll->count; ++index)
        epoll_unsubscribe(epoll, &epoll->watches[index]);
    hl_host_result result = epoll->linux_abi->host->event->close(epoll->linux_abi->host->context, epoll->wake);
    free(epoll->watches);
    pthread_mutex_destroy(&epoll->lock);
    free(epoll);
    return (hl_status)result.status;
}

static void epoll_retire(void *opaque) {
    hl_linux_epoll *epoll = opaque;
    (void)epoll->linux_abi->host->event->wake(epoll->linux_abi->host->context, epoll->wake);
}

static hl_status epoll_clone(void *opaque, void **out_context) {
    hl_linux_epoll *source = opaque;
    hl_linux_epoll *copy;
    hl_host_result created;
    copy = calloc(1, sizeof(*copy));
    if (copy == NULL) return HL_STATUS_OUT_OF_MEMORY;
    if (pthread_mutex_init(&copy->lock, NULL) != 0) {
        free(copy);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    created = source->linux_abi->host->event->create(source->linux_abi->host->context);
    if (created.status != HL_STATUS_OK) {
        pthread_mutex_destroy(&copy->lock);
        free(copy);
        return (hl_status)created.status;
    }
    pthread_mutex_lock(&source->lock);
    copy->linux_abi = source->linux_abi;
    copy->count = source->count;
    copy->capacity = source->capacity;
    copy->next_token = source->next_token;
    copy->wake = created.value;
    copy->watches = NULL;
    if (source->capacity != 0) {
        copy->watches = malloc((size_t)source->capacity * sizeof(*copy->watches));
        if (copy->watches == NULL) {
            pthread_mutex_unlock(&source->lock);
            (void)source->linux_abi->host->event->close(source->linux_abi->host->context, copy->wake);
            pthread_mutex_destroy(&copy->lock);
            free(copy);
            return HL_STATUS_OUT_OF_MEMORY;
        }
        memcpy(copy->watches, source->watches, (size_t)source->count * sizeof(*copy->watches));
        for (uint32_t index = 0; index < copy->count; ++index)
            if (copy->watches[index].subscribed == EPOLL_CALLBACK || copy->watches[index].subscribed == EPOLL_HOST)
                copy->watches[index].subscribed = EPOLL_UNSUBSCRIBED;
    }
    pthread_mutex_unlock(&source->lock);
    *out_context = copy;
    return HL_STATUS_OK;
}

static uint32_t epoll_ready(void *opaque, uint32_t interests) {
    hl_linux_epoll *epoll = opaque;
    uint32_t index;
    if ((interests & HL_LINUX_READY_READ) == 0) return 0;
    for (index = 0;; ++index) {
        hl_linux_object_pin target;
        hl_linux_epoll_watch snapshot;
        hl_linux_epoll_watch *watch = &snapshot;
        pthread_mutex_lock(&epoll->lock);
        if (index >= epoll->count) {
            pthread_mutex_unlock(&epoll->lock);
            break;
        }
        snapshot = epoll->watches[index];
        pthread_mutex_unlock(&epoll->lock);
        if (watch->disabled != 0) continue;
        if (hl_linux_object_pin_ofd(epoll->linux_abi, watch->ofd, watch->ofd_generation, &target) == HL_STATUS_OK) {
            uint32_t ready = hl_linux_object_ready(&target, watch->interests);
            hl_linux_object_unpin(&target);
            if (ready != 0) return HL_LINUX_READY_READ;
        }
    }
    return 0;
}

static const hl_linux_object_ops epoll_ops = {
    .fork_while_active_safe = 1,
    .readiness = epoll_ready,
    .retire = epoll_retire,
    .clone = epoll_clone,
    .close = epoll_close,
};

static hl_status epoll_subscribe(hl_linux_epoll *epoll, hl_linux_epoll_watch *watch) {
    hl_linux_object_pin target;
    hl_status status = hl_linux_object_pin_ofd(epoll->linux_abi, watch->ofd, watch->ofd_generation, &target);
    if ((status == HL_STATUS_NOT_SUPPORTED || status == HL_STATUS_NOT_FOUND) &&
        hl_provider_files_is_handle(watch->provider_handle)) {
        if (hl_provider_files_subscribe(watch->provider_handle, watch->interests, epoll_notify, epoll, watch->token) ==
            0) {
            watch->subscribed = EPOLL_CALLBACK;
            return HL_STATUS_OK;
        }
        return HL_STATUS_IO;
    }
    if (status != HL_STATUS_OK) return status;
    if (target.ops->wait_handle != NULL) {
        hl_host_result handle = target.ops->wait_handle(target.context);
        status = (hl_status)handle.status;
        if (status == HL_STATUS_OK) {
            hl_host_result registered =
                epoll->linux_abi->host->event->control(epoll->linux_abi->host->context, epoll->wake, HL_HOST_EVENT_ADD,
                                                       handle.value, watch->token, HL_HOST_READY_READ);
            status = (hl_status)registered.status;
            if (status == HL_STATUS_OK) {
                watch->wait_handle = handle.value;
                watch->subscribed = EPOLL_HOST;
            }
        }
        if (status == HL_STATUS_NOT_SUPPORTED && target.ops->subscribe != NULL && target.ops->unsubscribe != NULL) {
            status = target.ops->subscribe(target.context, epoll_notify, epoll, watch->token);
            if (status == HL_STATUS_OK) watch->subscribed = EPOLL_CALLBACK;
        }
    } else if (target.ops->subscribe == NULL || target.ops->unsubscribe == NULL)
        status = HL_STATUS_NOT_SUPPORTED;
    else {
        status = target.ops->subscribe(target.context, epoll_notify, epoll, watch->token);
        if (status == HL_STATUS_OK) watch->subscribed = EPOLL_CALLBACK;
    }
    hl_linux_object_unpin(&target);
    return status;
}

static int epoll_services(const hl_linux_abi *linux_abi) {
    const hl_host_event_services *event;
    if (linux_abi == NULL || linux_abi->host == NULL || (linux_abi->host->capabilities & HL_HOST_CAP_EVENT) == 0)
        return 0;
    event = linux_abi->host->event;
    return event != NULL && event->create != NULL && event->wait != NULL && event->wake != NULL && event->close != NULL;
}

int64_t hl_linux_epoll_create(hl_linux_abi *linux_abi, uint32_t descriptor_flags) {
    hl_linux_epoll *epoll;
    hl_linux_fd fd;
    hl_host_result created;
    hl_status status;
    if (!epoll_services(linux_abi)) return -HL_LINUX_ENOSYS;
    epoll = calloc(1, sizeof(*epoll));
    if (epoll == NULL) return -HL_LINUX_ENOMEM;
    if (pthread_mutex_init(&epoll->lock, NULL) != 0) {
        free(epoll);
        return -HL_LINUX_ENOMEM;
    }
    created = linux_abi->host->event->create(linux_abi->host->context);
    if (created.status != HL_STATUS_OK) {
        pthread_mutex_destroy(&epoll->lock);
        free(epoll);
        return epoll_error((hl_status)created.status);
    }
    epoll->linux_abi = linux_abi;
    epoll->wake = created.value;
    status = hl_linux_object_install(linux_abi, &epoll_ops, epoll, HL_LINUX_OBJECT_EPOLL, 0, descriptor_flags, &fd);
    if (status != HL_STATUS_OK) {
        (void)linux_abi->host->event->close(linux_abi->host->context, epoll->wake);
        pthread_mutex_destroy(&epoll->lock);
        free(epoll);
        return epoll_error(status);
    }
    return (int64_t)fd;
}

static int watch_index(const hl_linux_epoll *epoll, const hl_linux_fd_snapshot *target) {
    uint32_t index;
    for (index = 0; index < epoll->count; ++index)
        if (epoll->watches[index].fd == target->fd &&
            epoll->watches[index].descriptor_generation == target->descriptor_generation &&
            epoll->watches[index].ofd == target->ofd && epoll->watches[index].ofd_generation == target->ofd_generation)
            return (int)index;
    return -1;
}

int64_t hl_linux_epoll_control(hl_linux_abi *linux_abi, hl_linux_fd epoll_fd, uint32_t operation, hl_linux_fd target_fd,
                               uint32_t interests, uint64_t data) {
    hl_linux_object_pin pin;
    hl_linux_fd_snapshot target;
    hl_linux_epoll *epoll;
    hl_status status;
    int index;
    status = hl_linux_object_pin_fd(linux_abi, epoll_fd, &pin);
    if (status != HL_STATUS_OK) return epoll_error(status);
    if (pin.ops != &epoll_ops) {
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    epoll = pin.context;
    status = hl_linux_fd_snapshot_get(linux_abi, target_fd, &target);
    if (status != HL_STATUS_OK) {
        hl_linux_object_unpin(&pin);
        return epoll_error(status);
    }
    if (target.kind == HL_LINUX_OBJECT_EPOLL) {
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    if (target.ofd == pin.ofd && target.ofd_generation == pin.generation) {
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    index = watch_index(epoll, &target);
    if (operation == HL_LINUX_EPOLL_ADD) {
        hl_linux_epoll_watch *grown;
        if (index >= 0) {
            hl_linux_object_unpin(&pin);
            return -HL_LINUX_EEXIST;
        }
        pthread_mutex_lock(&epoll->lock);
        if (epoll->count == epoll->capacity) {
            uint32_t capacity = epoll->capacity == 0 ? 8u : epoll->capacity * 2u;
            grown = realloc(epoll->watches, (size_t)capacity * sizeof(*grown));
            if (grown == NULL) {
                pthread_mutex_unlock(&epoll->lock);
                hl_linux_object_unpin(&pin);
                return -HL_LINUX_ENOMEM;
            }
            epoll->watches = grown;
            epoll->capacity = capacity;
        }
        epoll->next_token++;
        if (epoll->next_token == 0) epoll->next_token++;
        epoll->watches[epoll->count] = (hl_linux_epoll_watch){.fd = target.fd,
                                                              .ofd = target.ofd,
                                                              .descriptor_generation = target.descriptor_generation,
                                                              .ofd_generation = target.ofd_generation,
                                                              .interests = interests,
                                                              .wait_handle = HL_HOST_HANDLE_INVALID,
                                                              .provider_handle = target.host_handle,
                                                              .data = data,
                                                              .token = epoll->next_token};
        pthread_mutex_unlock(&epoll->lock);
        status = epoll_subscribe(epoll, &epoll->watches[epoll->count]);
        if (status != HL_STATUS_OK) {
            hl_linux_object_unpin(&pin);
            return epoll_error(status);
        }
        pthread_mutex_lock(&epoll->lock);
        epoll->count++;
        pthread_mutex_unlock(&epoll->lock);
    } else if (operation == HL_LINUX_EPOLL_MODIFY) {
        if (index < 0) {
            hl_linux_object_unpin(&pin);
            return -HL_LINUX_ENOENT;
        }
        pthread_mutex_lock(&epoll->lock);
        epoll->watches[index].interests = interests;
        epoll->watches[index].data = data;
        epoll->watches[index].previous = 0;
        epoll->watches[index].disabled = 0;
        pthread_mutex_unlock(&epoll->lock);
    } else if (operation == HL_LINUX_EPOLL_DELETE) {
        hl_linux_epoll_watch removed;
        if (index < 0) {
            hl_linux_object_unpin(&pin);
            return -HL_LINUX_ENOENT;
        }
        pthread_mutex_lock(&epoll->lock);
        removed = epoll->watches[index];
        epoll->watches[index] = epoll->watches[--epoll->count];
        pthread_mutex_unlock(&epoll->lock);
        epoll_unsubscribe(epoll, &removed);
    } else {
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    (void)linux_abi->host->event->wake(linux_abi->host->context, epoll->wake);
    hl_linux_object_unpin(&pin);
    return 0;
}

static int64_t epoll_sample(hl_linux_epoll *epoll, hl_linux_epoll_event *events, uint32_t capacity) {
    uint32_t index;
    uint32_t delivered = 0;
    for (index = 0; delivered < capacity; ++index) {
        hl_linux_epoll_watch snapshot;
        hl_linux_epoll_watch *watch = &snapshot;
        hl_linux_object_pin target;
        uint32_t ready = 0;
        pthread_mutex_lock(&epoll->lock);
        if (index >= epoll->count) {
            pthread_mutex_unlock(&epoll->lock);
            break;
        }
        snapshot = epoll->watches[index];
        pthread_mutex_unlock(&epoll->lock);
        if (watch->disabled != 0) continue;
        if (watch->subscribed == EPOLL_STALE) continue;
        if (watch->subscribed == EPOLL_UNSUBSCRIBED) (void)epoll_subscribe(epoll, watch);
        if (hl_linux_object_pin_ofd(epoll->linux_abi, watch->ofd, watch->ofd_generation, &target) == HL_STATUS_OK) {
            ready = hl_linux_object_ready(&target, watch->interests);
            hl_linux_object_unpin(&target);
        } else if (hl_provider_files_is_handle(watch->provider_handle))
            ready = hl_provider_files_readiness(watch->provider_handle, watch->interests);
        else
            /* A final target close already quiesced this subscription by contract. */
            watch->subscribed = EPOLL_STALE;
        if ((watch->interests & HL_LINUX_EPOLL_EDGE) != 0) {
            uint32_t transition = ready & ~watch->previous;
            watch->previous = ready;
            ready = transition;
        }
        pthread_mutex_lock(&epoll->lock);
        for (uint32_t position = 0; position < epoll->count; ++position) {
            hl_linux_epoll_watch *current = &epoll->watches[position];
            if (current->token != watch->token) continue;
            current->subscribed = watch->subscribed;
            current->wait_handle = watch->wait_handle;
            current->previous = watch->previous;
            if (ready != 0 && (watch->interests & HL_LINUX_EPOLL_ONESHOT) != 0) current->disabled = 1;
            break;
        }
        pthread_mutex_unlock(&epoll->lock);
        if (ready != 0) { events[delivered++] = (hl_linux_epoll_event){ready, watch->data}; }
    }
    return (int64_t)delivered;
}

int64_t hl_linux_epoll_wait(hl_linux_abi *linux_abi, hl_linux_fd epoll_fd, hl_linux_epoll_event *events,
                            uint32_t capacity, uint64_t deadline_ns) {
    hl_linux_object_pin pin;
    hl_linux_epoll *epoll;
    hl_status status;
    if (events == NULL || capacity == 0) return -HL_LINUX_EINVAL;
    status = hl_linux_object_pin_fd(linux_abi, epoll_fd, &pin);
    if (status != HL_STATUS_OK) return epoll_error(status);
    if (pin.ops != &epoll_ops) {
        hl_linux_object_unpin(&pin);
        return -HL_LINUX_EINVAL;
    }
    epoll = pin.context;
    for (;;) {
        int64_t ready = epoll_sample(epoll, events, capacity);
        hl_host_result waited;
        if (ready != 0 || deadline_ns == 0) {
            hl_linux_object_unpin(&pin);
            return ready;
        }
        status = hl_linux_object_unlock(&pin);
        if (status != HL_STATUS_OK) {
            hl_linux_object_unpin(&pin);
            return epoll_error(status);
        }
        {
            hl_host_event_record ignored;
            waited = linux_abi->host->event->wait(linux_abi->host->context, epoll->wake, &ignored, 1, deadline_ns);
        }
        status = hl_linux_object_relock(&pin);
        if (status != HL_STATUS_OK) {
            hl_linux_object_abandon(&pin);
            return epoll_error(status);
        }
        if (hl_linux_object_retired(&pin)) {
            hl_linux_object_unpin(&pin);
            return -HL_LINUX_EBADF;
        }
        if (waited.status == HL_STATUS_WOULD_BLOCK) {
            hl_linux_object_unpin(&pin);
            return 0;
        }
        if (waited.status != HL_STATUS_OK) {
            hl_linux_object_unpin(&pin);
            return epoll_error((hl_status)waited.status);
        }
    }
}
