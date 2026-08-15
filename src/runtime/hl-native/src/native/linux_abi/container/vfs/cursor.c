#define HL_VFS_CURSOR_LAYERS (HL_LINUX_VFS_LOWER_CAPACITY + 1)
#define HL_VFS_MOUNT_NOEXEC (1u << 0)
#include <stdatomic.h>
#include <stdint.h>
#if defined(__GNUC__) || defined(__clang__)
#define HL_VFS_CURSOR_UNUSED __attribute__((unused))
#else
#define HL_VFS_CURSOR_UNUSED
#endif

typedef struct hl_vfs_cursor_parent hl_vfs_cursor_parent;

typedef enum hl_vfs_cursor_authority_kind {
    HL_VFS_CURSOR_AUTHORITY_INVALID = 0,
    HL_VFS_CURSOR_AUTHORITY_NATIVE = 1,
    HL_VFS_CURSOR_AUTHORITY_HOST = 2,
} hl_vfs_cursor_authority_kind;

typedef struct hl_vfs_cursor_authority {
    hl_vfs_cursor_authority_kind kind;

    union {
        int descriptor;

        struct {
            hl_host_handle handle;
            const hl_host_services *services;
        } host;
    } value;
} hl_vfs_cursor_authority;

typedef struct hl_vfs_cursor {
    hl_vfs_cursor_authority layers[HL_VFS_CURSOR_LAYERS];
    size_t count;
    int opaque_cut;
    uint32_t mount_flags;
    char guest[4200];
    hl_vfs_cursor_parent *parent;
} hl_vfs_cursor;

struct hl_vfs_cursor_parent {
    _Atomic unsigned references;
    hl_vfs_cursor cursor;
};

typedef enum hl_vfs_cursor_kind {
    HL_VFS_CURSOR_ABSENT = 0,
    HL_VFS_CURSOR_FILE = 1,
    HL_VFS_CURSOR_DIRECTORY = 2,
    HL_VFS_CURSOR_SYMLINK = 3,
} hl_vfs_cursor_kind;

typedef struct hl_vfs_cursor_entry {
    hl_vfs_cursor_kind kind;
    hl_vfs_cursor_authority file;
    struct stat status;
    uint32_t mount_flags;
    hl_vfs_cursor directory;
    char symlink[4096];
} hl_vfs_cursor_entry;

static void hl_vfs_cursor_release(hl_vfs_cursor *cursor);

static int hl_vfs_cursor_host_error(hl_host_result result) {
    if (result.status == HL_STATUS_OK) return 0;
    switch (result.status) {
    case HL_STATUS_INVALID_ARGUMENT: return -EINVAL;
    case HL_STATUS_NOT_SUPPORTED: return -ENOSYS;
    case HL_STATUS_OUT_OF_MEMORY: return -ENOMEM;
    case HL_STATUS_RESOURCE_LIMIT: return -EMFILE;
    case HL_STATUS_NOT_FOUND: return -ENOENT;
    case HL_STATUS_ALREADY_EXISTS: return -EEXIST;
    case HL_STATUS_PERMISSION_DENIED: return -EACCES;
    case HL_STATUS_WOULD_BLOCK: return -EAGAIN;
    case HL_STATUS_INTERRUPTED: return -EINTR;
    case HL_STATUS_BUSY: return -EBUSY;
    case HL_STATUS_NOT_DIRECTORY: return -ENOTDIR;
    case HL_STATUS_IS_DIRECTORY: return -EISDIR;
    case HL_STATUS_NAME_TOO_LONG: return -ENAMETOOLONG;
    case HL_STATUS_SYMLINK_LOOP: return -ELOOP;
    case HL_STATUS_READ_ONLY: return -EROFS;
    case HL_STATUS_CROSS_DEVICE: return -EXDEV;
    case HL_STATUS_NOT_EMPTY: return -ENOTEMPTY;
    default: return -EIO;
    }
}

static hl_vfs_cursor_authority hl_vfs_cursor_native(int descriptor) {
    hl_vfs_cursor_authority authority = {0};
    if (descriptor >= 0) {
        authority.kind = HL_VFS_CURSOR_AUTHORITY_NATIVE;
        authority.value.descriptor = descriptor;
    }
    return authority;
}

