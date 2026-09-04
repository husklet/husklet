// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
static int exec_resolve_proc_path(uint64_t *path, int *self_executable) {
    static char resolved_path[4200];
    char link_path[4200];
    const char *guest_path = (const char *)(uintptr_t)*path;
    *self_executable = 0;
    if (proc_self_exe(guest_path, link_path, sizeof link_path)) {
        snprintf(resolved_path, sizeof resolved_path, "%s", link_path);
        *path = (uint64_t)(uintptr_t)resolved_path;
        *self_executable = 1;
        return 0;
    }
    int fd = procfd_num(guest_path);
    if (fd < 0) return 0;
    char host_path[4200];
    if (hl_native_fd_path(fd, host_path, sizeof host_path) != 0 || !host_path[0]) return 0;
    if (g_rootfs) {
        char mapped_path[4200];
        int mapped = guest_from_host_raw(host_path, mapped_path, sizeof mapped_path);
        if (mapped <= 0) return mapped < 0 ? mapped : -EACCES;
        snprintf(resolved_path, sizeof resolved_path, "%s", mapped_path);
    } else {
        snprintf(resolved_path, sizeof resolved_path, "%s", host_path);
    }
    *path = (uint64_t)(uintptr_t)resolved_path;
    return 0;
}

static int exec_writable_fd_matches(int fd, const struct stat *image) {
    if (exec_fd_is_engine(fd)) return 0;
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || (flags & O_ACCMODE) == O_RDONLY) return 0;
    struct stat open_file;
    return fstat(fd, &open_file) == 0 && open_file.st_dev == image->st_dev && open_file.st_ino == image->st_ino;
}

static int exec_image_is_write_open_scan(const struct stat *image, int limit) {
    if (limit < 0 || limit > (1 << 20)) limit = 4096;
    for (int fd = 0; fd < limit; fd++)
        if (exec_writable_fd_matches(fd, image)) return 1;
    return 0;
}

// A real kernel pins an executable against writable opens while committing exec. This engine reloads the image
// in-process instead, so reproduce the pre-commit ETXTBSY check against every live guest open description. Match
// by inode rather than tracked pathname: dup aliases, renamed files, unlinked files, and overlay-resolved names must
// all retain the same text-busy identity. This check intentionally precedes thread_exec_owner_handoff; a failed exec
// must not retire sibling guest threads. Guest descriptor operations have no process-wide table lock today, so this
// is the same live-table snapshot used by the CLOEXEC sweep below rather than a claim of atomic host exec exclusion.
// Both images in one enumeration. Each /proc/self/fd walk is a kernel scan of the whole fd TABLE, not of the
// open descriptors: the engine-private band is anchored at the guest ceiling (HL_LINUX_FD_LIMIT = 65536), so
// the table this process carries is >= 65536 slots for its whole life and one walk measures ~1.1 ms on this
// host against ~1.3 us before the band is adopted. The two checks below are adjacent with no intervening
// operation, so serving them from a single snapshot is exactly the snapshot either one would have taken and
// changes no window: this call remains, as the comment above says, a live-table snapshot rather than a claim
// of atomic host exec exclusion.
static int exec_images_are_write_open(const struct stat *image, const struct stat *second) {
    if (hl_linux_writable_identity_open(g_linux_box, (uint64_t)image->st_dev, (uint64_t)image->st_ino)) return 1;
    if (second != NULL &&
        hl_linux_writable_identity_open(g_linux_box, (uint64_t)second->st_dev, (uint64_t)second->st_ino))
        return 1;
    hl_host_process_fd *fds = NULL;
    size_t count = 0;
    // One enumeration, not a sizing pass plus a listing pass: on Linux each pass is a full kernel walk of
    // the fd TABLE (65536+ slots once the engine-private band is anchored), so the second pass cost as much
    // as the first and bought only a length this call already gets back.
    if (!hl_host_process_fds_collect(getpid(), &fds, &count)) {
        free(fds);
        return exec_image_is_write_open_scan(image, getdtablesize()) ||
               (second != NULL && exec_image_is_write_open_scan(second, getdtablesize()));
    }
    int busy = 0;
    for (size_t index = 0; index < count && !busy; index++) {
        busy = exec_writable_fd_matches(fds[index].descriptor, image) ||
               (second != NULL && exec_writable_fd_matches(fds[index].descriptor, second));
    }
    free(fds);
    return busy;
}

typedef struct exec_image {
    int descriptor;
    struct stat status;
    hl_dac_snapshot dac;
    hl_exec_file_capabilities file_capabilities;
    hl_linux_image bytes;
    int script;
    hl_vfs_cursor_origin origin;
    char path[4200];
} exec_image;

static int HL_VFS_CURSOR_UNUSED exec_image_has_lower_origin(const exec_image *image, int lower) {
    return image != NULL && hl_vfs_cursor_origin_is_lower(&image->origin, lower);
}

static int exec_image_open_guest(const char *guest, exec_image *image);
static void exec_image_release(exec_image *image);
static int exec_image_adopt_cursor_entry(const char *guest, hl_vfs_cursor_entry *entry, exec_image *image);

