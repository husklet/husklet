/* Production host-services adapter for the typed Linux inotify object. Included after helpers.c. */

#include "../inotify.h"

typedef struct bound_inotify_watch {
    uint64_t token;
    uint32_t mask;
    char *path;
    char *snapshot;
    uint8_t directory;
} bound_inotify_watch;

typedef struct bound_inotify_queued {
    uint64_t token;
    uint32_t mask;
    uint32_t cookie;
    char name[256];
} bound_inotify_queued;

typedef struct bound_inotify_provider {
    const hl_host_services *host;
    hl_host_handle directory;
    hl_host_handle pollset;
    bound_inotify_watch *watches;
    uint32_t watch_count;
    uint32_t watch_capacity;
    bound_inotify_queued *queued;
    uint32_t queued_count;
    uint32_t queued_capacity;
    char delivered_names[32][256];
    struct bound_inotify_provider *next;
} bound_inotify_provider;

static bound_inotify_provider *g_bound_inotify_providers;
static uint32_t g_bound_inotify_cookie;

static char *bound_inotify_snapshot(bound_inotify_provider *provider, const char *path) {
    hl_host_file_entry entries[32];
    hl_host_result opened;
    hl_host_result read;
    hl_host_result closed;
    char *snapshot = NULL;
    size_t capacity = 256;
    size_t size = 0;
    opened = provider->host->file->open_relative(provider->host->context, HL_HOST_HANDLE_CWD, path, strlen(path),
                                                 HL_HOST_FILE_READ | HL_HOST_FILE_DIRECTORY, 0, 0);
    if (opened.status != HL_STATUS_OK) return NULL;
    snapshot = malloc(capacity);
    if (snapshot == NULL) {
        (void)provider->host->file->close(provider->host->context, opened.value);
        return NULL;
    }
    snapshot[0] = 0;
    for (;;) {
        uint32_t index;
        read = provider->host->file->read_directory(provider->host->context, opened.value, entries, 32, 16384);
        if (read.status != HL_STATUS_OK || read.value == 0) break;
        if (read.value > 32) {
            read.status = HL_STATUS_CORRUPT;
            break;
        }
        for (index = 0; index < (uint32_t)read.value; ++index) {
            size_t name_size = entries[index].name_size;
            char *grown;
            if ((name_size == 1 && entries[index].name[0] == '.') ||
                (name_size == 2 && entries[index].name[0] == '.' && entries[index].name[1] == '.'))
                continue;
            if (size > SIZE_MAX - name_size - 2u) {
                read.status = HL_STATUS_OUT_OF_MEMORY;
                break;
            }
            if (size + name_size + 2u > capacity) {
                size_t required = size + name_size + 2u;
                capacity = required > SIZE_MAX / 2u ? required : required * 2u;
                grown = realloc(snapshot, capacity);
                if (grown == NULL) {
                    read.status = HL_STATUS_OUT_OF_MEMORY;
                    break;
                }
                snapshot = grown;
            }
            memcpy(snapshot + size, entries[index].name, name_size);
            size += name_size;
            snapshot[size++] = '\n';
            snapshot[size] = 0;
        }
        if (read.status != HL_STATUS_OK) break;
    }
    closed = provider->host->file->close(provider->host->context, opened.value);
    if (read.status != HL_STATUS_OK || closed.status != HL_STATUS_OK) {
        free(snapshot);
        return NULL;
    }
    return snapshot;
}

static uint32_t bound_inotify_interests(uint32_t mask) {
    uint32_t interests = 0;
    if ((mask & (HL_LINUX_IN_ACCESS | HL_LINUX_IN_OPEN | HL_LINUX_IN_CLOSE_NOWRITE | HL_LINUX_IN_CLOSE_WRITE)) != 0)
        interests |= HL_HOST_DIRECTORY_ACCESS;
    if ((mask & (HL_LINUX_IN_MODIFY | HL_LINUX_IN_CLOSE_WRITE)) != 0) interests |= HL_HOST_DIRECTORY_MODIFY;
    if ((mask & HL_LINUX_IN_CREATE) != 0) interests |= HL_HOST_DIRECTORY_CREATE;
    if ((mask & (HL_LINUX_IN_DELETE | HL_LINUX_IN_DELETE_SELF)) != 0) interests |= HL_HOST_DIRECTORY_DELETE;
    if ((mask & (HL_LINUX_IN_MOVED_FROM | HL_LINUX_IN_MOVED_TO | HL_LINUX_IN_MOVE_SELF)) != 0)
        interests |= HL_HOST_DIRECTORY_RENAME;
    if ((mask & HL_LINUX_IN_ATTRIB) != 0) interests |= HL_HOST_DIRECTORY_ATTRIB;
    if ((mask & HL_LINUX_IN_ONESHOT) != 0) interests |= HL_HOST_DIRECTORY_ONESHOT;
    return interests;
}