static int hl_vfs_cursor_authority_clone(const hl_vfs_cursor_authority *source, hl_vfs_cursor_authority *output) {
    if (source == NULL || output == NULL || source->kind == HL_VFS_CURSOR_AUTHORITY_INVALID) return -EINVAL;
    memset(output, 0, sizeof *output);
    if (source->kind == HL_VFS_CURSOR_AUTHORITY_NATIVE) {
        int descriptor = fcntl(source->value.descriptor, F_DUPFD_CLOEXEC, 0);
        if (descriptor < 0) return -errno;
        *output = hl_vfs_cursor_native(descriptor);
        return 0;
    }
    if (source->kind != HL_VFS_CURSOR_AUTHORITY_HOST || source->value.host.services == NULL ||
        source->value.host.services->file == NULL || source->value.host.services->file->clone_for_fork == NULL)
        return -ENOSYS;
    hl_host_result cloned = source->value.host.services->file->clone_for_fork(source->value.host.services->context,
                                                                              source->value.host.handle);
    int error = hl_vfs_cursor_host_error(cloned);
    if (error != 0) return error;
    output->kind = HL_VFS_CURSOR_AUTHORITY_HOST;
    output->value.host.handle = cloned.value;
    output->value.host.services = source->value.host.services;
    return 0;
}

static void hl_vfs_cursor_authority_close(hl_vfs_cursor_authority *authority) {
    if (authority == NULL) return;
    if (authority->kind == HL_VFS_CURSOR_AUTHORITY_NATIVE) {
        close(authority->value.descriptor);
    } else if (authority->kind == HL_VFS_CURSOR_AUTHORITY_HOST && authority->value.host.services != NULL &&
               authority->value.host.services->file != NULL && authority->value.host.services->file->close != NULL) {
        (void)authority->value.host.services->file->close(authority->value.host.services->context,
                                                          authority->value.host.handle);
    }
    memset(authority, 0, sizeof *authority);
}

static int hl_vfs_cursor_authority_metadata(const hl_vfs_cursor_authority *authority, const char *component,
                                            struct stat *status) {
    if (authority == NULL || component == NULL || status == NULL) return -EINVAL;
    if (authority->kind == HL_VFS_CURSOR_AUTHORITY_NATIVE)
        return fstatat(authority->value.descriptor, component, status, AT_SYMLINK_NOFOLLOW) == 0 ? 0 : -errno;
    if (authority->kind != HL_VFS_CURSOR_AUTHORITY_HOST || authority->value.host.services == NULL ||
        authority->value.host.services->file == NULL)
        return -EINVAL;
    const hl_host_file_services *file = authority->value.host.services->file;
    if (file->open_relative == NULL || file->metadata == NULL || file->close == NULL) return -ENOSYS;
    hl_host_result opened =
        file->open_relative(authority->value.host.services->context, authority->value.host.handle, component,
                            strlen(component), HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0);
    int error = hl_vfs_cursor_host_error(opened);
    if (error != 0) return error;
    hl_host_file_metadata metadata;
    hl_host_result described = file->metadata(authority->value.host.services->context, opened.value, &metadata);
    (void)file->close(authority->value.host.services->context, opened.value);
    error = hl_vfs_cursor_host_error(described);
    if (error != 0) return error;
    memset(status, 0, sizeof *status);
    status->st_dev = (dev_t)metadata.stable_device;
    status->st_ino = (ino_t)metadata.stable_object;
    status->st_mode = (mode_t)(metadata.permissions & 07777u);
    if (metadata.type == HL_HOST_FILE_TYPE_DIRECTORY)
        status->st_mode |= S_IFDIR;
    else if (metadata.type == HL_HOST_FILE_TYPE_SYMLINK)
        status->st_mode |= S_IFLNK;
    else if (metadata.type == HL_HOST_FILE_TYPE_REGULAR)
        status->st_mode |= S_IFREG;
    else if (metadata.type == HL_HOST_FILE_TYPE_CHARACTER)
        status->st_mode |= S_IFCHR;
    else if (metadata.type == HL_HOST_FILE_TYPE_BLOCK)
        status->st_mode |= S_IFBLK;
    else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
        status->st_mode |= S_IFIFO;
    else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
        status->st_mode |= S_IFSOCK;
    return 0;
}

