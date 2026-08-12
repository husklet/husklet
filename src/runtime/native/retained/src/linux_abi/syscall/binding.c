/* Bridge opaque host-file bindings into the legacy native-fd guest runtime. */

#ifndef G_IS_DUP2_COMPAT
#define G_IS_DUP2_COMPAT() 0 /* aarch64 guests have no legacy dup2; every case 24 is a real dup3 */
#endif
#include "../object.h"
#include "../epoll.h"
#include "../eventfd.h"
#include "../watch.h"
#include "../bus.h"
#include "../../core/provider/files.h"

static int g_bound_sentinel = -1;

static void bound_epoll_provider_ready(void *opaque, uint64_t token) {
    ep_provider_watch *watch = opaque;
    uint32_t ready;
    struct kevent trigger;
    if (!ep_provider_callback_enter(watch, token)) return;
    if (watch->epoll < 0 || watch->epoll >= HL_NFD ||
        watch->epoll_generation != g_ep_provider_generations[watch->epoll]) {
        ep_provider_callback_leave(watch);
        return;
    }
    ready = hl_provider_files_cached_readiness(watch->handle, watch->interests);
    atomic_fetch_or(&watch->ready, ready);
    ep_wake_arm(watch->epoll);
    EV_SET(&trigger, EP_WAKE_IDENT, EVFILT_USER, 0, NOTE_TRIGGER, 0, NULL);
    (void)kevent(watch->epoll, &trigger, 1, NULL, 0, NULL);
    ep_provider_callback_leave(watch);
}

static int bound_sentinel_vacate(int target) {
    if (target < 0 || g_bound_sentinel != target) return 0;
    int relocated = fcntl(g_bound_sentinel, F_DUPFD_CLOEXEC, target < 64 ? 64 : target + 1);
    if (relocated < 0) return -errno;
    int adopted = hl_host_process_fd_private_adopt(relocated);
    /* No private band on this host means nothing to hoist into, and the guest
     * asked only that this NUMBER be free -- which the relocation above already
     * made it. Failing here instead would answer an ordinary F_DUPFD with EMFILE
     * purely because its floor happened to be the sentinel's number. Same test
     * bound_shadow_activate makes of the same refusal. */
    if (adopted < 0 && hl_host_process_fd_private_floor() >= 0) {
        close(relocated);
        return -ENOSPC;
    }
    hl_host_process_fd_private_remove(g_bound_sentinel);
    (void)hl_fdhandle_forget(g_bound_sentinel);
    close(g_bound_sentinel);
    g_bound_sentinel = adopted >= 0 ? adopted : relocated;
    return 0;
}

/* dup2 may transiently fail with EBUSY when another thread allocates a file
 * descriptor at the same instant.  Shadow publication is an internal engine
 * operation, so retry the exact target without exposing a false guest limit.
 * A persistent collision remains EBUSY after a bounded amount of work. */
static int bound_shadow_dup2(int target) {
    unsigned attempt;
    for (attempt = 0; attempt < 65u; ++attempt) {
        int descriptor = dup2(g_bound_sentinel, target);
        if (descriptor >= 0 || errno != EBUSY) return descriptor;
    }
    errno = EBUSY;
    return -1;
}

/* The sentry translates virtual descriptors before dispatch. A native descriptor may share an integer
 * with a logical typed descriptor in the sentry's forked ABI box; this per-servicer marker prevents that
 * native argument from being mistaken for typed authority. */
static _Thread_local int g_bound_source_native;

static int bound_source_is_native(void) {
    return g_bound_source_native;
}

static _Thread_local int g_bound_second_native;

typedef struct bound_watch_source {
    uint64_t token;
    uint64_t device;
    uint64_t inode;
    uint64_t size;
    hl_host_handle file;
    hl_host_handle watch;
    size_t references;
    struct bound_watch_source *next;
} bound_watch_source;

typedef struct bound_watch_state {
    pthread_mutex_t lock;
    pthread_t thread;
    hl_linux_watch_set changes;
    bound_watch_source *sources;
    hl_host_handle pollset;
    int initialized;
    int running;
    int stopping;
} bound_watch_state;

static bound_watch_state g_bound_watches = {
    .lock = PTHREAD_MUTEX_INITIALIZER,
    .pollset = HL_HOST_HANDLE_INVALID,
};
static pthread_mutex_t g_bound_mapping_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t g_bound_mapping_gate = PTHREAD_MUTEX_INITIALIZER;

typedef struct bound_mapping_object {
    hl_host_handle handle;
    hl_host_handle file;
    uint64_t address;
    uint64_t size;
    uint64_t device;
    uint64_t inode;
    uint64_t known_size;
    bound_watch_source *source;
    uint32_t identity_valid;
    uint32_t shared;
    size_t references;
} bound_mapping_object;

typedef struct bound_mapping {
    uint64_t address;
    uint64_t size;
    uint64_t object_offset;
    uint64_t file_offset;
    uint64_t follow_lo;
    uint64_t follow_hi;
    bound_mapping_object *object;
    struct bound_mapping *next;
} bound_mapping;

static void bound_watch_release(bound_watch_source *source);

static bound_mapping **bound_mapping_head(void) {
    size_t required = offsetof(hl_linux_abi, vma_state) + sizeof(g_linux_box->vma_state);
    if (g_linux_box == NULL || g_linux_box->abi != HL_LINUX_ABI_VERSION || g_linux_box->size < required) return NULL;
    return (bound_mapping **)&g_linux_box->vma_state;
}

static void bound_mapping_file_size_changed(const hl_linux_fd_snapshot *file, const hl_host_file_metadata *metadata,
                                            int have_metadata, uint64_t old_size, uint64_t new_size,
                                            hl_linux_bus_transition *transition);
static void bound_mapping_file_data_changed(const hl_linux_fd_snapshot *file, uint64_t device, uint64_t inode);

static bound_watch_source *bound_watch_find_token(uint64_t token) {
    for (bound_watch_source *source = g_bound_watches.sources; source != NULL; source = source->next)
        if (source->token == token) return source;
    return NULL;
}

static bound_watch_source *bound_watch_find_identity(uint64_t device, uint64_t inode) {
    for (bound_watch_source *source = g_bound_watches.sources; source != NULL; source = source->next)
        if (source->device == device && source->inode == inode) return source;
    return NULL;
}

static void bound_watch_publish_size(uint64_t device, uint64_t inode, uint64_t size) {
    pthread_mutex_lock(&g_bound_watches.lock);
    bound_watch_source *source = bound_watch_find_identity(device, inode);
    if (source != NULL) source->size = size;
    pthread_mutex_unlock(&g_bound_watches.lock);
}

static void bound_watch_apply(void *opaque, const hl_linux_watch_change *change) {
    (void)opaque;
    pthread_mutex_lock(&g_bound_watches.lock);
    bound_watch_source *source = bound_watch_find_token(change->token);
    if (source == NULL) {
        pthread_mutex_unlock(&g_bound_watches.lock);
        return;
    }
    if (change->new_size == source->size) {
        if ((change->flags & HL_HOST_WATCH_DATA) == 0) {
            pthread_mutex_unlock(&g_bound_watches.lock);
            return;
        }
        source->references++;
        hl_linux_fd_snapshot file = {.host_handle = source->file};
        uint64_t device = source->device, inode = source->inode;
        pthread_mutex_unlock(&g_bound_watches.lock);
        bound_mapping_file_data_changed(&file, device, inode);
        bound_watch_release(source);
        return;
    }
    source->references++;
    hl_linux_fd_snapshot file = {.host_handle = source->file};
    hl_host_file_metadata metadata = {.stable_device = source->device, .stable_object = source->inode};
    uint64_t old_size = source->size;
    source->size = change->new_size;
    pthread_mutex_unlock(&g_bound_watches.lock);
    hl_linux_bus_transition transition = {0};
    if (hl_linux_bus_transition_begin(&transition) != 0) {
        bound_watch_release(source);
        return;
    }
    pthread_mutex_lock(&g_bound_mapping_lock);
    bound_mapping_file_size_changed(&file, &metadata, 1, old_size, change->new_size, &transition);
    pthread_mutex_unlock(&g_bound_mapping_lock);
    bound_watch_release(source);
    hl_linux_bus_transition_end(&transition);
}

static void *bound_watch_waiter(void *opaque) {
    (void)opaque;
    for (;;) {
        hl_host_event_record events[16];
        hl_host_result ready = g_host_services->event->wait(g_host_services->context, g_bound_watches.pollset, events,
                                                            16, HL_HOST_DEADLINE_INFINITE);
        int drain_changes = 0;
        pthread_mutex_lock(&g_bound_watches.lock);
        if (g_bound_watches.stopping) {
            pthread_mutex_unlock(&g_bound_watches.lock);
            break;
        }
        if (ready.status == HL_STATUS_OK) {
            for (uint64_t index = 0; index < ready.value; ++index) {
                bound_watch_source *source = bound_watch_find_token(events[index].token);
                hl_host_watch_record record = {0};
                if (source == NULL) continue;
                hl_host_result drained =
                    g_host_services->watch->drain(g_host_services->context, source->watch, &record, 1);
                if (drained.status == HL_STATUS_OK && drained.value != 0)
                    drain_changes |= hl_linux_watch_enqueue(&g_bound_watches.changes, source->token, record.size,
                                                            record.changes) == HL_STATUS_OK;
            }
        }
        pthread_mutex_unlock(&g_bound_watches.lock);
        if (drain_changes) {
            size_t count = 0;
            (void)hl_linux_watch_drain(&g_bound_watches.changes, bound_watch_apply, NULL, &count);
        }
        if (ready.status != HL_STATUS_OK && ready.status != HL_STATUS_INTERRUPTED) break;
    }
    return NULL;
}

static int bound_watch_host_start_locked(void) {
    if (g_bound_watches.running || g_bound_watches.sources == NULL) return 1;
    hl_host_result pollset = g_host_services->event->create(g_host_services->context);
    if (pollset.status != HL_STATUS_OK) return 0;
    g_bound_watches.pollset = pollset.value;
    for (bound_watch_source *source = g_bound_watches.sources; source != NULL; source = source->next) {
        hl_host_result watched = g_host_services->watch->open(g_host_services->context, source->file);
        if (watched.status != HL_STATUS_OK) goto fail;
        source->watch = watched.value;
        if (g_host_services->event
                ->control(g_host_services->context, g_bound_watches.pollset, HL_HOST_EVENT_ADD, source->watch,
                          source->token, HL_HOST_READY_READ | HL_HOST_READY_EDGE)
                .status != HL_STATUS_OK)
            goto fail;
    }
    g_bound_watches.stopping = 0;
    if (pthread_create(&g_bound_watches.thread, NULL, bound_watch_waiter, NULL) != 0) goto fail;
    g_bound_watches.running = 1;
    return 1;
fail:
    for (bound_watch_source *source = g_bound_watches.sources; source != NULL; source = source->next) {
        if (source->watch == HL_HOST_HANDLE_INVALID) continue;
        (void)g_host_services->watch->close(g_host_services->context, source->watch);
        source->watch = HL_HOST_HANDLE_INVALID;
    }
    (void)g_host_services->event->close(g_host_services->context, g_bound_watches.pollset);
    g_bound_watches.pollset = HL_HOST_HANDLE_INVALID;
    return 0;
}

static void bound_watch_host_stop(int close_handles) {
    pthread_mutex_lock(&g_bound_watches.lock);
    if (g_bound_watches.running) {
        g_bound_watches.stopping = 1;
        (void)g_host_services->event->wake(g_host_services->context, g_bound_watches.pollset);
        pthread_mutex_unlock(&g_bound_watches.lock);
        (void)pthread_join(g_bound_watches.thread, NULL);
        pthread_mutex_lock(&g_bound_watches.lock);
        g_bound_watches.running = 0;
    }
    if (close_handles && g_bound_watches.pollset != HL_HOST_HANDLE_INVALID) {
        for (bound_watch_source *source = g_bound_watches.sources; source != NULL; source = source->next) {
            if (source->watch == HL_HOST_HANDLE_INVALID) continue;
            (void)g_host_services->event->control(g_host_services->context, g_bound_watches.pollset,
                                                  HL_HOST_EVENT_DELETE, source->watch, source->token,
                                                  HL_HOST_READY_READ);
            (void)g_host_services->watch->close(g_host_services->context, source->watch);
            source->watch = HL_HOST_HANDLE_INVALID;
        }
        (void)g_host_services->event->close(g_host_services->context, g_bound_watches.pollset);
        g_bound_watches.pollset = HL_HOST_HANDLE_INVALID;
    }
    pthread_mutex_unlock(&g_bound_watches.lock);
}

static bound_watch_source *bound_watch_retain(const hl_linux_fd_snapshot *file, uint64_t device, uint64_t inode,
                                              uint64_t size) {
    if (g_host_services == NULL || g_host_services->watch == NULL || g_host_services->event == NULL ||
        g_host_services->file == NULL || g_host_services->file->clone_for_fork == NULL ||
        (g_host_services->capabilities & (HL_HOST_CAP_WATCH | HL_HOST_CAP_EVENT)) !=
            (HL_HOST_CAP_WATCH | HL_HOST_CAP_EVENT))
        return NULL;
    pthread_mutex_lock(&g_bound_watches.lock);
    bound_watch_source *source = bound_watch_find_identity(device, inode);
    if (source != NULL) {
        source->references++;
        pthread_mutex_unlock(&g_bound_watches.lock);
        return source;
    }
    if (!g_bound_watches.initialized) {
        if (hl_linux_watch_init(&g_bound_watches.changes) != HL_STATUS_OK) {
            pthread_mutex_unlock(&g_bound_watches.lock);
            return NULL;
        }
        g_bound_watches.initialized = 1;
    }
    hl_host_result cloned = g_host_services->file->clone_for_fork(g_host_services->context, file->host_handle);
    source = cloned.status == HL_STATUS_OK ? calloc(1, sizeof(*source)) : NULL;
    if (source == NULL) {
        if (cloned.status == HL_STATUS_OK) (void)g_host_services->file->close(g_host_services->context, cloned.value);
        pthread_mutex_unlock(&g_bound_watches.lock);
        return NULL;
    }
    int created = 0;
    if (hl_linux_watch_retain(&g_bound_watches.changes, device, inode, size, &source->token, &created) !=
            HL_STATUS_OK ||
        !created) {
        (void)g_host_services->file->close(g_host_services->context, cloned.value);
        free(source);
        pthread_mutex_unlock(&g_bound_watches.lock);
        return NULL;
    }
    *source = (bound_watch_source){source->token,          device, inode, size, cloned.value, HL_HOST_HANDLE_INVALID, 1,
                                   g_bound_watches.sources};
    g_bound_watches.sources = source;
    int attached = 1;
    if (g_bound_watches.running) {
        hl_host_result watched = g_host_services->watch->open(g_host_services->context, source->file);
        attached = watched.status == HL_STATUS_OK;
        if (attached) {
            source->watch = watched.value;
            attached = g_host_services->event
                           ->control(g_host_services->context, g_bound_watches.pollset, HL_HOST_EVENT_ADD,
                                     source->watch, source->token, HL_HOST_READY_READ | HL_HOST_READY_EDGE)
                           .status == HL_STATUS_OK;
        }
        if (!attached && source->watch != HL_HOST_HANDLE_INVALID) {
            (void)g_host_services->watch->close(g_host_services->context, source->watch);
            source->watch = HL_HOST_HANDLE_INVALID;
        }
    } else {
        attached = bound_watch_host_start_locked();
    }
    if (!attached) {
        int removed = 0;
        g_bound_watches.sources = source->next;
        (void)hl_linux_watch_release(&g_bound_watches.changes, source->token, &removed);
        (void)g_host_services->file->close(g_host_services->context, source->file);
        free(source);
        source = NULL;
    }
    pthread_mutex_unlock(&g_bound_watches.lock);
    return source;
}

static void bound_watch_release(bound_watch_source *source) {
    if (source == NULL) return;
    pthread_mutex_lock(&g_bound_watches.lock);
    if (--source->references == 0) {
        bound_watch_source **link = &g_bound_watches.sources;
        while (*link != NULL && *link != source)
            link = &(*link)->next;
        if (*link == source) *link = source->next;
        if (source->watch != HL_HOST_HANDLE_INVALID) {
            (void)g_host_services->event->control(g_host_services->context, g_bound_watches.pollset,
                                                  HL_HOST_EVENT_DELETE, source->watch, source->token,
                                                  HL_HOST_READY_READ);
            (void)g_host_services->watch->close(g_host_services->context, source->watch);
        }
        int removed = 0;
        (void)hl_linux_watch_release(&g_bound_watches.changes, source->token, &removed);
        (void)g_host_services->file->close(g_host_services->context, source->file);
        free(source);
    }
    pthread_mutex_unlock(&g_bound_watches.lock);
}

static size_t bound_mapping_watch_capacity(void) {
    return g_linux_box == NULL ? 0 : g_linux_box->ofd_capacity;
}

static void bound_watch_fork_rebuild(void *opaque, uint64_t token, uint64_t device, uint64_t object) {
    (void)opaque;
    bound_watch_source *source = bound_watch_find_identity(device, object);
    if (source != NULL) source->token = token;
}

static int bound_mapping_fork_prepare(hl_linux_watch_fork_plan *plan) {
    if (!g_bound_watches.initialized) return 0;
    pthread_mutex_lock(&g_bound_mapping_gate);
    bound_watch_host_stop(1);
    pthread_mutex_lock(&g_bound_mapping_lock);
    if (hl_linux_watch_fork_snapshot(&g_bound_watches.changes, plan) == HL_STATUS_OK) { return 0; }
    pthread_mutex_unlock(&g_bound_mapping_lock);
    pthread_mutex_unlock(&g_bound_mapping_gate);
    return -1;
}

static int bound_mapping_fork_complete(hl_linux_watch_fork_plan *plan, int child) {
    hl_status status;
    if (!g_bound_watches.initialized) return 0;
    if (child) {
        pthread_mutex_t fresh = PTHREAD_MUTEX_INITIALIZER;
        memcpy(&g_bound_watches.lock, &fresh, sizeof(fresh));
        g_bound_watches.running = 0;
        g_bound_watches.stopping = 0;
        g_bound_watches.pollset = HL_HOST_HANDLE_INVALID;
        status = hl_linux_watch_fork_child(&g_bound_watches.changes, plan, bound_watch_fork_rebuild, NULL);
    } else {
        status = HL_STATUS_OK;
    }
    if (status != HL_STATUS_OK) {
        pthread_mutex_unlock(&g_bound_mapping_lock);
        pthread_mutex_unlock(&g_bound_mapping_gate);
        return -1;
    }
    pthread_mutex_lock(&g_bound_watches.lock);
    int started = bound_watch_host_start_locked();
    pthread_mutex_unlock(&g_bound_watches.lock);
    pthread_mutex_unlock(&g_bound_mapping_lock);
    pthread_mutex_unlock(&g_bound_mapping_gate);
    return started ? 0 : -1;
}

static int64_t bound_host_error(int32_t status) {
    switch ((hl_status)status) {
    case HL_STATUS_OK: return 0;
    case HL_STATUS_INVALID_ARGUMENT: return -EINVAL;
    case HL_STATUS_NOT_FOUND: return -ENOENT;
    case HL_STATUS_PERMISSION_DENIED: return -EACCES;
    case HL_STATUS_ALREADY_EXISTS: return -EEXIST;
    case HL_STATUS_RESOURCE_LIMIT: return -ENOMEM;
    case HL_STATUS_NOT_SUPPORTED: return -ENOTSUP;
    case HL_STATUS_INTERRUPTED: return -EINTR;
    case HL_STATUS_WOULD_BLOCK: return -EAGAIN;
    case HL_STATUS_OUT_OF_MEMORY: return -ENOMEM;
    case HL_STATUS_BUSY: return -EBUSY;
    case HL_STATUS_NOT_DIRECTORY: return -ENOTDIR;
    case HL_STATUS_IS_DIRECTORY: return -EISDIR;
    case HL_STATUS_NAME_TOO_LONG: return -ENAMETOOLONG;
    case HL_STATUS_SYMLINK_LOOP: return -ELOOP;
    case HL_STATUS_READ_ONLY: return -EROFS;
    case HL_STATUS_DISCONNECTED: return -EPIPE;
    case HL_STATUS_PROCESS_LIMIT: return -EMFILE;
    case HL_STATUS_CROSS_DEVICE: return -EXDEV;
    case HL_STATUS_NOT_EMPTY: return -ENOTEMPTY;
    case HL_STATUS_NO_SPACE: return -ENOSPC;
    case HL_STATUS_QUOTA: return -EDQUOT;
    case HL_STATUS_FILE_TOO_LARGE: return -EFBIG;
    case HL_STATUS_TIMED_OUT: return -ETIMEDOUT;
    case HL_STATUS_CONNECTION_REFUSED: return -ECONNREFUSED;
    case HL_STATUS_CONNECTION_RESET: return -ECONNRESET;
    case HL_STATUS_NETWORK_UNREACHABLE: return -ENETUNREACH;
    case HL_STATUS_ADDRESS_IN_USE: return -EADDRINUSE;
    default: return -EIO;
    }
}

static int bound_file_abi14(void) {
    const hl_host_file_services *file = g_host_services != NULL ? g_host_services->file : NULL;
    return file != NULL && file->abi == HL_HOST_FILE_ABI && file->size >= sizeof(*file);
}

static void bound_fill_statfs(uint8_t *output, const hl_host_filesystem_metadata *metadata) {
    const hl_linux_statfs_record record = {
        .type = INT64_C(0x01021994),
        .block_size = metadata->block_size,
        .blocks = metadata->blocks,
        .blocks_free = metadata->blocks_free,
        .blocks_available = metadata->blocks_available,
        .files = metadata->files,
        .files_free = metadata->files_free,
        .filesystem_id = {(uint32_t)metadata->filesystem_id[0], (uint32_t)metadata->filesystem_id[1]},
        .name_max = metadata->name_max,
        .fragment_size = metadata->fragment_size,
        .flags = metadata->flags,
    };
    (void)hl_linux_statfs_encode(&record, output, HL_LINUX_STATFS_RECORD_SIZE);
}

static void bound_fill_statx(uint8_t *output, const hl_linux_file_status *status) {
    memset(output, 0, 256);
    // Advertise STATX_BTIME only when the host actually reported a creation time. A caller trusts
    // stx_mask before reading stx_btime, so claiming the bit with a zero btime (a filesystem that
    // does not track it) would lie -- native leaves the bit clear there. See hl_statx_host_btime.
    *(uint32_t *)(output + 0) = 0x7ffu | (status->created_ns != 0 ? 0x800u : 0u);
    *(uint32_t *)(output + 4) = 4096;
    *(uint32_t *)(output + 16) = (uint32_t)status->link_count;
    *(uint32_t *)(output + 20) = status->user;
    *(uint32_t *)(output + 24) = status->group;
    *(uint16_t *)(output + 28) = (uint16_t)status->mode;
    *(uint64_t *)(output + 32) = status->object;
    *(uint64_t *)(output + 40) = status->size;
    *(uint64_t *)(output + 48) = status->blocks_512;
    const uint64_t timestamps[4] = {status->accessed_ns, status->created_ns, status->changed_ns, status->modified_ns};
    for (size_t index = 0; index < 4; ++index) {
        size_t offset = 64 + index * 16;
        *(int64_t *)(output + offset) = (int64_t)(timestamps[index] / UINT64_C(1000000000));
        *(uint32_t *)(output + offset + 8) = (uint32_t)(timestamps[index] % UINT64_C(1000000000));
    }
    *(uint32_t *)(output + 128) = hl_linux_device_major(status->special_device);
    *(uint32_t *)(output + 132) = hl_linux_device_minor(status->special_device);
    *(uint32_t *)(output + 136) = hl_linux_device_major(status->device);
    *(uint32_t *)(output + 140) = hl_linux_device_minor(status->device);
}

static void bound_virtualize_owner(const hl_linux_fd_snapshot *file, hl_linux_file_status *status) {
    char path[HL_LINUX_PATH_MAX + 1];
    hl_host_result named = g_host_services->file->path(g_host_services->context, file->host_handle,
                                                       (hl_host_bytes){path, HL_LINUX_PATH_MAX});
    if (named.status == HL_STATUS_OK && named.value <= HL_LINUX_PATH_MAX) {
        int uid, gid;
        path[named.value] = 0;
        struct stat native_status;
        if (lstat(path, &native_status) == 0 &&
            hl_owner_get(path, -1, &native_status, S_ISLNK(native_status.st_mode), &uid, &gid)) {
            if (uid >= 0) status->user = (uint32_t)uid;
            if (gid >= 0) status->group = (uint32_t)gid;
        }
    }
}