static bound_inotify_watch *bound_inotify_watch_token(bound_inotify_provider *provider, uint64_t token) {
    uint32_t index;
    for (index = 0; index < provider->watch_count; ++index)
        if (provider->watches[index].token == token) return &provider->watches[index];
    return NULL;
}

static hl_status bound_inotify_queue(bound_inotify_provider *provider, uint64_t token, uint32_t mask, uint32_t cookie,
                                     const char *name, size_t name_size) {
    bound_inotify_queued *grown;
    bound_inotify_queued *event;
    if (provider->queued_count == provider->queued_capacity) {
        uint32_t capacity = provider->queued_capacity == 0 ? 16u : provider->queued_capacity * 2u;
        grown = realloc(provider->queued, (size_t)capacity * sizeof(*grown));
        if (grown == NULL) return HL_STATUS_OUT_OF_MEMORY;
        provider->queued = grown;
        provider->queued_capacity = capacity;
    }
    event = &provider->queued[provider->queued_count++];
    event->token = token;
    event->mask = mask;
    event->cookie = cookie;
    if (name_size >= sizeof(event->name)) name_size = sizeof(event->name) - 1u;
    if (name_size != 0) memcpy(event->name, name, name_size);
    event->name[name_size] = 0;
    (void)provider->host->event->wake(provider->host->context, provider->pollset);
    return HL_STATUS_OK;
}

static void bound_inotify_registry_add(bound_inotify_provider *provider) {
    provider->next = g_bound_inotify_providers;
    g_bound_inotify_providers = provider;
}

static void bound_inotify_registry_remove(bound_inotify_provider *provider) {
    bound_inotify_provider **link = &g_bound_inotify_providers;
    while (*link != NULL) {
        if (*link == provider) {
            *link = provider->next;
            return;
        }
        link = &(*link)->next;
    }
}

static hl_status bound_provider_add(void *opaque, const char *path, size_t path_size, uint64_t token, uint32_t mask) {
    bound_inotify_provider *provider = opaque;
    bound_inotify_watch *grown;
    bound_inotify_watch *watch;
    hl_host_file_metadata metadata;
    hl_host_result probe;
    hl_host_result inspected;
    hl_host_result closed;
    hl_host_result opened;
    hl_host_result added;
    char *saved;
    int is_directory;
    if (path_size > SIZE_MAX - 1u) return HL_STATUS_NAME_TOO_LONG;
    saved = malloc(path_size + 1u);
    if (saved == NULL) return HL_STATUS_OUT_OF_MEMORY;
    memcpy(saved, path, path_size);
    saved[path_size] = 0;
    probe = provider->host->file->open_relative(provider->host->context, HL_HOST_HANDLE_CWD, path, path_size,
                                                HL_HOST_FILE_PATH_ONLY, 0, 0);
    if (probe.status != HL_STATUS_OK) {
        free(saved);
        return (hl_status)probe.status;
    }
    memset(&metadata, 0, sizeof(metadata));
    inspected = provider->host->file->metadata(provider->host->context, probe.value, &metadata);
    closed = provider->host->file->close(provider->host->context, probe.value);
    if (inspected.status != HL_STATUS_OK || closed.status != HL_STATUS_OK) {
        hl_status status = (hl_status)(inspected.status != HL_STATUS_OK ? inspected.status : closed.status);
        free(saved);
        return status;
    }
    is_directory = metadata.type == HL_HOST_FILE_TYPE_DIRECTORY;
    if ((mask & HL_LINUX_IN_ONLYDIR) != 0 && !is_directory) {
        free(saved);
        return HL_STATUS_NOT_DIRECTORY;
    }
    opened =
        provider->host->file->open_relative(provider->host->context, HL_HOST_HANDLE_CWD, path, path_size,
                                            HL_HOST_FILE_READ | (is_directory ? HL_HOST_FILE_DIRECTORY : 0u), 0, 0);
    if (opened.status != HL_STATUS_OK) {
        free(saved);
        return (hl_status)opened.status;
    }
    if (provider->watch_count == provider->watch_capacity) {
        uint32_t capacity = provider->watch_capacity == 0 ? 8u : provider->watch_capacity * 2u;
        grown = realloc(provider->watches, (size_t)capacity * sizeof(*grown));
        if (grown == NULL) {
            (void)provider->host->file->close(provider->host->context, opened.value);
            free(saved);
            return HL_STATUS_OUT_OF_MEMORY;
        }
        provider->watches = grown;
        provider->watch_capacity = capacity;
    }
    watch = &provider->watches[provider->watch_count];
    *watch = (bound_inotify_watch){token, mask, saved, NULL, 0};
    if (is_directory) {
        watch->directory = 1;
        watch->snapshot = bound_inotify_snapshot(provider, saved);
    }
    added = provider->host->directory->add(provider->host->context, provider->directory, opened.value, token,
                                           bound_inotify_interests(mask));
    closed = provider->host->file->close(provider->host->context, opened.value);
    if (added.status != HL_STATUS_OK) {
        free(watch->snapshot);
        free(saved);
        return (hl_status)added.status;
    }
    if (closed.status != HL_STATUS_OK) {
        (void)provider->host->directory->remove(provider->host->context, provider->directory, token);
        free(watch->snapshot);
        free(saved);
        return (hl_status)closed.status;
    }
    provider->watch_count++;
    return HL_STATUS_OK;
}