static int hl_vfs_cursor_authority_open_child(const hl_vfs_cursor_authority *authority, const char *component,
                                              int directory, hl_vfs_cursor_authority *output) {
    if (authority == NULL || component == NULL || output == NULL) return -EINVAL;
    if (authority->kind == HL_VFS_CURSOR_AUTHORITY_NATIVE) {
        int flags = O_RDONLY | O_CLOEXEC | O_NOFOLLOW | (directory ? O_DIRECTORY : 0);
        int descriptor = openat(authority->value.descriptor, component, flags);
        if (descriptor < 0) return -errno;
        *output = hl_vfs_cursor_native(descriptor);
        return 0;
    }
    if (authority->kind != HL_VFS_CURSOR_AUTHORITY_HOST || authority->value.host.services == NULL ||
        authority->value.host.services->file == NULL || authority->value.host.services->file->open_relative == NULL)
        return -ENOSYS;
    uint32_t access = HL_HOST_FILE_READ | HL_HOST_FILE_NOFOLLOW | (directory ? HL_HOST_FILE_DIRECTORY : 0);
    hl_host_result opened = authority->value.host.services->file->open_relative(authority->value.host.services->context,
                                                                                authority->value.host.handle, component,
                                                                                strlen(component), access, 0, 0);
    int error = hl_vfs_cursor_host_error(opened);
    if (error != 0) return error;
    output->kind = HL_VFS_CURSOR_AUTHORITY_HOST;
    output->value.host.handle = opened.value;
    output->value.host.services = authority->value.host.services;
    return 0;
}

static int hl_vfs_cursor_authority_open_path(const hl_vfs_cursor_authority *authority, const char *component,
                                             int directory, hl_vfs_cursor_authority *output) {
    if (authority == NULL || component == NULL || output == NULL) return -EINVAL;
    if (authority->kind == HL_VFS_CURSOR_AUTHORITY_NATIVE) {
#if defined(__linux__)
        int descriptor = openat(authority->value.descriptor, component,
                                O_PATH | O_CLOEXEC | O_NOFOLLOW | (directory ? O_DIRECTORY : 0));
#elif defined(__APPLE__)
        int descriptor = openat(authority->value.descriptor, component,
                                O_SYMLINK | O_CLOEXEC | (directory ? O_DIRECTORY : 0));
#else
        return -ENOSYS;
#endif
        if (descriptor < 0) return -errno;
        *output = hl_vfs_cursor_native(descriptor);
        return 0;
    }
    if (authority->kind != HL_VFS_CURSOR_AUTHORITY_HOST || authority->value.host.services == NULL ||
        authority->value.host.services->file == NULL || authority->value.host.services->file->open_relative == NULL)
        return -ENOSYS;
    hl_host_result opened = authority->value.host.services->file->open_relative(
        authority->value.host.services->context, authority->value.host.handle, component, strlen(component),
        HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW | (directory ? HL_HOST_FILE_DIRECTORY : 0), 0, 0);
    int error = hl_vfs_cursor_host_error(opened);
    if (error != 0) return error;
    output->kind = HL_VFS_CURSOR_AUTHORITY_HOST;
    output->value.host.handle = opened.value;
    output->value.host.services = authority->value.host.services;
    return 0;
}