#if defined(HL_NATIVE_TEST_HOOKS) && !defined(_WIN32)
static int exec_origin_write_image(const char *path, uint8_t marker) {
    uint8_t bytes[4096] = {0};
    memcpy(bytes, "\177ELF\2\1\1", 7);
    uint16_t type = 2, machine = HL_EXEC_ELF_MACHINE, ehsize = 64, phentsize = 56, phnum = 1;
    uint32_t version = 1, load = 1, flags = 5;
    uint64_t entry = UINT64_C(0x400100), phoff = 64, offset = 0, address = UINT64_C(0x400000);
    uint64_t image_size = sizeof bytes, alignment = 4096;
    memcpy(bytes + 16, &type, 2); memcpy(bytes + 18, &machine, 2); memcpy(bytes + 20, &version, 4);
    memcpy(bytes + 24, &entry, 8); memcpy(bytes + 32, &phoff, 8); memcpy(bytes + 52, &ehsize, 2);
    memcpy(bytes + 54, &phentsize, 2); memcpy(bytes + 56, &phnum, 2); memcpy(bytes + 64, &load, 4);
    memcpy(bytes + 68, &flags, 4); memcpy(bytes + 72, &offset, 8); memcpy(bytes + 80, &address, 8);
    memcpy(bytes + 88, &address, 8); memcpy(bytes + 96, &image_size, 8); memcpy(bytes + 104, &image_size, 8);
    memcpy(bytes + 112, &alignment, 8);
    bytes[768] = marker;
    int destination = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0700);
    ssize_t written = destination >= 0 ? write(destination, bytes, sizeof bytes) : -1;
    if (destination >= 0) close(destination);
    return written == (ssize_t)sizeof bytes ? 0 : -1;
}