static hl_status bound_provider_modify(void *opaque, uint64_t token, uint32_t mask) {
    bound_inotify_provider *provider = opaque;
    bound_inotify_watch *watch = bound_inotify_watch_token(provider, token);
    hl_host_result result;
    if (watch == NULL) return HL_STATUS_NOT_FOUND;
    result = provider->host->directory->modify(provider->host->context, provider->directory, token,
                                               bound_inotify_interests(mask));
    if (result.status == HL_STATUS_OK) watch->mask = mask;
    return (hl_status)result.status;
}

static hl_status bound_provider_remove(void *opaque, uint64_t token) {
    bound_inotify_provider *provider = opaque;
    bound_inotify_watch *watch = bound_inotify_watch_token(provider, token);
    uint32_t index;
    hl_host_result result;
    if (watch == NULL) return HL_STATUS_NOT_FOUND;
    result = provider->host->directory->remove(provider->host->context, provider->directory, token);
    if (result.status != HL_STATUS_OK && result.status != HL_STATUS_NOT_FOUND) return (hl_status)result.status;
    index = (uint32_t)(watch - provider->watches);
    free(watch->snapshot);
    free(watch->path);
    provider->watches[index] = provider->watches[--provider->watch_count];
    return HL_STATUS_OK;
}

static hl_status bound_inotify_snapshot_events(bound_inotify_provider *provider, bound_inotify_watch *watch) {
    char *current;
    uint32_t pass;
    if (!watch->directory) return HL_STATUS_OK;
    current = bound_inotify_snapshot(provider, watch->path);
    if (current == NULL) return HL_STATUS_IO;
    for (pass = 0; pass < 2; ++pass) {
        const char *source = pass == 0 ? current : watch->snapshot;
        const char *other = pass == 0 ? watch->snapshot : current;
        uint32_t mask = pass == 0 ? HL_LINUX_IN_CREATE : HL_LINUX_IN_DELETE;
        const char *name = source == NULL ? "" : source;
        while (*name != 0) {
            const char *end = strchr(name, '\n');
            size_t size = end == NULL ? strlen(name) : (size_t)(end - name);
            if (size != 0 && !snap_has(other, name, size)) {
                hl_status queued = bound_inotify_queue(provider, watch->token, mask, 0, name, size);
                if (queued != HL_STATUS_OK) {
                    free(current);
                    return queued;
                }
            }
            name = end == NULL ? name + size : end + 1;
        }
    }
    free(watch->snapshot);
    watch->snapshot = current;
    return HL_STATUS_OK;
}

