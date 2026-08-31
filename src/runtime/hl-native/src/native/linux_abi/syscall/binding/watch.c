/* Bridge opaque host-file bindings into the legacy native-fd guest runtime. */

#ifndef G_IS_DUP2_COMPAT
#define G_IS_DUP2_COMPAT() 0 /* aarch64 guests have no legacy dup2; every case 24 is a real dup3 */
#endif
#include "../../object.h"
#include "../../epoll.h"
#include "../../eventfd.h"
#include "../../watch.h"
#include "../../bus.h"
#include "../../../engine/provider/files.h"

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
        int64_t uid, gid;
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
    if (address == 0 && (linux_flags & 0x10u) == 0) address = hl_linux_snapshot_reserve(&g_ckpt_snapshot, size);
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
        if (protection & 4u)
            gnx_clear(lo, hi);
        else
            gnx_add(lo, hi);
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
    if (g_pcache && (protection & 4u) != 0 && ((pc_hint != 0 && mapped.address == pc_hint) ||
        (mapped.address >= PC_LIB_BASE && mapped.address < PC_LIB_BASE + PC_LIB_SPAN)))
        pcache_note_libmap(mapped.address, size, file->host_handle, &metadata);
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