static int exec_origin_basic_matrix_child(int scenario) {
    char root[] = "/tmp/hl-exec-origin-XXXXXX";
    if (mkdtemp(root) == NULL) return 1;
    char upper[4200], lower[4200], volume[4200], nested[4200], name_dir[4200];
    char upper_bin[4200], lower_bin[4200], upper_file[4200], lower_file[4200], name_placeholder[4200];
    char upper_links[4200], lower_links[4200], upper_u2l[4200], lower_l2u[4200];
    char upper_hidden[4200], lower_hidden[4200], opaque[4200], hidden_file[4200];
    char upper_mnt[4200], volume_nested[4200], volume_tool[4200], nested_tool[4200];
    char name_file[4200], upper_race[4200], race_file[4200], race_new[4200];
    snprintf(upper, sizeof upper, "%s/upper", root); snprintf(lower, sizeof lower, "%s/lower", root);
    snprintf(volume, sizeof volume, "%s/volume", root); snprintf(nested, sizeof nested, "%s/nested", root);
    snprintf(name_dir, sizeof name_dir, "%s/name", root);
    snprintf(upper_bin, sizeof upper_bin, "%s/bin", upper); snprintf(lower_bin, sizeof lower_bin, "%s/bin", lower);
    snprintf(upper_file, sizeof upper_file, "%s/upper", upper_bin);
    snprintf(lower_file, sizeof lower_file, "%s/lower", lower_bin);
    snprintf(name_placeholder, sizeof name_placeholder, "%s/alias", upper_bin);
    snprintf(upper_links, sizeof upper_links, "%s/links", upper);
    snprintf(lower_links, sizeof lower_links, "%s/links", lower);
    snprintf(upper_u2l, sizeof upper_u2l, "%s/u2l", upper_links);
    snprintf(lower_l2u, sizeof lower_l2u, "%s/l2u", lower_links);
    snprintf(upper_hidden, sizeof upper_hidden, "%s/hidden", upper);
    snprintf(lower_hidden, sizeof lower_hidden, "%s/hidden", lower);
    snprintf(opaque, sizeof opaque, "%s/.wh..wh..opq", upper_hidden);
    snprintf(hidden_file, sizeof hidden_file, "%s/tool", lower_hidden);
    snprintf(upper_mnt, sizeof upper_mnt, "%s/mnt", upper);
    snprintf(volume_nested, sizeof volume_nested, "%s/nested", volume);
    snprintf(volume_tool, sizeof volume_tool, "%s/tool", volume);
    snprintf(nested_tool, sizeof nested_tool, "%s/tool", nested);
    snprintf(name_file, sizeof name_file, "%s/tool", name_dir);
    snprintf(upper_race, sizeof upper_race, "%s/race", upper);
    snprintf(race_file, sizeof race_file, "%s/tool", upper_race);
    snprintf(race_new, sizeof race_new, "%s/new", upper_race);
    if (mkdir(upper, 0700) != 0 || mkdir(lower, 0700) != 0 || mkdir(volume, 0700) != 0 ||
        mkdir(nested, 0700) != 0 || mkdir(name_dir, 0700) != 0 || mkdir(upper_bin, 0700) != 0 ||
        mkdir(lower_bin, 0700) != 0 || mkdir(upper_links, 0700) != 0 || mkdir(lower_links, 0700) != 0 ||
        mkdir(upper_hidden, 0700) != 0 || mkdir(lower_hidden, 0700) != 0 || mkdir(upper_mnt, 0700) != 0 ||
        mkdir(volume_nested, 0700) != 0 || mkdir(upper_race, 0700) != 0 ||
        exec_origin_write_image(upper_file, 1) != 0 || exec_origin_write_image(lower_file, 2) != 0 ||
        exec_origin_write_image(hidden_file, 3) != 0 || exec_origin_write_image(volume_tool, 4) != 0 ||
        exec_origin_write_image(nested_tool, 5) != 0 || exec_origin_write_image(name_file, 6) != 0 ||
        exec_origin_write_image(race_file, 7) != 0 || exec_origin_write_image(race_new, 8) != 0 ||
        symlink("/bin/lower", upper_u2l) != 0 || symlink("/bin/upper", lower_l2u) != 0 ||
        close(open(opaque, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600)) != 0 ||
        close(open(name_placeholder, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600)) != 0)
        return 2;
    hl_vfs_cursor_state_clear();
    g_nvols = 0;
    g_name_binds_count = 0;
    memset(g_vols, 0, sizeof g_vols);
    memset(g_name_binds, 0, sizeof g_name_binds);
    g_rootfs = upper;
    snprintf(g_rootfs_canon, sizeof g_rootfs_canon, "%s", upper);
    g_rootfs_canon_len = strlen(g_rootfs_canon);
    g_root_fd = open(upper, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    g_nlower = 0;
    add_lower(lower);
    exec_image image;
    int error = -EINVAL;
    int exact = 0;
    if (scenario == 0 || scenario == 1) {
        error = exec_image_open_guest(scenario == 0 ? "/bin/upper" : "/bin/lower", &image);
        exact = error == 0 && (scenario == 0 ? image.origin.kind == HL_VFS_CURSOR_ORIGIN_UPPER
                                             : exec_image_has_lower_origin(&image, 0));
    } else if (scenario == 2) {
        error = exec_image_open_guest("/hidden/tool", &image);
        exact = error == -ENOENT;
    } else if (scenario == 3 || scenario == 4) {
        error = exec_image_open_guest(scenario == 3 ? "/links/u2l" : "/links/l2u", &image);
        exact = error == 0 && (scenario == 3 ? exec_image_has_lower_origin(&image, 0)
                                             : image.origin.kind == HL_VFS_CURSOR_ORIGIN_UPPER);
    } else if (scenario == 5 || scenario == 6) {
        snprintf(g_vols[0].guest, sizeof g_vols[0].guest, "/mnt");
        g_vols[0].glen = strlen(g_vols[0].guest);
        snprintf(g_vols[0].hcanon, sizeof g_vols[0].hcanon, "%s", volume);
        g_vols[0].hlen = strlen(g_vols[0].hcanon);
        g_vols[0].fd = open(volume, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        g_vols[0].handle = HL_HOST_HANDLE_INVALID;
        snprintf(g_vols[1].guest, sizeof g_vols[1].guest, "/mnt/nested");
        g_vols[1].glen = strlen(g_vols[1].guest);
        snprintf(g_vols[1].hcanon, sizeof g_vols[1].hcanon, "%s", nested);
        g_vols[1].hlen = strlen(g_vols[1].hcanon);
        g_vols[1].fd = open(nested, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        g_vols[1].handle = HL_HOST_HANDLE_INVALID;
        g_nvols = 2;
        error = exec_image_open_guest(scenario == 5 ? "/mnt/tool" : "/mnt/nested/tool", &image);
        exact = error == 0 && image.origin.kind == HL_VFS_CURSOR_ORIGIN_VOLUME &&
                image.origin.index == (scenario == 5 ? 0 : 1);
    } else if (scenario == 7) {
        snprintf(g_name_binds[0].names[0], sizeof g_name_binds[0].names[0], "alias");
        g_name_binds[0].names_count = 1;
        snprintf(g_name_binds[0].hcanon, sizeof g_name_binds[0].hcanon, "%s", name_file);
        g_name_binds[0].fd = open(name_dir, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        g_name_binds_count = 1;
        error = exec_image_open_guest("/bin/alias", &image);
        exact = error == 0 && image.origin.kind == HL_VFS_CURSOR_ORIGIN_NAME_BIND && image.origin.index == 0;
    } else if (scenario == 8) {
        hl_vfs_cursor_entry selected;
        error = hl_vfs_cursor_resolve_at(-100, "/race/tool", 0, &selected);
        if (error == 0 && rename(race_new, race_file) == 0)
            error = exec_image_adopt_cursor_entry("/race/tool", &selected, &image);
        else if (error == 0)
            hl_vfs_cursor_entry_release(&selected);
        exact = error == 0 && image.origin.kind == HL_VFS_CURSOR_ORIGIN_UPPER && image.bytes.size > 768 &&
                image.bytes.bytes[768] == 7;
    }
    if (error == 0) exec_image_release(&image);

    if (g_name_binds_count != 0) close(g_name_binds[0].fd);
    g_name_binds_count = 0;
    for (int index = 0; index < g_nvols; index++) close(g_vols[index].fd);
    g_nvols = 0;
    hl_vfs_cursor_state_clear();
    hl_vfs_lower_state_clear();
    if (g_root_fd >= 0) close(g_root_fd);
    unlink(upper_file); unlink(lower_file); unlink(name_placeholder); unlink(upper_u2l); unlink(lower_l2u);
    unlink(opaque); unlink(hidden_file);
    unlink(volume_tool); unlink(nested_tool); unlink(name_file); unlink(race_file); unlink(race_new);
    rmdir(upper_bin); rmdir(lower_bin); rmdir(upper_links); rmdir(lower_links); rmdir(upper_hidden); rmdir(lower_hidden);
    rmdir(upper_mnt); rmdir(upper_race); rmdir(volume_nested); rmdir(volume); rmdir(nested); rmdir(name_dir);
    rmdir(upper); rmdir(lower); rmdir(root);
    return exact ? 0 : 3;
}

static int exec_origin_basic_matrix_test(void) {
    for (int scenario = 0; scenario != 9; scenario++) {
        pid_t child = fork();
        if (child < 0) return 4;
        if (child == 0) _exit(exec_origin_basic_matrix_child(scenario));
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
            return 10 + scenario;
    }
    return 0;
}
#elif defined(HL_NATIVE_TEST_HOOKS)
static int exec_origin_basic_matrix_test(void) { return 0; }
#endif

static int exec_image_capabilities(int descriptor, hl_exec_file_capabilities *capabilities) {
    unsigned char bytes[24];
    *capabilities = (hl_exec_file_capabilities){0};
#if defined(_WIN32)
    (void)descriptor;
    return 0;
#else
    const char *name = "user.hl.guest.security.capability";
    ssize_t length = hl_native_fgetxattr(descriptor, name, bytes, sizeof bytes, 0, 0);
    if (length < 0) {
        /* A filesystem with no xattr support has no file capability to apply; Linux still executes the
         * image. Nix's sandbox /build is such a filesystem on this host, where rejecting ENOTSUP made every
         * BusyBox applet exec fail with shell status 126. glibc aliases ENOATTR/ENODATA and
         * ENOTSUP/EOPNOTSUPP, so compare the second spelling only where it is genuinely distinct. */
        if (errno == ENODATA || errno == ENOTSUP
#if defined(ENOATTR) && ENOATTR != ENODATA
            || errno == ENOATTR
#endif
#if defined(EOPNOTSUPP) && EOPNOTSUPP != ENOTSUP
            || errno == EOPNOTSUPP
#endif
        )
            return 0;
        if (errno == ERANGE) return -EINVAL;
        return -errno;
    }
    if (length != 20 && length != 24) return -EINVAL;
    return hl_exec_file_capabilities_parse(bytes, (size_t)length, capabilities);
#endif
}

static void exec_image_release(exec_image *image) {
    if (image == NULL) return;
    hl_linux_image_release(&image->bytes);
    if (image->descriptor >= 0) close(image->descriptor);
    memset(image, 0, sizeof *image);
    image->descriptor = -1;
}

static int exec_image_adopt_origin(int descriptor, const char *path, hl_vfs_cursor_origin origin, exec_image *image) {
    if (descriptor < 0 || path == NULL || image == NULL) {
        if (descriptor >= 0) close(descriptor);
        return -ENOENT;
    }
    memset(image, 0, sizeof *image);
    image->descriptor = -1;
#ifdef O_PATH
    int descriptor_flags = fcntl(descriptor, F_GETFL);
    if (descriptor_flags >= 0 && (descriptor_flags & O_PATH) != 0) {
        char descriptor_path[64];
        snprintf(descriptor_path, sizeof descriptor_path, "/proc/self/fd/%d", descriptor);
        int readable = open(descriptor_path, O_RDONLY | O_CLOEXEC);
        close(descriptor);
        if (readable < 0) return -errno;
        descriptor = readable;
    }
#endif
    image->descriptor = descriptor;
    image->origin = origin;
    if (fstat(descriptor, &image->status) != 0) {
        int error = -errno;
        exec_image_release(image);
        return error;
    }
    if (!S_ISREG(image->status.st_mode)) {
        exec_image_release(image);
        return -EACCES;
    }
    stat_virt_ids_raw(&image->status, NULL, descriptor, &image->dac.uid, &image->dac.gid);
    image->dac.mode = (uint32_t)stat_virt_mode_raw(&image->status, NULL, descriptor);
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    if (hl_dac_authorize_access(&image->dac, &credentials, HL_DAC_EXECUTE) != 0) {
        exec_image_release(image);
        return -EACCES;
    }
    /* No ETXTBSY probe here. exec_prepare_request re-checks the resolved main image and the program
     * interpreter immediately before committing, and that late check is the authoritative one: it runs after
     * exec_collect_argv / exec_prepare_script / exec_prepare_interpreter, so it observes the descriptor table
     * a sibling guest thread may have mutated in the meantime, which a probe taken here cannot. Probing at
     * adopt time as well cost a second full /proc/self/fd walk per image -- and a third and fourth on any
     * dynamically linked or #! guest -- for an answer the late check re-derives. */
    if (hl_linux_image_read_fd(descriptor, &image->bytes) != 0) {
        exec_image_release(image);
        return -EACCES;
    }
    const unsigned char *header = image->bytes.bytes;
    size_t got = image->bytes.size < 20 ? image->bytes.size : 20;
    int is_elf = got >= 4 && header[0] == 0x7f && header[1] == 'E' && header[2] == 'L' && header[3] == 'F';
    image->script = got >= 2 && header[0] == '#' && header[1] == '!';
    if (!is_elf && !image->script) {
        exec_image_release(image);
        return -ENOEXEC;
    }
    if (is_elf) {
        hl_linux_elf64_layout layout;
        if (hl_linux_elf64_validate(&image->bytes, HL_EXEC_ELF_MACHINE, &layout) != 0) {
            exec_image_release(image);
            return -ENOEXEC;
        }
    }
    /* Linux classifies an invalid image before consulting security.capability.
       Keep malformed xattrs from hiding ENOEXEC/ELIBBAD for the pinned file. */
    int capability_error = exec_image_capabilities(descriptor, &image->file_capabilities);
    if (capability_error != 0) {
        exec_image_release(image);
        return capability_error;
    }
    snprintf(image->path, sizeof image->path, "%s", path);
    return 0;
}

static int exec_image_adopt(int descriptor, const char *path, exec_image *image) {
    return exec_image_adopt_origin(descriptor, path, (hl_vfs_cursor_origin){HL_VFS_CURSOR_ORIGIN_UNKNOWN, 0}, image);
}

static int exec_image_open(const char *path, exec_image *image) {
    if (path == NULL) return -ENOENT;
    int descriptor = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) return errno == ELOOP ? -EACCES : -errno;
    return exec_image_adopt(descriptor, path, image);
}

static int exec_image_adopt_cursor_entry(const char *guest, hl_vfs_cursor_entry *entry, exec_image *image) {
    if (guest == NULL || entry == NULL || entry->kind != HL_VFS_CURSOR_FILE) {
        if (entry != NULL) hl_vfs_cursor_entry_release(entry);
        return -EACCES;
    }
    hl_vfs_cursor_origin origin = entry->file.origin;
    int descriptor = -1;
    if (entry->file.kind == HL_VFS_CURSOR_AUTHORITY_NATIVE) {
        descriptor = entry->file.value.descriptor;
        entry->file = (hl_vfs_cursor_authority){0};
    } else if (entry->file.kind == HL_VFS_CURSOR_AUTHORITY_HOST && entry->file.value.host.services != NULL &&
               entry->file.value.host.services->posix_attachment != NULL) {
        const hl_host_posix_attachment_services *attachments = entry->file.value.host.services->posix_attachment;
        if (attachments->borrow_file != NULL && attachments->release != NULL) {
            hl_host_result borrowed =
                attachments->borrow_file(entry->file.value.host.services->context, entry->file.value.host.handle);
            if (borrowed.status == HL_STATUS_OK && borrowed.value <= INT_MAX)
                descriptor = fcntl((int)borrowed.value, F_DUPFD_CLOEXEC, 0);
            if (borrowed.status == HL_STATUS_OK)
                (void)attachments->release(entry->file.value.host.services->context, borrowed.value);
        }
    }
    hl_vfs_cursor_entry_release(entry);
    if (descriptor < 0) return -EACCES;
    return exec_image_adopt_origin(descriptor, guest, origin, image);
}

static int exec_image_open_guest(const char *guest, exec_image *image) {
    if (guest == NULL) return -ENOENT;
    if (!g_rootfs) return exec_image_open(guest, image);
    char absolute[4200];
    const char *resolved_guest = guest;
    if (guest[0] != '/') {
        abs_guest(-100, guest, absolute, sizeof absolute);
        resolved_guest = absolute;
    }
    int name_bind = name_bind_pick(resolved_guest);
    if (name_bind >= 0) {
        int descriptor = open(g_name_binds[name_bind].hcanon, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
        if (descriptor < 0) return errno == ELOOP ? -EACCES : -errno;
        return exec_image_adopt_origin(descriptor, resolved_guest,
                                       (hl_vfs_cursor_origin){HL_VFS_CURSOR_ORIGIN_NAME_BIND, name_bind}, image);
    }
    hl_vfs_cursor_entry entry;
    int error = hl_vfs_cursor_resolve_at(-100, resolved_guest, 0, &entry);
    if (error != 0) return error;
    return exec_image_adopt_cursor_entry(resolved_guest, &entry, image);
}

static int exec_image_authorized(const char *path, exec_image *image) {
    const unsigned char *header = g_authorized_executable_image;
    if (path == NULL || image == NULL || header == NULL || g_authorized_executable_size < 2 ||
        !g_authorized_executable_metadata_ready)
        return -ENOENT;
    memset(image, 0, sizeof *image);
    image->descriptor = -1;
    image->status = g_authorized_executable_status;
    image->dac = g_authorized_executable_dac;
    image->file_capabilities = g_authorized_executable_file_capabilities;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    if (hl_dac_authorize_access(&image->dac, &credentials, HL_DAC_EXECUTE) != 0) return -EACCES;
    if (hl_linux_image_read_bytes(header, g_authorized_executable_size, &image->bytes) != 0) return -ENOMEM;
    int is_elf =
        image->bytes.size >= 4 && header[0] == 0x7f && header[1] == 'E' && header[2] == 'L' && header[3] == 'F';
    image->script = header[0] == '#' && header[1] == '!';
    if (!is_elf && !image->script) {
        exec_image_release(image);
        return -ENOEXEC;
    }
    if (is_elf) {
        hl_linux_elf64_layout layout;
        if (hl_linux_elf64_validate(&image->bytes, HL_EXEC_ELF_MACHINE, &layout) != 0) {
            exec_image_release(image);
            return -ENOEXEC;
        }
    }
    snprintf(image->path, sizeof image->path, "%s", path);
    return 0;
}

static void exec_authority_seed_initial(const hl_host_services *host, hl_host_handle executable,
                                        const hl_executable_authority *serialized) {
    const hl_host_posix_attachment_services *attachments = host != NULL ? host->posix_attachment : NULL;
    free(g_authorized_executable_owned);
    g_authorized_executable_owned = NULL;
    g_authorized_executable_path[0] = 0;
    g_authorized_executable_metadata_ready = 0;
    memset(&g_authorized_executable_status, 0, sizeof g_authorized_executable_status);
    memset(&g_authorized_executable_dac, 0, sizeof g_authorized_executable_dac);
    memset(&g_authorized_executable_file_capabilities, 0, sizeof g_authorized_executable_file_capabilities);
    if (attachments == NULL || attachments->borrow_file == NULL || attachments->release == NULL ||
        executable == HL_HOST_HANDLE_INVALID) {
        if (serialized != NULL && serialized->ready) {
            g_authorized_executable_status.st_dev = (dev_t)serialized->stable_device;
            g_authorized_executable_status.st_ino = (ino_t)serialized->stable_object;
            g_authorized_executable_status.st_mode = (mode_t)serialized->mode;
            g_authorized_executable_dac.uid = serialized->user;
            g_authorized_executable_dac.gid = serialized->group;
            g_authorized_executable_dac.mode = hl_executable_authority_guest_mode(serialized);
            g_authorized_executable_metadata_ready = 1;
        }
        return;
    }
    hl_host_result borrowed = attachments->borrow_file(host->context, executable);
    if (borrowed.status != HL_STATUS_OK) return;
    int descriptor = (int)borrowed.value;
    if (fstat(descriptor, &g_authorized_executable_status) == 0 && S_ISREG(g_authorized_executable_status.st_mode)) {
        stat_virt_ids_raw(&g_authorized_executable_status, NULL, descriptor, &g_authorized_executable_dac.uid,
                          &g_authorized_executable_dac.gid);
        g_authorized_executable_dac.mode =
            (uint32_t)stat_virt_mode_raw(&g_authorized_executable_status, NULL, descriptor);
        g_authorized_executable_metadata_ready = 1;
        if (exec_image_capabilities(descriptor, &g_authorized_executable_file_capabilities) != 0)
            g_authorized_executable_metadata_ready = 0;
    }
    (void)attachments->release(host->context, borrowed.value);
}

static void exec_authority_rotate(exec_image *image, const char *guest_path) {
    if (image == NULL || image->bytes.bytes == NULL || guest_path == NULL) return;
    free(g_authorized_executable_owned);
    g_authorized_executable_owned = image->bytes.bytes;
    g_authorized_executable_image = image->bytes.bytes;
    g_authorized_executable_size = image->bytes.size;
    g_authorized_executable_status = image->status;
    g_authorized_executable_dac = image->dac;
    g_authorized_executable_file_capabilities = image->file_capabilities;
    g_authorized_executable_metadata_ready = 1;
    snprintf(g_authorized_executable_path, sizeof g_authorized_executable_path, "%s", guest_path);
    image->bytes.bytes = NULL;
    image->bytes.size = 0;
}

static int exec_image_parse_shebang(const exec_image *image, char *interpreter, size_t interpreter_size, char *argument,
                                    size_t argument_size) {
    if (image == NULL || !image->script || image->bytes.size <= 3) return 0;
    char header[258];
    size_t count = image->bytes.size < sizeof header - 1 ? image->bytes.size : sizeof header - 1;
    memcpy(header, image->bytes.bytes, count);
    header[count] = 0;
    char *newline = strchr(header, '\n');
    if (newline != NULL) *newline = 0;
    char *start = header + 2;
    while (*start == ' ' || *start == '\t')
        start++;
    char *end = start;
    while (*end && *end != ' ' && *end != '\t')
        end++;
    char *optional = NULL;
    if (*end) {
        *end = 0;
        optional = end + 1;
        while (*optional == ' ' || *optional == '\t')
            optional++;
        if (!*optional) optional = NULL;
    }
    snprintf(interpreter, interpreter_size, "%s", start);
    if (optional != NULL)
        snprintf(argument, argument_size, "%s", optional);
    else
        argument[0] = 0;
    return interpreter[0] != 0;
}

static int exec_resolve_shebang_images(char **argv, int argc, int capacity, exec_image *image, char store[][256]) {
    int stored = 0;
    for (int level = 0;; level++) {
        char interpreter[256], optional[256];
        if (exec_image_parse_shebang(image, interpreter, sizeof interpreter, optional, sizeof optional) != 1)
            return argc;
        if (level >= SHEBANG_MAX) return -ELOOP;
        int inserted = optional[0] ? 2 : 1;
        if (argc + inserted >= capacity) return -E2BIG;
        char *stored_interpreter = store[stored++];
        snprintf(stored_interpreter, 256, "%s", interpreter);
        char *stored_optional = NULL;
        if (optional[0]) {
            stored_optional = store[stored++];
            snprintf(stored_optional, 256, "%s", optional);
        }
        for (int index = argc; index >= 0; index--)
            argv[index + inserted] = argv[index];
        argv[0] = stored_interpreter;
        if (stored_optional != NULL) argv[1] = stored_optional;
        argc += inserted;
        exec_image next;
        int error = exec_image_open_guest(stored_interpreter, &next);
        if (error != 0) return error;
#if defined(HL_NATIVE_TEST_HOOKS)
        exec_pin_test_wait(HL_EXEC_PIN_TEST_SHEBANG_HOP);
#endif
        exec_image_release(image);
        *image = next;
    }
}

static int exec_collect_argv(uint64_t argv_address, char **argv, int *argc) {
    size_t argument_bytes = 0;
    char *probe = malloc(HL_EXEC_ARGUMENT_STRING_BYTES + 1u);
    if (!probe) return -ENOMEM;
    *argc = 0;
    int error = 0;
    while (argv_address) {
        // Fail closed at the vector bound. Stopping here silently would hand the new program a TRUNCATED
        // argv -- a different command with a different last argument -- which is strictly worse than a
        // failed exec. Linux answers -E2BIG once its byte budgets are exhausted; so do we.
        if (*argc >= HL_MAXARGV - 1) {
            error = -E2BIG;
            break;
        }
        uint64_t guest_argument = 0;
        if (guest_copy_from(&guest_argument, argv_address + (uint64_t)*argc * sizeof guest_argument,
                            sizeof guest_argument) != sizeof guest_argument) {
            error = -EFAULT;
            break;
        }
        if (!guest_argument) break;
        // Per-string MAX_ARG_STRLEN and whole-vector ARG_MAX are separate limits; take the tighter of the
        // two as this string's copy bound so either overrun surfaces as -E2BIG below.
        size_t remaining = HL_EXEC_ARGUMENT_TOTAL_BYTES - argument_bytes;
        size_t string_limit = remaining < HL_EXEC_ARGUMENT_STRING_BYTES ? remaining : HL_EXEC_ARGUMENT_STRING_BYTES;
        int length = guest_copy_string(probe, string_limit + 1, guest_argument);
        if (length == -EFAULT) {
            error = -EFAULT;
            break;
        }
        if (length < 0 || (size_t)length + 1 > string_limit) {
            error = -E2BIG;
            break;
        }
        argument_bytes += (size_t)length + 1;
        argv[(*argc)++] = (char *)nonpie_p(guest_argument);
    }
    free(probe);
    argv[*argc] = NULL;
    return error;
}

static _Thread_local int g_exec_requested_descriptor = -1;

typedef struct exec_prepared {
    char comm[256];
    char guest_executable[4200];
    char *arguments[HL_MAXARGV];
    int argument_count;
    char script_arguments[SHEBANG_MAX * 2][256];
    exec_image main_image;
    exec_image program_interpreter;
    int has_program_interpreter;
    hl_exec_environment_update environment;
    hl_exec_credential_result credentials;
} exec_prepared;

static void exec_capture_comm(const char *path, char *comm, size_t capacity) {
    const char *separator = path != NULL ? strrchr(path, '/') : NULL;
    const char *name = separator != NULL ? separator + 1 : (path != NULL ? path : "");
    size_t name_length = strnlen(name, capacity - 1);
    memcpy(comm, name, name_length);
    comm[name_length] = 0;
}

static void exec_prepared_discard(exec_prepared *prepared) {
    exec_image_release(&prepared->program_interpreter);
    exec_image_release(&prepared->main_image);
    hl_exec_environment_discard(&prepared->environment);
}

static int exec_prepare_environment(uint64_t environment_address, hl_exec_environment_update *update) {
    char *staged = exec_stage_env(environment_address);
    if (staged == NULL) return -ENOMEM;
    int error = hl_exec_environment_prepare(update, staged) == 0 ? 0 : -ENOMEM;
    free(staged);
    return error;
}

static int exec_prepare_script(uint64_t path_address, exec_prepared *prepared) {
    char *script_argv[HL_MAXARGV];
    int script_argc = 0;
    script_argv[script_argc++] = (char *)(uintptr_t)path_address;
    for (int index = 1; index < prepared->argument_count && script_argc < HL_MAXARGV - 1; index++)
        script_argv[script_argc++] = prepared->arguments[index];
    script_argv[script_argc] = NULL;

    int original_argc = script_argc;
    int resolved_argc = exec_resolve_shebang_images(script_argv, script_argc, HL_MAXARGV, &prepared->main_image,
                                                    prepared->script_arguments);
    if (resolved_argc < 0) return resolved_argc;
    if (prepared->main_image.script && resolved_argc == original_argc) return -ENOEXEC;
    if (resolved_argc != original_argc) {
        snprintf(prepared->guest_executable, sizeof prepared->guest_executable, "%s", script_argv[0]);
        for (int index = 0; index <= resolved_argc; index++)
            prepared->arguments[index] = script_argv[index];
        prepared->argument_count = resolved_argc;
    }
    return 0;
}

static int exec_prepare_interpreter(exec_prepared *prepared) {
    char interpreter_path[256];
    prepared->program_interpreter.descriptor = -1;
    prepared->has_program_interpreter = elf_interp(prepared->main_image.path, interpreter_path, sizeof interpreter_path,
                                                   &prepared->main_image.bytes) == 0;
    if (!prepared->has_program_interpreter) return 0;

    int error = exec_image_open_guest(interpreter_path, &prepared->program_interpreter);
    /* Linux reports a malformed PT_INTERP target as ELIBBAD, while the same
       bytes used as the main image are ENOEXEC. */
    return error == -ENOEXEC ? -HL_LINUX_ELIBBAD : error;
}

static int exec_prepare_request(uint64_t path_address, uint64_t argv_address, uint64_t environment_address,
                                int requested_descriptor, exec_prepared *prepared) {
    memset(prepared, 0, sizeof *prepared);
    prepared->main_image.descriptor = -1;
    prepared->program_interpreter.descriptor = -1;
    exec_capture_comm((const char *)(uintptr_t)path_address, prepared->comm, sizeof prepared->comm);

    int self_executable = 0;
    int error = requested_descriptor >= 0 ? 0 : exec_resolve_proc_path(&path_address, &self_executable);
    if (error != 0) {
        if (requested_descriptor >= 0) close(requested_descriptor);
        return error;
    }
    char path_buffer[4200];
    const char *resolved_path = requested_descriptor >= 0 ? (const char *)(uintptr_t)path_address
                                                          : atpath(-100, (const char *)(uintptr_t)path_address,
                                                                   path_buffer, sizeof path_buffer, 0);
    error = requested_descriptor >= 0 ? exec_image_adopt(requested_descriptor, resolved_path, &prepared->main_image)
            : self_executable         ? exec_image_authorized(resolved_path, &prepared->main_image)
                                      : exec_image_open_guest((const char *)(uintptr_t)path_address,
                                                              &prepared->main_image);
    if (error != 0) return error;
#if defined(HL_NATIVE_TEST_HOOKS)
    exec_pin_test_wait(HL_EXEC_PIN_TEST_MAIN);
#endif

    error = exec_collect_argv(argv_address, prepared->arguments, &prepared->argument_count);
    if (error != 0) {
        exec_image_release(&prepared->main_image);
        return error;
    }
    error = exec_prepare_environment(environment_address, &prepared->environment);
    if (error != 0) {
        exec_image_release(&prepared->main_image);
        return error;
    }

    if (g_rootfs)
        abs_guest(-100, (const char *)(uintptr_t)path_address, prepared->guest_executable,
                  sizeof prepared->guest_executable);
    else
        snprintf(prepared->guest_executable, sizeof prepared->guest_executable, "%s",
                 (const char *)(uintptr_t)path_address);

    error = exec_prepare_script(path_address, prepared);
    if (error == 0) error = exec_prepare_interpreter(prepared);
    if (error == 0 &&
        exec_images_are_write_open(&prepared->main_image.status,
                                   prepared->has_program_interpreter ? &prepared->program_interpreter.status : NULL))
        error = -ETXTBSY;
    if (error != 0) {
        exec_prepared_discard(prepared);
        return error;
    }
    prepared->credentials = cred_exec_transition(&prepared->main_image.dac, &prepared->main_image.file_capabilities);
    if (prepared->credentials.error != 0) {
        error = -prepared->credentials.error;
        exec_prepared_discard(prepared);
        return error;
    }
#if defined(HL_NATIVE_TEST_HOOKS)
    exec_pin_test_wait(HL_EXEC_PIN_TEST_FINAL);
#endif

    char canonical[4200];
    exe_canon(prepared->guest_executable, canonical, sizeof canonical);
    snprintf(prepared->guest_executable, sizeof prepared->guest_executable, "%s", canonical);
    return 0;
}

static void exec_reset_caught_signals(void) {
    for (int signal = 1; signal < 65; signal++)
        if (g_sigact[signal].handler > 1) {
            g_sigact[signal].handler = 0;
            g_sigact[signal].flags = 0;
            g_sigact[signal].mask = 0;
        }
}

static void exec_copy_reload_arguments(exec_prepared *prepared, char **path_copy, char **argument_copies) {
    *path_copy = strdup(prepared->main_image.path);
    for (int index = 0; index < prepared->argument_count && index < HL_MAXARGV - 1; index++)
        argument_copies[index] = strdup(prepared->arguments[index]);
    argument_copies[prepared->argument_count < HL_MAXARGV - 1 ? prepared->argument_count : HL_MAXARGV - 1] = NULL;
}

static void exec_release_reload_arguments(int argument_count, char *path_copy, char **argument_copies) {
    free(path_copy);
    for (int index = 0; index < argument_count && index < HL_MAXARGV - 1; index++)
        free(argument_copies[index]);
}

static void exec_reload_image(struct cpu *cpu, exec_prepared *prepared) {
    char *path_copy;
    char *argument_copies[HL_MAXARGV];
    exec_copy_reload_arguments(prepared, &path_copy, argument_copies);

    bound_mapping_reset();
    hl_gmap_reset();
    gna_reset();
    hl_gmap_lock_reset();
    g_nonpie_lo = g_nonpie_hi = 0;
    for (int index = 0; index < prepared->argument_count && index < HL_MAXARGV - 1; index++)
        prepared->arguments[index] = argument_copies[index];
    prepared->arguments[prepared->argument_count < HL_MAXARGV - 1 ? prepared->argument_count : HL_MAXARGV - 1] = NULL;

    struct loaded main_loaded;
#ifdef PCACHE_EXEC_HOOKS
    hl_identity_digest interpreter_identity = {0};
    pcache_exec_force_main();
#endif
    load_elf(path_copy, &main_loaded, NULL, &prepared->main_image.bytes);
    uint64_t jump = main_loaded.entry, at_base = 0;
    if (prepared->has_program_interpreter) {
        const char *interpreter_host = prepared->program_interpreter.path;
#ifdef PCACHE_EXEC_HOOKS
        pcache_exec_force_interp();
#endif
        struct loaded interpreter_loaded;
        load_elf(interpreter_host, &interpreter_loaded, NULL, &prepared->program_interpreter.bytes);
#ifdef PCACHE_EXEC_HOOKS
        interpreter_identity = interpreter_loaded.identity;
#endif
        jump = interpreter_loaded.entry;
        at_base = interpreter_loaded.base;
    }
    /* Exec reuses the arena from offset zero.  Advance the cache generation through the
       architecture lifecycle hook so body-owner and generated-stub provenance cannot
       describe bytes the new image is about to overwrite. */
    G_CACHE_REWIND();
    map_clear();
    pend_reset();
    memset(g_ibtc, 0, sizeof g_ibtc);
#ifdef PCACHE_EXEC_HOOKS
    pcache_exec_reload(main_loaded.identity, interpreter_identity, prepared->arguments[0], jump);
#endif
    exec_authority_rotate(&prepared->main_image, prepared->guest_executable);
    exec_image_release(&prepared->program_interpreter);
    exec_image_release(&prepared->main_image);
    G_SHADOW_CLEAR(cpu);
    exec_reset_caught_signals();

    uint64_t heap;
    uint64_t heap_hint = hl_linux_snapshot_reserve(&g_ckpt_snapshot, 256u << 20);
    if (hl_gmap_map_anonymous(heap_hint, 256u << 20, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_PRIVATE,
                              &heap) != HL_STATUS_OK)
        hl_backend_tree_abnormal_exit(127);
    brk_lo = brk_cur = heap;
    brk_hi = brk_lo + (256u << 20);
    snprintf(g_exe_path_store, sizeof g_exe_path_store, "%s", prepared->guest_executable);
    g_exe_path = g_exe_path_store;
    uint64_t stack_pointer = build_stack(prepared->argument_count, prepared->arguments, &main_loaded, at_base);
    proc_reg_publish(prepared->guest_executable, prepared->argument_count, prepared->arguments);
    exec_release_reload_arguments(prepared->argument_count, path_copy, argument_copies);

    G_RESET_REGS(cpu);
    cpu->nzcv = 0;
    cpu->alt_sp = 0;
    cpu->alt_size = 0;
    cpu->alt_flags = 2;
    G_TLS(cpu) = 0;
    G_SP(cpu) = stack_pointer;
    G_PC(cpu) = jump;
    cpu->redirect = 1;
}

static int exec_commit_request(struct cpu *cpu, exec_prepared *prepared) {
    if (!thread_exec_owner_handoff(cpu)) return -EAGAIN;
    thread_exit_others(cpu);
    exec_publish_env(&prepared->environment);
    vfork_release_parent();
    set_guest_comm(prepared->comm);
    cred_after_exec_transition(&prepared->credentials);
#ifdef PCACHE_SAVE_HOOK
    PCACHE_SAVE_HOOK;
#endif
    exec_close_cloexec();
    sysv_after_exec();
    exec_reload_image(cpu, prepared);
    return 0;
}

static int svc_proc_221(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 221: {
        int requested_descriptor = g_exec_requested_descriptor;
        g_exec_requested_descriptor = -1;
        memf_materialize_all();
        exec_prepared prepared;
        int error = exec_prepare_request(a0, a1, a2, requested_descriptor, &prepared);
        if (error == 0) error = exec_commit_request(c, &prepared);
        if (error != 0) {
            if (error == -EAGAIN) exec_prepared_discard(&prepared);
            G_RET(c) = (uint64_t)(int64_t)error;
        }
        break;
    }
    // wait4(pid, *status, opts, *rusage)
    default: return 0;
    }
    return 1;
}

static int svc_proc_exec(struct cpu *cpu, const char *logical_path, uint64_t argv, uint64_t environment,
                         int owned_descriptor) {
    if (owned_descriptor < 0)
        return svc_proc_221(cpu, 221, (uint64_t)(uintptr_t)logical_path, argv, environment, 0, 0, 0);
    if (g_exec_requested_descriptor >= 0) {
        close(owned_descriptor);
        return 0;
    }
    g_exec_requested_descriptor = owned_descriptor;
    int handled = svc_proc_221(cpu, 221, (uint64_t)(uintptr_t)logical_path, argv, environment, 0, 0, 0);
    if (g_exec_requested_descriptor >= 0) close(g_exec_requested_descriptor);
    g_exec_requested_descriptor = -1;
    return handled;
}