static uint32_t bound_mode_type(uint32_t type) {
    switch (type) {
    case HL_HOST_FILE_TYPE_REGULAR: return 0100000;
    case HL_HOST_FILE_TYPE_DIRECTORY: return 0040000;
    case HL_HOST_FILE_TYPE_SYMLINK: return 0120000;
    case HL_HOST_FILE_TYPE_CHARACTER: return 0020000;
    case HL_HOST_FILE_TYPE_BLOCK: return 0060000;
    case HL_HOST_FILE_TYPE_FIFO: return 0010000;
    case HL_HOST_FILE_TYPE_SOCKET: return 0140000;
    default: return 0;
    }
}

static void bound_status_from_metadata(hl_linux_file_status *status, const hl_host_file_metadata *metadata) {
    memset(status, 0, sizeof(*status));
    status->device = metadata->stable_device;
    status->object = metadata->stable_object;
    status->size = metadata->size;
    status->blocks_512 = metadata->allocated_size / 512u;
    status->modified_ns = metadata->modified_ns;
    status->accessed_ns = metadata->accessed_ns;
    status->changed_ns = metadata->changed_ns;
    status->created_ns = metadata->created_ns;
    status->special_device = metadata->device;
    status->link_count = metadata->link_count;
    status->mode = bound_mode_type(metadata->type) | (metadata->permissions & 07777u);
    status->user = metadata->user;
    status->group = metadata->group;
}

static void bound_virtualize_namespace(int fd, hl_linux_file_status *status) {
    if (fd < 0 || fd >= HL_NFD || !g_fdpath[fd][0]) return;
    const hl_provider_node *node = hl_provider_namespace_launch_resolve(g_fdpath[fd], strlen(g_fdpath[fd]));
    if (node == NULL || node->kind == HL_PROVIDER_NODE_DIRECTORY || node->kind == HL_PROVIDER_NODE_SYMLINK) return;

    uint32_t type = node->kind == HL_PROVIDER_NODE_CHARACTER ? 0020000
                    : node->kind == HL_PROVIDER_NODE_BLOCK   ? 0060000
                                                             : 0100000;
    status->mode = type | (node->mode & 07777u);
    status->user = node->uid;
    status->group = node->gid;
    status->special_device = node->kind == HL_PROVIDER_NODE_CHARACTER || node->kind == HL_PROVIDER_NODE_BLOCK
                                 ? hl_linux_device_make(node->major, node->minor)
                                 : 0;
    status->link_count = 1;
}

static bound_mapping *bound_mapping_find(uint64_t address, uint64_t size) {
    bound_mapping **head = bound_mapping_head();
    bound_mapping *entry;
    if (head == NULL || size == 0) return NULL;
    for (entry = *head; entry != NULL; entry = entry->next)
        if (address >= entry->address && size <= entry->size && address - entry->address <= entry->size - size)
            return entry;
    return NULL;
}

static void bound_mapping_drop(bound_mapping *entry, bound_mapping *previous) {
    bound_mapping **head = bound_mapping_head();
    bound_mapping_object *object = entry->object;
    if (head == NULL) return;
    if (previous != NULL)
        previous->next = entry->next;
    else
        *head = entry->next;
    free(entry);
    if (--object->references == 0) {
        bound_watch_release(object->source);
        if (object->handle != HL_HOST_HANDLE_INVALID)
            (void)g_host_services->memory->release(g_host_services->context, object->handle);
        free(object);
    }
}

static void bound_mapping_retire(uint64_t address, uint64_t size) {
    bound_mapping **head = bound_mapping_head();
    uint64_t end;
    bound_mapping *entry, *previous = NULL;
    if (head == NULL || size == 0 || address > UINT64_MAX - size) return;
    end = address + size;
    entry = *head;
    while (entry != NULL) {
        bound_mapping *next = entry->next;
        uint64_t base = entry->address, mapped_end = base + entry->size;
        if (end <= base || address >= mapped_end) {
            previous = entry;
        } else if (address <= base && end >= mapped_end) {
            bound_mapping_drop(entry, previous);
        } else if (address > base && end < mapped_end) {
            bound_mapping *tail = malloc(sizeof(*tail));
            if (tail != NULL) {
                *tail = (bound_mapping){end,
                                        mapped_end - end,
                                        entry->object_offset + end - base,
                                        entry->file_offset + end - base,
                                        0,
                                        0,
                                        entry->object,
                                        entry->next};
                entry->object->references++;
                entry->next = tail;
                entry->size = address - base;
                previous = tail;
            }
        } else if (address <= base) {
            uint64_t cut = end - base;
            entry->address += cut;
            entry->object_offset += cut;
            entry->file_offset += cut;
            entry->size -= cut;
            previous = entry;
        } else {
            entry->size = address - base;
            previous = entry;
        }
        entry = next;
    }
}

static void bound_mapping_reset(void) {
    bound_mapping **head = bound_mapping_head();
    if (head == NULL) return;
    pthread_mutex_lock(&g_bound_mapping_gate);
    bound_watch_host_stop(1);
    pthread_mutex_lock(&g_bound_mapping_lock);
    while (*head != NULL)
        bound_mapping_drop(*head, NULL);
    pthread_mutex_lock(&g_bound_watches.lock);
    if (g_bound_watches.initialized) {
        hl_linux_watch_close(&g_bound_watches.changes);
        g_bound_watches.initialized = 0;
    }
    pthread_mutex_unlock(&g_bound_watches.lock);
    pthread_mutex_unlock(&g_bound_mapping_lock);
    pthread_mutex_unlock(&g_bound_mapping_gate);
}

static int64_t bound_mmap_file(const hl_linux_fd_snapshot *file, uint64_t address, uint64_t size, uint32_t protection,
                               uint32_t linux_flags, uint64_t offset) {
    hl_host_file_mapping mapped = {HL_HOST_FILE_MAPPING_ABI, sizeof(mapped), 0, 0, 0, 0};
    uint32_t flags = (linux_flags & 1u) ? HL_HOST_MEMORY_SHARED : HL_HOST_MEMORY_PRIVATE;
    bound_mapping_object *object;
    bound_mapping *entry;
    bound_mapping **head = bound_mapping_head();
    int64_t result;
    uint64_t bus_accessible = size;
    uint64_t stable_device = 0, stable_object = 0, known_size = 0;
    hl_host_file_metadata metadata = {0};
    int identity_valid = 0;
    int bus_prepared = 0;
    if (head == NULL || g_host_services == NULL || g_host_services->memory == NULL ||
        g_host_services->memory->map_file == NULL)
        return -ENOSYS;
    pthread_mutex_lock(&g_bound_mapping_gate);
    if (linux_flags & 0x10u) flags |= HL_HOST_MEMORY_FIXED;
    if (linux_flags & 0x100000u) flags = (flags & ~HL_HOST_MEMORY_FIXED) | HL_HOST_MEMORY_FIXED_NOREPLACE;
    if (g_host_services->file != NULL && g_host_services->file->metadata != NULL) {
        hl_host_result status = g_host_services->file->metadata(g_host_services->context, file->host_handle, &metadata);
        if (status.status == HL_STATUS_OK) {
            stable_device = metadata.stable_device;
            stable_object = metadata.stable_object;
            known_size = metadata.size;
            identity_valid = 1;
            uint64_t available = metadata.size > offset ? metadata.size - offset : 0;
            bus_accessible =
                available > UINT64_MAX - UINT64_C(4095) ? UINT64_MAX : (available + UINT64_C(4095)) & ~UINT64_C(4095);
            if (bus_accessible < size) {
                gbus_prepare();
                bus_prepared = 1;
            }
        }
    }
    if (identity_valid && hl_linux_file_events_enable() != 0) {
        if (bus_prepared) gbus_prepare_release();
        pthread_mutex_unlock(&g_bound_mapping_gate);
        return -ENOMEM;
    }
#ifdef PCACHE_MMAP_HINT
    /* The typed route precedes svc_mem, so it owns the production persistent-cache hint.
     * Metadata failure and non-regular files remain ordinary, uncacheable mappings. */
    uint64_t pc_hint = 0;
    if (address == 0 && (linux_flags & (0x10u | 0x20u)) == 0 && identity_valid &&
        metadata.type == HL_HOST_FILE_TYPE_REGULAR) {
        pc_hint = pcache_mmap_hint(size);
        if (pc_hint != 0) address = pc_hint;
    }
#endif
    result = hl_linux_map_file(g_linux_box, file->fd, address, offset, size, protection & 7u, flags, &mapped);
    if (result < 0) {
        if (bus_prepared) gbus_prepare_release();
        pthread_mutex_unlock(&g_bound_mapping_gate);
        return result;
    }
    if ((flags & HL_HOST_MEMORY_FIXED) != 0) hl_exec_mapping_discard_range(mapped.address, mapped.mapped_size);
    object = calloc(1, sizeof(*object));
    entry = calloc(1, sizeof(*entry));
    if (object == NULL || entry == NULL) {
        free(object);
        free(entry);
        (void)g_host_services->memory->release(g_host_services->context, mapped.handle);
        if (bus_prepared) gbus_prepare_release();
        pthread_mutex_unlock(&g_bound_mapping_gate);
        return -ENOMEM;
    }
    pthread_mutex_lock(&g_bound_mapping_lock);
    if ((flags & HL_HOST_MEMORY_FIXED) != 0) bound_mapping_retire(mapped.address, mapped.mapped_size);
    bound_watch_source *source =
        identity_valid ? bound_watch_retain(file, stable_device, stable_object, known_size) : NULL;
    *object = (bound_mapping_object){mapped.handle,
                                     file->host_handle,
                                     mapped.address,
                                     mapped.mapped_size,
                                     stable_device,
                                     stable_object,
                                     known_size,
                                     source,
                                     (uint32_t)identity_valid,
                                     (uint32_t)((linux_flags & 1u) != 0),
                                     1};
    *entry = (bound_mapping){mapped.address, mapped.mapped_size, mapped.reserved, offset, 0, 0, object, *head};
    *head = entry;
    if (mapped.address == 0 || mapped.mapped_size < size || mapped.address > UINT64_MAX - size) {
        if (bus_prepared) gbus_prepare_release();
        bound_mapping_drop(entry, NULL);
        pthread_mutex_unlock(&g_bound_mapping_lock);
        pthread_mutex_unlock(&g_bound_mapping_gate);
        return -EIO;
    }
    hl_gmap_add(mapped.address, mapped.mapped_size);
    hl_gmap_set_guest_length(mapped.address, size);
    gbus_clear(mapped.address, mapped.address + size);
    /* A typed file mapping bypasses svc_mem, so publish the same guest protection
       transition here.  In particular, MAP_FIXED over a PROT_NONE reservation
       must retire the stale inaccessible interval before pointer validation. */
    {
        uint64_t lo = mapped.address & ~UINT64_C(0xfff);
        uint64_t hi = (mapped.address + size + UINT64_C(0xfff)) & ~UINT64_C(0xfff);
        if ((protection & 7u) == 0)
            gna_add(lo, hi);
        else
            gna_clear(lo, hi);
        if ((protection & 2u) == 0 && (protection & 7u) != 0)
            gro_add(lo, hi);
        else
            gro_clear(lo, hi);
    }
    if (bus_prepared && gbus_add(mapped.address + bus_accessible, mapped.address + size) != 0) {
        gbus_prepare_release();
        bound_mapping_drop(entry, NULL);
        hl_gmap_unmap_range(mapped.address, mapped.address + mapped.mapped_size);
        pthread_mutex_unlock(&g_bound_mapping_lock);
        pthread_mutex_unlock(&g_bound_mapping_gate);
        return -ENOMEM;
    }
    if (bus_prepared) gbus_prepare_release();
#ifdef PCACHE_MMAP_HINT
    /* A hint is advisory. Publish the identity only after the provider reports that exact
     * address, otherwise this run cannot safely restore translations for the mapping. */
    if (pc_hint != 0 && mapped.address == pc_hint && (protection & 4u) != 0)
        pcache_note_libmap(mapped.address, size, &metadata);
#endif
    pthread_mutex_unlock(&g_bound_mapping_lock);
    pthread_mutex_unlock(&g_bound_mapping_gate);
    return (int64_t)mapped.address;
}

static uint64_t bound_file_accessible(const bound_mapping *mapping, uint64_t file_size) {
    uint64_t available, rounded;
    if (file_size <= mapping->file_offset) return 0;
    available = file_size - mapping->file_offset;
    if (available > UINT64_MAX - UINT64_C(4095)) return mapping->size;
    rounded = (available + UINT64_C(4095)) & ~UINT64_C(4095);
    return rounded < mapping->size ? rounded : mapping->size;
}

static int bound_mapping_same_file(const bound_mapping_object *object, const hl_linux_fd_snapshot *file,
                                   const hl_host_file_metadata *metadata, int have_metadata) {
    if (have_metadata && object->identity_valid)
        return object->device == metadata->stable_device && object->inode == metadata->stable_object;
    return object->file == file->host_handle;
}

/* Recompute every VMA of the truncated inode, including mappings made through
   dup'd descriptors or a separately opened handle with the same stable host
   identity.  Shrink is called while gbus_prepare owns the activation
   transition, so no old translated block can touch the host mapping before the
   newly invalid pages are published. */
static void bound_mapping_file_size_changed(const hl_linux_fd_snapshot *file, const hl_host_file_metadata *metadata,
                                            int have_metadata, uint64_t old_size, uint64_t new_size,
                                            hl_linux_bus_transition *transition) {
    bound_mapping **head = bound_mapping_head();
    if (head == NULL) return;
    for (bound_mapping *entry = *head; entry != NULL; entry = entry->next) {
        uint64_t old_accessible, new_accessible;
        if (!bound_mapping_same_file(entry->object, file, metadata, have_metadata)) continue;
        entry->object->known_size = new_size;
        old_accessible = bound_file_accessible(entry, old_size);
        new_accessible = bound_file_accessible(entry, new_size);
        if (new_size < old_size && new_size > entry->file_offset && new_size < entry->file_offset + entry->size) {
            uint64_t tail = new_size - entry->file_offset;
            uint64_t partial_end = (tail + UINT64_C(4095)) & ~UINT64_C(4095);
            if (partial_end > entry->size) partial_end = entry->size;
            if (partial_end > tail) memset((void *)(uintptr_t)(entry->address + tail), 0, (size_t)(partial_end - tail));
        }
        if (new_accessible < old_accessible) {
            if (transition != NULL)
                (void)hl_linux_bus_transition_add(transition, entry->address + new_accessible,
                                                  entry->address + entry->size);
            else
                (void)gbus_add(entry->address + new_accessible, entry->address + entry->size);
        } else if (new_accessible > old_accessible) {
            if (!entry->object->shared) {
                entry->follow_lo = old_accessible;
                entry->follow_hi = new_accessible;
            }
            if (transition != NULL)
                hl_linux_bus_transition_clear(transition, entry->address + old_accessible,
                                              entry->address + new_accessible);
            else
                gbus_clear(entry->address + old_accessible, entry->address + new_accessible);
        }
    }
}

static void bound_mapping_file_written(const hl_linux_fd_snapshot *file, uint64_t offset, uint64_t size) {
    bound_mapping **head = bound_mapping_head();
    hl_host_file_metadata metadata = {0};
    int have_metadata = 0;
    uint64_t old_size = 0;
    int resized = 0;
    if (head == NULL || size == 0 || offset > UINT64_MAX - size || g_host_services == NULL ||
        g_host_services->file == NULL || g_host_services->file->read_at == NULL)
        return;
    if (g_host_services->file->metadata != NULL) {
        hl_host_result status = g_host_services->file->metadata(g_host_services->context, file->host_handle, &metadata);
        have_metadata = status.status == HL_STATUS_OK;
    }
    uint64_t end = offset + size;
    pthread_mutex_lock(&g_bound_mapping_gate);
    pthread_mutex_lock(&g_bound_mapping_lock);
    if (have_metadata) {
        for (bound_mapping *entry = *head; entry != NULL; entry = entry->next) {
            if (!bound_mapping_same_file(entry->object, file, &metadata, 1)) continue;
            old_size = entry->object->known_size;
            if (old_size != metadata.size) {
                bound_mapping_file_size_changed(file, &metadata, 1, old_size, metadata.size, NULL);
                resized = 1;
            }
            break;
        }
    }
    for (bound_mapping *entry = *head; entry != NULL; entry = entry->next) {
        if (entry->object->shared || entry->follow_hi <= entry->follow_lo ||
            !bound_mapping_same_file(entry->object, file, &metadata, have_metadata))
            continue;
        uint64_t map_lo = entry->file_offset + entry->follow_lo;
        uint64_t map_hi = entry->file_offset + entry->follow_hi;
        uint64_t lo = offset > map_lo ? offset : map_lo;
        uint64_t hi = end < map_hi ? end : map_hi;
        if (hi > lo) {
            hl_host_bytes output = {(void *)(uintptr_t)(entry->address + lo - entry->file_offset), (size_t)(hi - lo)};
            (void)g_host_services->file->read_at(g_host_services->context, file->host_handle, lo, output);
        }
    }
    pthread_mutex_unlock(&g_bound_mapping_lock);
    pthread_mutex_unlock(&g_bound_mapping_gate);
    if (resized)
        hl_linux_file_event_publish(HL_LINUX_FILE_EVENT_RESIZE, metadata.stable_device, metadata.stable_object,
                                    old_size, metadata.size);
    if (have_metadata)
        hl_linux_file_event_publish(HL_LINUX_FILE_EVENT_WRITE, metadata.stable_device, metadata.stable_object, offset,
                                    size);
}

static void bound_mapping_journal_apply(void *opaque, uint32_t kind, uint64_t device, uint64_t inode, uint64_t first,
                                        uint64_t second) {
    bound_mapping **head = bound_mapping_head();
    hl_host_file_metadata metadata = {.stable_device = device, .stable_object = inode};
    (void)opaque;
    if (head == NULL) return;
    if (kind == HL_LINUX_FILE_EVENT_RESIZE) {
        hl_linux_bus_transition transition = {0};
        if (hl_linux_bus_transition_begin(&transition) != 0) return;
        pthread_mutex_lock(&g_bound_mapping_gate);
        pthread_mutex_lock(&g_bound_mapping_lock);
        for (bound_mapping *entry = *head; entry != NULL; entry = entry->next) {
            hl_linux_fd_snapshot file;
            uint64_t old_size;
            if (!entry->object->identity_valid || entry->object->device != device || entry->object->inode != inode)
                continue;
            old_size = entry->object->known_size;
            if (old_size == second) continue;
            file = (hl_linux_fd_snapshot){.host_handle = entry->object->file};
            bound_mapping_file_size_changed(&file, &metadata, 1, old_size, second, &transition);
            break;
        }
        pthread_mutex_unlock(&g_bound_mapping_lock);
        pthread_mutex_unlock(&g_bound_mapping_gate);
        hl_linux_bus_transition_end(&transition);
        return;
    }
    if (kind != HL_LINUX_FILE_EVENT_WRITE || g_host_services == NULL || g_host_services->file == NULL ||
        g_host_services->file->read_at == NULL)
        return;
    pthread_mutex_lock(&g_bound_mapping_gate);
    pthread_mutex_lock(&g_bound_mapping_lock);
    for (bound_mapping *entry = *head; entry != NULL; entry = entry->next) {
        uint64_t map_lo, map_hi, event_hi, lo, hi;
        hl_host_bytes output;
        if (entry->object->shared || entry->follow_hi <= entry->follow_lo || !entry->object->identity_valid ||
            entry->object->device != device || entry->object->inode != inode)
            continue;
        map_lo = entry->file_offset + entry->follow_lo;
        map_hi = entry->file_offset + entry->follow_hi;
        event_hi = first > UINT64_MAX - second ? UINT64_MAX : first + second;
        lo = first > map_lo ? first : map_lo;
        hi = event_hi < map_hi ? event_hi : map_hi;
        if (hi <= lo) continue;
        output = (hl_host_bytes){(void *)(uintptr_t)(entry->address + lo - entry->file_offset), (size_t)(hi - lo)};
        (void)g_host_services->file->read_at(g_host_services->context, entry->object->file, lo, output);
    }
    pthread_mutex_unlock(&g_bound_mapping_lock);
    pthread_mutex_unlock(&g_bound_mapping_gate);
}

/* A size-preserving external write may populate pages that became accessible
 * after EOF was extended. Refresh only that clean private follow range; pages
 * dirtied before the resize remain private and untouched. */
static void bound_mapping_file_data_changed(const hl_linux_fd_snapshot *file, uint64_t device, uint64_t inode) {
    bound_mapping **head = bound_mapping_head();
    hl_host_file_metadata metadata = {.stable_device = device, .stable_object = inode};
    if (head == NULL || g_host_services == NULL || g_host_services->file == NULL ||
        g_host_services->file->read_at == NULL)
        return;
    pthread_mutex_lock(&g_bound_mapping_gate);
    pthread_mutex_lock(&g_bound_mapping_lock);
    for (bound_mapping *entry = *head; entry != NULL; entry = entry->next) {
        if (entry->object->shared || entry->follow_hi <= entry->follow_lo ||
            !bound_mapping_same_file(entry->object, file, &metadata, 1))
            continue;
        hl_host_bytes output = {(void *)(uintptr_t)(entry->address + entry->follow_lo),
                                (size_t)(entry->follow_hi - entry->follow_lo)};
        (void)g_host_services->file->read_at(g_host_services->context, file->host_handle,
                                             entry->file_offset + entry->follow_lo, output);
    }
    pthread_mutex_unlock(&g_bound_mapping_lock);
    pthread_mutex_unlock(&g_bound_mapping_gate);
}

static int bound_snapshot(uint64_t value, hl_linux_fd_snapshot *snapshot) {
    if (g_linux_box == NULL || value > UINT32_MAX) return 0;
    return hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)value, snapshot) == HL_STATUS_OK;
}

/* Publish descriptors supplied through the embedding API as logical guest
 * descriptors too.  In particular, typed stdin/stdout/stderr intentionally
 * do not occupy native fds 0..2: those numbers remain available to the engine
 * process itself.  /proc/self/fd must nevertheless describe the supplied
 * Linux descriptors, not whichever engine-private objects happen to occupy
 * the same native numbers. */
static int bound_fdvis_publish_snapshot(int fd, const hl_linux_fd_snapshot *snapshot) {
    hl_host_file_metadata metadata = {0};
    uint32_t kind = HL_HOST_FD_OTHER;
    if (snapshot == NULL || g_host_services == NULL || g_host_services->file == NULL ||
        g_host_services->file->metadata == NULL)
        return proc_fdvis_publish(fd, kind, 0, 0);
    if (g_host_services->file->metadata(g_host_services->context, snapshot->host_handle, &metadata).status ==
        HL_STATUS_OK) {
        if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ||
            metadata.type == HL_HOST_FILE_TYPE_SYMLINK || metadata.type == HL_HOST_FILE_TYPE_CHARACTER ||
            metadata.type == HL_HOST_FILE_TYPE_BLOCK)
            kind = HL_HOST_FD_FILE;
        else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
            kind = HL_HOST_FD_PIPE;
        else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
            kind = HL_HOST_FD_SOCKET;
    }
    return proc_fdvis_publish(fd, kind, metadata.stable_device, metadata.stable_object);
}

static int bound_shadow_reserve(int minimum) {
    int shadow;
    if (g_bound_sentinel < 0 || minimum < 0) {
        errno = EBADF;
        return -1;
    }
    /* Every live typed slot already owns a sentinel shadow, while private
     * engine descriptors are rehomed above the guest interval.  Let the host
     * kernel select and reserve the lowest free slot atomically: scanning and
     * then dup2 allowed a concurrent open to claim the candidate in between,
     * after which a successful dup2 silently replaced the other thread's fd. */
    shadow = fcntl(g_bound_sentinel, F_DUPFD_CLOEXEC, minimum);
    if (shadow < 0) return -1;
    if (shadow >= guest_nofile_cur()) {
        close(shadow);
        errno = EMFILE;
        return -1;
    }
    return shadow;
}

static int bound_shadow_matches(int fd) {
    struct stat sentinel_status;
    struct stat shadow_status;
    return g_bound_sentinel >= 0 && fstat(g_bound_sentinel, &sentinel_status) == 0 && fstat(fd, &shadow_status) == 0 &&
           sentinel_status.st_dev == shadow_status.st_dev && sentinel_status.st_ino == shadow_status.st_ino &&
           sentinel_status.st_rdev == shadow_status.st_rdev &&
           (sentinel_status.st_mode & S_IFMT) == (shadow_status.st_mode & S_IFMT);
}

