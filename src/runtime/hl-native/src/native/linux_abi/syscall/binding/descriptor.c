static int bound_snapshot(uint64_t value, hl_linux_fd_snapshot *snapshot) {
    if (g_linux_box == NULL || value > UINT32_MAX) return 0;
    return hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)value, snapshot) == HL_STATUS_OK;
}

/* Return an independently closeable native descriptor for an execveat
 * AT_EMPTY_PATH request. A bound guest fd's same-number native descriptor is
 * only its sentinel shadow; duplicating that shadow executes the wrong object.
 * Detach the host attachment from the private-descriptor registry because the
 * exec image takes ordinary close(2) ownership from this point onward. */
static int bound_exec_descriptor(int descriptor) {
    hl_linux_fd_snapshot snapshot;
    const hl_host_posix_attachment_services *attachments;
    hl_host_result borrowed;
    int native;
    if (!bound_snapshot((uint64_t)(uint32_t)descriptor, &snapshot)) return dup(descriptor);
    attachments = g_host_services != NULL ? g_host_services->posix_attachment : NULL;
    if (attachments == NULL || attachments->borrow_file == NULL) {
        errno = ENOSYS;
        return -1;
    }
    borrowed = attachments->borrow_file(g_host_services->context, snapshot.host_handle);
    if (borrowed.status != HL_STATUS_OK || borrowed.value > INT_MAX) {
        errno = borrowed.status == HL_STATUS_OK ? EOVERFLOW : (int)-bound_host_error(borrowed.status);
        return -1;
    }
    native = (int)borrowed.value;
    hl_host_process_fd_private_remove(native);
    return native;
}

#include "vector_validation.h"

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
    memmove(g_fdpath[(int)target], g_fdpath[(int)source], sizeof g_fdpath[(int)target]);
    g_fdpath_guest[(int)target] = g_fdpath_guest[(int)source];
    memmove(g_ovldir[(int)target], g_ovldir[(int)source], sizeof g_ovldir[(int)target]);
    ovldents_duplicate((int)source, (int)target);
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

static void bound_evict_handle(hl_host_handle file) {
    char path[HL_LINUX_PATH_MAX + 1];
    if (bound_handle_host_path(file, path, sizeof(path)) == 0) hl_fdcache_evict_path(path);
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
    uint8_t guest_path_is_guest = 0;
    guest_path[0] = 0;
    if (opened < 0) return opened;
    if (opened < HL_NFD && g_fdpath[(int)opened][0]) {
        snprintf(guest_path, sizeof guest_path, "%s", g_fdpath[(int)opened]);
        guest_path_is_guest = g_fdpath_guest[(int)opened];
    }
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
    if (opened < HL_NFD) {
        g_fdpath[(int)opened][0] = 0;
        g_fdpath_guest[(int)opened] = 0;
    }
    if (duplicated >= 0 && duplicated < HL_NFD && guest_path[0]) {
        snprintf(g_fdpath[(int)duplicated], sizeof g_fdpath[(int)duplicated], "%s", guest_path);
        g_fdpath_guest[(int)duplicated] = guest_path_is_guest;
    }
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

#if defined(HL_NATIVE_TEST_HOOKS)
typedef int64_t (*bound_vector_test_provider)(hl_host_iovec *, uint32_t, int);
static _Thread_local bound_vector_test_provider g_bound_vector_test_provider;
#endif

static int bound_vectors_copy(uint64_t address, uint64_t count, hl_host_iovec vectors[HL_LINUX_IOV_MAX]) {
    uint64_t index;
    uint64_t total = 0;
    size_t array_size;
    if (count > HL_LINUX_IOV_MAX) return -HL_LINUX_EINVAL;
    if (count == 0) return 0;
    if (address == 0 || count > SIZE_MAX / sizeof(*vectors)) return -HL_LINUX_EFAULT;
    array_size = (size_t)count * sizeof(*vectors);
    if (guest_copy_from(vectors, address, array_size) != (ssize_t)array_size) return -HL_LINUX_EFAULT;
    for (index = 0; index < count; ++index) {
        uint64_t base = vectors[index].address, size = vectors[index].size;
        /* import_iovec validates each segment's accumulated byte count and address range in sequence.
           Keep that ordering before any bounce allocation or provider operation. */
        int validated = hl_guest_iov_validate(base, size, &total);
        if (validated != 0) return validated;
    }
    return 0;
}

static int bound_vectors_prepare(const hl_linux_fd_snapshot *file, int output, uint64_t address, uint64_t count,
                                 hl_host_iovec vectors[HL_LINUX_IOV_MAX]) {
    if (bound_access_rejects(file, output)) return -EBADF;
    return bound_vectors_copy(address, count, vectors);
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
            size_t copied = 0;
            while (copied < size) {
                uint64_t address = guest_vectors[index].address + copied;
                size_t page = 4096u - (size_t)(address & 4095u);
                size_t chunk = size - copied < page ? size - copied : page;
                if (guest_copy_from((char *)buffers[usable] + copied, address, chunk) != (ssize_t)chunk) break;
                copied += chunk;
            }
            if (copied == 0) {
                result = bound_access_rejects(file, 0) ? -EBADF : -EFAULT;
                goto cleanup;
            }
            size = copied;
        }
        host_vectors[usable] = (hl_host_iovec){(uint64_t)(uintptr_t)buffers[usable], size};
        usable++;
        if (size != guest_vectors[index].size) break;
    }