static int hl_vfs_cursor_authority_readlink(const hl_vfs_cursor_authority *authority, const char *component,
                                            char *output, size_t capacity) {
    if (authority == NULL || component == NULL || output == NULL || capacity == 0) return -EINVAL;
    if (authority->kind == HL_VFS_CURSOR_AUTHORITY_NATIVE) {
        ssize_t length = readlinkat(authority->value.descriptor, component, output, capacity - 1);
        if (length < 0) return -errno;
        output[length] = 0;
        return 0;
    }
    if (authority->kind != HL_VFS_CURSOR_AUTHORITY_HOST || authority->value.host.services == NULL ||
        authority->value.host.services->file == NULL || authority->value.host.services->file->open_relative == NULL ||
        authority->value.host.services->file->readlink == NULL || authority->value.host.services->file->close == NULL)
        return -ENOSYS;
    const hl_host_file_services *file = authority->value.host.services->file;
    hl_host_result opened =
        file->open_relative(authority->value.host.services->context, authority->value.host.handle, component,
                            strlen(component), HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0);
    int error = hl_vfs_cursor_host_error(opened);
    if (error != 0) return error;
    hl_host_result read =
        file->readlink(authority->value.host.services->context, opened.value, (hl_host_bytes){output, capacity - 1});
    (void)file->close(authority->value.host.services->context, opened.value);
    error = hl_vfs_cursor_host_error(read);
    if (error != 0) return error;
    if (read.value >= capacity) return -ENAMETOOLONG;
    output[read.value] = 0;
    return 0;
}

static void hl_vfs_cursor_parent_retain(hl_vfs_cursor_parent *parent) {
    if (parent != NULL) atomic_fetch_add_explicit(&parent->references, 1, memory_order_relaxed);
}

static void hl_vfs_cursor_parent_release(hl_vfs_cursor_parent *parent) {
    if (parent == NULL || atomic_fetch_sub_explicit(&parent->references, 1, memory_order_acq_rel) != 1) return;
    hl_vfs_cursor_release(&parent->cursor);
    free(parent);
}

static void hl_vfs_cursor_release(hl_vfs_cursor *cursor) {
    if (cursor == NULL) return;
    for (size_t index = 0; index < cursor->count; index++)
        hl_vfs_cursor_authority_close(&cursor->layers[index]);
    hl_vfs_cursor_parent_release(cursor->parent);
    memset(cursor, 0, sizeof *cursor);
}

static int hl_vfs_cursor_clone(const hl_vfs_cursor *source, hl_vfs_cursor *output) {
    if (source == NULL || output == NULL || source->count > HL_VFS_CURSOR_LAYERS) return -EINVAL;
    memset(output, 0, sizeof *output);
    output->opaque_cut = source->opaque_cut;
    output->mount_flags = source->mount_flags;
    output->parent = source->parent;
    hl_vfs_cursor_parent_retain(output->parent);
    snprintf(output->guest, sizeof output->guest, "%s", source->guest);
    for (size_t index = 0; index < source->count; index++) {
        int error = hl_vfs_cursor_authority_clone(&source->layers[index], &output->layers[output->count]);
        if (error != 0) {
            hl_vfs_cursor_release(output);
            return error;
        }
        output->count++;
    }
    return 0;
}

static int hl_vfs_cursor_parent_create(const hl_vfs_cursor *cursor, hl_vfs_cursor_parent **output) {
    hl_vfs_cursor_parent *parent = calloc(1, sizeof *parent);
    if (parent == NULL) return -ENOMEM;
    atomic_init(&parent->references, 1);
    int error = hl_vfs_cursor_clone(cursor, &parent->cursor);
    if (error != 0) {
        free(parent);
        return error;
    }
    *output = parent;
    return 0;
}

static int HL_VFS_CURSOR_UNUSED hl_vfs_cursor_root(int upper, const int *lowers, size_t lower_count,
                                                   hl_vfs_cursor *output) {
    if (upper < 0 || output == NULL || lower_count > HL_LINUX_VFS_LOWER_CAPACITY ||
        (lower_count != 0 && lowers == NULL))
        return -EINVAL;
    hl_vfs_cursor source;
    memset(&source, 0, sizeof source);
    source.layers[source.count++] = hl_vfs_cursor_native(upper);
    for (size_t index = 0; index < lower_count; index++) {
        if (lowers[index] < 0) return -EINVAL;
        source.layers[source.count++] = hl_vfs_cursor_native(lowers[index]);
    }
    snprintf(source.guest, sizeof source.guest, "/");
    return hl_vfs_cursor_clone(&source, output);
}

