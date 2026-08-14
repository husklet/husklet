#define HL_VFS_CURSOR_LAYERS (HL_LINUX_VFS_LOWER_CAPACITY + 1)
#if defined(__GNUC__) || defined(__clang__)
#define HL_VFS_CURSOR_UNUSED __attribute__((unused))
#else
#define HL_VFS_CURSOR_UNUSED
#endif

typedef struct hl_vfs_cursor {
    int descriptors[HL_VFS_CURSOR_LAYERS];
    size_t count;
    int opaque_cut;
    char guest[4200];
} hl_vfs_cursor;

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
    hl_vfs_cursor directory;
    char symlink[4096];
} hl_vfs_cursor_entry;

static void hl_vfs_cursor_release(hl_vfs_cursor *cursor) {
    if (cursor == NULL) return;
    for (size_t index = 0; index < cursor->count; index++)
        if (cursor->descriptors[index] >= 0) close(cursor->descriptors[index]);
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
        if (errno != ENOENT && errno != ENOTDIR) return -errno;
        if (hl_vfs_cursor_marker(cursor->descriptors[index], component)) return -ENOENT;
    }
    if (selected == cursor->count) return -ENOENT;
    output->status = selected_status;
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
    output->kind = HL_VFS_CURSOR_DIRECTORY;
    return 0;
}