static int bound_shadow_install(int fd) {
    int shadow;
    if (g_bound_sentinel < 0 || fd < 0 || fd >= guest_nofile_cur()) {
        errno = EBADF;
        return -1;
    }
    engine_fd_vacate(fd);
    shadow = bound_shadow_dup2(fd);
    if (shadow < 0) return -1;
    if (fcntl(shadow, F_SETFD, FD_CLOEXEC) != 0) {
        int error = errno;
        close(shadow);
        errno = error;
        return -1;
    }
    return shadow;
}

static int bound_private_dup(int source, int minimum) {
    hl_linux_fd_snapshot snapshot;
    int candidate = minimum;
    for (;;) {
        int duplicate = fcntl(source, F_DUPFD_CLOEXEC, candidate);
        if (duplicate < 0) return -1;
        if (!bound_snapshot((uint64_t)(unsigned)duplicate, &snapshot)) return duplicate;
        close(duplicate);
        if (duplicate == INT_MAX) {
            errno = EMFILE;
            return -1;
        }
        candidate = duplicate + 1;
    }
}

/* Called once in the isolated worker, before any guest-visible native descriptor allocation. */
static int bound_shadow_activate(void) {
    hl_linux_fd_snapshot snapshot;
    uint32_t fd;
    int opened;
    if (g_linux_box == NULL) return 0;
    /* Typed stdio alone still requires a sentinel so dup/F_DUPFD can allocate guest-number shadows. */
    if (g_bound_sentinel >= 0) {
        for (fd = 0; fd < g_linux_box->fd_capacity; ++fd) {
            if (hl_linux_fd_snapshot_get(g_linux_box, fd, &snapshot) == HL_STATUS_OK && fd >= 3 &&
                !bound_shadow_matches((int)fd))
                return -1;
        }
        return 0;
    }
    /* openat(AT_FDCWD, ...) IS open() on every host, and on a host whose
       descriptors carry no metadata of their own it is also the call that files
       the descriptor's access mode and close-on-exec in the handle table. The
       bare open() left the sentinel unrecorded there, and the very next line
       asks fcntl to duplicate it -- which on such a host can only answer for a
       descriptor the table knows. */
    opened = openat(AT_FDCWD, HL_LINUX_HOST_NULL_DEVICE, O_RDWR | O_CLOEXEC);
    if (opened < 0) return -1;
    g_bound_sentinel = bound_private_dup(opened, 64);
    if (g_bound_sentinel < 0) {
        int error = errno;
        (void)hl_fdhandle_forget(opened);
        close(opened);
        errno = error;
        return -1;
    }
    /* The record is only ever as good as the number it is filed under, and this
       number is about to be free for reuse by a path that publishes nothing. */
    (void)hl_fdhandle_forget(opened);
    close(opened);
    int adopted = hl_host_process_fd_private_adopt(g_bound_sentinel);
    /* A host that publishes no private descriptor band cannot hoist anything,
       and refusing to activate on that account would refuse the typed box
       outright. The sentinel then simply stays at the number bound_private_dup
       chose, which is what an unhoisted engine descriptor is everywhere: a
       collision hazard if a guest names it, not an incorrect answer. Same test
       activation.c already makes of the same refusal. */
    if (adopted < 0 && hl_host_process_fd_private_floor() >= 0) {
        int error = ENOSPC;
        close(g_bound_sentinel);
        g_bound_sentinel = -1;
        errno = error;
        return -1;
    }
    if (adopted >= 0) g_bound_sentinel = adopted;
    for (fd = 0; fd < g_linux_box->fd_capacity; ++fd) {
        int shadow;
        if (hl_linux_fd_snapshot_get(g_linux_box, fd, &snapshot) != HL_STATUS_OK) continue;
        if (fd >= 3) {
            shadow = bound_shadow_install((int)fd);
            if (shadow != (int)fd) {
                int error = shadow < 0 ? errno : EBUSY;
                if (shadow >= 0) close(shadow);
                errno = error;
                goto activation_failed;
            }
        }
        if (bound_fdvis_publish_snapshot((int)fd, &snapshot) != 0) {
            errno = ENOSPC;
            goto activation_failed;
        }
    }
    return 0;

activation_failed: {
    int error = errno;
    uint32_t rollback;
    for (rollback = 0; rollback <= fd && rollback < g_linux_box->fd_capacity; ++rollback)
        if (hl_linux_fd_snapshot_get(g_linux_box, rollback, &snapshot) == HL_STATUS_OK) {
            proc_fdvis_close((int)rollback);
            if (rollback >= 3) close((int)rollback);
        }
    hl_host_process_fd_private_remove(g_bound_sentinel);
    close(g_bound_sentinel);
    g_bound_sentinel = -1;
    errno = error;
    return -1;
}
}

static void bound_path_duplicate(hl_linux_fd source, int64_t target) {
    if (source >= HL_NFD || target < 0 || target >= HL_NFD) return;
    snprintf(g_fdpath[(int)target], sizeof g_fdpath[(int)target], "%s", g_fdpath[(int)source]);
}

static int64_t bound_dup_at_least(hl_linux_fd source, int minimum, uint32_t descriptor_flags) {
    struct fdvis_reservation fdvis;
    int shadow = bound_shadow_reserve(minimum);
    int64_t result;
    if (shadow < 0) return -(int64_t)errno;
    if (shadow >= guest_nofile_cur()) {
        close(shadow);
        return -EMFILE;
    }
    if (proc_fdvis_reserve(&fdvis) != 0) {
        close(shadow);
        return -ENOSPC;
    }
    result = hl_linux_dup3(g_linux_box, source, (hl_linux_fd)shadow, descriptor_flags != 0 ? HL_LINUX_O_CLOEXEC : 0);
    if (result < 0) {
        proc_fdvis_reservation_cancel(&fdvis);
        close(shadow);
    } else {
        hl_linux_fd_snapshot snapshot;
        hl_host_file_metadata metadata = {0};
        uint32_t kind = HL_HOST_FD_OTHER;
        if (bound_snapshot((uint64_t)result, &snapshot) && g_host_services != NULL && g_host_services->file != NULL &&
            g_host_services->file->metadata != NULL &&
            g_host_services->file->metadata(g_host_services->context, snapshot.host_handle, &metadata).status ==
                HL_STATUS_OK) {
            if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ||
                metadata.type == HL_HOST_FILE_TYPE_SYMLINK)
                kind = HL_HOST_FD_FILE;
            else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
                kind = HL_HOST_FD_PIPE;
            else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
                kind = HL_HOST_FD_SOCKET;
        }
        proc_fdvis_reservation_publish(&fdvis, (int)result, kind, metadata.stable_device, metadata.stable_object);
        bound_path_duplicate(source, result);
    }
    return result;
}

static int bound_handle_reserve(void *opaque) {
    bound_handle_slot *slot = opaque;
    hl_status status;
    int shadow = bound_shadow_reserve(0);
    if (slot == NULL || slot->active) return -EINVAL;
    if (proc_fdvis_reserve(&slot->fdvis) != 0) return -ENOSPC;
    if (shadow < 0 || shadow >= guest_nofile_cur()) {
        if (shadow >= 0) close(shadow);
        proc_fdvis_reservation_cancel(&slot->fdvis);
        return -EMFILE;
    }
    for (;;) {
        status = hl_linux_fd_reserve_at(g_linux_box, (hl_linux_fd)shadow, &slot->reservation);
        if (status != HL_STATUS_ALREADY_EXISTS) break;
        close(shadow);
        shadow = bound_shadow_reserve(shadow + 1);
        if (shadow < 0 || shadow >= guest_nofile_cur()) break;
    }
    if (status != HL_STATUS_OK || shadow < 0 || shadow >= guest_nofile_cur()) {
        if (shadow >= 0) close(shadow);
        proc_fdvis_reservation_cancel(&slot->fdvis);
        return -EMFILE;
    }
    slot->shadow = shadow;
    slot->active = 1;
    return 0;
}

static void bound_handle_cancel(bound_handle_slot *slot) {
    if (slot == NULL || !slot->active) return;
    (void)hl_linux_fd_cancel(g_linux_box, &slot->reservation);
    close(slot->shadow);
    proc_fdvis_reservation_cancel(&slot->fdvis);
    slot->active = 0;
}

static int64_t bound_adopt_handle(bound_handle_slot *slot, hl_host_handle file, uint32_t flags) {
    hl_host_file_metadata metadata = {0};
    uint32_t kind = HL_HOST_FD_OTHER;
    if (slot == NULL || !slot->active) return -EMFILE;
    if (g_host_services != NULL && g_host_services->file != NULL && g_host_services->file->metadata != NULL &&
        g_host_services->file->metadata(g_host_services->context, file, &metadata).status == HL_STATUS_OK) {
        if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ||
            metadata.type == HL_HOST_FILE_TYPE_SYMLINK)
            kind = HL_HOST_FD_FILE;
        else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
            kind = HL_HOST_FD_PIPE;
        else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
            kind = HL_HOST_FD_SOCKET;
    }
    int64_t result = hl_linux_file_adopt_reserved(g_linux_box, &slot->reservation, file, flags);
    if (result < 0) {
        bound_handle_cancel(slot);
    } else {
        slot->active = 0;
        proc_fdvis_reservation_publish(&slot->fdvis, (int)result, kind, metadata.stable_device, metadata.stable_object);
    }
    return result;
}

static int bound_handle_dirfd_error(int fd) {
    hl_linux_fd_snapshot snapshot;
    hl_host_file_metadata metadata;
    if (fd < 0 || !bound_snapshot((uint64_t)(uint32_t)fd, &snapshot)) return -EBADF;
    /* Pipes and sockets are known non-directories from the descriptor table
       itself; do not submit their provider-specific opaque handle to the
       host-file metadata port.  HL_HOST_FD_FILE and HL_HOST_FD_OTHER cover
       regular files, directories, and device nodes: a directory opened through
       the absolute-path resolver is classified HL_HOST_FD_OTHER, so its type
       must be confirmed by the metadata port rather than assumed. */
    if (snapshot.kind == HL_HOST_FD_PIPE || snapshot.kind == HL_HOST_FD_SOCKET) return -ENOTDIR;
    if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->metadata == NULL)
        return -ENOTDIR;
    hl_host_result result = g_host_services->file->metadata(g_host_services->context, snapshot.host_handle, &metadata);
    if (result.status != HL_STATUS_OK) return bound_host_error(result.status);
    return metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ? -EACCES : -ENOTDIR;
}

static int bound_handle_host_path(hl_host_handle file, char *path, size_t size) {
    hl_host_result named;
    if (path == NULL || size == 0 || g_host_services == NULL || g_host_services->file == NULL ||
        g_host_services->file->path == NULL)
        return -1;
    named = g_host_services->file->path(g_host_services->context, file, (hl_host_bytes){path, size - 1});
    if (named.status != HL_STATUS_OK || named.value >= size) return -1;
    path[named.value] = '\0';
    return 0;
}

static int bound_handle_chdir(int fd, int *result) {
    hl_linux_fd_snapshot snapshot;
    char path[HL_LINUX_PATH_MAX + 1];
    char guest[sizeof g_cwd];
    if (result == NULL || !bound_snapshot((uint64_t)(uint32_t)fd, &snapshot)) return 0;
    if (bound_handle_host_path(snapshot.host_handle, path, sizeof path) != 0) {
        *result = -EBADF;
        return 1;
    }
    int mapped = g_rootfs ? guest_from_host(path, guest, sizeof guest) : 0;
    if (mapped < 0) {
        *result = mapped;
        return 1;
    }
    if (g_rootfs && mapped == 0) {
        *result = -EACCES;
        return 1;
    }
    *result = chdir(path) == 0 ? 0 : -errno;
    if (*result == 0 && g_rootfs) (void)path_copy(g_cwd, sizeof g_cwd, guest);
    return 1;
}

static void bound_evict_relative(hl_host_handle directory, const char *path) {
    char base[HL_LINUX_PATH_MAX + 1];
    char joined[HL_LINUX_PATH_MAX + 1];
    int written;
    if (path == NULL || path[0] == '\0' || bound_handle_host_path(directory, base, sizeof(base)) != 0) return;
    written = snprintf(joined, sizeof(joined), "%s%s%s", base,
                       base[0] != '\0' && base[strlen(base) - 1] == '/' ? "" : "/", path);
    if (written < 0 || (size_t)written >= sizeof(joined)) return;
    hl_fdcache_evict_path(joined);
}

/* Resolution may temporarily occupy low native descriptors. Once its opaque
 * handles are closed, republish the new typed OFD at the true lowest logical
 * guest slot and retire the temporary shadow. */
static int64_t bound_relocate_lowest(int64_t opened) {
    struct fdvis_reservation fdvis;
    int shadow;
    int64_t duplicated;
    hl_linux_fd_snapshot snapshot;
    char guest_path[sizeof g_fdpath[0]];
    guest_path[0] = 0;
    if (opened < 0) return opened;
    if (opened < HL_NFD && g_fdpath[(int)opened][0])
        snprintf(guest_path, sizeof guest_path, "%s", g_fdpath[(int)opened]);
    shadow = bound_shadow_reserve(0);
    if (shadow < 0) return opened;
    /* `opened` already holds a descriptor, so bound_shadow_reserve()'s lowest-free scan can never return
     * opened's own number -- it returns the lowest OTHER free slot. When that slot is ABOVE opened, opened
     * is itself the lowest number the guest can be handed (e.g. it reused a just-closed low fd), so
     * relocating to `shadow` would drift the descriptor UPWARD, breaking Linux's lowest-free-fd contract
     * (a close(N)+reopen must return N). Keep opened in place; its fdvis view + path were already published
     * by bound_adopt_handle and the caller. Only relocate when a strictly-lower free slot exists. */
    if (shadow > opened) {
        close(shadow);
        return opened;
    }
    if (proc_fdvis_reserve(&fdvis) != 0) {
        close(shadow);
        return opened;
    }
    uint32_t flags = hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)opened, &snapshot) == HL_STATUS_OK &&
                             snapshot.descriptor_flags != 0
                         ? HL_LINUX_O_CLOEXEC
                         : 0;
    duplicated = hl_linux_dup3(g_linux_box, (hl_linux_fd)opened, (hl_linux_fd)shadow, flags);
    if (duplicated < 0) {
        proc_fdvis_reservation_cancel(&fdvis);
        close(shadow);
        return opened;
    }
    (void)hl_linux_close(g_linux_box, (hl_linux_fd)opened);
    proc_fdvis_close((int)opened);
    (void)close((int)opened);
    if (opened < HL_NFD) g_fdpath[(int)opened][0] = 0;
    if (duplicated >= 0 && duplicated < HL_NFD && guest_path[0])
        snprintf(g_fdpath[(int)duplicated], sizeof g_fdpath[(int)duplicated], "%s", guest_path);
    {
        hl_linux_fd_snapshot duplicate;
        hl_host_file_metadata metadata = {0};
        uint32_t kind = HL_HOST_FD_OTHER;
        if (bound_snapshot((uint64_t)duplicated, &duplicate) && g_host_services != NULL &&
            g_host_services->file != NULL && g_host_services->file->metadata != NULL &&
            g_host_services->file->metadata(g_host_services->context, duplicate.host_handle, &metadata).status ==
                HL_STATUS_OK) {
            if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ||
                metadata.type == HL_HOST_FILE_TYPE_SYMLINK)
                kind = HL_HOST_FD_FILE;
            else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
                kind = HL_HOST_FD_PIPE;
            else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
                kind = HL_HOST_FD_SOCKET;
        }
        proc_fdvis_reservation_publish(&fdvis, (int)duplicated, kind, metadata.stable_device, metadata.stable_object);
    }
    return duplicated;
}

static int bound_path_copy(uint64_t address, char path[HL_LINUX_PATH_MAX + 1], size_t *path_size) {
    size_t index;
    if (address == 0 || path == NULL || path_size == NULL) return -HL_LINUX_EFAULT;
    for (index = 0; index < HL_LINUX_PATH_MAX; ++index) {
        if (address > UINTPTR_MAX - index) return -HL_LINUX_EFAULT;
        if (guest_copy_from(path + index, address + index, 1) != 1) return -HL_LINUX_EFAULT;
        if (path[index] == 0) {
            if (index == 0) return -HL_LINUX_ENOENT;
            *path_size = index;
            return 0;
        }
    }
    return -HL_LINUX_ENAMETOOLONG;
}

/*
 * The bounce buffer these routes use runs BEFORE the typed file is consulted, so it must not be able to
 * fail first: Linux tests the descriptor's access mode next to fdget_pos, and a read that moves no bytes
 * never reaches copy_to_user.  `for_read` names the access mode the descriptor has to carry.
 */
static int bound_access_rejects(const hl_linux_fd_snapshot *file, int for_read) {
    uint32_t mode = file->status_flags & HL_LINUX_O_ACCMODE;
    return for_read ? mode == HL_LINUX_O_WRONLY : mode == HL_LINUX_O_RDONLY;
}

/* Classify an unusable read destination: a 1-byte pread at the position the read would use reveals EOF
   (and EISDIR on a directory) without consuming anything or moving the position. */
static int64_t bound_read_no_copy(const hl_linux_fd_snapshot *file, uint64_t offset, int positioned) {
    unsigned char probe;
    int64_t probed;
    if (bound_access_rejects(file, 1)) return -EBADF;
    probed = hl_linux_pread64(g_linux_box, file->fd, &probe, 1, positioned ? offset : file->offset);
    return probed <= 0 ? probed : -EFAULT;
}

static int bound_vectors_copy(uint64_t address, uint64_t count, hl_host_iovec vectors[HL_LINUX_IOV_MAX]) {
    uint64_t index;
    size_t array_size;
    if (count > HL_LINUX_IOV_MAX) return -HL_LINUX_EINVAL;
    if (count == 0) return 0;
    if (address == 0 || count > SIZE_MAX / sizeof(*vectors)) return -HL_LINUX_EFAULT;
    array_size = (size_t)count * sizeof(*vectors);
    if (guest_copy_from(vectors, address, array_size) != (ssize_t)array_size) return -HL_LINUX_EFAULT;
    for (index = 0; index < count; ++index) {
        // Only the descriptor ARRAY is validated here.  import_iovec does not dereference the payload
        // bases, so an unusable base must be judged later, where EOF can still make it irrelevant.
        if (vectors[index].size > SIZE_MAX) return -HL_LINUX_EFAULT;
    }
    return 0;
}

static int64_t bound_vector_io(const hl_linux_fd_snapshot *file, hl_host_iovec guest_vectors[HL_LINUX_IOV_MAX],
                               uint32_t count, int output, int positioned, uint64_t offset) {
    hl_host_iovec host_vectors[HL_LINUX_IOV_MAX] = {{0}};
    void *buffers[HL_LINUX_IOV_MAX] = {0};
    uint32_t usable = 0;
    int64_t result;
    if (count == 0)
        return output ? (positioned ? hl_linux_preadv(g_linux_box, file->fd, host_vectors, 0, offset)
                                    : hl_linux_readv(g_linux_box, file->fd, host_vectors, 0))
                      : (positioned ? hl_linux_pwritev(g_linux_box, file->fd, host_vectors, 0, offset)
                                    : hl_linux_writev(g_linux_box, file->fd, host_vectors, 0));
    for (uint32_t index = 0; index < count; ++index) {
        size_t size = (size_t)guest_vectors[index].size;
        if (size == 0) {
            host_vectors[usable++] = (hl_host_iovec){0, 0};
            continue;
        }
        if (output) {
            size_t prefix = guest_accessible_prefix(guest_vectors[index].address, size, HL_LOGICAL_VMA_WRITE);
            if (prefix == 0) {
                if (usable == 0) result = bound_read_no_copy(file, offset, positioned);
                goto issue_or_fail;
            }
            size = prefix;
        }
        buffers[usable] = malloc(size);
        if (buffers[usable] == NULL) {
            result = -ENOMEM;
            goto cleanup;
        }
        if (!output) {
            ssize_t copied = guest_copy_from(buffers[usable], guest_vectors[index].address, size);
            if (copied <= 0) {
                if (usable == 0) {
                    result = bound_access_rejects(file, 0) ? -EBADF : -EFAULT;
                    goto cleanup;
                }
                goto issue_or_fail;
            }
            size = (size_t)copied;
        }
        host_vectors[usable] = (hl_host_iovec){(uint64_t)(uintptr_t)buffers[usable], size};
        usable++;
        if (size != guest_vectors[index].size) break;
    }
issue_or_fail:
    if (usable == 0) goto cleanup;
    if (output)
        result = positioned ? hl_linux_preadv(g_linux_box, file->fd, host_vectors, usable, offset)
                            : hl_linux_readv(g_linux_box, file->fd, host_vectors, usable);
    else
        result = positioned ? hl_linux_pwritev(g_linux_box, file->fd, host_vectors, usable, offset)
                            : hl_linux_writev(g_linux_box, file->fd, host_vectors, usable);
    if (output && result > 0) {
        int64_t remaining = result, copied_total = 0;
        for (uint32_t index = 0; index < usable && remaining > 0; ++index) {
            size_t amount =
                (uint64_t)remaining < host_vectors[index].size ? (size_t)remaining : (size_t)host_vectors[index].size;
            ssize_t copied = guest_copy_to(guest_vectors[index].address, buffers[index], amount);
            if (copied <= 0) {
                result = copied_total != 0 ? copied_total : -EFAULT;
                break;
            }
            copied_total += copied;
            remaining -= copied;
            if ((size_t)copied != amount) {
                result = copied_total;
                break;
            }
        }
    }
cleanup:
    for (uint32_t index = 0; index < usable; ++index)
        free(buffers[index]);
    return result;
}

static int bound_poll_references(uint64_t address, uint64_t count) {
    struct pollfd *fds = NULL;
    uint64_t index;
    hl_linux_fd_snapshot snapshot;
    if (count > SIZE_MAX / sizeof(*fds)) return 0;
    if (count != 0) {
        size_t bytes = (size_t)count * sizeof(*fds);
        fds = malloc(bytes);
        if (fds == NULL || guest_copy_from(fds, address, bytes) != (ssize_t)bytes) {
            free(fds);
            return 0;
        }
    }
    for (index = 0; index < count; ++index)
        if (fds[index].fd >= 0 && bound_snapshot((uint64_t)(unsigned)fds[index].fd, &snapshot)) {
            free(fds);
            return 1;
        }
    free(fds);
    return 0;
}

static int bound_fdsets_reference(uint64_t count, uint64_t read_set, uint64_t write_set, uint64_t except_set) {
    uint64_t fd;
    size_t bytes;
    hl_linux_fd_snapshot snapshot;
    if (count > HL_LINUX_FD_LIMIT) count = HL_LINUX_FD_LIMIT;
    bytes = (size_t)((count + 7u) / 8u);
    uint8_t *sets = calloc(3, bytes == 0 ? 1 : bytes);
    if (sets == NULL) return 0;
    if ((read_set != 0 && guest_copy_from(sets, read_set, bytes) != (ssize_t)bytes) ||
        (write_set != 0 && guest_copy_from(sets + bytes, write_set, bytes) != (ssize_t)bytes) ||
        (except_set != 0 && guest_copy_from(sets + bytes * 2, except_set, bytes) != (ssize_t)bytes)) {
        free(sets);
        return 0;
    }
    for (fd = 0; fd < count; ++fd) {
        uint8_t mask = (uint8_t)(1u << (fd & 7u));
        size_t byte = (size_t)(fd >> 3);
        if (((read_set != 0 && (sets[byte] & mask) != 0) || (write_set != 0 && (sets[bytes + byte] & mask) != 0) ||
             (except_set != 0 && (sets[bytes * 2 + byte] & mask) != 0)) &&
            bound_snapshot(fd, &snapshot)) {
            free(sets);
            return 1;
        }
    }
    free(sets);
    return 0;
}

static uint32_t bound_poll_interests(short events) {
    uint32_t interests = 0;
    if ((events & POLLIN) != 0) interests |= HL_LINUX_READY_READ;
    if ((events & POLLOUT) != 0) interests |= HL_LINUX_READY_WRITE;
    if ((events & POLLPRI) != 0) interests |= HL_LINUX_READY_PRIORITY;
    return interests;
}

static short bound_poll_readiness(uint32_t readiness) {
    short events = 0;
    if ((readiness & HL_LINUX_READY_READ) != 0) events |= POLLIN;
    if ((readiness & HL_LINUX_READY_WRITE) != 0) events |= POLLOUT;
    if ((readiness & HL_LINUX_READY_PRIORITY) != 0) events |= POLLPRI;
    if ((readiness & HL_LINUX_READY_ERROR) != 0) events |= POLLERR;
    if ((readiness & HL_LINUX_READY_HANGUP) != 0) events |= POLLHUP;
    return events;
}