static int HL_VFS_CURSOR_UNUSED hl_vfs_cursor_root_authorities(const hl_vfs_cursor_authority *upper,
                                                               const hl_vfs_cursor_authority *lowers,
                                                               size_t lower_count, hl_vfs_cursor *output) {
    if (upper == NULL || upper->kind == HL_VFS_CURSOR_AUTHORITY_INVALID || output == NULL ||
        lower_count > HL_LINUX_VFS_LOWER_CAPACITY || (lower_count != 0 && lowers == NULL))
        return -EINVAL;
    hl_vfs_cursor source;
    memset(&source, 0, sizeof source);
    source.layers[source.count++] = *upper;
    for (size_t index = 0; index < lower_count; index++) {
        if (lowers[index].kind == HL_VFS_CURSOR_AUTHORITY_INVALID) return -EINVAL;
        source.layers[source.count++] = lowers[index];
    }
    snprintf(source.guest, sizeof source.guest, "/");
    return hl_vfs_cursor_clone(&source, output);
}

static int hl_vfs_cursor_native_descriptor(const hl_vfs_cursor *cursor) {
    return cursor != NULL && cursor->count != 0 && cursor->layers[0].kind == HL_VFS_CURSOR_AUTHORITY_NATIVE
               ? cursor->layers[0].value.descriptor
               : -1;
}

static int hl_vfs_cursor_component_valid(const char *component) {
    return component != NULL && component[0] && strcmp(component, ".") && strcmp(component, "..") &&
           strchr(component, '/') == NULL && strlen(component) <= 255;
}

static int hl_vfs_cursor_component_hidden(const char *component) {
    return !strncmp(component, ".wh.", 4);
}

static int hl_vfs_cursor_marker(const hl_vfs_cursor_authority *directory, const char *component) {
    char marker[260];
    int length = snprintf(marker, sizeof marker, ".wh.%s", component);
    return length > 0 && (size_t)length < sizeof marker &&
           hl_vfs_cursor_authority_metadata(directory, marker, &(struct stat){0}) == 0;
}

static int hl_vfs_cursor_opaque(const hl_vfs_cursor_authority *directory) {
    return hl_vfs_cursor_authority_metadata(directory, ".wh..wh..opq", &(struct stat){0}) == 0;
}

static uint32_t hl_vfs_mount_flags_for_guest(const char *guest, uint32_t inherited) {
    static const char *const noexec[] = {"/proc", "/sys", "/dev"};
    for (size_t index = 0; index < sizeof noexec / sizeof noexec[0]; index++) {
        size_t length = strlen(noexec[index]);
        if (!strncmp(guest, noexec[index], length) && (guest[length] == 0 || guest[length] == '/'))
            return inherited | HL_VFS_MOUNT_NOEXEC;
    }
    return inherited;
}

static void hl_vfs_cursor_entry_release(hl_vfs_cursor_entry *entry) {
    if (entry == NULL) return;
    hl_vfs_cursor_authority_close(&entry->file);
    hl_vfs_cursor_release(&entry->directory);
    memset(entry, 0, sizeof *entry);
}

