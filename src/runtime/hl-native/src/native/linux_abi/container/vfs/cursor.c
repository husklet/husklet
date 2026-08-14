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

typedef struct hl_vfs_cursor {
    int descriptors[HL_VFS_CURSOR_LAYERS];
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
    int descriptor;
    struct stat status;
    uint32_t mount_flags;
    hl_vfs_cursor directory;
    char symlink[4096];
} hl_vfs_cursor_entry;

static void hl_vfs_cursor_release(hl_vfs_cursor *cursor);

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
        if (cursor->descriptors[index] >= 0) close(cursor->descriptors[index]);
    hl_vfs_cursor_parent_release(cursor->parent);
    memset(cursor, 0, sizeof *cursor);
    for (size_t index = 0; index < HL_VFS_CURSOR_LAYERS; index++)
        cursor->descriptors[index] = -1;
}

static int hl_vfs_cursor_clone(const hl_vfs_cursor *source, hl_vfs_cursor *output) {
    if (source == NULL || output == NULL || source->count > HL_VFS_CURSOR_LAYERS) return -EINVAL;
    memset(output, 0, sizeof *output);
    for (size_t index = 0; index < HL_VFS_CURSOR_LAYERS; index++)
        output->descriptors[index] = -1;
    output->opaque_cut = source->opaque_cut;
    output->mount_flags = source->mount_flags;
    output->parent = source->parent;
    hl_vfs_cursor_parent_retain(output->parent);
    snprintf(output->guest, sizeof output->guest, "%s", source->guest);
    for (size_t index = 0; index < source->count; index++) {
        int descriptor = fcntl(source->descriptors[index], F_DUPFD_CLOEXEC, 0);
        if (descriptor < 0) {
            int error = -errno;
            hl_vfs_cursor_release(output);
            return error;
        }
        output->descriptors[output->count++] = descriptor;
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
    for (size_t index = 0; index < HL_VFS_CURSOR_LAYERS; index++)
        source.descriptors[index] = -1;
    source.descriptors[source.count++] = upper;
    for (size_t index = 0; index < lower_count; index++) {
        if (lowers[index] < 0) return -EINVAL;
        source.descriptors[source.count++] = lowers[index];
    }
    snprintf(source.guest, sizeof source.guest, "/");
    return hl_vfs_cursor_clone(&source, output);
}

static int hl_vfs_cursor_component_valid(const char *component) {
    return component != NULL && component[0] && strcmp(component, ".") && strcmp(component, "..") &&
           strchr(component, '/') == NULL && strlen(component) <= 255;
}

static int hl_vfs_cursor_component_hidden(const char *component) {
    return !strncmp(component, ".wh.", 4);
}

static int hl_vfs_cursor_marker(int directory, const char *component) {
    char marker[260];
    int length = snprintf(marker, sizeof marker, ".wh.%s", component);
    return length > 0 && (size_t)length < sizeof marker &&
           fstatat(directory, marker, &(struct stat){0}, AT_SYMLINK_NOFOLLOW) == 0;
}

static int hl_vfs_cursor_opaque(int directory) {
    return fstatat(directory, ".wh..wh..opq", &(struct stat){0}, AT_SYMLINK_NOFOLLOW) == 0;
}

static void hl_vfs_cursor_entry_release(hl_vfs_cursor_entry *entry) {
    if (entry == NULL) return;
    if (entry->descriptor >= 0) close(entry->descriptor);
    hl_vfs_cursor_release(&entry->directory);
    memset(entry, 0, sizeof *entry);
    entry->descriptor = -1;
}

static int HL_VFS_CURSOR_UNUSED hl_vfs_cursor_lookup(const hl_vfs_cursor *cursor, const char *component,
                                                     hl_vfs_cursor_entry *output) {
    if (cursor == NULL || output == NULL || !hl_vfs_cursor_component_valid(component)) return -EINVAL;
    if (hl_vfs_cursor_component_hidden(component)) return -ENOENT;
    memset(output, 0, sizeof *output);
    output->descriptor = -1;
    for (size_t index = 0; index < HL_VFS_CURSOR_LAYERS; index++)
        output->directory.descriptors[index] = -1;

    size_t selected = cursor->count;
    struct stat selected_status;
    for (size_t index = 0; index < cursor->count; index++) {
        if (fstatat(cursor->descriptors[index], component, &selected_status, AT_SYMLINK_NOFOLLOW) == 0) {
            selected = index;
            break;
        }
        // Only genuine absence permits consulting a lower layer. ENOTDIR means a higher-layer ancestor or
        // entry masks every descendant; EACCES/EIO/resource failures likewise cannot grant lower authority.
        if (errno != ENOENT) return -errno;
        if (hl_vfs_cursor_marker(cursor->descriptors[index], component)) return -ENOENT;
    }
    if (selected == cursor->count) return -ENOENT;
    output->status = selected_status;
    output->mount_flags = cursor->mount_flags;
    if (S_ISLNK(selected_status.st_mode)) {
        ssize_t length =
            readlinkat(cursor->descriptors[selected], component, output->symlink, sizeof output->symlink - 1);
        if (length < 0) return -errno;
        output->symlink[length] = 0;
        output->kind = HL_VFS_CURSOR_SYMLINK;
        return 0;
    }
    if (!S_ISDIR(selected_status.st_mode)) {
        output->descriptor = openat(cursor->descriptors[selected], component, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
        if (output->descriptor < 0) return -errno;
        output->kind = HL_VFS_CURSOR_FILE;
        return 0;
    }

    int selected_directory =
        openat(cursor->descriptors[selected], component, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    if (selected_directory < 0) return -errno;
    output->directory.descriptors[output->directory.count++] = selected_directory;
    output->directory.opaque_cut = hl_vfs_cursor_opaque(selected_directory);
    output->directory.mount_flags = cursor->mount_flags;
    if (!output->directory.opaque_cut)
        for (size_t index = selected + 1; index < cursor->count; index++) {
            struct stat status;
            if (hl_vfs_cursor_marker(cursor->descriptors[index], component)) break;
            if (fstatat(cursor->descriptors[index], component, &status, AT_SYMLINK_NOFOLLOW) != 0 ||
                !S_ISDIR(status.st_mode))
                continue;
            int directory = openat(cursor->descriptors[index], component,
                                   O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
            if (directory < 0) {
                int error = -errno;
                hl_vfs_cursor_entry_release(output);
                return error;
            }
            output->directory.descriptors[output->directory.count++] = directory;
            if (hl_vfs_cursor_opaque(directory)) {
                output->directory.opaque_cut = 1;
                break;
            }
        }
    int length = !strcmp(cursor->guest, "/")
                     ? snprintf(output->directory.guest, sizeof output->directory.guest, "/%s", component)
                     : snprintf(output->directory.guest, sizeof output->directory.guest, "%s/%s", cursor->guest,
                                component);
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
                                                   const char *path, int nofollow_final,
                                                   hl_vfs_cursor_entry *output) {
    if (root == NULL || start == NULL || path == NULL || output == NULL || !path[0]) return -ENOENT;
    hl_vfs_cursor *frames = calloc(HL_VFS_CURSOR_DEPTH, sizeof *frames);
    if (frames == NULL) return -ENOMEM;
    for (size_t frame = 0; frame < HL_VFS_CURSOR_DEPTH; frame++)
        for (size_t layer = 0; layer < HL_VFS_CURSOR_LAYERS; layer++)
            frames[frame].descriptors[layer] = -1;
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
            output->descriptor = -1;
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
        error = hl_vfs_cursor_lookup(&frames[depth], name, &entry);
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
        entry.descriptor = -1;
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