static uint64_t bound_now_ns(void) {
    struct timespec now = {0, 0};
    if (hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static uint64_t bound_deadline(const struct timespec *timeout) {
    uint64_t now;
    uint64_t delta;
    if (timeout == NULL) return UINT64_MAX;
    if (timeout->tv_sec < 0) return 0;
    now = bound_now_ns();
    if ((uint64_t)timeout->tv_sec > UINT64_MAX / UINT64_C(1000000000)) return UINT64_MAX;
    delta = (uint64_t)timeout->tv_sec * UINT64_C(1000000000) + (uint64_t)timeout->tv_nsec;
    return delta > UINT64_MAX - now ? UINT64_MAX : now + delta;
}

/* Poll native descriptors from a private copy: typed guest slots are never host descriptors. */
static int64_t bound_ppoll(struct cpu *c, uint64_t address, uint64_t count, uint64_t timeout_address,
                           uint64_t mask_address) {
    struct pollfd *guest;
    struct timespec timeout_value;
    struct timespec *timeout = timeout_address ? &timeout_value : NULL;
    struct pollfd *native;
    hl_linux_poll_entry *objects;
    uint32_t *object_indices;
    uint64_t deadline;
    uint64_t index;
    uint32_t object_count = 0;
    uint64_t saved = 0;
    uint64_t mask = 0;
    int mask_on;
    int64_t result = 0;
    if (count > (uint64_t)guest_nofile_cur()) return -EINVAL;
    if (count > SIZE_MAX / sizeof(*guest)) return -EFAULT;
    size_t guest_bytes = (size_t)count * sizeof(*guest);
    guest = calloc(count != 0 ? (size_t)count : 1, sizeof(*guest));
    if (!guest) return -ENOMEM;
    if ((count != 0 && guest_copy_from(guest, address, guest_bytes) != (ssize_t)guest_bytes) ||
        (timeout != NULL && guest_copy_from(timeout, timeout_address, sizeof(*timeout)) != sizeof(*timeout))) {
        free(guest);
        return -EFAULT;
    }
    if (timeout != NULL && (timeout->tv_nsec < 0 || timeout->tv_nsec >= 1000000000L)) {
        free(guest);
        return -EINVAL;
    }
    if (mask_address != 0 && (size_t)G_A4(c) != 8) {
        free(guest);
        return -EINVAL;
    }
    if (mask_address != 0 && guest_copy_from(&mask, mask_address, sizeof(mask)) != sizeof(mask)) {
        free(guest);
        return -EFAULT;
    }
    native = calloc(count != 0 ? (size_t)count : 1, sizeof(*native));
    objects = calloc(count != 0 ? (size_t)count : 1, sizeof(*objects));
    object_indices = calloc(count != 0 ? (size_t)count : 1, sizeof(*object_indices));
    if (native == NULL || objects == NULL || object_indices == NULL) {
        free(native);
        free(objects);
        free(object_indices);
        free(guest);
        return -ENOMEM;
    }
    memcpy(native, guest, (size_t)count * sizeof(*native));
    for (index = 0; index < count; ++index) {
        hl_linux_fd_snapshot snapshot;
        guest[index].revents = 0;
        if (guest[index].fd >= 0 && bound_snapshot((uint64_t)(unsigned)guest[index].fd, &snapshot)) {
            object_indices[object_count] = (uint32_t)index;
            objects[object_count++] = (hl_linux_poll_entry){snapshot.fd, bound_poll_interests(guest[index].events), 0};
            native[index].fd = -1;
        }
    }
    deadline = bound_deadline(timeout);
    mask_on = poll_sigmask_enter(c, mask_address != 0, mask, &saved);
    for (;;) {
        int native_ready;
        int64_t object_ready = hl_linux_object_poll(g_linux_box, objects, object_count, 0);
        int wait_ms = 0;
        uint64_t now = bound_now_ns();
        if (object_ready < 0) {
            result = object_ready;
            break;
        }
        if (object_ready == 0 && deadline != 0 && now < deadline) wait_ms = 1;
        native_ready = poll(native, (nfds_t)count, wait_ms);
        if (native_ready < 0) {
            if (svc_poll_retry(c)) continue;
            result = -errno;
            break;
        }
        if (object_ready != 0 || native_ready != 0 || deadline == 0 ||
            (deadline != UINT64_MAX && bound_now_ns() >= deadline)) {
            result = native_ready + object_ready;
            for (index = 0; index < count; ++index)
                guest[index].revents = native[index].revents;
            for (index = 0; index < object_count; ++index)
                guest[object_indices[index]].revents = bound_poll_readiness(objects[index].readiness);
            break;
        }
    }
    if (mask_on) poll_sigmask_leave(c, saved);
    if (result >= 0 && timeout != NULL) {
        uint64_t now = bound_now_ns();
        uint64_t left = deadline != UINT64_MAX && deadline > now ? deadline - now : 0;
        timeout->tv_sec = (time_t)(left / UINT64_C(1000000000));
        timeout->tv_nsec = (long)(left % UINT64_C(1000000000));
    }
    if (result >= 0 &&
        ((count != 0 && guest_copy_to(address, guest, guest_bytes) != (ssize_t)guest_bytes) ||
         (timeout != NULL && guest_copy_to(timeout_address, timeout, sizeof(*timeout)) != sizeof(*timeout))))
        result = -EFAULT;
    free(objects);
    free(object_indices);
    free(native);
    free(guest);
    return result;
}

static int bound_set_test(const uint8_t *set, uint32_t fd) {
    return set != NULL && (set[fd >> 3] & (uint8_t)(1u << (fd & 7u))) != 0;
}

static void bound_set_mark(uint8_t *set, uint32_t fd) {
    if (set != NULL) set[fd >> 3] |= (uint8_t)(1u << (fd & 7u));
}

static int64_t bound_pselect(struct cpu *c, uint64_t count_value, uint64_t read_address, uint64_t write_address,
                             uint64_t except_address) {
    uint32_t count = count_value > HL_LINUX_FD_LIMIT ? HL_LINUX_FD_LIMIT : (uint32_t)count_value;
    size_t bytes = ((size_t)count + 7u) / 8u;
    uint8_t *guest_read = NULL;
    uint8_t *guest_write = NULL;
    uint8_t *guest_except = NULL;
    uint8_t *sets = NULL;
    struct timespec timeout_value;
    struct timespec *timeout = G_A4(c) ? &timeout_value : NULL;
    uint64_t mask_pair_address = G_A5(c);
    uint8_t *requested;
    struct pollfd *native;
    hl_linux_poll_entry *objects;
    uint32_t *object_indices;
    uint32_t object_count = 0;
    uint32_t fd;
    uint64_t deadline;
    uint64_t mask_address = 0;
    uint64_t saved = 0;
    uint64_t mask = 0;
    int mask_on;
    int64_t result = 0;
    if (count_value > INT_MAX) return -EINVAL;
    sets = calloc(bytes != 0 ? bytes * 3 : 1, 1);
    if (!sets) return -ENOMEM;
    if (read_address) guest_read = sets;
    if (write_address) guest_write = sets + bytes;
    if (except_address) guest_except = sets + bytes * 2;
    if ((guest_read && guest_copy_from(guest_read, read_address, bytes) != (ssize_t)bytes) ||
        (guest_write && guest_copy_from(guest_write, write_address, bytes) != (ssize_t)bytes) ||
        (guest_except && guest_copy_from(guest_except, except_address, bytes) != (ssize_t)bytes) ||
        (timeout && guest_copy_from(timeout, G_A4(c), sizeof(*timeout)) != sizeof(*timeout))) {
        free(sets);
        return -EFAULT;
    }
    if (timeout != NULL && (timeout->tv_nsec < 0 || timeout->tv_nsec >= 1000000000L)) {
        free(sets);
        return -EINVAL;
    }
    if (mask_pair_address != 0) {
        uint64_t pair[2];
        if (guest_copy_from(pair, mask_pair_address, sizeof(pair)) != sizeof(pair)) {
            free(sets);
            return -EFAULT;
        }
        if (pair[0] != 0) {
            if (pair[1] != 8) {
                free(sets);
                return -EINVAL;
            }
            if (guest_copy_from(&mask, pair[0], sizeof(mask)) != sizeof(mask)) {
                free(sets);
                return -EFAULT;
            }
            mask_address = pair[0];
        }
    }
    requested = calloc(bytes != 0 ? bytes * 3 : 1, 1);
    native = calloc(count != 0 ? count : 1, sizeof(*native));
    objects = calloc(count != 0 ? count : 1, sizeof(*objects));
    object_indices = calloc(count != 0 ? count : 1, sizeof(*object_indices));
    if (requested == NULL || native == NULL || objects == NULL || object_indices == NULL) {
        result = -ENOMEM;
        goto done;
    }
    if (guest_read != NULL) memcpy(requested, guest_read, bytes);
    if (guest_write != NULL) memcpy(requested + bytes, guest_write, bytes);
    if (guest_except != NULL) memcpy(requested + bytes * 2, guest_except, bytes);
    for (fd = 0; fd < count; ++fd) {
        uint32_t interests = 0;
        hl_linux_fd_snapshot snapshot;
        if (bound_set_test(requested, fd)) interests |= HL_LINUX_READY_READ;
        if (bound_set_test(requested + bytes, fd)) interests |= HL_LINUX_READY_WRITE;
        if (bound_set_test(requested + bytes * 2, fd)) interests |= HL_LINUX_READY_PRIORITY;
        native[fd] = (struct pollfd){.fd = interests != 0 ? (int)fd : -1, .events = bound_poll_readiness(interests)};
        if (interests != 0 && bound_snapshot(fd, &snapshot)) {
            object_indices[object_count] = fd;
            objects[object_count++] = (hl_linux_poll_entry){snapshot.fd, interests, 0};
            native[fd].fd = -1;
        }
    }
    deadline = bound_deadline(timeout);
    mask_on = poll_sigmask_enter(c, mask_address != 0, mask, &saved);
    for (;;) {
        int native_ready;
        int64_t object_ready = hl_linux_object_poll(g_linux_box, objects, object_count, 0);
        uint64_t now = bound_now_ns();
        if (object_ready < 0) {
            result = object_ready;
            break;
        }
        native_ready = poll(native, count, object_ready == 0 && deadline != 0 && now < deadline ? 1 : 0);
        if (native_ready < 0) {
            if (svc_poll_retry(c)) continue;
            result = -errno;
            break;
        }
        for (fd = 0; fd < count; ++fd)
            if ((native[fd].revents & POLLNVAL) != 0) {
                result = -EBADF;
                goto waited;
            }
        if (native_ready != 0 || object_ready != 0 || deadline == 0 ||
            (deadline != UINT64_MAX && bound_now_ns() >= deadline)) {
            if (guest_read != NULL) memset(guest_read, 0, bytes);
            if (guest_write != NULL) memset(guest_write, 0, bytes);
            if (guest_except != NULL) memset(guest_except, 0, bytes);
            result = 0;
            for (fd = 0; fd < count; ++fd) {
                int ready = 0;
                if ((native[fd].revents & (POLLIN | POLLHUP | POLLERR)) != 0 && bound_set_test(requested, fd)) {
                    bound_set_mark(guest_read, fd);
                    ready = 1;
                }
                if ((native[fd].revents & (POLLOUT | POLLERR)) != 0 && bound_set_test(requested + bytes, fd)) {
                    bound_set_mark(guest_write, fd);
                    ready = 1;
                }
                if ((native[fd].revents & POLLPRI) != 0 && bound_set_test(requested + bytes * 2, fd)) {
                    bound_set_mark(guest_except, fd);
                    ready = 1;
                }
                result += ready;
            }
            for (fd = 0; fd < object_count; ++fd) {
                uint32_t descriptor = object_indices[fd];
                int ready = 0;
                if ((objects[fd].readiness & (HL_LINUX_READY_READ | HL_LINUX_READY_HANGUP | HL_LINUX_READY_ERROR)) !=
                        0 &&
                    bound_set_test(requested, descriptor)) {
                    bound_set_mark(guest_read, descriptor);
                    ready = 1;
                }
                if ((objects[fd].readiness & (HL_LINUX_READY_WRITE | HL_LINUX_READY_ERROR)) != 0 &&
                    bound_set_test(requested + bytes, descriptor)) {
                    bound_set_mark(guest_write, descriptor);
                    ready = 1;
                }
                if ((objects[fd].readiness & HL_LINUX_READY_PRIORITY) != 0 &&
                    bound_set_test(requested + bytes * 2, descriptor)) {
                    bound_set_mark(guest_except, descriptor);
                    ready = 1;
                }
                result += ready;
            }
            break;
        }
    }
waited:
    if (mask_on) poll_sigmask_leave(c, saved);
    if (result >= 0 && timeout != NULL) {
        uint64_t now = bound_now_ns();
        uint64_t left = deadline != UINT64_MAX && deadline > now ? deadline - now : 0;
        timeout->tv_sec = (time_t)(left / UINT64_C(1000000000));
        timeout->tv_nsec = (long)(left % UINT64_C(1000000000));
    }
done:
    if (result >= 0 && ((guest_read && guest_copy_to(read_address, guest_read, bytes) != (ssize_t)bytes) ||
                        (guest_write && guest_copy_to(write_address, guest_write, bytes) != (ssize_t)bytes) ||
                        (guest_except && guest_copy_to(except_address, guest_except, bytes) != (ssize_t)bytes) ||
                        (timeout && guest_copy_to(G_A4(c), timeout, sizeof(*timeout)) != sizeof(*timeout))))
        result = -EFAULT;
    free(object_indices);
    free(objects);
    free(native);
    free(requested);
    free(sets);
    return result;
}

static int bound_rights_reference(uint64_t message_address) {
    uint8_t message[56];
    uint8_t *control;
    uint64_t control_address;
    uint64_t control_size;
    uint64_t offset = 0;
    hl_linux_fd_snapshot snapshot;
    if (guest_copy_from(message, message_address, sizeof(message)) != (ssize_t)sizeof(message)) return 0;
    memcpy(&control_address, message + 32, sizeof(control_address));
    memcpy(&control_size, message + 40, sizeof(control_size));
#if SIZE_MAX < UINT64_MAX
    if (control_size > SIZE_MAX) return 0;
#endif
    if (control_address == 0 || control_size < 16) return 0;
    control = malloc((size_t)control_size);
    if (control == NULL || guest_copy_from(control, control_address, (size_t)control_size) != (ssize_t)control_size) {
        free(control);
        return 0;
    }
    while (offset + 16 <= control_size) {
        uint64_t length;
        int32_t level;
        int32_t type;
        uint64_t data;
        memcpy(&length, control + offset, sizeof(length));
        memcpy(&level, control + offset + 8, sizeof(level));
        memcpy(&type, control + offset + 12, sizeof(type));
        if (length < 16 || length > control_size - offset) break;
        if (level == LX_SOL_SOCKET && type == SCM_RIGHTS) {
            for (data = 16; data + sizeof(int32_t) <= length; data += sizeof(int32_t)) {
                int32_t fd;
                memcpy(&fd, control + offset + data, sizeof(fd));
                if (fd >= 0 && bound_snapshot((uint64_t)(uint32_t)fd, &snapshot)) {
                    free(control);
                    return 1;
                }
            }
        }
        if (length > UINT64_MAX - 7u) break;
        offset += (length + 7u) & ~UINT64_C(7);
    }
    free(control);
    return 0;
}

/* Return 1 with a scoped native alias for a typed file, 0 for an already-native fd, or -errno. */
static int bound_attachment_borrow(int guest_fd, int *native_fd) {
    hl_linux_fd_snapshot snapshot;
    const hl_host_posix_attachment_services *attachments;
    hl_host_result borrowed;
    if (native_fd == NULL || guest_fd < 0) return -EBADF;
    if (!bound_snapshot((uint64_t)(uint32_t)guest_fd, &snapshot)) {
        if (fcntl(guest_fd, F_GETFD) < 0) return -EBADF;
        *native_fd = guest_fd;
        return 0;
    }
    attachments = g_host_services == NULL ? NULL : g_host_services->posix_attachment;
    if (attachments == NULL || attachments->abi != HL_HOST_POSIX_ATTACHMENT_ABI ||
        attachments->size < sizeof(*attachments) || attachments->borrow_file == NULL)
        return -EOPNOTSUPP;
    borrowed = attachments->borrow_file(g_host_services->context, snapshot.host_handle);
    if (borrowed.status != HL_STATUS_OK) return bound_host_error(borrowed.status);
    if (borrowed.value > INT_MAX) {
        if (attachments->release != NULL) (void)attachments->release(g_host_services->context, borrowed.value);
        return -EIO;
    }
    *native_fd = (int)borrowed.value;
    return 1;
}

static void bound_attachment_release(int native_fd) {
    const hl_host_posix_attachment_services *attachments =
        g_host_services == NULL ? NULL : g_host_services->posix_attachment;
    if (attachments != NULL && attachments->release != NULL)
        (void)attachments->release(g_host_services->context, (uint64_t)(unsigned)native_fd);
    else
        close(native_fd);
}

static int64_t bound_stream_read(const hl_linux_fd_snapshot *file, int native_fd, void *buffer, size_t size,
                                 off_t *offset) {
    if (file != NULL)
        return offset != NULL ? hl_linux_pread64(g_linux_box, file->fd, buffer, size, (uint64_t)*offset)
                              : hl_linux_read(g_linux_box, file->fd, buffer, size);
    ssize_t count = offset != NULL ? pread(native_fd, buffer, size, *offset) : read(native_fd, buffer, size);
    return count < 0 ? -errno : count;
}

static int64_t bound_stream_write(const hl_linux_fd_snapshot *file, int native_fd, const void *buffer, size_t size,
                                  off_t *offset) {
    if (file != NULL)
        return offset != NULL ? hl_linux_pwrite64(g_linux_box, file->fd, buffer, size, (uint64_t)*offset)
                              : hl_linux_write(g_linux_box, file->fd, buffer, size);
    ssize_t count = offset != NULL ? pwrite(native_fd, buffer, size, *offset) : write(native_fd, buffer, size);
    return count < 0 ? -errno : count;
}

static int64_t bound_guest_read(const hl_linux_fd_snapshot *file, uint64_t guest, size_t size, uint64_t offset,
                                int positioned) {
    if (size == 0)
        return positioned ? hl_linux_pread64(g_linux_box, file->fd, NULL, 0, offset)
                          : hl_linux_read(g_linux_box, file->fd, NULL, 0);
    size_t accessible = guest_accessible_prefix(guest, size, HL_LOGICAL_VMA_WRITE);
    if (accessible == 0) return bound_read_no_copy(file, offset, positioned);
    void *buffer = malloc(accessible);
    if (buffer == NULL) return -ENOMEM;
    int64_t result = positioned ? hl_linux_pread64(g_linux_box, file->fd, buffer, accessible, offset)
                                : hl_linux_read(g_linux_box, file->fd, buffer, accessible);
    if (result > 0) {
        ssize_t copied = guest_copy_to(guest, buffer, (size_t)result);
        if (copied != result) result = copied > 0 ? copied : -EFAULT;
    }
    free(buffer);
    return result;
}

static int64_t bound_guest_write(const hl_linux_fd_snapshot *file, uint64_t guest, size_t size, uint64_t offset,
                                 int positioned) {
    if (size == 0)
        return positioned ? hl_linux_pwrite64(g_linux_box, file->fd, NULL, 0, offset)
                          : hl_linux_write(g_linux_box, file->fd, NULL, 0);
    if (bound_access_rejects(file, 0)) return -EBADF;
    void *buffer = malloc(size);
    if (buffer == NULL) return -ENOMEM;
    ssize_t copied = guest_copy_from(buffer, guest, size);
    if (copied <= 0) {
        free(buffer);
        return -EFAULT;
    }
    int64_t result = positioned ? hl_linux_pwrite64(g_linux_box, file->fd, buffer, (size_t)copied, offset)
                                : hl_linux_write(g_linux_box, file->fd, buffer, (size_t)copied);
    free(buffer);
    return result;
}

static int64_t bound_sendfile(const hl_linux_fd_snapshot *output, int output_fd, const hl_linux_fd_snapshot *input,
                              int input_fd, uint64_t offset_address, uint64_t count) {
    off_t supplied_offset = 0;
    off_t *input_offset = NULL;
    uint64_t done = 0;
    int64_t error = 0;
    char buffer[8192];
    if (input == NULL) {
        struct stat metadata;
        if (fstat(input_fd, &metadata) != 0) return -errno;
        if (!S_ISREG(metadata.st_mode)) return -EINVAL;
    } else if (g_host_services != NULL && g_host_services->file != NULL && g_host_services->file->metadata != NULL) {
        hl_host_file_metadata metadata;
        hl_host_result status =
            g_host_services->file->metadata(g_host_services->context, input->host_handle, &metadata);
        if (status.status != HL_STATUS_OK) return bound_host_error(status.status);
        if (metadata.type != HL_HOST_FILE_TYPE_REGULAR) return -EINVAL;
    }
    if (offset_address != 0) {
        if (guest_copy_from(&supplied_offset, offset_address, sizeof(supplied_offset)) !=
            (ssize_t)sizeof(supplied_offset))
            return -EFAULT;
        if (supplied_offset < 0) return -EINVAL;
        input_offset = &supplied_offset;
    }
    if (count > UINT64_C(0x7ffff000)) count = UINT64_C(0x7ffff000); /* Linux MAX_RW_COUNT */
    while (done < count) {
        uint64_t remaining = count - done;
        size_t chunk = remaining < sizeof(buffer) ? (size_t)remaining : sizeof(buffer);
        int64_t read_count = bound_stream_read(input, input_fd, buffer, chunk, input_offset);
        if (read_count <= 0) {
            error = read_count;
            break;
        }
        int64_t written = bound_stream_write(output, output_fd, buffer, (size_t)read_count, NULL);
        if (written <= 0) {
            error = written;
            if (input_offset == NULL)
                (void)(input != NULL ? hl_linux_lseek(g_linux_box, input->fd, -read_count, SEEK_CUR)
                                     : lseek(input_fd, (off_t)-read_count, SEEK_CUR));
            break;
        }
        if (input_offset != NULL) *input_offset += (off_t)written;
        if (output != NULL) bound_mapping_file_written(output, output->offset + done, (uint64_t)written);
        done += (uint64_t)written;
        if (written != read_count) {
            if (input_offset == NULL)
                (void)(input != NULL ? hl_linux_lseek(g_linux_box, input->fd, written - read_count, SEEK_CUR)
                                     : lseek(input_fd, (off_t)(written - read_count), SEEK_CUR));
            break;
        }
    }
    if (offset_address != 0 &&
        guest_copy_to(offset_address, &supplied_offset, sizeof(supplied_offset)) != (ssize_t)sizeof(supplied_offset))
        return done != 0 ? (int64_t)done : -EFAULT;
    return done != 0 ? (int64_t)done : error;
}

static int bound_native_pipe(int fd) {
    struct stat metadata;
    return fstat(fd, &metadata) == 0 && S_ISFIFO(metadata.st_mode);
}

static int64_t bound_splice(const hl_linux_fd_snapshot *input, int input_fd, uint64_t input_offset_address,
                            const hl_linux_fd_snapshot *output, int output_fd, uint64_t output_offset_address,
                            uint64_t size, uint64_t flags) {
    off_t input_value = 0, output_value = 0;
    off_t *input_offset = input_offset_address != 0 ? &input_value : NULL;
    off_t *output_offset = output_offset_address != 0 ? &output_value : NULL;
    int input_pipe = input == NULL && bound_native_pipe(input_fd);
    int output_pipe = output == NULL && bound_native_pipe(output_fd);
    static _Thread_local char buffer[65536];
    int64_t read_count, write_count, write_error = 0;
    size_t pushed = 0;
    if (flags & ~UINT64_C(0xf)) return -EINVAL;
    if (!input_pipe && !output_pipe) return -EINVAL;
    if ((input_pipe && input_offset != NULL) || (output_pipe && output_offset != NULL)) return -ESPIPE;
    if ((input_offset != NULL && guest_copy_from(input_offset, input_offset_address, sizeof(*input_offset)) !=
                                     (ssize_t)sizeof(*input_offset)) ||
        (output_offset != NULL && guest_copy_from(output_offset, output_offset_address, sizeof(*output_offset)) !=
                                      (ssize_t)sizeof(*output_offset)))
        return -EFAULT;
    if (size > UINT64_C(0x7ffff000)) size = UINT64_C(0x7ffff000);
    if (size > sizeof(buffer)) size = sizeof(buffer);
    if (size == 0) return 0;
    if (input_pipe) pushed = pipe_pushback_take(input_fd, buffer, (size_t)size);
    read_count = pushed != 0 ? (int64_t)pushed : bound_stream_read(input, input_fd, buffer, (size_t)size, input_offset);
    if (read_count <= 0) return read_count;
    write_count = bound_stream_write(output, output_fd, buffer, (size_t)read_count, output_offset);
    if (write_count < 0) {
        write_error = write_count;
        write_count = 0;
    }
    if (write_count < read_count) {
        size_t remainder = (size_t)(read_count - write_count);
        if (input_pipe)
            pipe_pushback_set(input_fd, buffer + write_count, remainder);
        else if (input_offset == NULL)
            (void)(input != NULL ? hl_linux_lseek(g_linux_box, input->fd, write_count - read_count, SEEK_CUR)
                                 : lseek(input_fd, (off_t)(write_count - read_count), SEEK_CUR));
    }
    if (write_count == 0) return write_error;
    if (input_offset != NULL) *input_offset += (off_t)write_count;
    if (output != NULL)
        bound_mapping_file_written(output, output_offset != NULL ? (uint64_t)*output_offset : output->offset,
                                   (uint64_t)write_count);
    if (output_offset != NULL) *output_offset += (off_t)write_count;
    if ((input_offset != NULL &&
         guest_copy_to(input_offset_address, input_offset, sizeof(*input_offset)) != (ssize_t)sizeof(*input_offset)) ||
        (output_offset != NULL && guest_copy_to(output_offset_address, output_offset, sizeof(*output_offset)) !=
                                      (ssize_t)sizeof(*output_offset)))
        return -EFAULT;
    return write_count;
}

// Enforce the guest soft RLIMIT_FSIZE on a bound-descriptor write of `count` bytes starting at absolute
// offset `pos`. Mirrors the native fsize_gate (io.c): a regular-file write at/beyond the limit raises SIGXFSZ
// and returns -EFBIG; a straddling write is clamped to the limit. Zero cost when the limit is infinite.
static int64_t bound_fsize_gate(struct cpu *c, const hl_linux_fd_snapshot *source, uint64_t pos, uint64_t count) {
    uint64_t limit = guest_fsize_cur();
    if (limit == ~UINT64_C(0) || count == 0) return (int64_t)count;
    if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->metadata == NULL)
        return (int64_t)count;
    hl_host_file_metadata metadata;
    hl_host_result status = g_host_services->file->metadata(g_host_services->context, source->host_handle, &metadata);
    if (status.status != HL_STATUS_OK || metadata.type != HL_HOST_FILE_TYPE_REGULAR) return (int64_t)count;
    if (pos >= limit) {
        raise_guest_signal(c, 25); // SIGXFSZ
        return -EFBIG;
    }
    uint64_t room = limit - pos;
    return count > room ? (int64_t)room : (int64_t)count;
}

/* renameat2(RENAME_EXCHANGE) across bound directories.  The host exposes only a
   replacing rename (renameat), so an atomic swap is staged through a private
   temporary in the destination directory: new->temp, old->new, temp->old.  Both
   operands must exist, matching the Linux contract; a failed middle step rolls
   the temporary back so neither name is lost. */
static int64_t bound_rename_exchange(hl_host_handle old_dir, const char *old_path, size_t old_size,
                                     hl_host_handle new_dir, const char *new_path, size_t new_size) {
    static uint64_t counter;
    const hl_host_file_services *file = g_host_services->file;
    void *ctx = g_host_services->context;
    char temp[64];
    int written = snprintf(temp, sizeof temp, ".hl-xchg-%d-%llu", (int)getpid(),
                           (unsigned long long)__atomic_add_fetch(&counter, 1, __ATOMIC_RELAXED));
    if (written <= 0 || (size_t)written >= sizeof temp) return -EIO;
    size_t temp_size = (size_t)written;
    hl_host_result step = file->rename_relative(ctx, new_dir, new_path, new_size, new_dir, temp, temp_size);
    if (step.status != HL_STATUS_OK) return bound_host_error(step.status);
    step = file->rename_relative(ctx, old_dir, old_path, old_size, new_dir, new_path, new_size);
    if (step.status != HL_STATUS_OK) {
        (void)file->rename_relative(ctx, new_dir, temp, temp_size, new_dir, new_path, new_size);
        return bound_host_error(step.status);
    }
    step = file->rename_relative(ctx, new_dir, temp, temp_size, old_dir, old_path, old_size);
    if (step.status != HL_STATUS_OK) return bound_host_error(step.status);
    return 0;
}

#if defined(_WIN32)
/*
 * The descriptor-shaped operations on a socket, on the one host where a socket
 * descriptor is not a kernel descriptor.
 *
 * Everything the socket FAMILY names -- socket, bind, connect, sendmsg and the
 * rest of 198..212 -- already reaches the network group through svc_net and the
 * REAL vocabulary in host_socket.h, and none of it comes through here. What
 * comes through here is the operations that are NOT about sockets and happen to
 * be applied to one: read, write, close, dup, fcntl, ioctl and the two waits.
 * Those resolve to the C library on this host, and the C library's descriptor
 * table does not know that this number names a socket.
 *
 * It is routed here rather than inside those calls because this is the first
 * router the dispatcher consults, and because it runs BEFORE the bound-slot
 * gate below -- a socket descriptor is not a bound slot, and on this host there
 * are no bound slots at all.
 */
static int64_t bound_socket_transfer(uint64_t address, uint64_t count, int descriptor, int writing) {
    void *buffer;
    int64_t result;
    size_t size = count > (uint64_t)(1u << 20) ? (size_t)(1u << 20) : (size_t)count;
    buffer = malloc(size != 0 ? size : 1);
    if (buffer == NULL) return -ENOMEM;
    if (writing) {
        if (size != 0 && guest_copy_from(buffer, address, size) != (ssize_t)size) {
            free(buffer);
            return -EFAULT;
        }
        result = (int64_t)hl_linux_socket_write(descriptor, buffer, size);
    } else {
        result = (int64_t)hl_linux_socket_read(descriptor, buffer, size);
        if (result > 0 && guest_copy_to(address, buffer, (size_t)result) != (ssize_t)result) {
            free(buffer);
            return -EFAULT;
        }
    }
    if (result < 0) result = -(int64_t)errno;
    free(buffer);
    return result;
}

/* readv/writev are coalesced through one buffer rather than issued per vector.
 * That is not an optimisation: a datagram socket must produce or consume one
 * message per call, and a loop over the vectors would turn one datagram into
 * several. */
static int64_t bound_socket_vector(struct cpu *c, uint64_t address, uint64_t count, int descriptor, int writing) {
    struct iovec vectors[64];
    unsigned char *buffer;
    uint64_t total = 0;
    uint64_t index;
    int64_t result;
    (void)c;
    if (count > HL_ARRAY_COUNT(vectors)) return -EINVAL;
    if (count != 0 && guest_copy_from(vectors, address, (size_t)count * sizeof(vectors[0])) !=
                          (ssize_t)((size_t)count * sizeof(vectors[0])))
        return -EFAULT;
    for (index = 0; index < count; ++index) {
        if (vectors[index].iov_len > (size_t)(1u << 20)) return -EINVAL;
        total += (uint64_t)vectors[index].iov_len;
    }
    if (total > (uint64_t)(1u << 22)) return -EINVAL;
    buffer = malloc(total != 0 ? (size_t)total : 1);
    if (buffer == NULL) return -ENOMEM;
    if (writing) {
        uint64_t offset = 0;
        for (index = 0; index < count; ++index) {
            const size_t length = vectors[index].iov_len;
            if (length != 0 && guest_copy_from(buffer + offset, (uint64_t)(uintptr_t)vectors[index].iov_base, length) !=
                                   (ssize_t)length) {
                free(buffer);
                return -EFAULT;
            }
            offset += length;
        }
        result = (int64_t)hl_linux_socket_write(descriptor, buffer, (size_t)total);
    } else {
        result = (int64_t)hl_linux_socket_read(descriptor, buffer, (size_t)total);
        if (result > 0) {
            uint64_t remaining = (uint64_t)result;
            uint64_t offset = 0;
            for (index = 0; index < count && remaining != 0; ++index) {
                size_t length = vectors[index].iov_len;
                if ((uint64_t)length > remaining) length = (size_t)remaining;
                if (length != 0 && guest_copy_to((uint64_t)(uintptr_t)vectors[index].iov_base, buffer + offset,
                                                 length) != (ssize_t)length) {
                    free(buffer);
                    return -EFAULT;
                }
                offset += length;
                remaining -= length;
            }
        }
    }
    if (result < 0) result = -(int64_t)errno;
    free(buffer);
    return result;
}

static short bound_socket_poll_events(uint32_t ready, short requested) {
    short revents = 0;
    if ((ready & HL_HOST_READY_READ) != 0) revents |= (short)(POLLIN & requested);
    if ((ready & HL_HOST_READY_WRITE) != 0) revents |= (short)(POLLOUT & requested);
    /* POLLERR and POLLHUP are reported whether or not they were asked for; that
     * is poll(2)'s rule and the reason a caller may pass events == 0. */
    if ((ready & HL_HOST_READY_ERROR) != 0) revents |= POLLERR;
    if ((ready & HL_HOST_READY_HANGUP) != 0) revents |= (short)(POLLHUP | (POLLRDHUP & requested));
    return revents;
}

/*
 * poll over a set whose every member is a socket. A mixed set is declined and
 * falls through to the ambient path, because the two populations have no shared
 * waitable form on this host yet and half-answering a set is worse than not
 * claiming it.
 *
 * The wait is a bounded re-derivation loop rather than a block, which is the
 * readiness model this contract is built on: nothing here is woken by name, and
 * a caller asks again. The slice is short enough that a guest select() with a
 * millisecond timeout still behaves like one.
 */
static int bound_socket_poll(struct cpu *c, uint64_t address, uint64_t count, int64_t timeout_ms, int64_t *out) {
    struct pollfd entries[64];
    uint64_t index;
    uint64_t elapsed = 0;
    (void)c;
    if (count == 0 || count > HL_ARRAY_COUNT(entries)) return 0;
    if (guest_copy_from(entries, address, (size_t)count * sizeof(entries[0])) !=
        (ssize_t)((size_t)count * sizeof(entries[0])))
        return 0;
    for (index = 0; index < count; ++index)
        if (entries[index].fd >= 0 && !hl_linux_socket_is(entries[index].fd)) return 0;
    for (;;) {
        int ready_count = 0;
        for (index = 0; index < count; ++index) {
            uint32_t ready = 0;
            entries[index].revents = 0;
            if (entries[index].fd < 0) continue;
            if (hl_linux_socket_readiness(entries[index].fd, 0, &ready) != 0) {
                entries[index].revents = POLLNVAL;
                ready_count++;
                continue;
            }
            entries[index].revents = bound_socket_poll_events(ready, entries[index].events);
            if (entries[index].revents != 0) ready_count++;
        }
        if (ready_count != 0 || timeout_ms == 0) {
            if (guest_copy_to(address, entries, (size_t)count * sizeof(entries[0])) !=
                (ssize_t)((size_t)count * sizeof(entries[0])))
                *out = -EFAULT;
            else
                *out = ready_count;
            return 1;
        }
        if (timeout_ms > 0 && (int64_t)elapsed >= timeout_ms) {
            *out = 0;
            return 1;
        }
        {
            struct timespec slice = {0, 1000000};
            nanosleep(&slice, NULL);
        }
        elapsed++;
    }
}

static int bound_socket_route(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3) {
    int64_t result;
    const int descriptor = (int)(int32_t)a0;
    /* execve keeps the descriptor numbering and drops the close-on-exec ones.
     * Done here and NOT claimed -- the exec itself belongs to whoever handles it
     * -- because a socket's close-on-exec bit lives in this layer's own record
     * and no other sweep can see it. Harmless if the exec then fails: the guest
     * asked for these to be gone the moment it succeeded, and a failed execve
     * that left them open would be the surprising outcome. */
    if (nr == 221 || nr == 281) {
        (void)hl_linux_socket_release_cloexec();
        return 0;
    }
    switch (nr) {
    case 57: /* close */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = hl_linux_socket_close(descriptor) == 0 ? 0 : -(int64_t)errno;
        break;
    case 63: /* read */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = bound_socket_transfer(a1, a2, descriptor, 0);
        break;
    case 64: /* write */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = bound_socket_transfer(a1, a2, descriptor, 1);
        break;
    case 65: /* readv */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = bound_socket_vector(c, a1, a2, descriptor, 0);
        break;
    case 66: /* writev */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = bound_socket_vector(c, a1, a2, descriptor, 1);
        break;
    case 23: /* dup */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = hl_linux_socket_dup(descriptor, -1);
        if (result < 0) result = -(int64_t)errno;
        break;
    case 24: /* dup3, and the legacy dup2 the normalizer folds into it */
        if (!hl_linux_socket_is(descriptor)) return 0;
        if ((int)(int32_t)a1 == descriptor) {
            /* dup2 of a descriptor onto itself is a no-op that must NOT close
             * it; dup3 of the same pair is EINVAL. G_IS_DUP2_COMPAT tells the
             * two apart on the arch where both exist. */
            result = G_IS_DUP2_COMPAT() ? (int64_t)descriptor : -EINVAL;
            break;
        }
        result = hl_linux_socket_dup(descriptor, (int)(int32_t)a1);
        if (result >= 0 && (a2 & (uint64_t)HL_LINUX_O_CLOEXEC) != 0)
            (void)hl_linux_socket_set_cloexec((int)(int32_t)a1, 1);
        if (result < 0) result = -(int64_t)errno;
        break;
    case 25: { /* fcntl */
        uint32_t flags = 0;
        if (!hl_linux_socket_is(descriptor)) return 0;
        if (hl_linux_socket_get_flags(descriptor, &flags) != 0) return 0;
        switch ((int32_t)a1) {
        case HL_LINUX_F_DUPFD:
        case HL_LINUX_F_DUPFD_CLOEXEC:
            result = hl_linux_socket_dup(descriptor, -1);
            if (result >= 0 && (int32_t)a1 == HL_LINUX_F_DUPFD_CLOEXEC)
                (void)hl_linux_socket_set_cloexec((int)result, 1);
            if (result < 0) result = -(int64_t)errno;
            break;
        case HL_LINUX_F_GETFD: result = (flags & HL_LINUX_SOCKET_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0; break;
        case HL_LINUX_F_SETFD:
            result =
                hl_linux_socket_set_cloexec(descriptor, (a2 & HL_LINUX_FD_CLOEXEC) != 0) == 0 ? 0 : -(int64_t)errno;
            break;
        case HL_LINUX_F_GETFL:
            /* A socket is always readable and writable, so the access mode is
             * O_RDWR; the only settable bit this layer records is O_NONBLOCK. */
            result = (int64_t)(uint64_t)(HL_LINUX_O_RDWR |
                                         ((flags & HL_LINUX_SOCKET_NONBLOCK) != 0 ? HL_LINUX_O_NONBLOCK : 0u));
            break;
        case HL_LINUX_F_SETFL:
            result =
                hl_linux_socket_set_nonblock(descriptor, (a2 & HL_LINUX_O_NONBLOCK) != 0) == 0 ? 0 : -(int64_t)errno;
            break;
        default: result = -EINVAL; break;
        }
        break;
    }
    case 29: { /* ioctl */
        uint32_t ready = 0;
        if (!hl_linux_socket_is(descriptor)) return 0;
        if ((uint32_t)a1 == 0x5421u) { /* FIONBIO */
            int requested = 0;
            if (guest_copy_from(&requested, a2, sizeof(requested)) != (ssize_t)sizeof(requested))
                result = -EFAULT;
            else
                result = hl_linux_socket_set_nonblock(descriptor, requested != 0) == 0 ? 0 : -(int64_t)errno;
            break;
        }
        if ((uint32_t)a1 == 0x541bu) { /* FIONREAD */
            uint64_t pending = 0;
            int available;
            if (hl_linux_socket_readiness_and_pending(descriptor, 0, &ready, &pending) != 0) {
                result = -(int64_t)errno;
                break;
            }
            available = pending > (uint64_t)INT_MAX ? INT_MAX : (int)pending;
            result = guest_copy_to(a2, &available, sizeof(available)) == (ssize_t)sizeof(available) ? 0 : -EFAULT;
            break;
        }
        result = -EINVAL;
        break;
    }
    case 73: { /* ppoll */
        struct timespec timeout;
        int64_t milliseconds = -1;
        if (a2 != 0) {
            if (guest_copy_from(&timeout, a2, sizeof(timeout)) != (ssize_t)sizeof(timeout)) return 0;
            milliseconds = (int64_t)timeout.tv_sec * 1000 + timeout.tv_nsec / 1000000;
        }
        if (!bound_socket_poll(c, a0, a1, milliseconds, &result)) return 0;
        break;
    }
    default: return 0;
    }
    (void)a3;
    G_RET(c) = (uint64_t)result;
    return 1;
}
#endif /* _WIN32 */

static int bound_route(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4) {
    hl_linux_fd_snapshot source;
    int64_t result;
    int source_bound = !g_bound_source_native && bound_snapshot(a0, &source);
#if defined(_WIN32)
    if (!g_bound_source_native && bound_socket_route(c, nr, a0, a1, a2, a3)) return 1;
    /* eventfd2 over the typed counter provider.
     *
     * Windows only. The emulated eventfd is a host pipe pair plus a counter in a
     * shared arena, and on a host with no pipe there is nothing under it; the
     * typed provider is a real counter object with the kernel's own semantics.
     * Every other host keeps the emulation, which is mature and cross-process,
     * so this arm is deliberately not taken there.
     *
     * The shadow-descriptor reservation is the same one inotify_init1 does below
     * and for the same reason: the object lives only in the typed box table, so
     * without a real kernel descriptor holding the identical number a later
     * non-bound open is handed that number and silently aliases the eventfd.
     *
     * Flag validation matches fs/eventfd.c -- only EFD_SEMAPHORE, EFD_CLOEXEC
     * and EFD_NONBLOCK, everything else EINVAL. glibc's EventFD probe calls
     * eventfd2(0, ~0) and REQUIRES it to fail, so a permissive mask here is a
     * feature-detection lie rather than a harmless leniency. */
    if (nr == 19 && g_linux_box != NULL) {
        const uint64_t semaphore = UINT64_C(0x1), nonblock = UINT64_C(0x800), cloexec = UINT64_C(0x80000);
        struct fdvis_reservation fdvis;
        hl_linux_fd_reservation reservation;
        hl_status status;
        int shadow;
        if ((a1 & ~(semaphore | nonblock | cloexec)) != 0) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            return 1;
        }
        shadow = bound_shadow_reserve(0);
        if (shadow < 0) {
            G_RET(c) = (uint64_t)(int64_t)-(int64_t)errno;
            return 1;
        }
        if (shadow >= guest_nofile_cur()) {
            close(shadow);
            G_RET(c) = (uint64_t)(int64_t)-EMFILE;
            return 1;
        }
        if (proc_fdvis_reserve(&fdvis) != 0) {
            close(shadow);
            G_RET(c) = (uint64_t)(int64_t)-ENOSPC;
            return 1;
        }
        for (;;) {
            status = hl_linux_fd_reserve_at(g_linux_box, (hl_linux_fd)shadow, &reservation);
            if (status != HL_STATUS_ALREADY_EXISTS) break;
            close(shadow);
            shadow = bound_shadow_reserve(shadow + 1);
            if (shadow < 0 || shadow >= guest_nofile_cur()) break;
        }
        if (status != HL_STATUS_OK || shadow < 0 || shadow >= guest_nofile_cur()) {
            if (shadow >= 0) close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
            G_RET(c) = (uint64_t)(int64_t)-EMFILE;
            return 1;
        }
        /* The token only proves the slot is free; the installer publishes it. */
        (void)hl_linux_fd_cancel(g_linux_box, &reservation);
        result = hl_linux_eventfd_create_at(g_linux_box, (hl_linux_fd)shadow, a0,
                                            (uint32_t)(((a1 & semaphore) != 0 ? HL_LINUX_EVENTFD_SEMAPHORE : 0u) |
                                                       ((a1 & nonblock) != 0 ? HL_LINUX_EVENTFD_NONBLOCK : 0u)),
                                            (a1 & cloexec) != 0 ? HL_LINUX_FD_CLOEXEC : 0);
        if (result < 0) {
            close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
        } else {
            proc_fdvis_reservation_publish(&fdvis, (int)result, HL_HOST_FD_OTHER, 0, 0);
        }
        G_RET(c) = (uint64_t)result;
        return 1;
    }
#endif
    if (nr == 26 && g_linux_box != NULL) {
        bound_inotify_provider *provider;
        struct fdvis_reservation fdvis;
        hl_linux_fd_reservation reservation;
        hl_status status;
        int shadow;
        if ((a0 & ~(UINT64_C(0x800) | UINT64_C(0x80000))) != 0) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            return 1;
        }
        /* Hold the guest fd number in the host kernel fd space as well.  The
           inotify object lives only in the typed box table, so without a real
           descriptor reserving the same slot a later non-bound (absolute-path)
           open is handed the identical number by the kernel and silently
           clobbers the watch -- read/poll/select then fail with EBADF.  Mirror
           the bound-openat reservation so the box and host fd allocators agree. */
        shadow = bound_shadow_reserve(0);
        if (shadow < 0) {
            G_RET(c) = (uint64_t)(int64_t)-(int64_t)errno;
            return 1;
        }
        if (shadow >= guest_nofile_cur()) {
            close(shadow);
            G_RET(c) = (uint64_t)(int64_t)-EMFILE;
            return 1;
        }
        if (proc_fdvis_reserve(&fdvis) != 0) {
            close(shadow);
            G_RET(c) = (uint64_t)(int64_t)-ENOSPC;
            return 1;
        }
        for (;;) {
            status = hl_linux_fd_reserve_at(g_linux_box, (hl_linux_fd)shadow, &reservation);
            if (status != HL_STATUS_ALREADY_EXISTS) break;
            close(shadow);
            shadow = bound_shadow_reserve(shadow + 1);
            if (shadow < 0 || shadow >= guest_nofile_cur()) break;
        }
        if (status != HL_STATUS_OK || shadow < 0 || shadow >= guest_nofile_cur()) {
            if (shadow >= 0) close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
            G_RET(c) = (uint64_t)(int64_t)-EMFILE;
            return 1;
        }
        /* The token only proves the slot is free; the object installer publishes
           the slot itself, so drop the token and install at the same number. */
        (void)hl_linux_fd_cancel(g_linux_box, &reservation);
        provider = bound_inotify_provider_create(g_host_services);
        if (provider == NULL) {
            close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
            G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
            return 1;
        }
        result = hl_linux_inotify_create_at(g_linux_box, (hl_linux_fd)shadow, &bound_inotify_ops, provider,
                                            (a0 & UINT64_C(0x80000)) != 0 ? HL_LINUX_FD_CLOEXEC : 0,
                                            (a0 & UINT64_C(0x800)) != 0 ? HL_LINUX_O_NONBLOCK : 0);
        if (result < 0) {
            close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
        } else {
            proc_fdvis_reservation_publish(&fdvis, (int)result, HL_HOST_FD_OTHER, 0, 0);
        }
        G_RET(c) = (uint64_t)result;
        return 1;
    }
    if (nr == 27 && source_bound) {
        char path[4200];
        char guest_path[HL_LINUX_PATH_MAX + 1];
        size_t guest_path_size;
        // EFAULT on an inaccessible path pointer BEFORE atpath dereferences it. inotify_add_watch(fd, NULL,
        // mask) and a wild/unmapped path return -EFAULT on Linux; without this guard atpath reads the
        // unmapped guest address and the engine child SIGSEGVs, killing the guest with signal 11 instead
        // (guest-triggerable crash). Mirrors the guarded sibling path syscalls (nr 78 below, fs.c openat).
        if (bound_path_copy(a1, guest_path, &guest_path_size) != 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            return 1;
        }
        const char *resolved = atpath(-100, guest_path, path, sizeof(path), 0);
        if (resolved == NULL)
            result = -errno;
        else
            result = hl_linux_inotify_add(g_linux_box, source.fd, resolved, strlen(resolved), (uint32_t)a2);
        G_RET(c) = (uint64_t)result;
        return 1;
    }
    if (nr == 28 && source_bound) {
        G_RET(c) = (uint64_t)hl_linux_inotify_remove(g_linux_box, source.fd, (int32_t)a1);
        return 1;
    }
    if (nr == 78 && a1 != 0 && a2 != 0 && (int64_t)a3 > 0) {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        if (bound_path_copy(a1, path, &path_size) != 0) return 0;
        int guest_fd = procfd_num(path);
        hl_linux_fd_snapshot target;
        if (guest_fd >= 0 && bound_snapshot((uint64_t)(uint32_t)guest_fd, &target)) {
            char target_path[4200];
            int length = proc_fd_link_pid((int)getpid(), guest_fd, target_path, sizeof target_path);
            if (length < 0) return 0;
            size_t copied = (size_t)length > (size_t)a3 ? (size_t)a3 : (size_t)length;
            if (guest_copy_to(a2, target_path, copied) != (ssize_t)copied) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return 1;
            }
            G_RET(c) = (uint64_t)copied;
            return 1;
        }
    }
    if (nr == 73 && bound_poll_references(a0, a1)) {
        G_RET(c) = (uint64_t)bound_ppoll(c, a0, a1, a2, a3);
        return 1;
    }
    if (nr == 72 && bound_fdsets_reference(a0, a1, a2, a3)) {
        G_RET(c) = (uint64_t)bound_pselect(c, a0, a1, a2, a3);
        return 1;
    }
    if (nr == 222 && (a3 & 0x20u) == 0) {
        hl_linux_fd_snapshot mapped;
        if (bound_snapshot(G_A4(c), &mapped)) {
            G_RET(c) = (uint64_t)bound_mmap_file(&mapped, a0, a1, (uint32_t)a2, (uint32_t)a3, G_A5(c));
            return 1;
        }
    }
    if (nr == 215 || nr == 226 || nr == 227) {
        pthread_mutex_lock(&g_bound_mapping_gate);
        pthread_mutex_lock(&g_bound_mapping_lock);
        bound_mapping *mapping = bound_mapping_find(a0, a1);
        if (mapping != NULL) {
            if (mapping->object->handle == HL_HOST_HANDLE_INVALID) {
                pthread_mutex_unlock(&g_bound_mapping_lock);
                pthread_mutex_unlock(&g_bound_mapping_gate);
                return 0;
            }
            uint64_t offset = a0 - mapping->address;
            hl_host_result operation;
            /* Guest mprotect is modeled by the 4 KiB Linux VMA/SMC registries in svc_mem. Routing a
             * typed file mapping to host protect applies macOS's 16 KiB granularity and can protect
             * adjacent ELF segments, breaking ld.so RELRO. Keep the typed mapping ledger, but let the
             * common guest-logical path validate the range and update permissions. */
            if (nr == 226) {
                pthread_mutex_unlock(&g_bound_mapping_lock);
                pthread_mutex_unlock(&g_bound_mapping_gate);
                return 0;
            }
            if (nr == 227 && (((a2 & ~(uint64_t)7u) != 0) || (a2 & 5u) == 0 || (a2 & 5u) == 5u)) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                pthread_mutex_unlock(&g_bound_mapping_lock);
                pthread_mutex_unlock(&g_bound_mapping_gate);
                return 1;
            }
            uint64_t operation_size = a1;
            if (nr == 215 && offset == 0 && a1 == hl_gmap_find_guest_length(a0)) operation_size = mapping->size;
            if (nr == 215)
                operation = g_host_services->memory->unmap_range(g_host_services->context, mapping->object->handle,
                                                                 mapping->object_offset + offset, operation_size);
            else
                operation = g_host_services->memory->sync(g_host_services->context, mapping->object->handle,
                                                          mapping->object_offset + offset, a1);
            if (operation.status == HL_STATUS_OK && nr == 215) {
                hl_host_handle retired = mapping->object->handle;
                mapping->object->handle = HL_HOST_HANDLE_INVALID;
                (void)g_host_services->memory->discard(g_host_services->context, retired);
                bound_mapping_retire(a0, operation_size);
                hl_gmap_unmap_range(a0, a0 + operation_size);
                gbus_clear(a0, a0 + operation_size);
            }
            G_RET(c) = (uint64_t)bound_host_error(operation.status);
            pthread_mutex_unlock(&g_bound_mapping_lock);
            pthread_mutex_unlock(&g_bound_mapping_gate);
            return 1;
        }
        pthread_mutex_unlock(&g_bound_mapping_lock);
        pthread_mutex_unlock(&g_bound_mapping_gate);
    }
    if (nr == 71) {
        hl_linux_fd_snapshot second;
        int second_bound = !g_bound_second_native && bound_snapshot(a1, &second);
        if (source_bound || second_bound) {
            G_RET(c) = (uint64_t)bound_sendfile(source_bound ? &source : NULL, (int)a0, second_bound ? &second : NULL,
                                                (int)a1, a2, a3);
            return 1;
        }
    }
    if (nr == 76) {
        hl_linux_fd_snapshot second;
        int second_bound = bound_snapshot(a2, &second);
        if (source_bound || second_bound) {
            G_RET(c) = (uint64_t)bound_splice(source_bound ? &source : NULL, (int)a0, a1, second_bound ? &second : NULL,
                                              (int)a2, a3, G_A4(c), G_A5(c));
            return 1;
        }
    }
    if ((nr == 75 || nr == 77) && source_bound) {
        /* vmsplice and tee require pipe endpoints. Typed descriptors currently name ordinary files. */
        G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
        return 1;
    }
    if (nr == 77) {
        hl_linux_fd_snapshot second;
        if (bound_snapshot(a1, &second)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            return 1;
        }
    }
    if (nr == 36) {
        hl_linux_fd_snapshot directory;
        if (bound_snapshot(a1, &directory)) {
            char target[HL_LINUX_PATH_MAX + 1], path[HL_LINUX_PATH_MAX + 1];
            size_t target_size, path_size;
            result = bound_path_copy(a0, target, &target_size);
            if (result == 0) result = bound_path_copy(a2, path, &path_size);
            if (result == 0 && path[0] == '/') return 0;
            if (result == 0 && (!bound_file_abi14() || g_host_services->file->make_symlink == NULL)) result = -ENOSYS;
            if (result == 0)
                result = bound_host_error(g_host_services->file
                                              ->make_symlink(g_host_services->context, target, target_size,
                                                             directory.host_handle, path, path_size)
                                              .status);
            if (result == 0) bound_evict_relative(directory.host_handle, path);
            G_RET(c) = (uint64_t)result;
            return 1;
        }
    }
    if (nr == 37 || nr == 38 || nr == 276) {
        hl_linux_fd_snapshot destination;
        int destination_bound = bound_snapshot(a2, &destination);
        if (source_bound || destination_bound) {
            char old_path[HL_LINUX_PATH_MAX + 1], new_path[HL_LINUX_PATH_MAX + 1];
            size_t old_size, new_size;
            result = bound_path_copy(a1, old_path, &old_size);
            if (result == 0) result = bound_path_copy(a3, new_path, &new_size);
            if (result == 0 && source_bound && old_path[0] != '/') {
                int error = bound_handle_dirfd_error((int)a0);
                if (error != -EACCES) result = error;
            }
            if (result == 0 && destination_bound && new_path[0] != '/') {
                int error = bound_handle_dirfd_error((int)a2);
                if (error != -EACCES) result = error;
            }
            if (result != 0) {
                G_RET(c) = (uint64_t)result;
                return 1;
            }
            /* Absolute operands ignore their corresponding dirfd.  Let the
               ordinary jailed path resolver handle mixed bound/native fds in
               that case instead of rejecting a descriptor Linux never reads. */
            if (old_path[0] == '/' || new_path[0] == '/') return 0;
            if (!source_bound || !destination_bound) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOSYS);
                return 1;
            }
            if (nr == 37) {
                uint64_t flags = G_A4(c);
                if ((flags & ~UINT64_C(0x400)) != 0)
                    result = -EINVAL;
                else if (!bound_file_abi14() || g_host_services->file->make_link == NULL)
                    result = -ENOSYS;
                else if (result == 0)
                    result = bound_host_error(g_host_services->file
                                                  ->make_link(g_host_services->context, source.host_handle, old_path,
                                                              old_size, destination.host_handle, new_path, new_size,
                                                              (flags & UINT64_C(0x400)) != 0 ? 1u : 0u)
                                                  .status);
            } else {
                uint64_t flags = nr == 276 ? G_A4(c) : 0;
                /* RENAME_NOREPLACE (0x1) and RENAME_EXCHANGE (0x2) are honored;
                   they are mutually exclusive and no other flag is supported. */
                if ((flags & ~UINT64_C(0x3)) != 0 || (flags & UINT64_C(0x3)) == UINT64_C(0x3))
                    result = -EINVAL;
                else if (g_host_services->file->rename_relative == NULL)
                    result = -ENOSYS;
                else if (result != 0) {
                    /* preserve earlier path-copy error */
                } else if ((flags & UINT64_C(0x2)) != 0) {
                    result = (int)bound_rename_exchange(source.host_handle, old_path, old_size, destination.host_handle,
                                                        new_path, new_size);
                } else if ((flags & UINT64_C(0x1)) != 0) {
                    hl_host_result probe = g_host_services->file->open_relative(
                        g_host_services->context, destination.host_handle, new_path, new_size,
                        HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0);
                    if (probe.status == HL_STATUS_OK) {
                        (void)g_host_services->file->close(g_host_services->context, probe.value);
                        result = -EEXIST;
                    } else if (probe.status != HL_STATUS_NOT_FOUND) {
                        result = (int)bound_host_error(probe.status);
                    } else {
                        result = (int)bound_host_error(
                            g_host_services->file
                                ->rename_relative(g_host_services->context, source.host_handle, old_path, old_size,
                                                  destination.host_handle, new_path, new_size)
                                .status);
                    }
                } else {
                    result =
                        bound_host_error(g_host_services->file
                                             ->rename_relative(g_host_services->context, source.host_handle, old_path,
                                                               old_size, destination.host_handle, new_path, new_size)
                                             .status);
                }
            }
            if (result == 0) {
                bound_evict_relative(source.host_handle, old_path);
                bound_evict_relative(destination.host_handle, new_path);
            }
            G_RET(c) = (uint64_t)result;
            return 1;
        }
    }
    if (nr == 24 && !source_bound) {
        hl_linux_fd_snapshot target;
        if (bound_snapshot(a1, &target)) {
            unsigned flags = (unsigned)a2;
            int is_dup2 = G_IS_DUP2_COMPAT();
            int source_fd = (int)a0;
            int target_fd = (int)a1;
            if (source_fd == target_fd) {
                G_RET(c) = is_dup2 ? (uint64_t)(unsigned)target_fd : (uint64_t)(int64_t)-EINVAL;
                return 1;
            }
            if ((!is_dup2 && (flags & ~HL_LINUX_O_CLOEXEC) != 0) || target_fd < 0 || target_fd >= guest_nofile_cur()) {
                G_RET(c) = (uint64_t)(int64_t)(target_fd < 0 || target_fd >= guest_nofile_cur() ? -EBADF : -EINVAL);
                return 1;
            }
            /* Validate the native source before displacing the typed target: dup2 must leave target
             * untouched when oldfd is invalid.  Once validated, retire the opaque target and let the
             * common native dup path install oldfd at the exact guest number. */
            if (source_fd < 0 || fcntl(source_fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)-EBADF;
                return 1;
            }
            flock_broker_detach(&target);
            (void)hl_linux_close(g_linux_box, target.fd);
            proc_fdvis_close(target_fd);
            (void)close(target_fd);
            return 0;
        }
    }
    if (nr == 21 && !source_bound) {
        hl_linux_fd_snapshot watched;
        if (bound_snapshot(a2, &watched)) {
            int64_t epoll_result = -ENOSYS;
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && (int)a2 >= 0 && (int)a2 < HL_NFD &&
                hl_provider_files_is_handle(watched.host_handle)) {
                int registry_ep = epoll_slot((int)a0);
                uint32_t epoll_generation = g_ep_provider_generations[registry_ep];
                ep_provider_watch *watch = ep_provider_find(g_ep_provider_watches, EP_PROVIDER_WATCH_LIMIT, registry_ep,
                                                            epoll_generation, (int)a2, watched.descriptor_generation);
                if (a1 == HL_LINUX_EPOLL_DELETE) {
                    if (watch == NULL)
                        epoll_result = -ENOENT;
                    else {
                        ep_provider_retire(watch);
                        epoll_result = 0;
                    }
                } else if ((a1 == HL_LINUX_EPOLL_ADD || a1 == HL_LINUX_EPOLL_MODIFY) && a3 != 0) {
                    uint8_t encoded[G_EPEV_DOFF + sizeof(uint64_t)];
                    uint32_t events = 0;
                    uint64_t data = 0;
                    if (guest_copy_from(encoded, a3, sizeof(encoded)) != (ssize_t)sizeof(encoded)) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        return 1;
                    }
                    memcpy(&events, encoded, sizeof(events));
                    memcpy(&data, encoded + G_EPEV_DOFF, sizeof(data));
                    if (a1 == HL_LINUX_EPOLL_ADD && watch != NULL)
                        epoll_result = -EEXIST;
                    else if (a1 == HL_LINUX_EPOLL_MODIFY && watch == NULL)
                        epoll_result = -ENOENT;
                    else {
                        ep_provider_watch *previous = watch;
                        ep_provider_watch *replacement =
                            ep_provider_alloc(g_ep_provider_watches, EP_PROVIDER_WATCH_LIMIT);
                        if (replacement == NULL) {
                            G_RET(c) = (uint64_t)(int64_t)-ENOSPC;
                            return 1;
                        }
                        uint32_t serial = g_ep_provider_serial = ep_provider_next(g_ep_provider_serial);
                        uint32_t interests =
                            ((events & 1u) ? HL_LINUX_READY_READ : 0u) | ((events & 4u) ? HL_LINUX_READY_WRITE : 0u);
                        ep_provider_activate(replacement, registry_ep, epoll_generation, (int)a2,
                                             watched.descriptor_generation, serial, watched.host_handle, events,
                                             interests, data);
                        ep_wake_arm((int)a0);
                        epoll_result = hl_provider_files_subscribe(replacement->handle, replacement->interests,
                                                                   bound_epoll_provider_ready, replacement,
                                                                   atomic_load(&replacement->serial)) == 0
                                           ? 0
                                           : -EIO;
                        if (epoll_result != 0)
                            ep_provider_retire(replacement);
                        else if (previous != NULL)
                            ep_provider_retire(previous);
                    }
                } else
                    epoll_result = -EINVAL;
                G_RET(c) = (uint64_t)epoll_result;
                return 1;
            }
            /* Typed box objects (an inotify watch is the canonical case) own host
               observation and expose readiness only through their object adapter,
               never as a host descriptor -- watched.host_handle is INVALID.  Route
               them through the object-sampling epoll registry, the same way
               poll()/select() observe these objects (hl_linux_object_poll). */
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && (int)a2 >= 0 && (int)a2 < HL_NFD) {
                hl_linux_object_pin pin;
                int object_epollable = 0;
                if (hl_linux_object_pin_fd(g_linux_box, (hl_linux_fd)a2, &pin) == HL_STATUS_OK) {
                    object_epollable = pin.ops != NULL && pin.ops->readiness != NULL;
                    hl_linux_object_unpin(&pin);
                }
                if (object_epollable) {
                    int registry_ep = epoll_slot((int)a0);
                    uint32_t epoll_generation = g_ep_provider_generations[registry_ep];
                    ep_object_watch *watch =
                        ep_object_find(registry_ep, epoll_generation, (int)a2, watched.descriptor_generation);
                    if (a1 == HL_LINUX_EPOLL_DELETE) {
                        if (watch == NULL)
                            epoll_result = -ENOENT;
                        else {
                            ep_object_free(watch);
                            epoll_result = 0;
                        }
                    } else if ((a1 == HL_LINUX_EPOLL_ADD || a1 == HL_LINUX_EPOLL_MODIFY) && a3 != 0) {
                        uint8_t encoded[G_EPEV_DOFF + sizeof(uint64_t)];
                        uint32_t events = 0;
                        uint64_t data = 0;
                        if (guest_copy_from(encoded, a3, sizeof(encoded)) != (ssize_t)sizeof(encoded)) {
                            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                            return 1;
                        }
                        memcpy(&events, encoded, sizeof(events));
                        memcpy(&data, encoded + G_EPEV_DOFF, sizeof(data));
                        if (a1 == HL_LINUX_EPOLL_ADD && watch != NULL)
                            epoll_result = -EEXIST;
                        else if (a1 == HL_LINUX_EPOLL_MODIFY && watch == NULL)
                            epoll_result = -ENOENT;
                        else {
                            uint32_t interests = ((events & 0x1u) ? HL_LINUX_READY_READ : 0u) |
                                                 ((events & 0x4u) ? HL_LINUX_READY_WRITE : 0u);
                            if (watch == NULL) {
                                watch = ep_object_alloc();
                                if (watch == NULL) {
                                    G_RET(c) = (uint64_t)(int64_t)-ENOSPC;
                                    return 1;
                                }
                                watch->epoll = registry_ep;
                                watch->epoll_generation = epoll_generation;
                                watch->descriptor = (int)a2;
                                watch->descriptor_generation = watched.descriptor_generation;
                                g_ep_object_count[registry_ep]++;
                            }
                            watch->events = events;
                            watch->interests = interests;
                            watch->data = data;
                            ep_wake_arm((int)a0);
                            epoll_result = 0;
                        }
                    } else
                        epoll_result = -EINVAL;
                    G_RET(c) = (uint64_t)epoll_result;
                    return 1;
                }
            }
            if (a1 == HL_LINUX_EPOLL_ADD && g_host_services != NULL && g_host_services->file != NULL &&
                g_host_services->file->metadata != NULL) {
                hl_host_file_metadata metadata;
                hl_host_result status =
                    g_host_services->file->metadata(g_host_services->context, watched.host_handle, &metadata);
                if (status.status == HL_STATUS_OK &&
                    (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY))
                    epoll_result = -EPERM;
            }
            G_RET(c) = (uint64_t)epoll_result;
            return 1;
        }
    }
    if (!source_bound) return 0;
    switch (nr) {
    case 7:    /* fsetxattr */
    case 10:   /* fgetxattr */
    case 13:   /* flistxattr */
    case 16: { /* fremovexattr */
        char path[HL_LINUX_PATH_MAX + 1];
        hl_host_result named;
        if (g_host_services->file->path == NULL) {
            result = -ENOSYS;
            break;
        }
        named = g_host_services->file->path(g_host_services->context, source.host_handle,
                                            (hl_host_bytes){path, HL_LINUX_PATH_MAX});
        if (named.status != HL_STATUS_OK || named.value > HL_LINUX_PATH_MAX) {
            result = bound_host_error(named.status);
            break;
        }
        path[named.value] = 0;
        if (nr == 7)
            result = guest_xattr_set(path, (const char *)a1, (const void *)a2, (size_t)a3, a4, 0);
        else if (nr == 10)
            result = guest_xattr_get(path, (const char *)a1, (void *)a2, (size_t)a3, 0);
        else if (nr == 13)
            result = guest_xattr_list(path, (char *)a1, (size_t)a2, 0);
        else
            result = guest_xattr_remove(path, (const char *)a1, 0);
        if (result < 0) result = -hl_linux_errno_from_macos((int)-result);
        break;
    }
    case 33: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        if (((mode_t)a2 & S_IFMT) != S_IFIFO || a3 != 0) return 0;
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if (g_host_services->file->abi != HL_HOST_FILE_ABI ||
            g_host_services->file->size < sizeof(*g_host_services->file) || g_host_services->file->make_fifo == NULL) {
            result = -ENOSYS;
            break;
        }
        result = bound_host_error(
            g_host_services->file
                ->make_fifo(g_host_services->context, source.host_handle, path, path_size, (uint32_t)a2 & 07777u)
                .status);
        if (result == 0) bound_evict_relative(source.host_handle, path);
        break;
    }
    case 34: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if (!bound_file_abi14() || g_host_services->file->make_directory == NULL) {
            result = -ENOSYS;
            break;
        }
        result = bound_host_error(
            g_host_services->file
                ->make_directory(g_host_services->context, source.host_handle, path, path_size, (uint32_t)a2 & 07777u)
                .status);
        if (result == 0) bound_evict_relative(source.host_handle, path);
        break;
    }
    case 35: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        if ((a2 & ~UINT64_C(0x200)) != 0) {
            result = -EINVAL;
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if ((a2 & UINT64_C(0x200)) != 0 && g_host_services->file->remove_directory == NULL) {
            result = -ENOSYS;
            break;
        }
        if ((a2 & UINT64_C(0x200)) != 0) {
            result = bound_host_error(
                g_host_services->file->remove_directory(g_host_services->context, source.host_handle, path, path_size)
                    .status);
        } else if (g_host_services->file->unlink_relative == NULL) {
            result = -ENOSYS;
        } else {
            result = bound_host_error(
                g_host_services->file->unlink_relative(g_host_services->context, source.host_handle, path, path_size)
                    .status);
        }
        if (result == 0) bound_evict_relative(source.host_handle, path);
        break;
    }
    case 53:
    case 452: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        uint64_t flags = nr == 452 ? a3 : 0;
        if ((flags & ~UINT64_C(0x1100)) != 0) {
            result = -EINVAL;
            break;
        }
        char first_path_byte;
        if (nr == 452 && (flags & UINT64_C(0x1000)) != 0 && a1 != 0 && guest_copy_from(&first_path_byte, a1, 1) == 1 &&
            first_path_byte == '\0') {
            if (g_host_services->file->set_permissions == NULL) {
                result = -ENOSYS;
                break;
            }
            result = bound_host_error(
                g_host_services->file
                    ->set_permissions(g_host_services->context, source.host_handle, (uint32_t)a2 & 07777u)
                    .status);
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if (g_host_services->file->open_relative == NULL || g_host_services->file->set_permissions == NULL) {
            result = -ENOSYS;
            break;
        }
        uint32_t access = HL_HOST_FILE_PATH_ONLY;
        if ((flags & UINT64_C(0x100)) != 0) access |= HL_HOST_FILE_NOFOLLOW;
        hl_host_result opened = g_host_services->file->open_relative(g_host_services->context, source.host_handle, path,
                                                                     path_size, access, 0, 0);
        if (opened.status != HL_STATUS_OK) {
            result = bound_host_error(opened.status);
            break;
        }
        result = bound_host_error(
            g_host_services->file->set_permissions(g_host_services->context, opened.value, (uint32_t)a2 & 07777u)
                .status);
        (void)g_host_services->file->close(g_host_services->context, opened.value);
        break;
    }
    case 48:
    case 439: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        uint64_t flags = nr == 439 ? a3 : 0;
        if (a2 > 7 || (flags & ~UINT64_C(0x1300)) != 0) {
            result = -EINVAL;
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        uint32_t access = a2 == 0 ? HL_HOST_FILE_PATH_ONLY : 0;
        if ((a2 & 4u) != 0) access |= HL_HOST_FILE_READ;
        if ((a2 & 2u) != 0) access |= HL_HOST_FILE_WRITE;
        if ((a2 & 1u) != 0) access |= HL_HOST_FILE_PATH_ONLY;
        /* AT_SYMLINK_NOFOLLOW checks the link itself instead of its target. */
        if ((flags & UINT64_C(0x100)) != 0) access |= HL_HOST_FILE_NOFOLLOW;
        hl_host_result opened = g_host_services->file->open_relative(g_host_services->context, source.host_handle, path,
                                                                     path_size, access, 0, 0);
        result = bound_host_error(opened.status);
        if (opened.status == HL_STATUS_OK) {
            if ((a2 & 1u) != 0) {
                hl_host_file_metadata metadata;
                hl_host_result measured =
                    g_host_services->file->metadata(g_host_services->context, opened.value, &metadata);
                if (measured.status != HL_STATUS_OK)
                    result = bound_host_error(measured.status);
                else if (metadata.type != HL_HOST_FILE_TYPE_DIRECTORY && (metadata.permissions & 0111u) == 0)
                    result = -EACCES;
            }
            (void)g_host_services->file->close(g_host_services->context, opened.value);
        }
        break;
    }
    case 79: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        if ((a3 & ~UINT64_C(0x1900)) != 0) {
            result = -EINVAL;
            break;
        }
        if (guest_accessible_prefix(a2, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) != GUEST_LINUX_STAT_BYTES) {
            result = -EFAULT;
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        int empty = result == -HL_LINUX_ENOENT && (a3 & UINT64_C(0x1000)) != 0;
        if (result != 0 && !empty) break;
        if (!empty && path[0] == '/') return 0;
        if (!empty) {
            char backing[HL_LINUX_PATH_MAX + 1];
            if (bound_handle_host_path(source.host_handle, backing, sizeof backing) == 0 &&
                strstr(backing, "/.hl-proc-fd") != NULL) {
                char synthetic[HL_LINUX_PATH_MAX + 1];
                struct stat status;
                int written = snprintf(synthetic, sizeof synthetic, "/proc/self/fd/%s", path);
                int measured = written > 0 && (size_t)written < sizeof synthetic
                                   ? ((a3 & UINT64_C(0x100)) != 0 ? synth_stat_raw(synthetic, &status)
                                                                  : procfd_follow_stat(synthetic, &status))
                                   : 0;
                if (measured <= 0) {
                    result = -ENOENT;
                } else {
                    result = guest_fill_linux_stat(a2, &status, NULL, -1);
                }
                break;
            }
        }
        hl_host_handle target = source.host_handle;
        int close_target = 0;
        if (!empty) {
            uint32_t access = HL_HOST_FILE_PATH_ONLY;
            if ((a3 & UINT64_C(0x100)) != 0) access |= HL_HOST_FILE_NOFOLLOW;
            hl_host_result opened = g_host_services->file->open_relative(g_host_services->context, source.host_handle,
                                                                         path, path_size, access, 0, 0);
            if (opened.status != HL_STATUS_OK) {
                result = bound_host_error(opened.status);
                break;
            }
            target = opened.value;
            close_target = 1;
        }
        hl_host_file_metadata metadata;
        hl_host_result measured = g_host_services->file->metadata(g_host_services->context, target, &metadata);
        if (measured.status != HL_STATUS_OK) {
            result = bound_host_error(measured.status);
        } else {
            hl_linux_file_status status;
            hl_linux_fd_snapshot target_snapshot = {.host_handle = target};
            bound_status_from_metadata(&status, &metadata);
            bound_virtualize_owner(&target_snapshot, &status);
            uint8_t encoded[GUEST_LINUX_STAT_BYTES];
            fill_linux_bound_stat(encoded, &status);
            result = guest_copy_to(a2, encoded, sizeof encoded) == sizeof encoded ? 0 : -EFAULT;
        }
        if (close_target) (void)g_host_services->file->close(g_host_services->context, target);
        break;
    }
    case 78: {
        char path[HL_LINUX_PATH_MAX + 1];
        char output[HL_LINUX_PATH_MAX];
        size_t path_size;
        size_t capacity = a3 < sizeof output ? (size_t)a3 : sizeof output;
        if (a3 == 0 || a3 > SIZE_MAX || guest_accessible_prefix(a2, capacity, HL_LOGICAL_VMA_WRITE) != capacity) {
            result = a3 == 0 ? -EINVAL : -EFAULT;
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        hl_host_result opened =
            g_host_services->file->open_relative(g_host_services->context, source.host_handle, path, path_size,
                                                 HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0);
        if (opened.status != HL_STATUS_OK) {
            result = bound_host_error(opened.status);
            break;
        }
        hl_host_result read =
            g_host_services->file->readlink(g_host_services->context, opened.value, (hl_host_bytes){output, capacity});
        result = read.status == HL_STATUS_OK ? (int64_t)read.value : bound_host_error(read.status);
        if (result > 0 && guest_copy_to(a2, output, (size_t)result) != result) result = -EFAULT;
        (void)g_host_services->file->close(g_host_services->context, opened.value);
        break;
    }
    case 56: {
        struct fdvis_reservation fdvis;
        const uint32_t supported = HL_LINUX_O_ACCMODE | HL_LINUX_O_CREAT | HL_LINUX_O_EXCL | HL_LINUX_O_TRUNC |
                                   HL_LINUX_O_APPEND | HL_LINUX_O_NONBLOCK | HL_LINUX_O_NOFOLLOW |
                                   HL_LINUX_O_DIRECTORY | HL_LINUX_O_PATH | HL_LINUX_O_CLOEXEC;
        uint32_t flags = typed_open_flags(a2);
        size_t path_size;
        char path[HL_LINUX_PATH_MAX + 1];
        int shadow;
        hl_linux_fd_reservation reservation;
        hl_status status;
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if ((flags & ~supported) != 0) {
            result = -HL_LINUX_EINVAL;
            break;
        }
        shadow = bound_shadow_reserve(0);
        if (shadow < 0) {
            result = -(int64_t)errno;
            break;
        }
        if (shadow >= guest_nofile_cur()) {
            close(shadow);
            result = -HL_LINUX_EMFILE;
            break;
        }
        if (proc_fdvis_reserve(&fdvis) != 0) {
            close(shadow);
            result = -HL_LINUX_ENOSPC;
            break;
        }
        for (;;) {
            status = hl_linux_fd_reserve_at(g_linux_box, (hl_linux_fd)shadow, &reservation);
            if (status != HL_STATUS_ALREADY_EXISTS) break;
            close(shadow);
            shadow = bound_shadow_reserve(shadow + 1);
            if (shadow < 0 || shadow >= guest_nofile_cur()) break;
        }
        if (status != HL_STATUS_OK || shadow < 0 || shadow >= guest_nofile_cur()) {
            if (shadow >= 0) close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
            result = -HL_LINUX_EMFILE;
            break;
        }
        result = hl_linux_openat_reserved(g_linux_box, &reservation, (int32_t)source.fd, path, path_size, flags,
                                          (uint32_t)a3);
        HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "openat-bound path=%s flags=%#x result=%lld", path, flags,
                (long long)result);
        if (result < 0) {
            (void)hl_linux_fd_cancel(g_linux_box, &reservation);
            close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
        } else {
            hl_linux_fd_snapshot opened;
            hl_host_file_metadata metadata = {0};
            uint32_t kind = HL_HOST_FD_OTHER;
            if (bound_snapshot((uint64_t)result, &opened) && g_host_services != NULL && g_host_services->file != NULL &&
                g_host_services->file->metadata != NULL &&
                g_host_services->file->metadata(g_host_services->context, opened.host_handle, &metadata).status ==
                    HL_STATUS_OK) {
                if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ||
                    metadata.type == HL_HOST_FILE_TYPE_SYMLINK)
                    kind = HL_HOST_FD_FILE;
                else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
                    kind = HL_HOST_FD_PIPE;
                else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
                    kind = HL_HOST_FD_SOCKET;
            }
            proc_fdvis_reservation_publish(&fdvis, (int)result, kind, metadata.stable_device, metadata.stable_object);
        }
        break;
    }
    case 57: /* close */
        ep_provider_retire_endpoint((int)source.fd);
        ep_object_retire_endpoint((int)source.fd);
        flock_broker_detach(&source);
        if (g_host_services != NULL && g_host_services->file != NULL && g_host_services->file->metadata != NULL) {
            hl_host_file_metadata metadata;
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            if (status.status == HL_STATUS_OK && metadata.type == HL_HOST_FILE_TYPE_REGULAR)
                flock_on_close_identity((int)source.fd, metadata.stable_device, metadata.stable_object);
            if (status.status == HL_STATUS_OK && metadata.type == HL_HOST_FILE_TYPE_REGULAR)
                poslk_on_close_identity(metadata.stable_device, metadata.stable_object);
        }
        result = hl_linux_close(g_linux_box, source.fd);
        proc_fdvis_close((int)source.fd);
        (void)close((int)source.fd);
        break;
    case 62: result = hl_linux_lseek(g_linux_box, source.fd, (int64_t)a1, (int32_t)a2); break;
    case 63: result = bound_guest_read(&source, a1, (size_t)a2, 0, 0); break;
    case 64: {
        int64_t allowed = bound_fsize_gate(c, &source, source.offset, a2); // RLIMIT_FSIZE -> SIGXFSZ/EFBIG
        result = allowed < 0 ? allowed : bound_guest_write(&source, a1, (size_t)allowed, 0, 0);
    } break;
    case 67: result = bound_guest_read(&source, a1, (size_t)a2, a3, 1); break;
    case 68:
        if (source.status_flags & HL_LINUX_O_APPEND) {
            // Linux quirk: pwrite() on an O_APPEND fd IGNORES the supplied offset and appends at EOF (the
            // append is atomic, driven by the file's O_APPEND status flag, not the position argument). The
            // typed path honored a3 and overwrote, so route an O_APPEND pwrite through the appending write.
            int64_t allowed = bound_fsize_gate(c, &source, source.offset, a2);
            result = allowed < 0 ? allowed : bound_guest_write(&source, a1, (size_t)allowed, 0, 0);
            if (result > 0) bound_mapping_file_written(&source, source.offset, (uint64_t)result);
        } else {
            int64_t allowed = bound_fsize_gate(c, &source, a3, a2); // RLIMIT_FSIZE at the explicit pwrite offset
            result = allowed < 0 ? allowed : bound_guest_write(&source, a1, (size_t)allowed, a3, 1);
            if (result > 0) bound_mapping_file_written(&source, a3, (uint64_t)result);
        }
        break;
    case 65:
    case 66:
    case 69:
    case 70: {
        static _Thread_local hl_host_iovec vectors[HL_LINUX_IOV_MAX];
        result = bound_vectors_copy(a1, a2, vectors);
        if (result != 0) {
            // do_readv/do_writev test the access mode at fdget_pos, ahead of import_iovec.
            if (result == -HL_LINUX_EFAULT && bound_access_rejects(&source, nr == 65 || nr == 69)) result = -EBADF;
            break;
        }
        result = bound_vector_io(&source, vectors, (uint32_t)a2, nr == 65 || nr == 69, nr == 69 || nr == 70, a3);
        break;
    }
    case 213: {
        hl_host_file_metadata metadata;
        if ((int64_t)a1 < 0)
            result = -EINVAL;
        else if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->metadata == NULL)
            result = -95; /* Linux EOPNOTSUPP; this route bypasses the native-to-Linux errno mapper. */
        else {
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            result = status.status != HL_STATUS_OK                               ? bound_host_error(status.status)
                     : metadata.type != HL_HOST_FILE_TYPE_REGULAR                ? -EINVAL
                     : hl_linux_pread64(g_linux_box, source.fd, NULL, 0, a1) < 0 ? -EBADF
                                                                                 : 0;
        }
        break;
    }
    case 286:
    case 287: {
        static _Thread_local hl_host_iovec vectors[HL_LINUX_IOV_MAX];
#ifdef CANON_X86ONLY
        uint64_t vector_offset = a3; /* x86-64 passes the complete 64-bit offset in argument 4. */
#else
        uint64_t vector_offset =
            (uint64_t)(uint32_t)a3 | ((uint64_t)(uint32_t)G_A4(c) << 32); /* AArch64 split offset. */
#endif
        result = bound_vectors_copy(a1, a2, vectors);
        if (result != 0) {
            if (result == -HL_LINUX_EFAULT && bound_access_rejects(&source, nr == 286)) result = -EBADF;
            break;
        }
        /* Flags are semantic requirements, not hints. Do not silently erase RWF_NOWAIT/APPEND/SYNC. */
        // RWF_APPEND is a semantic requirement: pwritev2 must ignore the supplied offset and land the
        // write at end-of-file, without moving the file position. The typed box takes no flags, so
        // resolve end-of-file from the file's own metadata and issue a positioned write there.
        uint64_t vector_flags = G_A5(c);
        if (nr == 287 && (vector_flags & 0x10u) != 0 && g_host_services != NULL && g_host_services->file != NULL &&
            g_host_services->file->metadata != NULL) {
            hl_host_file_metadata metadata;
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            if (status.status != HL_STATUS_OK) {
                result = bound_host_error(status.status);
                break;
            }
            vector_offset = metadata.size;
            vector_flags &= ~UINT64_C(0x10);
        }
        if (vector_flags != 0) {
            result = -95; /* Linux EOPNOTSUPP; macOS's native value is 102. */
            break;
        }
        result = bound_vector_io(&source, vectors, (uint32_t)a2, nr == 286, vector_offset != UINT64_MAX, vector_offset);
        if (nr == 287 && result > 0)
            bound_mapping_file_written(&source, vector_offset == UINT64_MAX ? source.offset : vector_offset,
                                       (uint64_t)result);
        break;
    }
    case 46: {
        // RLIMIT_FSIZE: Linux (do_sys_ftruncate) raises SIGXFSZ and returns -EFBIG when the target length
        // exceeds the soft file-size limit, before touching the filesystem. The bound-descriptor path used
        // to skip this (only the bound write path enforced it), so an ftruncate grow past the limit on a
        // /tmp-backed fd silently succeeded. No-op for an infinite limit (the common case).
        {
            uint64_t fslim = guest_fsize_cur();
            if (fslim != ~UINT64_C(0) && a1 > fslim) {
                raise_guest_signal(c, 25); // SIGXFSZ
                result = -EFBIG;
                break;
            }
        }
        hl_host_file_metadata metadata = {0};
        int have_metadata = 0, prepared = 0;
        if (g_host_services != NULL && g_host_services->file != NULL && g_host_services->file->metadata != NULL) {
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            have_metadata = status.status == HL_STATUS_OK;
        }
        if (have_metadata && a1 < metadata.size) {
            gbus_prepare();
            prepared = 1;
        }
        result = hl_linux_ftruncate(g_linux_box, source.fd, a1);
        if (result == 0 && have_metadata) {
            /* The local truncate is authoritative. Publish its generation
             * before releasing the BUS transition so the host watcher drops
             * the matching notification instead of replaying the shrink. */
            bound_watch_publish_size(metadata.stable_device, metadata.stable_object, a1);
            pthread_mutex_lock(&g_bound_mapping_gate);
            pthread_mutex_lock(&g_bound_mapping_lock);
            bound_mapping_file_size_changed(&source, &metadata, 1, metadata.size, a1, NULL);
            pthread_mutex_unlock(&g_bound_mapping_lock);
            pthread_mutex_unlock(&g_bound_mapping_gate);
            hl_linux_file_event_publish(HL_LINUX_FILE_EVENT_RESIZE, metadata.stable_device, metadata.stable_object,
                                        metadata.size, a1);
        }
        if (prepared) { gbus_prepare_release(); }
        break;
    }
    case 82: /* fsync */
    case 83: /* fdatasync */
        /* An O_PATH descriptor names a file but is not open for I/O; Linux
           rejects the sync family through it with EBADF (fs/sync.c). */
        result = (source.status_flags & HL_LINUX_O_PATH) != 0
                     ? -EBADF
                     : (nr == 82 ? hl_linux_fsync(g_linux_box, source.fd) : hl_linux_fdatasync(g_linux_box, source.fd));
        break;
    case 84:
        if ((G_A3(c) & ~(uint64_t)7u) != 0)
            result = -EINVAL;
        else
            result = hl_linux_sync_range(g_linux_box, source.fd, a1, a2, (uint32_t)G_A3(c));
        break;
    case 267: result = hl_linux_sync_filesystem(g_linux_box, source.fd); break;
    case 80: {
        hl_linux_file_status status;
        result = hl_linux_fstat(g_linux_box, source.fd, &status);
        if (result == 0 &&
            guest_accessible_prefix(a1, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) != GUEST_LINUX_STAT_BYTES)
            result = -EFAULT;
        if (result == 0) bound_virtualize_namespace(source.fd, &status);
        if (result == 0) bound_virtualize_owner(&source, &status);
        if (result == 0) {
            uint8_t encoded[GUEST_LINUX_STAT_BYTES];
            fill_linux_bound_stat(encoded, &status);
            if (guest_copy_to(a1, encoded, sizeof encoded) != sizeof encoded) result = -EFAULT;
        }
        break;
    }
    case 291: {
        uint64_t flags = a2;
        uint64_t mask = a3;
        uint64_t output = G_A4(c);
        char path_byte;
        if ((flags & ~UINT64_C(0x7900)) != 0 || (flags & UINT64_C(0x6000)) == UINT64_C(0x6000) ||
            (mask & UINT64_C(0x80000000)) != 0) {
            result = -EINVAL;
            break;
        }
        if (a1 == 0 || guest_copy_from(&path_byte, a1, 1) != 1) {
            result = -EFAULT;
            break;
        }
        if (path_byte != 0 || (flags & UINT64_C(0x1000)) == 0) return 0;
        if (guest_accessible_prefix(output, 256, HL_LOGICAL_VMA_WRITE) != 256) {
            result = -EFAULT;
            break;
        }
        hl_linux_file_status status;
        result = hl_linux_fstat(g_linux_box, source.fd, &status);
        if (result == 0) {
            bound_virtualize_namespace(source.fd, &status);
            bound_virtualize_owner(&source, &status);
            uint8_t encoded[256];
            bound_fill_statx(encoded, &status);
            if (guest_copy_to(output, encoded, sizeof encoded) != sizeof encoded) result = -EFAULT;
        }
        break;
    }
    case 44: {
        hl_host_filesystem_metadata metadata;
        hl_host_result status;
        if (!bound_file_abi14() || g_host_services->file->filesystem_metadata == NULL) {
            result = -ENOSYS;
            break;
        }
        status = g_host_services->file->filesystem_metadata(g_host_services->context, source.host_handle, &metadata);
        if (status.status != HL_STATUS_OK) {
            result = bound_host_error(status.status);
            break;
        }
        if (guest_accessible_prefix(a1, 120, HL_LOGICAL_VMA_WRITE) != 120) {
            result = -EFAULT;
            break;
        }
        uint8_t encoded[120];
        bound_fill_statfs(encoded, &metadata);
        result = guest_copy_to(a1, encoded, sizeof encoded) == sizeof encoded ? 0 : -EFAULT;
        break;
    }
    case 47: {
        hl_host_file_metadata before = {0}, after = {0};
        hl_host_result status;
        uint32_t mode = (uint32_t)a1;
        int prepared = 0;
        if (a1 > UINT32_MAX || a2 > INT64_MAX || a3 == 0 || a3 > INT64_MAX) {
            result = -EINVAL;
            break;
        }
        // A mode bit outside FALLOC_FL_SUPPORTED_MASK (KEEP_SIZE|PUNCH_HOLE|COLLAPSE_RANGE|ZERO_RANGE|
        // INSERT_RANGE|UNSHARE_RANGE == 0x7b) is -EOPNOTSUPP on Linux, not -EINVAL (do_fallocate returns
        // -EOPNOTSUPP for anything it does not implement). Mirrors the native path in fs.c case 47.
        if (mode & ~0x7bu) {
            result = -95; // Linux EOPNOTSUPP; hard-coded because this result is not run through the
                          // macOS->Linux errno map (a host EOPNOTSUPP is 102 on Darwin). Cf. time.c case 115.
            break;
        }
        if (a2 > INT64_MAX - a3) {
            result = -EFBIG;
            break;
        }
        // RLIMIT_FSIZE: a fallocate that reserves past the soft file-size limit raises SIGXFSZ/-EFBIG (Linux
        // gates it even with FALLOC_FL_KEEP_SIZE). Only the size-extending modes are bounded -- PUNCH_HOLE /
        // COLLAPSE_RANGE / INSERT_RANGE never place data beyond the current end, so they are exempt.
        {
            uint64_t fslim = guest_fsize_cur();
            if (fslim != ~UINT64_C(0) && (a2 + a3) > fslim &&
                (mode & (HL_HOST_FILE_ALLOC_PUNCH_HOLE | HL_HOST_FILE_ALLOC_COLLAPSE_RANGE |
                         HL_HOST_FILE_ALLOC_INSERT_RANGE)) == 0) {
                raise_guest_signal(c, 25); // SIGXFSZ
                result = -EFBIG;
                break;
            }
        }
        if (!bound_file_abi14() || g_host_services->file->allocate_range == NULL) {
            result = -ENOSYS;
            break;
        }
        status = g_host_services->file->metadata(g_host_services->context, source.host_handle, &before);
        if (status.status != HL_STATUS_OK) {
            result = bound_host_error(status.status);
            break;
        }
        if ((mode & HL_HOST_FILE_ALLOC_COLLAPSE_RANGE) != 0) {
            gbus_prepare();
            prepared = 1;
        }
        status = g_host_services->file->allocate_range(g_host_services->context, source.host_handle, mode, a2, a3);
        result = bound_host_error(status.status);
        if (status.status == HL_STATUS_OK &&
            g_host_services->file->metadata(g_host_services->context, source.host_handle, &after).status ==
                HL_STATUS_OK) {
            bound_watch_publish_size(after.stable_device, after.stable_object, after.size);
            pthread_mutex_lock(&g_bound_mapping_gate);
            pthread_mutex_lock(&g_bound_mapping_lock);
            if (before.size != after.size)
                bound_mapping_file_size_changed(&source, &after, 1, before.size, after.size, NULL);
            pthread_mutex_unlock(&g_bound_mapping_lock);
            pthread_mutex_unlock(&g_bound_mapping_gate);
            bound_mapping_file_data_changed(&source, after.stable_device, after.stable_object);
        }
        if (prepared) gbus_prepare_release();
        break;
    }
    case 52: {
        if (g_host_services->file->set_permissions == NULL) {
            result = -ENOSYS;
            break;
        }
        hl_host_result status =
            g_host_services->file->set_permissions(g_host_services->context, source.host_handle, (uint32_t)a1 & 07777u);
        result = bound_host_error(status.status);
        if (result == 0) {
            char path[HL_LINUX_PATH_MAX + 1];
            hl_host_result named = g_host_services->file->path(g_host_services->context, source.host_handle,
                                                               (hl_host_bytes){path, HL_LINUX_PATH_MAX});
            if (named.status == HL_STATUS_OK && named.value <= HL_LINUX_PATH_MAX) {
                path[named.value] = 0;
                mode_xattr_set_path(path, (mode_t)a1);
                hl_fdcache_evict_path(path);
            }
        }
        break;
    }
    case 55: {
        char path[HL_LINUX_PATH_MAX + 1];
        hl_host_result status = g_host_services->file->path(g_host_services->context, source.host_handle,
                                                            (hl_host_bytes){path, HL_LINUX_PATH_MAX});
        if (status.status != HL_STATUS_OK || status.value > HL_LINUX_PATH_MAX) {
            result = bound_host_error(status.status);
            break;
        }
        path[status.value] = 0;
        hl_owner_set_path(path, (int)(int32_t)(uint32_t)a1, (int)(int32_t)(uint32_t)a2, 0);
        result = 0;
        break;
    }
    case 88: {
        hl_host_file_time times[2];
        struct timespec guest_times[2];
        const struct timespec *guest = a2 ? guest_times : NULL;
        char relative[HL_LINUX_PATH_MAX + 1];
        size_t relative_size = 0;
        hl_host_handle target = source.host_handle;
        int close_target = 0;
        if (a3 & ~UINT64_C(0x100)) {
            result = -EINVAL;
            break;
        }
        if (a1 != 0) {
            result = bound_path_copy(a1, relative, &relative_size);
            if (result != 0) { break; }
            /* Absolute paths ignore dirfd and remain on the common namespace route. Relative paths
             * resolve beneath the opaque directory and update the independently opened target. */
            if (relative[0] == '/') return 0;
            if (g_host_services->file->open_relative == NULL) {
                result = -ENOSYS;
                break;
            }
            uint32_t access = HL_HOST_FILE_PATH_ONLY;
            if (a3 & UINT64_C(0x100)) access |= HL_HOST_FILE_NOFOLLOW;
            hl_host_result opened = g_host_services->file->open_relative(g_host_services->context, source.host_handle,
                                                                         relative, relative_size, access, 0, 0);
            if (opened.status != HL_STATUS_OK) {
                result = bound_host_error(opened.status);
                break;
            }
            target = opened.value;
            close_target = 1;
        }
        if (g_host_services->file->set_times == NULL) {
            result = -ENOSYS;
            goto bound_set_times_done;
        }
        if (guest != NULL && guest_copy_from(guest_times, a2, sizeof guest_times) != sizeof guest_times) {
            result = -EFAULT;
            goto bound_set_times_done;
        }
        for (int index = 0; index < 2; ++index) {
            int64_t nanoseconds = guest == NULL ? INT64_C(0x3fffffff) : (int64_t)guest[index].tv_nsec;
            times[index].seconds = guest == NULL ? 0 : (int64_t)guest[index].tv_sec;
            times[index].nanoseconds = 0;
            if (nanoseconds == INT64_C(0x3fffffff))
                times[index].mode = HL_HOST_FILE_TIME_NOW;
            else if (nanoseconds == INT64_C(0x3ffffffe))
                times[index].mode = HL_HOST_FILE_TIME_OMIT;
            else if (nanoseconds >= 0 && nanoseconds < INT64_C(1000000000)) {
                times[index].nanoseconds = (uint32_t)nanoseconds;
                times[index].mode = HL_HOST_FILE_TIME_EXPLICIT;
            } else {
                result = -EINVAL;
                goto bound_set_times_done;
            }
        }
        result = bound_host_error(g_host_services->file->set_times(g_host_services->context, target, times).status);
    bound_set_times_done:
        if (close_target) (void)g_host_services->file->close(g_host_services->context, target);
        break;
    }
    case 32: {
        hl_host_file_metadata metadata;
        hl_host_result status =
            g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
        if (status.status != HL_STATUS_OK) {
            result = bound_host_error(status.status);
            break;
        }
        result = hl_flock_identity(&source, metadata.stable_device, metadata.stable_object, (int)a1) < 0
                     ? -(int64_t)(errno == EWOULDBLOCK ? 11 : errno)
                     : 0;
        break;
    }
    case 29: {
        uint32_t request = (uint32_t)a1;
        uint64_t guest_argument = a2;
        if (hl_provider_files_is_handle(source.host_handle)) {
            uint32_t direction = request >> 30;
            uint32_t argument_size = (request >> 16) & 0x3fffu;
            if (argument_size > 16384) {
                result = -E2BIG;
                break;
            }
            unsigned char *provider_argument = argument_size == 0 ? NULL : calloc(argument_size, 1);
            if (argument_size != 0 && provider_argument == NULL) {
                result = -ENOMEM;
                break;
            }
            if (argument_size != 0 && guest_argument == 0) {
                free(provider_argument);
                result = -EFAULT;
                break;
            }
            if ((direction & 1u) != 0 &&
                guest_copy_from(provider_argument, guest_argument, argument_size) != argument_size) {
                free(provider_argument);
                result = -EFAULT;
                break;
            }
            hl_provider_ioctl_result ioctl_result = {0};
            hl_host_result called =
                hl_provider_files_ioctl(source.host_handle, request, provider_argument, argument_size, &ioctl_result);
            if (called.status != HL_STATUS_OK)
                result = called.detail != 0 ? -(int64_t)called.detail : bound_host_error(called.status);
            else
                result = (int64_t)called.value;
            if (result >= 0 && (direction & 2u) != 0 &&
                guest_copy_to(guest_argument, provider_argument, argument_size) != argument_size)
                result = -EFAULT;
            if (result >= 0) {
                for (uint32_t i = 0; i < ioctl_result.write_count; ++i) {
                    hl_provider_ioctl_write *write = &ioctl_result.writes[i];
                    if (write->address == 0 ||
                        guest_copy_to(write->address, write->bytes, write->size) != write->size) {
                        result = -EFAULT;
                        break;
                    }
                }
            }
            hl_provider_files_ioctl_result_destroy(&ioctl_result);
            free(provider_argument);
            break;
        }
        uint8_t argument[44] = {0};
        size_t argument_size = 0;
        int argument_input = 0, argument_output = 0;
        if (request == 0x5401u) argument_size = 36, argument_output = 1;
        if (request >= 0x5402u && request <= 0x5404u) argument_size = 36, argument_input = 1;
        if (request == 0x802c542au) argument_size = 44, argument_output = 1;
        if (request >= 0x402c542bu && request <= 0x402c542du) argument_size = 44, argument_input = 1;
        if (request == 0x5413u) argument_size = sizeof(struct winsize), argument_output = 1;
        if (request == 0x5414u) argument_size = sizeof(struct winsize), argument_input = 1;
        if (request == 0x5421u || request == 0x5410u) argument_size = sizeof(int), argument_input = 1;
        if (request == 0x541bu || request == 0x540fu) argument_size = sizeof(int), argument_output = 1;
        if (argument_input && guest_copy_from(argument, guest_argument, argument_size) != (ssize_t)argument_size) {
            result = -EFAULT;
            break;
        }
        if (argument_size) a2 = (uint64_t)(uintptr_t)argument;
        if (request == 0x5451u || request == 0x5450u) { /* FIOCLEX / FIONCLEX */
            result =
                hl_linux_fcntl(g_linux_box, source.fd, HL_LINUX_F_SETFD, request == 0x5451u ? HL_LINUX_FD_CLOEXEC : 0);
        } else if (request == 0x5421u) { /* FIONBIO */
            int enabled = 0;
            memcpy(&enabled, argument, sizeof(enabled));
            int64_t flags = hl_linux_fcntl(g_linux_box, source.fd, HL_LINUX_F_GETFL, 0);
            if (flags < 0) {
                result = flags;
                break;
            }
            if (enabled)
                flags |= HL_LINUX_O_NONBLOCK;
            else
                flags &= ~(int64_t)HL_LINUX_O_NONBLOCK;
            result = hl_linux_fcntl(g_linux_box, source.fd, HL_LINUX_F_SETFL, (uint64_t)flags);
        } else if (request == 0x541bu) { /* FIONREAD */
            hl_host_file_metadata metadata;
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            int64_t offset = hl_linux_lseek(g_linux_box, source.fd, 0, SEEK_CUR);
            if (status.status != HL_STATUS_OK)
                result = bound_host_error(status.status);
            else if (metadata.type != HL_HOST_FILE_TYPE_REGULAR || offset < 0)
                result = metadata.type != HL_HOST_FILE_TYPE_REGULAR ? -ENOTTY : offset;
            else {
                uint64_t available = metadata.size > (uint64_t)offset ? metadata.size - (uint64_t)offset : 0;
                int encoded = available > INT_MAX ? INT_MAX : (int)available;
                memcpy(argument, &encoded, sizeof(encoded));
                result = 0;
            }
        } else if (request == 0x5401u || request == 0x5402u || request == 0x5403u || request == 0x5404u ||
                   request == 0x5413u || request == 0x5414u || request == 0x540fu || request == 0x5410u ||
                   request == 0x540eu || request == 0x802c542au || request == 0x402c542bu || request == 0x402c542cu ||
                   request == 0x402c542du) {
            int native_fd = -1;
            int borrowed = bound_attachment_borrow((int)source.fd, &native_fd);
            if (borrowed < 0) {
                result = borrowed;
                break;
            }
            if (request == 0x5401u) { /* TCGETS */
                struct termios native;
                if (tcgetattr(native_fd, &native) != 0)
                    result = -errno;
                else {
#if defined(__linux__)
                    memcpy(argument, &native, 36);
#else
                    termios_m2l(&native, argument);
#endif
                    result = 0;
                }
            } else if (request == 0x802c542au) { /* TCGETS2 */
                /* Linux termios2 has an encoded 44-byte payload. On the Linux/aarch64 host its ABI is
                 * byte-identical to the aarch64 guest ABI, so preserve the extended speed fields and
                 * forward the complete request. macOS has no termios2 request, so translate its native
                 * termios and explicitly populate the two Linux speed fields. */
#if defined(__linux__)
                result = ioctl(native_fd, request, argument) == 0 ? 0 : -errno;
#else
                {
                    struct termios native;
                    if (tcgetattr(native_fd, &native) != 0)
                        result = -errno;
                    else {
                        uint32_t input_speed = (uint32_t)cfgetispeed(&native);
                        uint32_t output_speed = (uint32_t)cfgetospeed(&native);
                        termios_m2l(&native, argument);
                        memcpy(argument + 36, &input_speed, sizeof(input_speed));
                        memcpy(argument + 40, &output_speed, sizeof(output_speed));
                        result = 0;
                    }
                }
#endif
            } else if (request >= 0x402c542bu && request <= 0x402c542du) { /* TCSETS2/W2/F2 */
#if defined(__linux__)
                result = ioctl(native_fd, request, argument) == 0 ? 0 : -errno;
#else
                {
                    struct termios native;
                    uint32_t input_speed, output_speed;
                    termios_l2m(argument, &native);
                    memcpy(&input_speed, argument + 36, sizeof(input_speed));
                    memcpy(&output_speed, argument + 40, sizeof(output_speed));
                    (void)cfsetispeed(&native, input_speed);
                    (void)cfsetospeed(&native, output_speed);
                    int action = request == 0x402c542bu ? TCSANOW : request == 0x402c542cu ? TCSADRAIN : TCSAFLUSH;
                    result = tcsetattr(native_fd, action, &native) == 0 ? 0 : -errno;
                }
#endif
            } else if (request >= 0x5402u && request <= 0x5404u) { /* TCSETS{,W,F} */

                struct termios native;
                {
#if defined(__linux__)
                    memset(&native, 0, sizeof(native));
                    memcpy(&native, argument, 36);
#else
                    termios_l2m(argument, &native);
#endif
                    int action = request == 0x5402u ? TCSANOW : request == 0x5403u ? TCSADRAIN : TCSAFLUSH;
                    result = tcsetattr(native_fd, action, &native) == 0 ? 0 : -errno;
                }
            } else if (request == 0x5413u || request == 0x5414u) { /* TIOCGWINSZ/TIOCSWINSZ */
                result = ioctl(native_fd, request == 0x5413u ? TIOCGWINSZ : TIOCSWINSZ, argument) == 0 ? 0 : -errno;
            } else if (request == 0x540fu) { /* TIOCGPGRP */
                {
                    pid_t group = tcgetpgrp(native_fd);
                    if (group < 0)
                        result = -errno;
                    else {
                        int encoded = group == g_init_hostpid ? 1 : (int)group;
                        memcpy(argument, &encoded, sizeof(encoded));
                        result = 0;
                    }
                }
            } else if (request == 0x5410u) { /* TIOCSPGRP */
                {
                    int encoded;
                    memcpy(&encoded, argument, sizeof(encoded));
                    pid_t group = encoded;
                    if (group == 1 && g_init_hostpid) group = g_init_hostpid;
                    result = tcsetpgrp(native_fd, group) == 0 ? 0 : -errno;
                }
            } else { /* TIOCSCTTY */
                result = ioctl(native_fd, TIOCSCTTY, 0) == 0 || errno == EPERM ? 0 : -errno;
            }
            if (borrowed > 0) bound_attachment_release(native_fd);
        } else {
            result = -ENOTTY;
        }
        if (result >= 0 && argument_output &&
            guest_copy_to(guest_argument, argument, argument_size) != (ssize_t)argument_size)
            result = -EFAULT;
        break;
    }
    case 61: {
        uint64_t byte_capacity = a2 > UINT32_C(1 << 20) ? UINT32_C(1 << 20) : a2;
        if (a2 < 24) {
            result = -EINVAL;
            break;
        }
        if (a1 == 0 || byte_capacity > SIZE_MAX ||
            guest_accessible_prefix(a1, (size_t)byte_capacity, HL_LOGICAL_VMA_WRITE) != byte_capacity) {
            result = -EFAULT;
            break;
        }
        uint32_t capacity = (uint32_t)(byte_capacity / 24);
        hl_host_file_entry *entries = calloc(capacity, sizeof(*entries));
        if (entries == NULL) {
            result = -ENOMEM;
            break;
        }
        hl_host_result read = g_host_services->file->read_directory(g_host_services->context, source.host_handle,
                                                                    entries, capacity, (uint32_t)byte_capacity);
        if (read.status != HL_STATUS_OK) {
            result = bound_host_error(read.status);
            free(entries);
            break;
        }
        if (read.value > capacity) {
            result = -EIO;
            free(entries);
            break;
        }
        uint8_t *output = calloc(1, (size_t)byte_capacity);
        if (output == NULL) {
            free(entries);
            result = -ENOMEM;
            break;
        }
        size_t used = 0;
        result = 0;
        for (uint32_t index = 0; index < (uint32_t)read.value; ++index) {
            size_t record_size = (19u + entries[index].name_size + 1u + 7u) & ~(size_t)7u;
            if (entries[index].name_size > 255 || record_size > byte_capacity - used) {
                result = -EIO;
                break;
            }
            uint8_t *record = output + used;
            memset(record, 0, record_size);
            *(uint64_t *)(record + 0) = entries[index].object;
            *(uint64_t *)(record + 8) = entries[index].next_offset;
            *(uint16_t *)(record + 16) = (uint16_t)record_size;
            record[18] = (uint8_t)entries[index].type;
            memcpy(record + 19, entries[index].name, entries[index].name_size);
            used += record_size;
        }
        if (result == 0 && guest_copy_to(a1, output, used) != (ssize_t)used)
            result = -EFAULT;
        else if (result == 0)
            result = (int64_t)used;
        free(output);
        free(entries);
        break;
    }
    case 23: result = bound_dup_at_least(source.fd, 0, 0); break;
    case 24: {
        struct fdvis_reservation fdvis;
        uint32_t flags = (uint32_t)a2;
        int is_dup2 = G_IS_DUP2_COMPAT();
        int target = (int)a1;
        if (source.fd == (hl_linux_fd)target) {
            result = is_dup2 ? (int64_t)source.fd : -EINVAL;
        } else if ((!is_dup2 && (flags & ~HL_LINUX_O_CLOEXEC) != 0) || target < 0 || target >= guest_nofile_cur()) {
            result = target < 0 || target >= guest_nofile_cur() ? -EBADF : -EINVAL;
        } else {
            hl_linux_fd_snapshot target_snapshot;
            int target_bound = bound_snapshot((uint64_t)(uint32_t)target, &target_snapshot);
            int shadow;
            if (proc_fdvis_reserve_at(target, &fdvis) != 0) {
                result = -ENOSPC;
                break;
            }
            if (target_bound) {
                shadow = target;
            } else {
                engine_fd_vacate(target);
                fd_reset_emul(target);
                shadow = bound_shadow_dup2(target);
                if (shadow < 0) {
                    proc_fdvis_reservation_cancel(&fdvis);
                    result = -(int64_t)errno;
                    break;
                }
                (void)fcntl(target, F_SETFD, FD_CLOEXEC);
            }
            result = hl_linux_dup3(g_linux_box, source.fd, (hl_linux_fd)target,
                                   flags & HL_LINUX_O_CLOEXEC ? HL_LINUX_O_CLOEXEC : 0);
            if (result < 0) {
                proc_fdvis_reservation_cancel(&fdvis);
                if (!target_bound) close(shadow);
            } else {
                hl_linux_fd_snapshot duplicate;
                hl_host_file_metadata metadata = {0};
                uint32_t kind = HL_HOST_FD_OTHER;
                if (bound_snapshot((uint64_t)target, &duplicate) && g_host_services != NULL &&
                    g_host_services->file != NULL && g_host_services->file->metadata != NULL &&
                    g_host_services->file->metadata(g_host_services->context, duplicate.host_handle, &metadata)
                            .status == HL_STATUS_OK) {
                    if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ||
                        metadata.type == HL_HOST_FILE_TYPE_SYMLINK)
                        kind = HL_HOST_FD_FILE;
                    else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
                        kind = HL_HOST_FD_PIPE;
                    else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
                        kind = HL_HOST_FD_SOCKET;
                }
                proc_fdvis_reservation_publish(&fdvis, target, kind, metadata.stable_device, metadata.stable_object);
                bound_path_duplicate(source.fd, result);
            }
        }
        break;
    }
    case 25:
        if ((int32_t)a1 == HL_LINUX_F_DUPFD || (int32_t)a1 == HL_LINUX_F_DUPFD_CLOEXEC) {
            if (a2 > INT_MAX)
                result = -EINVAL;
            else
                result = bound_dup_at_least(source.fd, (int)a2,
                                            (int32_t)a1 == HL_LINUX_F_DUPFD_CLOEXEC ? HL_LINUX_FD_CLOEXEC : 0);
        } else if (a1 == 5 || a1 == 6 || a1 == 7) {
            uint8_t lock[32];
            hl_host_file_metadata metadata;
            hl_host_result status;
            int64_t current = 0;
            int lock_result = 0;
            if (guest_copy_from(lock, a2, sizeof(lock)) != (ssize_t)sizeof(lock)) {
                result = -EFAULT;
                break;
            }
            short whence;
            memcpy(&whence, lock + 2, sizeof(whence));
            if (whence != SEEK_SET && whence != SEEK_CUR && whence != SEEK_END) {
                result = -EINVAL;
                break;
            }
            if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->metadata == NULL) {
                result = -ENOSYS;
                break;
            }
            status = g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            if (status.status != HL_STATUS_OK) {
                result = bound_host_error(status.status);
                break;
            }
            if (metadata.type != HL_HOST_FILE_TYPE_REGULAR) {
                result = -EBADF;
                break;
            }
            if (whence == SEEK_CUR) {
                current = hl_linux_lseek(g_linux_box, source.fd, 0, SEEK_CUR);
                if (current < 0) {
                    result = current;
                    break;
                }
            }
            for (;;) {
                (void)poslk_op_identity(metadata.stable_device, metadata.stable_object, current, metadata.size, (int)a1,
                                        lock, &lock_result);
                if (a1 != 7 || lock_result != -EAGAIN) break;
                uint64_t pending =
                    __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
                int interrupted = 0;
                for (int signal_number = 1; signal_number < 64; ++signal_number)
                    if ((pending & (UINT64_C(1) << signal_number)) &&
                        !(c->sigmask & (UINT64_C(1) << (signal_number - 1)))) {
                        interrupted = 1;
                        break;
                    }
                if (interrupted) {
                    lock_result = -EINTR;
                    break;
                }
                struct timespec delay = {0, 1000000};
                nanosleep(&delay, NULL);
            }
            /* poslk_apply is shared with the legacy Darwin syscall path and therefore reports native
             * errno numbers. This typed route bypasses svc_done(), so translate at this boundary. */
            result = lock_result < 0 ? -hl_linux_errno_from_macos(-lock_result) : lock_result;
            if (result == 0 && a1 == 5 && guest_copy_to(a2, lock, sizeof(lock)) != (ssize_t)sizeof(lock))
                result = -EFAULT;
        } else if ((int32_t)a1 == HL_LINUX_F_SETFL) {
            // O_DIRECT is a settable status flag whose bit is arch-specific (G_O_DIRECT: aarch64 0x10000 --
            // which aliases HL_LINUX_O_DIRECTORY -- vs x86-64 0x4000). Normalize the guest's arch bit to the
            // canonical arch-neutral HL_LINUX_O_DIRECT before the arch-neutral core stores it, so F_GETFL
            // reflects it (fcntl-cmds direct.consistent) instead of it being silently dropped.
            uint64_t normalized = a2 & ~(uint64_t)G_O_DIRECT;
            if (a2 & G_O_DIRECT) normalized |= HL_LINUX_O_DIRECT;
            result = hl_linux_fcntl(g_linux_box, source.fd, (int32_t)a1, normalized);
        } else if ((int32_t)a1 == HL_LINUX_F_GETFL) {
            result = hl_linux_fcntl(g_linux_box, source.fd, (int32_t)a1, a2);
            if (result >= 0 && (result & HL_LINUX_O_DIRECT)) // map the canonical bit back to the guest arch bit
                result = (result & ~(int64_t)HL_LINUX_O_DIRECT) | (int64_t)(uint64_t)G_O_DIRECT;
        } else if ((int32_t)a1 == HL_LINUX_F_GETFD || (int32_t)a1 == HL_LINUX_F_SETFD) {
            // Descriptor flags (FD_CLOEXEC) live in the virtual descriptor table, not the backing host fd.
            result = hl_linux_fcntl(g_linux_box, source.fd, (int32_t)a1, a2);
        } else {
            // Every remaining fcntl command -- F_SETOWN/F_GETOWN, F_SETSIG/F_GETSIG, the F_OFD_* open-file
            // description locks, F_SETLEASE/F_GETLEASE, F_NOTIFY, F_SET/GETPIPE_SZ, F_ADD/GET_SEALS, the
            // RW_HINT family -- operates on the real open file description, not the virtual fd table (which
            // only knows FD/FL/DUPFD/POSIX-lock). The arch-neutral core answered EINVAL for all of them, so
            // a bound (typed) descriptor lost OFD locks, memfd seals, owner/signal ownership, etc. The host
            // is Linux and the bound descriptor has a same-number native shadow, so forward the command
            // verbatim to that real host fd; pointer args (OFD flock, rw_hint u64) are already host-mapped.
            int native_fd = -1;
            int borrowed = bound_attachment_borrow((int)source.fd, &native_fd);
            if (borrowed < 0) {
                result = borrowed;
                break;
            }
            long r = fcntl(native_fd, (int)a1, (unsigned long)a2);
            result = r < 0 ? -(int64_t)errno : (int64_t)r;
        }
        break;
    case 285: {
        hl_linux_fd_snapshot output;
        off_t input_value = 0, output_value = 0;
        off_t *input_offset = a1 != 0 ? &input_value : NULL;
        off_t *output_offset = a3 != 0 ? &output_value : NULL;
        size_t done = 0;
        char buffer[8192];
        result = 0;
        // copy_file_range defines NO flags: Linux rejects a non-zero `flags` with -EINVAL before copying
        // anything. Mirrors the native path in io.c case 285.
        if (G_A5(c)) {
            result = -EINVAL;
            break;
        }
        if (!bound_snapshot(a2, &output)) {
            result = -ENOSYS;
            break;
        }
        if ((input_offset &&
             guest_copy_from(input_offset, a1, sizeof(*input_offset)) != (ssize_t)sizeof(*input_offset)) ||
            (output_offset &&
             guest_copy_from(output_offset, a3, sizeof(*output_offset)) != (ssize_t)sizeof(*output_offset))) {
            result = -EFAULT;
            break;
        }
        // Linux rejects a same-file copy whose ranges overlap (EINVAL) instead of copying through the
        // overlap.  Mirrors the native path in io.c case 285, using the typed identity for sameness.
        if (G_A4(c) > 0 && g_host_services != NULL && g_host_services->file != NULL &&
            g_host_services->file->metadata != NULL) {
            hl_host_file_metadata in_meta, out_meta;
            hl_host_result in_status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &in_meta);
            hl_host_result out_status =
                g_host_services->file->metadata(g_host_services->context, output.host_handle, &out_meta);
            if (in_status.status == HL_STATUS_OK && out_status.status == HL_STATUS_OK &&
                in_meta.stable_device == out_meta.stable_device && in_meta.stable_object == out_meta.stable_object) {
                off_t in_start = input_offset ? *input_offset : (off_t)source.offset;
                off_t out_start = output_offset ? *output_offset : (off_t)output.offset;
                off_t length = (off_t)G_A4(c);
                if (in_start >= 0 && out_start >= 0 && in_start < out_start + length && out_start < in_start + length) {
                    result = -EINVAL;
                    break;
                }
            }
        }
        while (done < (size_t)G_A4(c)) {
            size_t chunk = (size_t)G_A4(c) - done;
            if (chunk > sizeof(buffer)) chunk = sizeof(buffer);
            int64_t nr_read = input_offset
                                  ? hl_linux_pread64(g_linux_box, source.fd, buffer, chunk, (uint64_t)*input_offset)
                                  : hl_linux_read(g_linux_box, source.fd, buffer, chunk);
            if (nr_read <= 0) {
                if (!done) result = nr_read;
                break;
            }
            int64_t nr_written = output_offset ? hl_linux_pwrite64(g_linux_box, output.fd, buffer, (size_t)nr_read,
                                                                   (uint64_t)*output_offset)
                                               : hl_linux_write(g_linux_box, output.fd, buffer, (size_t)nr_read);
            if (nr_written < 0) {
                if (!done) result = nr_written;
                break;
            }
            done += (size_t)nr_written;
            if (input_offset) *input_offset += (off_t)nr_written;
            if (output_offset) *output_offset += (off_t)nr_written;
            result = (int64_t)done;
            if (nr_written < nr_read) break;
        }
        if (result >= 0 && ((input_offset && guest_copy_to(a1, input_offset, sizeof(*input_offset)) !=
                                                 (ssize_t)sizeof(*input_offset)) ||
                            (output_offset && guest_copy_to(a3, output_offset, sizeof(*output_offset)) !=
                                                  (ssize_t)sizeof(*output_offset))))
            result = done != 0 ? (int64_t)done : -EFAULT;
        break;
    }
    case 20: return 0; /* epoll_create1: a0 is flags, not an fd */
    case 21:           /* epoll_ctl */
    case 22:           /* epoll_pwait */
    case 71:           /* sendfile */
    case 75:           /* vmsplice */
    case 76:           /* splice */
    case 77:           /* tee */
    case 200:          /* bind */
    case 201:          /* listen */
    case 202:          /* accept */
    case 203:          /* connect */
    case 204:          /* getsockname */
    case 205:          /* getpeername */
    case 206:          /* sendto */
    case 207:          /* recvfrom */
    case 208:          /* setsockopt */
    case 209:          /* getsockopt */
    case 210:          /* shutdown */
    case 211:          /* sendmsg */
    case 212:          /* recvmsg */
        /* A bound slot is never a native descriptor. Unsupported fd operations cannot touch its shadow. */
        result = -ENOSYS;
        break;
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}