static int HL_VFS_CURSOR_UNUSED hl_vfs_cursor_lookup(const hl_vfs_cursor *cursor, const char *component,
                                                     int path_only_file, hl_vfs_cursor_entry *output) {
    if (cursor == NULL || output == NULL || !hl_vfs_cursor_component_valid(component)) return -EINVAL;
    if (hl_vfs_cursor_component_hidden(component)) return -ENOENT;
    memset(output, 0, sizeof *output);

    size_t selected = cursor->count;
    struct stat selected_status;
    for (size_t index = 0; index < cursor->count; index++) {
        int metadata_error = hl_vfs_cursor_authority_metadata(&cursor->layers[index], component, &selected_status);
        if (metadata_error == 0) {
            selected = index;
            break;
        }
        // Only genuine absence permits consulting a lower layer. ENOTDIR means a higher-layer ancestor or
        // entry masks every descendant; EACCES/EIO/resource failures likewise cannot grant lower authority.
        if (metadata_error != -ENOENT) return metadata_error;
        if (hl_vfs_cursor_marker(&cursor->layers[index], component)) return -ENOENT;
    }
    if (selected == cursor->count) return -ENOENT;
    output->status = selected_status;
    char guest_entry[4200];
    int guest_length = !strcmp(cursor->guest, "/")
                           ? snprintf(guest_entry, sizeof guest_entry, "/%s", component)
                           : snprintf(guest_entry, sizeof guest_entry, "%s/%s", cursor->guest, component);
    if (guest_length < 0 || (size_t)guest_length >= sizeof guest_entry) return -ENAMETOOLONG;
    output->mount_flags = hl_vfs_mount_flags_for_guest(guest_entry, cursor->mount_flags);
    if (S_ISLNK(selected_status.st_mode)) {
        int error = hl_vfs_cursor_authority_readlink(&cursor->layers[selected], component, output->symlink,
                                                     sizeof output->symlink);
        if (error != 0) return error;
        if (path_only_file) {
            error = hl_vfs_cursor_authority_open_path(&cursor->layers[selected], component, 0, &output->file);
            if (error != 0) return error;
        }
        output->kind = HL_VFS_CURSOR_SYMLINK;
        return 0;
    }
    if (!S_ISDIR(selected_status.st_mode)) {
        hl_vfs_cursor_authority opened;
        int error = path_only_file ? hl_vfs_cursor_authority_open_path(&cursor->layers[selected], component, 0,
                                                                       &opened)
                                   : hl_vfs_cursor_authority_open_child(&cursor->layers[selected], component, 0,
                                                                        &opened);
        if (error != 0) return error;
        output->file = opened;
        output->kind = HL_VFS_CURSOR_FILE;
        return 0;
    }

    int error = path_only_file
                    ? hl_vfs_cursor_authority_open_path(&cursor->layers[selected], component, 1,
                                                        &output->directory.layers[output->directory.count])
                    : hl_vfs_cursor_authority_open_child(&cursor->layers[selected], component, 1,
                                                         &output->directory.layers[output->directory.count]);
    if (error != 0) return error;
    output->directory.count++;
    output->directory.opaque_cut = hl_vfs_cursor_opaque(&output->directory.layers[0]);
    output->directory.mount_flags = output->mount_flags;
    if (!output->directory.opaque_cut)
        for (size_t index = selected + 1; index < cursor->count; index++) {
            struct stat status;
            if (hl_vfs_cursor_marker(&cursor->layers[index], component)) break;
            if (hl_vfs_cursor_authority_metadata(&cursor->layers[index], component, &status) != 0 ||
                !S_ISDIR(status.st_mode))
                continue;
            error = path_only_file
                        ? hl_vfs_cursor_authority_open_path(&cursor->layers[index], component, 1,
                                                            &output->directory.layers[output->directory.count])
                        : hl_vfs_cursor_authority_open_child(&cursor->layers[index], component, 1,
                                                             &output->directory.layers[output->directory.count]);
            if (error != 0) {
                hl_vfs_cursor_entry_release(output);
                return error;
            }
            if (hl_vfs_cursor_opaque(&output->directory.layers[output->directory.count++])) {
                output->directory.opaque_cut = 1;
                break;
            }
        }
    int length =
        !strcmp(cursor->guest, "/")
            ? snprintf(output->directory.guest, sizeof output->directory.guest, "/%s", component)
            : snprintf(output->directory.guest, sizeof output->directory.guest, "%s/%s", cursor->guest, component);
    if (length < 0 || (size_t)length >= sizeof output->directory.guest) {
        hl_vfs_cursor_entry_release(output);
        return -ENAMETOOLONG;
    }
    int parent_error = hl_vfs_cursor_parent_create(cursor, &output->directory.parent);
    if (parent_error != 0) {
        hl_vfs_cursor_entry_release(output);
        return parent_error;
    }
    output->kind = HL_VFS_CURSOR_DIRECTORY;
    return 0;
}

#define HL_VFS_CURSOR_DEPTH 260