static uint32_t bound_inotify_mask(uint32_t changes, uint32_t requested, int directory) {
    uint32_t mask = 0;
    if ((changes & HL_HOST_DIRECTORY_ACCESS) != 0)
        mask |=
            requested & (HL_LINUX_IN_ACCESS | HL_LINUX_IN_OPEN | HL_LINUX_IN_CLOSE_NOWRITE | HL_LINUX_IN_CLOSE_WRITE);
    if ((changes & HL_HOST_DIRECTORY_MODIFY) != 0) mask |= requested & (HL_LINUX_IN_MODIFY | HL_LINUX_IN_CLOSE_WRITE);
    if ((changes & HL_HOST_DIRECTORY_ATTRIB) != 0) mask |= requested & HL_LINUX_IN_ATTRIB;
    if (!directory && (changes & HL_HOST_DIRECTORY_DELETE) != 0) mask |= requested & HL_LINUX_IN_DELETE_SELF;
    if (!directory && (changes & HL_HOST_DIRECTORY_RENAME) != 0) mask |= requested & HL_LINUX_IN_MOVE_SELF;
    if ((changes & HL_HOST_DIRECTORY_IGNORED) != 0) mask |= HL_LINUX_IN_IGNORED;
    return mask;
}

static hl_status bound_inotify_collect(bound_inotify_provider *provider) {
    hl_host_directory_record records[32];
    hl_host_result read;
    uint32_t index;
    read = provider->host->directory->read(provider->host->context, provider->directory, records, 32);
    if (read.status == HL_STATUS_WOULD_BLOCK) return HL_STATUS_OK;
    if (read.status != HL_STATUS_OK) return (hl_status)read.status;
    for (index = 0; index < (uint32_t)read.value; ++index) {
        bound_inotify_watch *watch = bound_inotify_watch_token(provider, records[index].token);
        uint32_t mask;
        hl_status status;
        if (watch == NULL) continue;
        status = bound_inotify_snapshot_events(provider, watch);
        if (status != HL_STATUS_OK && status != HL_STATUS_IO) return status;
        mask = bound_inotify_mask(records[index].changes, watch->mask, watch->directory);
        if (mask != 0) {
            status = bound_inotify_queue(provider, watch->token, mask, 0, NULL, 0);
            if (status != HL_STATUS_OK) return status;
        }
    }
    return HL_STATUS_OK;
}

static hl_status bound_provider_drain(void *opaque, hl_linux_inotify_provider_event *events, uint32_t capacity,
                                      uint32_t *out_count) {
    bound_inotify_provider *provider = opaque;
    uint32_t count;
    uint32_t index;
    hl_status status = bound_inotify_collect(provider);
    if (status != HL_STATUS_OK) return status;
    count = capacity < provider->queued_count ? capacity : provider->queued_count;
    if (count > 32) count = 32;
    for (index = 0; index < count; ++index) {
        bound_inotify_queued *source = &provider->queued[index];
        size_t name_size = strlen(source->name);
        memcpy(provider->delivered_names[index], source->name, name_size + 1u);
        events[index] = (hl_linux_inotify_provider_event){source->token, source->mask, source->cookie,
                                                          provider->delivered_names[index], name_size};
    }
    provider->queued_count -= count;
    if (provider->queued_count != 0)
        memmove(provider->queued, provider->queued + count, (size_t)provider->queued_count * sizeof(*provider->queued));
    *out_count = count;
    return count == 0 ? HL_STATUS_WOULD_BLOCK : HL_STATUS_OK;
}

static hl_status bound_provider_wait(void *opaque) {
    bound_inotify_provider *provider = opaque;
    hl_host_event_record event;
    hl_host_result waited;
    if (provider->queued_count != 0) return HL_STATUS_OK;
    waited = provider->host->event->wait(provider->host->context, provider->pollset, &event, 1, UINT64_MAX);
    return waited.status == HL_STATUS_OK && waited.value != 0 ? HL_STATUS_OK : (hl_status)waited.status;
}

static hl_host_result bound_provider_wait_handle(void *opaque) {
    bound_inotify_provider *provider = opaque;
    return (hl_host_result){HL_STATUS_OK, 0, provider->directory, 0};
}

