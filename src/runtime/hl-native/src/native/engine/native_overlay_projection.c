/* Native overlay projection runs after the merged view is mounted and before the
 * guest enters it. This file is included by native_supervised.c so the projector
 * remains private to that launch transaction. */

static int hl_native_supervised_name_copyup(int parent, const char *name) {
    struct stat status;
    if (fstatat(parent, name, &status, AT_SYMLINK_NOFOLLOW) != 0) return -1;
    struct timespec times[2] = {status.st_atim, status.st_mtim};
    if (utimensat(parent, name, times, AT_SYMLINK_NOFOLLOW) != 0) return -1;
    if (!S_ISDIR(status.st_mode)) return 0;
    int child = openat(parent, name, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    if (child < 0) return -1;
    int iterator_fd = fcntl(child, F_DUPFD_CLOEXEC, 3);
    DIR *iterator = iterator_fd < 0 ? NULL : fdopendir(iterator_fd);
    if (iterator == NULL) {
        if (iterator_fd >= 0) close(iterator_fd);
        close(child);
        return -1;
    }
    int result = 0;
    errno = 0;
    for (struct dirent *entry = readdir(iterator); entry != NULL; entry = readdir(iterator)) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) continue;
        if (hl_native_supervised_name_copyup(child, entry->d_name) != 0) { result = -1; break; }
        errno = 0;
    }
    if (result == 0 && errno != 0) result = -1;
    int failure = errno;
    closedir(iterator);
    close(child);
    if (result != 0) errno = failure;
    return result;
}

static int hl_native_supervised_name_normalized(const char *path) {
    if (path == NULL || path[0] == 0 || path[0] == '/') return 0;
    const char *component = path;
    for (const char *cursor = path;; ++cursor) {
        if (*cursor != '/' && *cursor != 0) continue;
        size_t length = (size_t)(cursor - component);
        if (length == 0 || (length == 1 && component[0] == '.') ||
            (length == 2 && component[0] == '.' && component[1] == '.'))
            return 0;
        if (*cursor == 0) return 1;
        component = cursor + 1;
    }
}

static int hl_native_supervised_name_parent(int root, char *path, const char **leaf) {
    if (!hl_native_supervised_name_normalized(path)) return errno = EINVAL, -1;
    char *slash = strrchr(path, '/');
    char *parent = ".";
    *leaf = path;
    if (slash != NULL) {
        *slash = 0;
        parent = path;
        *leaf = slash + 1;
    }
    if ((*leaf)[0] == 0 || strcmp(*leaf, ".") == 0 || strcmp(*leaf, "..") == 0) return errno = EINVAL, -1;
    struct open_how how = {.flags = O_PATH | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW,
                           .resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS};
    return (int)syscall(SYS_openat2, root, parent, &how, sizeof how);
}

static int hl_native_supervised_name_project(int root, char *source, char *guest) {
    const char *source_leaf = NULL, *guest_leaf = NULL;
    int source_parent = hl_native_supervised_name_parent(root, source, &source_leaf);
    if (source_parent < 0) return -1;
    int guest_parent = hl_native_supervised_name_parent(root, guest, &guest_leaf);
    if (guest_parent < 0) { close(source_parent); return -1; }
    struct stat source_status;
    if (fstatat(source_parent, source_leaf, &source_status, AT_SYMLINK_NOFOLLOW) != 0) {
        int failure = errno;
        close(guest_parent); close(source_parent);
        if (failure == ENOENT) return 0;
        return errno = failure, -1;
    }
    if (S_ISDIR(source_status.st_mode) && hl_native_supervised_name_copyup(source_parent, source_leaf) != 0) {
        int failure = errno;
        close(guest_parent); close(source_parent); return errno = failure, -1;
    }
    struct stat guest_status;
    if (fstatat(guest_parent, guest_leaf, &guest_status, AT_SYMLINK_NOFOLLOW) == 0) {
        close(guest_parent); close(source_parent); return errno = EEXIST, -1;
    }
    if (errno != ENOENT) {
        int failure = errno;
        close(guest_parent); close(source_parent); return errno = failure, -1;
    }
    int projected = (int)syscall(SYS_renameat2, source_parent, source_leaf, guest_parent, guest_leaf, RENAME_NOREPLACE);
    int failure = errno;
    if (projected != 0 && (failure == ENOENT || failure == EEXIST) &&
        fstatat(source_parent, source_leaf, &source_status, AT_SYMLINK_NOFOLLOW) != 0 && errno == ENOENT)
        projected = 0;
    close(guest_parent); close(source_parent);
    if (projected != 0) errno = failure;
    return projected;
}