// Walk from retained directory provenance. `root` is the namespace restart authority for absolute paths
// and absolute symlink targets; `start` is the exact directory authority supplied by a relative dirfd.
// Every frame owns its contributing descriptors, so renaming/unlinking an ancestor cannot redirect a later
// component. The returned entry owns its file descriptor or merged-directory cursor.
static int HL_VFS_CURSOR_UNUSED hl_vfs_cursor_walk(const hl_vfs_cursor *root, const hl_vfs_cursor *start,
                                                   const char *path, int nofollow_final, int path_only_final,
                                                   hl_vfs_cursor_entry *output) {
    if (root == NULL || start == NULL || path == NULL || output == NULL || !path[0]) return -ENOENT;
    hl_vfs_cursor *frames = calloc(HL_VFS_CURSOR_DEPTH, sizeof *frames);
    if (frames == NULL) return -ENOMEM;
    size_t depth = 0;
    int error = hl_vfs_cursor_clone(path[0] == '/' ? root : start, &frames[0]);
    if (error != 0) {
        free(frames);
        return error;
    }
    char rest[8192];
    if (snprintf(rest, sizeof rest, "%s", path) >= (int)sizeof rest) {
        error = -ENAMETOOLONG;
        goto done;
    }
    int follows = 0;
    for (;;) {
        char *component = rest;
        while (*component == '/')
            component++;
        if (!*component) {
            memset(output, 0, sizeof *output);
            output->kind = HL_VFS_CURSOR_DIRECTORY;
            error = hl_vfs_cursor_clone(&frames[depth], &output->directory);
            goto done;
        }
        char *end = component;
        while (*end && *end != '/')
            end++;
        size_t component_length = (size_t)(end - component);
        if (component_length > 255) {
            error = -ENAMETOOLONG;
            goto done;
        }
        char name[256];
        memcpy(name, component, component_length);
        name[component_length] = 0;
        char tail[8192];
        if (snprintf(tail, sizeof tail, "%s", end) >= (int)sizeof tail) {
            error = -ENAMETOOLONG;
            goto done;
        }
        char *tail_component = tail;
        while (*tail_component == '/')
            tail_component++;
        int final = !*tail_component;
        if (!strcmp(name, ".")) {
            snprintf(rest, sizeof rest, "%s", tail);
            continue;
        }
        if (!strcmp(name, "..")) {
            if (depth != 0)
                hl_vfs_cursor_release(&frames[depth--]);
            else if (frames[0].parent != NULL) {
                hl_vfs_cursor parent;
                error = hl_vfs_cursor_clone(&frames[0].parent->cursor, &parent);
                if (error != 0) goto done;
                hl_vfs_cursor_release(&frames[0]);
                frames[0] = parent;
            }
            snprintf(rest, sizeof rest, "%s", tail);
            continue;
        }
        hl_vfs_cursor_entry entry;
        error = hl_vfs_cursor_lookup(&frames[depth], name, path_only_final, &entry);
        if (error != 0) goto done;
        if (entry.kind == HL_VFS_CURSOR_SYMLINK && !(final && nofollow_final)) {
            if (++follows > 40) {
                hl_vfs_cursor_entry_release(&entry);
                error = -ELOOP;
                goto done;
            }
            char next[8192];
            int length = tail[0] ? snprintf(next, sizeof next, "%s%s", entry.symlink, tail)
                                 : snprintf(next, sizeof next, "%s", entry.symlink);
            int absolute = entry.symlink[0] == '/';
            hl_vfs_cursor_entry_release(&entry);
            if (length < 0 || (size_t)length >= sizeof next) {
                error = -ENAMETOOLONG;
                goto done;
            }
            if (absolute) {
                while (depth != 0)
                    hl_vfs_cursor_release(&frames[depth--]);
                hl_vfs_cursor_release(&frames[0]);
                error = hl_vfs_cursor_clone(root, &frames[0]);
                if (error != 0) goto done;
            }
            snprintf(rest, sizeof rest, "%s", next);
            continue;
        }
        if (final) {
            *output = entry;
            error = 0;
            goto done;
        }
        if (entry.kind != HL_VFS_CURSOR_DIRECTORY) {
            hl_vfs_cursor_entry_release(&entry);
            error = -ENOTDIR;
            goto done;
        }
        if (depth + 1 >= HL_VFS_CURSOR_DEPTH) {
            hl_vfs_cursor_entry_release(&entry);
            error = -ENAMETOOLONG;
            goto done;
        }
        depth++;
        frames[depth] = entry.directory;
        memset(&entry.directory, 0, sizeof entry.directory);
        snprintf(rest, sizeof rest, "%s", tail);
    }
done:
    for (size_t frame = 0; frame <= depth; frame++)
        hl_vfs_cursor_release(&frames[frame]);
    free(frames);
    return error;
}