static uint32_t bound_provider_readiness(void *opaque) {
    bound_inotify_provider *provider = opaque;
    if (provider->queued_count != 0) return 1;
    (void)bound_inotify_collect(provider);
    return provider->queued_count != 0 ? 1u : 0u;
}

static hl_status bound_provider_clone(void *opaque, void **out_context);
static hl_status bound_provider_close(void *opaque);

static const hl_linux_inotify_provider_ops bound_inotify_ops = {
    .add = bound_provider_add,
    .modify = bound_provider_modify,
    .remove = bound_provider_remove,
    .drain = bound_provider_drain,
    .wait = bound_provider_wait,
    .wait_handle = bound_provider_wait_handle,
    .readiness = bound_provider_readiness,
    .clone = bound_provider_clone,
    .close = bound_provider_close,
};

static bound_inotify_provider *bound_inotify_provider_create(const hl_host_services *host) {
    bound_inotify_provider *provider;
    hl_host_result directory;
    hl_host_result pollset;
    hl_host_result controlled;
    if (host == NULL || host->directory == NULL || host->event == NULL || host->file == NULL) return NULL;
    provider = calloc(1, sizeof(*provider));
    if (provider == NULL) return NULL;
    provider->host = host;
    directory = host->directory->create(host->context);
    if (directory.status != HL_STATUS_OK) goto failed;
    provider->directory = directory.value;
    pollset = host->event->create(host->context);
    if (pollset.status != HL_STATUS_OK) goto failed_directory;
    provider->pollset = pollset.value;
    controlled =
        host->event->control(host->context, pollset.value, HL_HOST_EVENT_ADD, directory.value, 1, HL_HOST_READY_READ);
    if (controlled.status != HL_STATUS_OK) goto failed_pollset;
    bound_inotify_registry_add(provider);
    return provider;
failed_pollset:
    (void)host->event->close(host->context, pollset.value);
failed_directory:
    (void)host->directory->close(host->context, directory.value);
failed:
    free(provider);
    return NULL;
}

static hl_status bound_provider_clone(void *opaque, void **out_context) {
    bound_inotify_provider *source = opaque;
    bound_inotify_provider *copy = bound_inotify_provider_create(source->host);
    uint32_t index;
    if (copy == NULL) return HL_STATUS_OUT_OF_MEMORY;
    for (index = 0; index < source->watch_count; ++index) {
        bound_inotify_watch *watch = &source->watches[index];
        hl_status status = bound_provider_add(copy, watch->path, strlen(watch->path), watch->token, watch->mask);
        if (status != HL_STATUS_OK) {
            (void)bound_provider_close(copy);
            return status;
        }
    }
    for (index = 0; index < source->queued_count; ++index) {
        bound_inotify_queued *event = &source->queued[index];
        hl_status status =
            bound_inotify_queue(copy, event->token, event->mask, event->cookie, event->name, strlen(event->name));
        if (status != HL_STATUS_OK) {
            (void)bound_provider_close(copy);
            return status;
        }
    }
    *out_context = copy;
    return HL_STATUS_OK;
}

static hl_status bound_provider_close(void *opaque) {
    bound_inotify_provider *provider = opaque;
    hl_status status = HL_STATUS_OK;
    uint32_t index;
    bound_inotify_registry_remove(provider);
    for (index = 0; index < provider->watch_count; ++index) {
        free(provider->watches[index].snapshot);
        free(provider->watches[index].path);
    }
    if (provider->host->event->close(provider->host->context, provider->pollset).status != HL_STATUS_OK)
        status = HL_STATUS_IO;
    if (provider->host->directory->close(provider->host->context, provider->directory).status != HL_STATUS_OK)
        status = HL_STATUS_IO;
    free(provider->queued);
    free(provider->watches);
    free(provider);
    return status;
}

static void bound_inotify_move_side(bound_inotify_provider *provider, const char *path, uint32_t mask,
                                    uint32_t cookie) {
    const char *slash = strrchr(path, '/');
    size_t directory_size;
    uint32_t index;
    if (slash == NULL || slash == path || slash[1] == 0) return;
    directory_size = (size_t)(slash - path);
    for (index = 0; index < provider->watch_count; ++index) {
        bound_inotify_watch *watch = &provider->watches[index];
        if (!watch->directory || strlen(watch->path) != directory_size ||
            memcmp(watch->path, path, directory_size) != 0 || (watch->mask & mask) == 0)
            continue;
        (void)bound_inotify_queue(provider, watch->token, mask, cookie, slash + 1, strlen(slash + 1));
        {
            char *current = bound_inotify_snapshot(provider, watch->path);
            if (current != NULL) {
                free(watch->snapshot);
                watch->snapshot = current;
            }
        }
    }
}