issue_or_fail:
    if (usable == 0) goto cleanup;
#if defined(HL_NATIVE_TEST_HOOKS)
    if (g_bound_vector_test_provider != NULL)
        result = g_bound_vector_test_provider(host_vectors, usable, output);
    else
#endif
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
    if (!output && result > 0) bound_evict_handle(file->host_handle);
cleanup:
    for (uint32_t index = 0; index < usable; ++index)
        free(buffers[index]);
    return result;
}

#if defined(HL_NATIVE_TEST_HOOKS) && !defined(_WIN32)
static _Thread_local uint32_t g_bound_vector_test_calls;
static _Thread_local uint64_t g_bound_vector_test_bytes;

static int64_t bound_vector_test_issue(hl_host_iovec *vectors, uint32_t count, int output) {
    uint64_t total = 0;
    g_bound_vector_test_calls++;
    for (uint32_t index = 0; index < count; ++index) {
        if (vectors[index].size > INT64_MAX - total) return -EINVAL;
        if (output) memset((void *)(uintptr_t)vectors[index].address, 0x5a, (size_t)vectors[index].size);
        total += vectors[index].size;
    }
    g_bound_vector_test_bytes = total;
    return (int64_t)total;
}

/* Test-only export: drive the production bounce/prefix routine while replacing only its final provider call. */
HL_API int HL_TARGET_LOCAL(bound_vector_io_test)(uint32_t scenario, int64_t *result, uint32_t *calls, uint64_t *bytes) {
    const uint64_t guest = UINT64_C(0x700000000000);
    hl_linux_fd_snapshot file = {.fd = 3, .status_flags = HL_LINUX_O_RDWR};
    hl_host_iovec vectors[2] = {{guest, 8192}, {0, 0}};
    void *storage;
    int64_t observed;
    if (result == NULL || calls == NULL || bytes == NULL || scenario > 3) return -EINVAL;
    if (scenario == 3) {
        file.status_flags = HL_LINUX_O_RDONLY;
        *result = bound_vectors_prepare(&file, 0, 0, UINT64_MAX, vectors);
        *calls = 0;
        *bytes = 0;
        return 0;
    }
    storage = aligned_alloc(4096, 8192);
    if (storage == NULL) return -ENOMEM;
    memset(storage, 0x31, 8192);
    if (hl_logical_vma_global_map_direct(guest, 4096, HL_LOGICAL_VMA_READ | HL_LOGICAL_VMA_WRITE,
                                         (uint64_t)(uintptr_t)storage) != 0 ||
        hl_logical_vma_global_map_direct(guest + 4096, 4096, 0, (uint64_t)(uintptr_t)((char *)storage + 4096)) != 0) {
        (void)hl_logical_vma_global_unmap(guest, 8192);
        free(storage);
        return -ENOMEM;
    }
    if (scenario == 1) {
        vectors[0] = (hl_host_iovec){guest, 16};
        vectors[1] = (hl_host_iovec){guest + 4096, 16};
    }
    g_bound_vector_test_calls = 0;
    g_bound_vector_test_bytes = 0;
    g_bound_vector_test_provider = bound_vector_test_issue;
    observed = bound_vector_io(&file, vectors, scenario == 1 ? 2 : 1, scenario == 2, 1, 17);
    g_bound_vector_test_provider = NULL;
    *result = observed;
    *calls = g_bound_vector_test_calls;
    *bytes = g_bound_vector_test_bytes;
    (void)hl_logical_vma_global_unmap(guest, 8192);
    free(storage);
    return 0;
}
#elif defined(HL_NATIVE_TEST_HOOKS)
/* The fixture backs its guest slice with aligned_alloc, which no Windows C runtime supplies -- UCRT
 * offers only _aligned_malloc, whose storage free() may not release. The loader resolves every
 * exported test symbol, so the hook exists here and refuses. This is the one guard in this sweep
 * whose block is otherwise portable: give the fixture a page-aligned allocation the host can free
 * and the real scenarios could run on Windows. */
HL_API int HL_TARGET_LOCAL(bound_vector_io_test)(uint32_t scenario, int64_t *result, uint32_t *calls, uint64_t *bytes);

HL_API int HL_TARGET_LOCAL(bound_vector_io_test)(uint32_t scenario, int64_t *result, uint32_t *calls, uint64_t *bytes) {
    (void)scenario;
    (void)result;
    (void)calls;
    (void)bytes;
    errno = ENOTSUP;
    return -1;
}
#endif