static int hl_native_supervised_names_apply(const char *rootfs, const char *records, int diagnostics) {
    if (records == NULL) return 0;
    int root = open(rootfs, O_PATH | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    char *copy = strdup(records);
    if (root < 0 || copy == NULL) { if (root >= 0) close(root); free(copy); return -1; }
    char *save = NULL;
    for (char *record = strtok_r(copy, "\n", &save); record != NULL; record = strtok_r(NULL, "\n", &save)) {
        char *guest = strchr(record, '\t');
        if (guest == NULL) { close(root); free(copy); return errno = EINVAL, -1; }
        *guest++ = 0;
        char source_receipt[PATH_MAX] = {0}, guest_receipt[PATH_MAX] = {0};
        int source_length = snprintf(source_receipt, sizeof source_receipt, "%s", record);
        int guest_length = snprintf(guest_receipt, sizeof guest_receipt, "%s", guest);
        int too_long = source_length < 0 || source_length >= (int)sizeof source_receipt ||
                       guest_length < 0 || guest_length >= (int)sizeof guest_receipt;
        if (too_long) errno = ENAMETOOLONG;
        if (too_long || hl_native_supervised_name_project(root, record, guest) != 0) {
            int failure = errno;
            if (diagnostics)
                fprintf(stderr, "[hl-native-supervised]\tname_source=%s guest=%s errno=%d\n",
                        source_receipt, guest_receipt, failure);
            close(root); free(copy); return errno = failure, -1;
        }
    }
    close(root); free(copy); return 0;
}

static int hl_native_supervised_owners_apply(const char *rootfs, const char *records, int diagnostics) {
    if (records == NULL) return 0;
    int root = open(rootfs, O_PATH | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    char *copy = strdup(records);
    if (root < 0 || copy == NULL) { if (root >= 0) close(root); free(copy); return -1; }
    char *save = NULL;
    for (char *record = strtok_r(copy, "\n", &save); record != NULL; record = strtok_r(NULL, "\n", &save)) {
        char *uid_text = strchr(record, '\t');
        char *gid_text = uid_text == NULL ? NULL : strchr(uid_text + 1, '\t');
        char *end_uid = NULL, *end_gid = NULL;
        if (uid_text == NULL || gid_text == NULL) { close(root); free(copy); return -1; }
        *uid_text++ = 0; *gid_text++ = 0;
        unsigned long uid = strtoul(uid_text, &end_uid, 10), gid = strtoul(gid_text, &end_gid, 10);
        struct open_how how = {.flags = O_PATH | O_CLOEXEC | O_NOFOLLOW,
                               .resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS};
        int entry = (int)syscall(SYS_openat2, root, record, &how, sizeof(how));
        struct stat status;
        int valid = record[0] != 0 && *end_uid == 0 && *end_gid == 0 && uid <= UINT_MAX && gid <= UINT_MAX;
        int inspected = entry >= 0 && fstatat(entry, "", &status, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) == 0;
        int ownership_current = inspected && status.st_uid == (uid_t)uid && status.st_gid == (gid_t)gid;
        if (!valid) errno = EINVAL;
        if (!valid || !inspected ||
            (!ownership_current && fchownat(entry, "", (uid_t)uid, (gid_t)gid,
                                            AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0)) {
            int failure = errno;
            if (diagnostics)
                fprintf(stderr, "[hl-native-supervised]\towner_path=%s operation=%s errno=%d\n", record,
                        entry < 0 ? "openat2" : !inspected ? "fstat" : "validate-or-chown", failure);
            if (entry >= 0) close(entry);
            close(root);
            free(copy);
            errno = failure;
            return -1;
        }
        close(entry);
    }
    close(root); free(copy); return 0;
}