static void bound_inotify_notify_move(int source_directory, const char *source_path, int destination_directory,
                                      const char *destination_path) {
    char source_buffer[4300];
    char destination_buffer[4300];
    const char *source = atpath(source_directory, source_path, source_buffer, sizeof(source_buffer), 1);
    const char *destination =
        atpath(destination_directory, destination_path, destination_buffer, sizeof(destination_buffer), 1);
    bound_inotify_provider *provider;
    uint32_t cookie;
    if (source == NULL || destination == NULL) return;
    cookie = ++g_bound_inotify_cookie;
    if (cookie == 0) cookie = ++g_bound_inotify_cookie;
    for (provider = g_bound_inotify_providers; provider != NULL; provider = provider->next) {
        bound_inotify_move_side(provider, source, HL_LINUX_IN_MOVED_FROM, cookie);
        bound_inotify_move_side(provider, destination, HL_LINUX_IN_MOVED_TO, cookie);
    }
}

int typed_inotify_scm_image_export(struct hl_cmsg_kqueue_meta *metadata, int marker) {
    if (metadata == NULL || metadata->kind != 3) return 0;
    size_t size = 0;
    if (metadata->source_fd < 0 || g_linux_box == NULL ||
        hl_linux_inotify_export(g_linux_box, (hl_linux_fd)metadata->source_fd, NULL, 0, &size) != HL_STATUS_OK ||
        size == 0 || size > 64u * 1024u * 1024u)
        return -1;
    void *image = malloc(size);
    size_t actual = 0;
    if (image == NULL ||
        hl_linux_inotify_export(g_linux_box, (hl_linux_fd)metadata->source_fd, image, size, &actual) != HL_STATUS_OK ||
        actual != size || pwrite(marker, image, size, (off_t)sizeof *metadata) != (ssize_t)size) {
        free(image);
        return -1;
    }
    free(image);
    metadata->image_size = size;
    return 0;
}

int typed_inotify_scm_image_import(int fd, const struct hl_cmsg_kqueue_meta *metadata, int marker) {
    if (metadata == NULL || metadata->kind != 3 || metadata->image_size == 0 ||
        metadata->image_size > 64u * 1024u * 1024u || metadata->image_size > SIZE_MAX || g_linux_box == NULL) {
        fprintf(stderr, "[scm-inotify] invalid image metadata kind=%u size=%llu box=%p\n",
                metadata ? metadata->kind : 0, (unsigned long long)(metadata ? metadata->image_size : 0),
                (void *)g_linux_box);
        return -1;
    }
    size_t size = (size_t)metadata->image_size;
    void *image = malloc(size);
    if (image == NULL || pread(marker, image, size, (off_t)sizeof *metadata) != (ssize_t)size ||
        bound_shadow_install(fd) != fd) {
        fprintf(stderr, "[scm-inotify] cannot load/install fd=%d marker=%d size=%zu errno=%d\n", fd, marker, size,
                errno);
        free(image);
        return -1;
    }
    void *provider = bound_inotify_provider_create(g_host_services);
    int64_t imported = provider == NULL ? -HL_LINUX_ENOMEM
                                        : hl_linux_inotify_import_at(g_linux_box, (hl_linux_fd)fd, &bound_inotify_ops,
                                                                     provider, metadata->descriptor_flags,
                                                                     metadata->nonblock ? O_NONBLOCK : 0, image, size);
    free(image);
    if (imported < 0) {
        fprintf(stderr, "[scm-inotify] typed import fd=%d failed=%lld\n", fd, (long long)imported);
        close(fd);
        return -1;
    }
    hl_linux_fd_snapshot snapshot;
    if (!bound_snapshot((uint64_t)(uint32_t)fd, &snapshot) || bound_fdvis_publish_snapshot(fd, &snapshot) != 0) {
        fprintf(stderr, "[scm-inotify] typed publication fd=%d failed errno=%d\n", fd, errno);
        close(fd);
        return -1;
    }
    return 0;
}