// Directory-descriptor provenance is deliberately separate from g_fdpath: a pathname cannot identify the
// directory object after rename/unlink. Dup aliases clone this authority; final close releases it.
static hl_vfs_cursor *g_vfs_fd_cursor[HL_NFD];

static void hl_vfs_fd_cursor_drop(int descriptor) {
    if (descriptor < 0 || descriptor >= HL_NFD || g_vfs_fd_cursor[descriptor] == NULL) return;
    hl_vfs_cursor_release(g_vfs_fd_cursor[descriptor]);
    free(g_vfs_fd_cursor[descriptor]);
    g_vfs_fd_cursor[descriptor] = NULL;
}

static int hl_vfs_fd_cursor_publish(int descriptor, const hl_vfs_cursor *cursor) {
    if (descriptor < 0 || descriptor >= HL_NFD || cursor == NULL) return -EINVAL;
    hl_vfs_cursor *copy = calloc(1, sizeof *copy);
    if (copy == NULL) return -ENOMEM;
    int error = hl_vfs_cursor_clone(cursor, copy);
    if (error != 0) {
        free(copy);
        return error;
    }
    hl_vfs_fd_cursor_drop(descriptor);
    g_vfs_fd_cursor[descriptor] = copy;
    return 0;
}

static void hl_vfs_fd_cursor_duplicate(int source, int destination) {
    if (destination < 0 || destination >= HL_NFD) return;
    hl_vfs_fd_cursor_drop(destination);
    if (source >= 0 && source < HL_NFD && g_vfs_fd_cursor[source] != NULL)
        (void)hl_vfs_fd_cursor_publish(destination, g_vfs_fd_cursor[source]);
}

static const hl_vfs_cursor *HL_VFS_CURSOR_UNUSED hl_vfs_fd_cursor_get(int descriptor) {
    return descriptor >= 0 && descriptor < HL_NFD ? g_vfs_fd_cursor[descriptor] : NULL;
}

static void hl_vfs_fd_cursor_clear(void) {
    for (int descriptor = 0; descriptor < HL_NFD; ++descriptor)
        hl_vfs_fd_cursor_drop(descriptor);
}

/* Provider handles are process-owned capabilities. A forked child must acquire its own references before
 * either process can close a cursor; retaining the COW-copied numeric handles would make ownership depend on
 * provider implementation details. Native descriptors already have kernel fork ownership and need no duplicate. */
static int hl_vfs_fd_cursor_clone_table(hl_vfs_cursor **replacements) {
    for (int descriptor = 0; descriptor < HL_NFD; ++descriptor) {
        hl_vfs_cursor *cursor = g_vfs_fd_cursor[descriptor];
        if (cursor == NULL) continue;
        int provider = 0;
        for (size_t layer = 0; layer < cursor->count; ++layer)
            provider |= cursor->layers[layer].kind == HL_VFS_CURSOR_AUTHORITY_HOST;
        if (!provider) continue;
        hl_vfs_cursor *replacement = calloc(1, sizeof *replacement);
        if (replacement == NULL || hl_vfs_cursor_clone(cursor, replacement) != 0) {
            free(replacement);
            return -1;
        }
        replacements[descriptor] = replacement;
    }
    return 0;
}

static void hl_vfs_fd_cursor_replace_table(hl_vfs_cursor **replacements) {
    for (int descriptor = 0; descriptor < HL_NFD; ++descriptor) {
        if (replacements[descriptor] == NULL) continue;
        hl_vfs_cursor_release(g_vfs_fd_cursor[descriptor]);
        free(g_vfs_fd_cursor[descriptor]);
        g_vfs_fd_cursor[descriptor] = replacements[descriptor];
        replacements[descriptor] = NULL;
    }
}

static void hl_vfs_fd_cursor_release_table(hl_vfs_cursor **replacements) {
    for (int descriptor = 0; descriptor < HL_NFD; ++descriptor) {
        if (replacements[descriptor] == NULL) continue;
        hl_vfs_cursor_release(replacements[descriptor]);
        free(replacements[descriptor]);
    }
}
