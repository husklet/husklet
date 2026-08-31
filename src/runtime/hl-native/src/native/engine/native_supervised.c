static int hl_native_supervised_selected(const hl_options *options) {
    const char *value = hl_options_get(options, "HL_NATIVE_SUPERVISED");
    return value != NULL && value[0] != 0 && value[0] != '0';
}

#if defined(__linux__) && defined(__x86_64__)
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/futex.h>
#include <linux/seccomp.h>
#include <linux/capability.h>
#include <linux/sched.h>
#include <linux/openat2.h>
#include <linux/mount.h>
#include <sched.h>
#include <grp.h>
#include <poll.h>
#include <dirent.h>
#include <limits.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <sys/uio.h>
#include <termios.h>
#include <net/if.h>

static int hl_native_supervised_available(void) { return 1; }

uint64_t hl_linux_abi_constructed(void);
uint64_t hl_linux_abi_destroyed(void);

typedef struct {
    _Atomic int listener;
    _Atomic int target_pid;
    _Atomic int acknowledged;
    _Atomic int result_signal;
    _Atomic int projected_overlay;
    _Atomic int clone_stages;
#if defined(HL_NATIVE_TEST_HOOKS)
    _Atomic int listener_wakes;
#endif
    char projected_root[PATH_MAX];
} hl_native_supervised_bootstrap;

static int hl_native_supervised_listener_wait(hl_native_supervised_bootstrap *bootstrap, int leader_pidfd,
                                              const hl_options *options) {
    struct pollfd death = {leader_pidfd, POLLIN, 0};
#if defined(HL_NATIVE_TEST_HOOKS)
    const char *test = hl_options_get(options, "HL_NATIVE_SUPERVISED_REFUSE");
    if (test != NULL && strcmp(test, "994:38") == 0) usleep(10000);
#endif
    for (int attempt = 0; attempt < 5000; ++attempt) {
        int remote = atomic_load_explicit(&bootstrap->listener, memory_order_acquire);
        if (remote >= 0) {
#if defined(HL_NATIVE_TEST_HOOKS)
            if (test != NULL && (strcmp(test, "993:38") == 0 || strcmp(test, "994:38") == 0)) {
                int wake_receipt;
                for (int attempt = 0; attempt < 1000; ++attempt) {
                    wake_receipt = atomic_load_explicit(&bootstrap->listener_wakes, memory_order_acquire);
                    if (wake_receipt != 0) break;
                    sched_yield();
                }
                int expected = strcmp(test, "993:38") == 0 ? 2 : 1;
                if (wake_receipt != expected) return -1;
            }
#endif
            return (int)syscall(SYS_pidfd_getfd, leader_pidfd, remote, 0);
        }
        if (poll(&death, 1, 0) != 0) break;
        struct timespec timeout = {.tv_sec = 0, .tv_nsec = 1000000};
        int waited;
#if defined(HL_NATIVE_TEST_HOOKS)
        if (test != NULL && strcmp(test, "995:38") == 0 && attempt == 0) {
            errno = EINTR;
            waited = -1;
        } else
#endif
            waited = (int)syscall(SYS_futex, &bootstrap->listener, FUTEX_WAIT, -1, &timeout, NULL, 0);
        if (waited != 0 && errno != EAGAIN && errno != EINTR && errno != ETIMEDOUT) break;
    }
    return -1;
}

static void hl_native_supervised_projection_cleanup(hl_native_supervised_bootstrap *bootstrap) {
    if (bootstrap != NULL && atomic_load_explicit(&bootstrap->projected_overlay, memory_order_acquire))
        (void)rmdir(bootstrap->projected_root);
}

typedef struct {
    int source;
    int read_only;
    char guest[PATH_MAX];
} hl_native_supervised_volume;

typedef struct {
    hl_native_supervised_volume entries[32];
    size_t count;
} hl_native_supervised_volumes;

static int hl_native_supervised_write_text(const char *path, const char *text) {
    int fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0) return -1;
    size_t length = strlen(text);
    int result = write(fd, text, length) == (ssize_t)length ? 0 : -1;
    close(fd);
    return result;
}

static int hl_native_supervised_write_process_text(pid_t process, const char *name, const char *text) {
    char path[64];
    if (snprintf(path, sizeof(path), "/proc/%d/%s", process, name) >= (int)sizeof(path)) return -1;
    return hl_native_supervised_write_text(path, text);
}

static int hl_native_supervised_close_except(int keep) {
#ifdef SYS_close_range
    int first = keep > 3 ? (int)syscall(SYS_close_range, 3u, (unsigned int)keep - 1u, 0) : 0;
    int second = syscall(SYS_close_range, (unsigned int)keep + 1u, UINT_MAX, 0);
    if (first == 0 && second == 0) return 0;
    if (errno != ENOSYS && errno != EINVAL) return -1;
#endif
    DIR *directory = opendir("/proc/self/fd");
    if (directory == NULL) return -1;
    int scan = dirfd(directory);
    struct dirent *entry;
    while ((entry = readdir(directory)) != NULL) {
        char *end = NULL;
        long fd = strtol(entry->d_name, &end, 10);
        if (*entry->d_name == 0 || *end != 0 || fd < 3 || fd == keep || fd == scan) continue;
        close((int)fd);
    }
    return closedir(directory);
}

static int hl_native_supervised_guest_path_valid(const char *path) {
    if (path == NULL || path[0] != '/' || path[1] == 0) return 0;
    for (const char *part = path + 1; *part;) {
        const char *end = strchr(part, '/');
        size_t length = end == NULL ? strlen(part) : (size_t)(end - part);
        if (length == 0 || (length == 1 && part[0] == '.') ||
            (length == 2 && part[0] == '.' && part[1] == '.'))
            return 0;
        if (end == NULL) break;
        part = end + 1;
    }
    return 1;
}

static int hl_native_supervised_path_contains(const char *parent, const char *child) {
    size_t length = strlen(parent);
    return strncmp(parent, child, length) == 0 && (child[length] == 0 || child[length] == '/');
}

static int hl_native_supervised_volumes_open(const char *spec, hl_native_supervised_volumes *volumes) {
    memset(volumes, 0, sizeof(*volumes));
    if (spec == NULL) return 0;
    char *copy = strdup(spec);
    if (copy == NULL) return -1;
    char *save = NULL;
    for (char *record = strtok_r(copy, ",", &save); record != NULL; record = strtok_r(NULL, ",", &save)) {
        if (volumes->count == 32) goto failed;
        int read_only = 0;
        if (strncmp(record, "ro:", 3) == 0) { read_only = 1; record += 3; }
        else if (strncmp(record, "rw:", 3) == 0) record += 3;
        char *colon = strchr(record, ':');
        if (colon == NULL) goto failed;
        *colon++ = 0;
        if (!hl_native_supervised_guest_path_valid(record) || colon[0] != '/' || strchr(colon, ':') != NULL ||
            strlen(record) >= sizeof(volumes->entries[0].guest))
            goto failed;
        if (hl_native_supervised_path_contains("/proc", record)) goto failed;
        for (size_t index = 0; index < volumes->count; ++index)
            if (hl_native_supervised_path_contains(volumes->entries[index].guest, record) ||
                hl_native_supervised_path_contains(record, volumes->entries[index].guest))
                goto failed;
        int host_root = open("/", O_PATH | O_DIRECTORY | O_CLOEXEC);
        struct open_how source_how = {.flags = O_PATH | O_DIRECTORY | O_CLOEXEC,
                                      .resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS};
        int source = host_root < 0 ? -1 : (int)syscall(SYS_openat2, host_root, colon + 1, &source_how, sizeof(source_how));
        if (host_root >= 0) close(host_root);
        if (source < 0) goto failed;
        int tree = (int)syscall(SYS_open_tree, AT_FDCWD, colon, OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_RECURSIVE);
        struct stat source_status, tree_status;
        if (tree < 0 || fstat(source, &source_status) != 0 || fstat(tree, &tree_status) != 0 ||
            source_status.st_dev != tree_status.st_dev || source_status.st_ino != tree_status.st_ino) {
            if (tree >= 0) close(tree);
            close(source);
            goto failed;
        }
        close(source);
        hl_native_supervised_volume *volume = &volumes->entries[volumes->count++];
        volume->source = tree;
        volume->read_only = read_only;
        strcpy(volume->guest, record);
    }
    free(copy);
    return 0;
failed:
    for (size_t index = 0; index < volumes->count; ++index) close(volumes->entries[index].source);
    free(copy);
    return -1;
}

static int hl_native_supervised_volumes_mount(const char *rootfs, const hl_native_supervised_volumes *volumes) {
    int root = open(rootfs, O_PATH | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    if (root < 0) return -1;
    for (size_t index = 0; index < volumes->count; ++index) {
        const hl_native_supervised_volume *volume = &volumes->entries[index];
        struct open_how how = {.flags = O_PATH | O_DIRECTORY | O_CLOEXEC,
                               .resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS};
        int target = (int)syscall(SYS_openat2, root, volume->guest + 1, &how, sizeof(how));
        if (target < 0) { close(root); return -1; }
        int tree = volume->source;
        struct mount_attr attributes = {.attr_set = MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV |
                                                     (volume->read_only ? MOUNT_ATTR_RDONLY : 0)};
        if (tree < 0 || syscall(SYS_mount_setattr, tree, "", AT_EMPTY_PATH | AT_RECURSIVE, &attributes, sizeof(attributes)) != 0 ||
            syscall(SYS_move_mount, tree, "", target, "", MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH) != 0) {
            if (tree >= 0) close(tree);
            close(target); close(root); return -1;
        }
        close(tree);
        close(target);
    }
    close(root);
    return 0;
}

static int hl_native_supervised_overlay_mount(const hl_engine_config *config, const hl_options *options,
                                              char target[PATH_MAX]) {
    const char *lower = config->box->lower_layers;
    const char *work = hl_options_get(options, "HL_OVERLAY_WORK");
    if (lower == NULL) {
        if (snprintf(target, PATH_MAX, "%s", config->rootfs) >= PATH_MAX) return -1;
        return 0;
    }
    if (work == NULL || strchr(lower, '\n') != NULL) return -1;
    if (snprintf(target, PATH_MAX, "/var/tmp/husklet-native-overlay.XXXXXX") >= PATH_MAX || mkdtemp(target) == NULL)
        return -1;
    int filesystem = (int)syscall(SYS_fsopen, "overlay", FSOPEN_CLOEXEC);
    int mounted = -1;
    if (filesystem >= 0 && syscall(SYS_fsconfig, filesystem, FSCONFIG_SET_STRING, "lowerdir", lower, 0) == 0 &&
        syscall(SYS_fsconfig, filesystem, FSCONFIG_SET_STRING, "upperdir", config->rootfs, 0) == 0 &&
        syscall(SYS_fsconfig, filesystem, FSCONFIG_SET_STRING, "workdir", work, 0) == 0 &&
        syscall(SYS_fsconfig, filesystem, FSCONFIG_CMD_CREATE, NULL, NULL, 0) == 0) {
        int tree = (int)syscall(SYS_fsmount, filesystem, FSMOUNT_CLOEXEC, 0);
        int directory = open(target, O_PATH | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
        if (tree >= 0 && directory >= 0 &&
            syscall(SYS_move_mount, tree, "", directory, "", MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH) == 0)
            mounted = 0;
        if (directory >= 0) close(directory);
        if (tree >= 0) close(tree);
    }
    if (filesystem >= 0) close(filesystem);
    if (mounted != 0) rmdir(target);
    return mounted;
}

static int hl_native_supervised_owners_apply(const char *rootfs, const char *records) {
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
        if (record[0] == 0 || *end_uid != 0 || *end_gid != 0 || uid > UINT_MAX || gid > UINT_MAX || entry < 0 ||
            fchownat(entry, "", (uid_t)uid, (gid_t)gid, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0) {
            if (entry >= 0) close(entry);
            close(root);
            free(copy);
            return -1;
        }
        close(entry);
    }
    close(root); free(copy); return 0;
}

static int hl_native_supervised_id_compare(const void *left, const void *right) {
    uint32_t a = *(const uint32_t *)left, b = *(const uint32_t *)right;
    return a > b ? 1 : a < b ? -1 : 0;
}

static int hl_native_supervised_id_map(char *output, size_t capacity, uint32_t process_id, const char *records,
                                       int gid_column) {
    size_t count = 1, allocated = 16;
    uint32_t *ids = malloc(allocated * sizeof(*ids));
    char *copy = records == NULL ? NULL : strdup(records);
    if (ids == NULL || (records != NULL && copy == NULL)) { free(ids); free(copy); return -1; }
    ids[0] = process_id;
    char *save = NULL;
    for (char *record = copy == NULL ? NULL : strtok_r(copy, "\n", &save); record != NULL;
         record = strtok_r(NULL, "\n", &save)) {
        char *uid_text = strchr(record, '\t');
        char *gid_text = uid_text == NULL ? NULL : strchr(uid_text + 1, '\t');
        char *text = gid_column ? (gid_text == NULL ? NULL : gid_text + 1) : (uid_text == NULL ? NULL : uid_text + 1);
        char *end = NULL;
        unsigned long value = text == NULL ? ULONG_MAX : strtoul(text, &end, 10);
        if (!gid_column && gid_text != NULL) *gid_text = 0;
        if (text == NULL || *end != 0 || value > UINT_MAX) { free(ids); free(copy); return -1; }
        if (count == allocated) {
            allocated *= 2;
            uint32_t *grown = realloc(ids, allocated * sizeof(*ids));
            if (grown == NULL) { free(ids); free(copy); return -1; }
            ids = grown;
        }
        ids[count++] = (uint32_t)value;
    }
    qsort(ids, count, sizeof(*ids), hl_native_supervised_id_compare);
    size_t used = 0, extents = 0;
    for (size_t index = 0; index < count;) {
        uint32_t first = ids[index], last = first;
        while (++index < count && (ids[index] == last || (last != UINT_MAX && ids[index] == last + 1)))
            if (ids[index] != last) last = ids[index];
        int length = snprintf(output + used, capacity - used, "%u %u %llu\n", first, first,
                              (unsigned long long)last - first + 1);
        if (length <= 0 || (size_t)length >= capacity - used || ++extents > 340) {
            free(ids); free(copy); return -1;
        }
        used += (size_t)length;
    }
    free(ids); free(copy); return 0;
}

static const char *hl_native_supervised_policy_rejection(const hl_engine_config *config) {
    const hl_engine_box_config *box = config->box;
    if (geteuid() != 0 || getegid() != 0) return "host-root-required";
    if (config->rootfs == NULL || box == NULL) return "typed-box-and-rootfs-required";
    if (config->memory_limit != 0 || config->pid_limit != 0 || config->cpu_limit != 0) return "cgroup-limits";
    if (box->uid < -1 || box->gid < -1) return "identity";
    if (box->lower_layers != NULL && strchr(box->lower_layers, '\n') != NULL) return "multiple-lower-layers";
    if (box->publish_count != 0) return "published-network";
    if (box->network_interface_count != 0 || box->network_bridge != NULL || box->ip != NULL ||
        box->egress_proxy != NULL)
        return "bridged-network";
    /* The generation file invalidates the translated backend's user-space pathname caches after a
     * daemon-side write.  Native-supervised has no such cache: every lookup goes through the kernel
     * VFS, so retaining the typed field is semantics-preserving and requires no poll or mapping. */
    if (box->file_owners != NULL && box->lower_layers == NULL) return "ownership-without-overlay";
    if (box->checkpoint_mode != 0 || box->checkpoint_policy != 0) return "checkpoint";
    int isolated = (box->flags & HL_ENGINE_BOX_NETWORK_ISOLATED) != 0;
    if (box->network_mode == 2) {
        if (isolated || box->network_namespace != NULL) return "host-network-policy";
    } else if (box->network_mode == 0) {
        if (!isolated) return "bridged-network";
    } else {
        return "network-mode";
    }
    if ((box->flags & ~(HL_ENGINE_BOX_ROOTFS_READ_ONLY | HL_ENGINE_BOX_NETWORK_ISOLATED |
                        HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED)) != 0)
        return "box-flags";
    return NULL;
}

static int hl_native_supervised_loopback_up(void) {
    int socket_fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    if (socket_fd < 0) return -1;
    struct ifreq request = {0};
    memcpy(request.ifr_name, "lo", 3);
    int result = ioctl(socket_fd, SIOCGIFFLAGS, &request);
    if (result == 0) {
        request.ifr_flags |= IFF_UP | IFF_RUNNING;
        result = ioctl(socket_fd, SIOCSIFFLAGS, &request);
    }
    int failure = errno;
    close(socket_fd);
    errno = failure;
    return result;
}

/* An isolated native guest owns a private UTS namespace but has no DNS path.
 * Keep its own hostname local, as the translated network does, without
 * modifying the image's identity file: bind a mode/owner-preserving copy over
 * the existing /etc/hosts only inside this mount namespace. */
static int hl_native_supervised_hostname_valid(const char *hostname) {
    size_t hostname_length = strlen(hostname);
    int valid_hostname = hostname_length > 0 && hostname_length <= HOST_NAME_MAX;
    for (size_t index = 0; valid_hostname && index < hostname_length; ++index) {
        unsigned char byte = (unsigned char)hostname[index];
        int alphanumeric = (byte >= 'a' && byte <= 'z') || (byte >= 'A' && byte <= 'Z') ||
                           (byte >= '0' && byte <= '9');
        if (!alphanumeric && byte != '-' && byte != '.') valid_hostname = 0;
        if (byte == '-' && (index == 0 || index + 1 == hostname_length || hostname[index - 1] == '.' ||
                            hostname[index + 1] == '.')) valid_hostname = 0;
        if (byte == '.' && (index == 0 || index + 1 == hostname_length || hostname[index - 1] == '.' ||
                            hostname[index - 1] == '-')) valid_hostname = 0;
    }
    return valid_hostname;
}

static int hl_native_supervised_project_hostname(const char *root, const char *hostname, int read_only) {
    char inherited[HOST_NAME_MAX + 1];
    if (hostname == NULL || hostname[0] == 0) {
        if (gethostname(inherited, HOST_NAME_MAX) != 0) return -1;
        inherited[HOST_NAME_MAX] = 0;
        hostname = inherited;
    }
    if (!hl_native_supervised_hostname_valid(hostname)) {
        errno = EINVAL;
        return -1;
    }
    int rootfd = open(root, O_PATH | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    if (rootfd < 0) return -1;
    struct open_how hosts_how = {
        .flags = O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK,
        .resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    };
    int input = (int)syscall(SYS_openat2, rootfd, "etc/hosts", &hosts_how, sizeof(hosts_how));
    int open_failure = errno;
    close(rootfd);
    errno = open_failure;
    if (input < 0 && errno == ENOENT) return 0;
    if (input < 0) return -1;
    struct stat metadata;
    if (fstat(input, &metadata) != 0 || !S_ISREG(metadata.st_mode)) {
        int failure = errno != 0 ? errno : EINVAL;
        close(input);
        errno = failure;
        return -1;
    }
    char pinned_target[64];
    if (snprintf(pinned_target, sizeof pinned_target, "/proc/self/fd/%d", input) >= (int)sizeof pinned_target) {
        close(input);
        errno = ENAMETOOLONG;
        return -1;
    }
    char temporary[] = "/var/tmp/husklet-native-hosts.XXXXXX";
    int output = mkstemp(temporary);
    if (output < 0) { close(input); return -1; }
    int exact = fchown(output, metadata.st_uid, metadata.st_gid) == 0 &&
                fchmod(output, metadata.st_mode & 07777) == 0;
    char *contents = malloc(1024u * 1024u + 1);
    if (contents == NULL) { close(input); close(output); unlink(temporary); return -1; }
    size_t total = 0;
    while (exact) {
        ssize_t count = read(input, contents + total, 1024u * 1024u + 1 - total);
        if (count < 0) { if (errno == EINTR) continue; exact = 0; break; }
        if (count == 0) break;
        total += (size_t)count;
        if (total > 1024u * 1024u) { errno = EFBIG; exact = 0; break; }
    }
    if (exact) {
        char record[HOST_NAME_MAX + 16];
        int length = snprintf(record, sizeof record, "127.0.1.1\t%s\n", hostname);
        exact = length > 0 && (size_t)length < sizeof record && write(output, record, (size_t)length) == length;
        if (!exact && errno == 0) errno = EINVAL;
    }
    size_t written = 0;
    while (exact && written < total) {
        ssize_t step = write(output, contents + written, total - written);
        if (step < 0 && errno == EINTR) continue;
        if (step <= 0) { exact = 0; break; }
        written += (size_t)step;
    }
    free(contents);
    int failure = errno;
    if (close(output) != 0 && exact) { exact = 0; failure = errno; }
    if (exact) {
        /* Mount through the descriptor pinned above: pathname replacement cannot redirect the target. */
        exact = mount(temporary, pinned_target, NULL, MS_BIND, NULL) == 0;
        if (!exact) failure = errno;
    }
    if (exact && read_only) {
        exact = mount(NULL, pinned_target, NULL, MS_BIND | MS_REMOUNT | MS_RDONLY, NULL) == 0;
        if (!exact) failure = errno;
    }
    close(input);
    (void)unlink(temporary);
    if (!exact) { errno = failure; return -1; }
    return 0;
}

#if defined(HL_NATIVE_TEST_HOOKS) && defined(HL_NATIVE_TEST_HOOK_EXPORT)
HL_API int hl_native_supervised_hostname_projection_test(uint32_t scenario) {
    static const char *const hostile[] = {"line\nbreak", "white space", "under_score", "control\001byte"};
    if (scenario == 4) {
        char root[] = "/var/tmp/husklet-hostname-root.XXXXXX";
        char outside[] = "/var/tmp/husklet-hostname-outside.XXXXXX";
        if (mkdtemp(root) == NULL || mkdtemp(outside) == NULL) return 97;
        char outside_hosts[PATH_MAX], etc[PATH_MAX];
        int status = 0;
        if (snprintf(outside_hosts, sizeof outside_hosts, "%s/hosts", outside) >= (int)sizeof outside_hosts ||
            snprintf(etc, sizeof etc, "%s/etc", root) >= (int)sizeof etc) status = 98;
        static const char original[] = "127.0.0.1\toutside\n";
        int descriptor = status == 0 ? open(outside_hosts, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0640) : -1;
        if (status == 0 && (descriptor < 0 || write(descriptor, original, sizeof original - 1) != sizeof original - 1 ||
                            close(descriptor) != 0 || symlink(outside, etc) != 0)) status = 99;
        errno = 0;
        if (status == 0 && hl_native_supervised_project_hostname(root, "builder", 0) != -1) status = 100;
        char receipt[sizeof original] = {0};
        descriptor = status == 0 ? open(outside_hosts, O_RDONLY | O_CLOEXEC) : -1;
        if (status == 0 && (descriptor < 0 || read(descriptor, receipt, sizeof receipt) != sizeof original - 1 ||
                            memcmp(receipt, original, sizeof original) != 0)) status = 101;
        if (descriptor >= 0) close(descriptor);
        unlink(etc);
        unlink(outside_hosts);
        rmdir(root);
        rmdir(outside);
        return status;
    }
    if (scenario >= sizeof hostile / sizeof hostile[0]) return 90;
    char root[] = "/var/tmp/husklet-hostname-hook.XXXXXX";
    if (mkdtemp(root) == NULL) return 91;
    char etc[PATH_MAX], hosts[PATH_MAX];
    int status = 0;
    if (snprintf(etc, sizeof etc, "%s/etc", root) >= (int)sizeof etc || mkdir(etc, 0700) != 0 ||
        snprintf(hosts, sizeof hosts, "%s/hosts", etc) >= (int)sizeof hosts) status = 92;
    int descriptor = status == 0 ? open(hosts, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0640) : -1;
    static const char original[] = "127.0.0.1\toriginal\n";
    if (status == 0 && (descriptor < 0 || write(descriptor, original, sizeof original - 1) != sizeof original - 1 ||
                        close(descriptor) != 0)) status = 93;
    if (status == 0 && hl_native_supervised_hostname_valid(hostile[scenario])) status = 96;
    errno = 0;
    if (status == 0 && (hl_native_supervised_project_hostname(root, hostile[scenario], 0) != -1 || errno != EINVAL))
        status = 94;
    char receipt[sizeof original] = {0};
    descriptor = status == 0 ? open(hosts, O_RDONLY | O_CLOEXEC) : -1;
    if (status == 0 && (descriptor < 0 || read(descriptor, receipt, sizeof receipt) != sizeof original - 1 ||
                        memcmp(receipt, original, sizeof original) != 0)) status = 95;
    if (descriptor >= 0) close(descriptor);
    unlink(hosts);
    rmdir(etc);
    rmdir(root);
    return status;
}
#endif

static int hl_native_supervised_limit_resource(const char *name) {
    static const struct { const char *name; int resource; } resources[] = {
        {"cpu", RLIMIT_CPU}, {"fsize", RLIMIT_FSIZE}, {"data", RLIMIT_DATA}, {"stack", RLIMIT_STACK},
        {"core", RLIMIT_CORE}, {"rss", RLIMIT_RSS}, {"nproc", RLIMIT_NPROC}, {"nofile", RLIMIT_NOFILE},
        {"memlock", RLIMIT_MEMLOCK}, {"as", RLIMIT_AS}, {"locks", RLIMIT_LOCKS},
        {"sigpending", RLIMIT_SIGPENDING}, {"msgqueue", RLIMIT_MSGQUEUE}, {"nice", RLIMIT_NICE},
        {"rtprio", RLIMIT_RTPRIO}, {"rttime", RLIMIT_RTTIME}, {NULL, -1}};
    for (size_t index = 0; resources[index].name != NULL; ++index)
        if (strcmp(name, resources[index].name) == 0) return resources[index].resource;
    return -1;
}

static int hl_native_supervised_limit_value(const char *text, rlim_t *value) {
    if (strcmp(text, "unlimited") == 0 || strcmp(text, "-1") == 0) { *value = RLIM_INFINITY; return 0; }
    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(text, &end, 10);
    if (errno != 0 || end == text || *end != 0 || (rlim_t)parsed != parsed) return -1;
    *value = (rlim_t)parsed;
    return 0;
}

static int hl_native_supervised_limits_apply(const char *spec) {
    if (spec == NULL) return 0;
    char *copy = strdup(spec);
    if (copy == NULL) return -1;
    char *save = NULL;
    for (char *record = strtok_r(copy, ",", &save); record != NULL; record = strtok_r(NULL, ",", &save)) {
        char *equals = strchr(record, '=');
        if (equals == NULL) { free(copy); return -1; }
        *equals++ = 0;
        int resource = hl_native_supervised_limit_resource(record);
        char *colon = strchr(equals, ':');
        if (colon != NULL) *colon++ = 0;
        struct rlimit limit;
        if (resource < 0 || hl_native_supervised_limit_value(equals, &limit.rlim_cur) != 0 ||
            (colon != NULL ? hl_native_supervised_limit_value(colon, &limit.rlim_max) :
                             (limit.rlim_max = limit.rlim_cur, 0)) != 0 ||
            limit.rlim_cur > limit.rlim_max || setrlimit(resource, &limit) != 0) {
            free(copy); return -1;
        }
    }
    free(copy);
    return 0;
}

static int hl_native_supervised_project_container(const hl_engine_config *config, const hl_options *options,
                                                  hl_native_supervised_bootstrap *bootstrap,
                                                  const hl_native_supervised_volumes *volumes, int mapping_fd,
                                                  const char *uid_map, const char *gid_map) {
    const hl_engine_box_config *box = config->box;
    if ((box->flags & HL_ENGINE_BOX_NETWORK_ISOLATED) != 0 && hl_native_supervised_loopback_up() != 0) return -1;
    if (mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL) != 0) return -1;
    char projected_root[PATH_MAX];
    if (hl_native_supervised_overlay_mount(config, options, projected_root) != 0) return -1;
    int projected_overlay = box->lower_layers != NULL;
    if (projected_overlay) {
        memcpy(bootstrap->projected_root, projected_root, strlen(projected_root) + 1);
        atomic_store_explicit(&bootstrap->projected_overlay, 1, memory_order_release);
    }
    if (hl_native_supervised_owners_apply(projected_root, box->file_owners) != 0) goto projection_failed;
    char byte;
    if (config->box->lower_layers == NULL && strcmp(projected_root, "/") != 0 &&
        mount(projected_root, projected_root, NULL, MS_BIND, NULL) != 0) return -1;
    if (hl_native_supervised_volumes_mount(projected_root, volumes) != 0) return -1;
    if ((box->flags & HL_ENGINE_BOX_NETWORK_ISOLATED) != 0 &&
        hl_native_supervised_project_hostname(projected_root, box->hostname,
                                              (box->flags & HL_ENGINE_BOX_ROOTFS_READ_ONLY) != 0) != 0)
        return -1;
    char proc_target[PATH_MAX];
    if (snprintf(proc_target, sizeof(proc_target), "%s%s", projected_root, "/proc") >= (int)sizeof(proc_target)) return -1;
    if (umount2(proc_target, MNT_DETACH) != 0 && errno != EINVAL && errno != ENOENT) return -1;
    if (mount("proc", proc_target, "proc", MS_NOSUID | MS_NODEV | MS_NOEXEC, NULL) != 0) return -1;
    if ((box->flags & HL_ENGINE_BOX_ROOTFS_READ_ONLY) != 0 &&
        mount(NULL, projected_root, NULL, MS_BIND | MS_REMOUNT | MS_RDONLY, NULL) != 0)
        return -1;
    if (box->hostname != NULL && sethostname(box->hostname, strlen(box->hostname)) != 0) return -1;
    if (setgroups(0, NULL) != 0 || prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0 || unshare(CLONE_NEWUSER) != 0 ||
        (mapping_fd >= 0
             ? (write(mapping_fd, "1", 1) != 1 || read(mapping_fd, &byte, 1) != 1)
             : (hl_native_supervised_write_text("/proc/self/setgroups", "deny") != 0 ||
                hl_native_supervised_write_text("/proc/self/uid_map", uid_map) != 0 ||
                hl_native_supervised_write_text("/proc/self/gid_map", gid_map) != 0)) ||
        prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0)
        return -1;
    if (mapping_fd >= 0) close(mapping_fd);
    if (chroot(projected_root) != 0) return -1;
    if (chdir(box->working_directory == NULL ? "/" : box->working_directory) != 0) return -1;
    if (hl_native_supervised_limits_apply(box->limits) != 0) return -1;
    for (int capability = 0; capability <= CAP_LAST_CAP; ++capability)
        if (prctl(PR_CAPBSET_DROP, capability, 0, 0, 0) != 0) return -1;
    if (setresgid(box->gid < 0 ? 0 : box->gid, box->gid < 0 ? 0 : box->gid, box->gid < 0 ? 0 : box->gid) != 0 ||
        setresuid(box->uid < 0 ? 0 : box->uid, box->uid < 0 ? 0 : box->uid, box->uid < 0 ? 0 : box->uid) != 0)
        return -1;
    struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
    struct __user_cap_data_struct data[2] = {{0}};
    if (syscall(SYS_capset, &header, data) != 0 || prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) return -1;
    return 0;
projection_failed: {
        int failure = errno;
        if (projected_overlay) {
            (void)umount2(projected_root, MNT_DETACH);
            (void)rmdir(projected_root);
        }
        errno = failure;
        return -1;
    }
}

static int hl_native_supervised_create_listener(const hl_options *options) {
#define HL_NATIVE_NOTIFY(number) \
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (number), 0, 1), \
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF)
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        HL_NATIVE_NOTIFY(SYS_open), HL_NATIVE_NOTIFY(SYS_openat), HL_NATIVE_NOTIFY(SYS_creat),
#ifdef SYS_openat2
        HL_NATIVE_NOTIFY(SYS_openat2),
#endif
        HL_NATIVE_NOTIFY(SYS_execve), HL_NATIVE_NOTIFY(SYS_clone), HL_NATIVE_NOTIFY(SYS_fork),
#ifdef SYS_execveat
        HL_NATIVE_NOTIFY(SYS_execveat),
#endif
#ifdef SYS_clone3
        HL_NATIVE_NOTIFY(SYS_clone3),
#endif
        HL_NATIVE_NOTIFY(SYS_vfork), HL_NATIVE_NOTIFY(SYS_unlink), HL_NATIVE_NOTIFY(SYS_unlinkat),
        HL_NATIVE_NOTIFY(SYS_rename), HL_NATIVE_NOTIFY(SYS_renameat), HL_NATIVE_NOTIFY(SYS_renameat2),
        HL_NATIVE_NOTIFY(SYS_mkdir), HL_NATIVE_NOTIFY(SYS_mkdirat), HL_NATIVE_NOTIFY(SYS_rmdir),
        HL_NATIVE_NOTIFY(SYS_link), HL_NATIVE_NOTIFY(SYS_linkat), HL_NATIVE_NOTIFY(SYS_symlink),
        HL_NATIVE_NOTIFY(SYS_symlinkat), HL_NATIVE_NOTIFY(SYS_chmod), HL_NATIVE_NOTIFY(SYS_fchmod),
        HL_NATIVE_NOTIFY(SYS_fchmodat), HL_NATIVE_NOTIFY(SYS_chown), HL_NATIVE_NOTIFY(SYS_fchown),
        HL_NATIVE_NOTIFY(SYS_lchown), HL_NATIVE_NOTIFY(SYS_fchownat), HL_NATIVE_NOTIFY(SYS_truncate),
        HL_NATIVE_NOTIFY(SYS_ftruncate), HL_NATIVE_NOTIFY(SYS_mknod), HL_NATIVE_NOTIFY(SYS_mknodat),
        HL_NATIVE_NOTIFY(SYS_mount), HL_NATIVE_NOTIFY(SYS_umount2), HL_NATIVE_NOTIFY(SYS_pivot_root),
        HL_NATIVE_NOTIFY(SYS_chroot), HL_NATIVE_NOTIFY(SYS_setns), HL_NATIVE_NOTIFY(SYS_unshare),
        HL_NATIVE_NOTIFY(SYS_socket), HL_NATIVE_NOTIFY(SYS_socketpair), HL_NATIVE_NOTIFY(SYS_connect),
        HL_NATIVE_NOTIFY(SYS_bind), HL_NATIVE_NOTIFY(SYS_listen), HL_NATIVE_NOTIFY(SYS_accept),
        HL_NATIVE_NOTIFY(SYS_accept4), HL_NATIVE_NOTIFY(SYS_ioctl), HL_NATIVE_NOTIFY(SYS_ptrace),
        HL_NATIVE_NOTIFY(SYS_seccomp), HL_NATIVE_NOTIFY(SYS_sendmsg),
        /* Internal refusal-test probe. Production policy otherwise lets identity reads stay native. */
        HL_NATIVE_NOTIFY(SYS_getpid),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_filter selective[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        HL_NATIVE_NOTIFY(SYS_clone),
#ifdef SYS_clone3
        HL_NATIVE_NOTIFY(SYS_clone3),
#endif
        HL_NATIVE_NOTIFY(SYS_ioctl), HL_NATIVE_NOTIFY(SYS_ptrace), HL_NATIVE_NOTIFY(SYS_seccomp),
        HL_NATIVE_NOTIFY(SYS_mount), HL_NATIVE_NOTIFY(SYS_umount2), HL_NATIVE_NOTIFY(SYS_pivot_root),
        HL_NATIVE_NOTIFY(SYS_chroot), HL_NATIVE_NOTIFY(SYS_setns), HL_NATIVE_NOTIFY(SYS_unshare),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
#undef HL_NATIVE_NOTIFY
    int refusal = hl_options_get(options, "HL_NATIVE_SUPERVISED_REFUSE") != NULL;
    struct sock_fprog program = refusal
        ? (struct sock_fprog){(unsigned short)(sizeof(instructions) / sizeof(instructions[0])), instructions}
        : (struct sock_fprog){(unsigned short)(sizeof(selective) / sizeof(selective[0])), selective};
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) return -1;
    return (int)syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_NEW_LISTENER, &program);
}

static int hl_native_supervised_refusal(const hl_options *options, int *number, int *error) {
    const char *value = hl_options_get(options, "HL_NATIVE_SUPERVISED_REFUSE");
    *number = -1;
    *error = 0;
    if (value == NULL) return 0;
    char *end = NULL;
    long parsed_number = strtol(value, &end, 10);
    if (end == value || *end != ':') return -1;
    char *error_end = NULL;
    long parsed_error = strtol(end + 1, &error_end, 10);
    if (*error_end != 0 || parsed_number < 0 || parsed_number > INT_MAX ||
        (parsed_error != EPERM && parsed_error != ENOSYS)) return -1;
    *number = (int)parsed_number;
    *error = (int)parsed_error;
    return 0;
}

static int hl_native_supervised_denied(int number) {
    return number == SYS_ptrace || number == SYS_seccomp || number == SYS_mount ||
           number == SYS_umount2 || number == SYS_pivot_root || number == SYS_chroot || number == SYS_setns ||
           number == SYS_unshare;
}

static int hl_native_supervised_clone_namespaces(uint64_t flags) {
    const uint64_t namespaces = CLONE_NEWCGROUP | CLONE_NEWIPC | CLONE_NEWNET | CLONE_NEWNS |
                                CLONE_NEWPID | CLONE_NEWTIME | CLONE_NEWUSER | CLONE_NEWUTS;
    return (flags & namespaces) != 0;
}

static int hl_native_supervised_ioctl_allowed(uint64_t request) {
    return request == TCGETS || request == TCSETS || request == TCSETSW || request == TCSETSF ||
           request == TIOCGWINSZ || request == TIOCSWINSZ || request == TIOCGPGRP || request == TIOCSPGRP ||
           request == FIONREAD || request == TIOCGPTN || request == TIOCSPTLCK;
}

static int hl_native_supervised_single_child(pid_t parent, pid_t *child) {
    char path[64], bytes[128];
    if (snprintf(path, sizeof(path), "/proc/%d/task/%d/children", parent, parent) >= (int)sizeof(path)) return -1;
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return -1;
    ssize_t count = read(fd, bytes, sizeof(bytes) - 1);
    close(fd);
    if (count <= 0) return -1;
    bytes[count] = 0;
    char *end = NULL;
    long value = strtol(bytes, &end, 10);
    while (end != NULL && *end == ' ') ++end;
    if (value <= 0 || value > INT_MAX || end == NULL || *end != 0) return -1;
    *child = (pid_t)value;
    return 0;
}

static int hl_native_supervised_single_thread_no_children(pid_t process) {
    char path[64];
    if (snprintf(path, sizeof(path), "/proc/%d/task", process) >= (int)sizeof(path)) return -1;
    DIR *tasks = opendir(path);
    if (tasks == NULL) return -1;
    size_t count = 0;
    struct dirent *entry;
    while ((entry = readdir(tasks)) != NULL)
        if (entry->d_name[0] != '.' && ++count > 1) break;
    closedir(tasks);
    pid_t child;
    return count == 1 && hl_native_supervised_single_child(process, &child) != 0 ? 0 : -1;
}

static int hl_native_supervised_stopped(pid_t process) {
    char path[64], bytes[256];
    if (snprintf(path, sizeof(path), "/proc/%d/status", process) >= (int)sizeof(path)) return 0;
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return 0;
    ssize_t count = read(fd, bytes, sizeof(bytes) - 1);
    close(fd);
    if (count <= 0) return 0;
    bytes[count] = 0;
    char *state = strstr(bytes, "\nState:\t");
    return state != NULL && (state[8] == 'T' || state[8] == 't');
}

static int hl_native_supervised_checkpoint_phase1(pid_t workload, uint32_t generation, const hl_options *options) {
    if (workload <= 0 || hl_native_supervised_single_thread_no_children(workload) != 0) {
        hl_ckpt_request refusal = {.op = HL_CKPT_OP_CAPTURE_REFUSED, .generation = generation};
        (void)hl_ckpt_channel_acquire();
        (void)hl_ckpt_channel_notify(&refusal, "native phase-1 refuses descendants or multiple threads");
        const char *receipt = hl_options_get(options, "HL_NATIVE_CKPT_TEST_RECEIPT");
        if (receipt != NULL)
            (void)hl_native_supervised_write_text(receipt,
                                                  "registered=0 frozen=0 thawed=0 refusal=unsupported-state\n");
        return -1;
    }
    if (kill(workload, SIGSTOP) != 0) return -1;
    int frozen = 0;
    for (int attempt = 0; attempt < 1000 && !frozen; ++attempt) {
        frozen = hl_native_supervised_stopped(workload);
        if (!frozen) usleep(1000);
    }
    int registered = 0;
    if (frozen && hl_options_get(options, "HL_NATIVE_CKPT_TEST_SKIP_REGISTER") == NULL) {
        /* The trigger is bumped while the host still owns the capture-state lock; wait until the
         * matching membership ledger is visible before announcing this stopped participant. */
        usleep(10000);
        unsigned char payload[12] = {0};
        uint32_t one = 1, executor = (uint32_t)workload;
        memcpy(payload, &one, 4);
        memcpy(payload + 8, &executor, 4);
        hl_ckpt_reply reply = {0};
        int called = -1;
        for (int attempt = 0; attempt < 1000 && !registered; ++attempt) {
            hl_ckpt_request request = {
                .op = HL_CKPT_OP_REGISTER_READY, .length = sizeof(payload), .generation = generation};
            called = hl_ckpt_channel_call(&request, NULL, payload, &reply, NULL, 0);
            registered = called == 0 && reply.status == HL_CKPT_STATUS_OK && reply.value != 0;
            if (!registered) usleep(1000);
        }
        if (!registered && hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL)
            fprintf(stderr, "[hl-native-checkpoint]\tregister_call=%d status=%d member=%llu failure=%s\n",
                    called, reply.status, (unsigned long long)reply.value,
                    hl_ckpt_channel_failure() == NULL ? "none" : hl_ckpt_channel_failure());
    }
    hl_ckpt_request refusal = {.op = HL_CKPT_OP_CAPTURE_REFUSED, .generation = generation};
    (void)hl_ckpt_channel_notify(&refusal, registered ? "native phase-1 supports freeze only, not image capture"
                                                       : "native phase-1 participant registration failed");
    int thawed = kill(workload, SIGCONT) == 0;
    if (hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL)
        fprintf(stderr, "[hl-native-checkpoint]\tgeneration=%u registered=%d frozen=%d thawed=%d\n",
                generation, registered, frozen, thawed);
    const char *receipt = hl_options_get(options, "HL_NATIVE_CKPT_TEST_RECEIPT");
    if (receipt != NULL) {
        char line[128];
        snprintf(line, sizeof(line), "generation=%u registered=%d frozen=%d thawed=%d\n",
                 generation, registered, frozen, thawed);
        (void)hl_native_supervised_write_text(receipt, line);
    }
    return registered && frozen && thawed ? 0 : -1;
}

static char **hl_native_supervised_environment(const hl_options *options) {
    const char *encoded = hl_options_get(options, "HL_GUEST_ENV");
    int escaped = hl_options_get(options, "HL_GUEST_ENV_ESC") != NULL;
    if (encoded == NULL || encoded[0] == 0) return calloc(1, sizeof(char *));
    size_t count = 1;
    for (const char *cursor = encoded; *cursor; ++cursor) count += *cursor == '\n';
    char **environment = calloc(count + 1, sizeof(char *));
    char *storage = strdup(encoded);
    if (environment == NULL || storage == NULL) { free(environment); free(storage); return NULL; }
    size_t index = 0;
    char *record = storage;
    for (char *cursor = storage;; ++cursor) {
        if (*cursor != '\n' && *cursor != 0) continue;
        int last = *cursor == 0;
        *cursor = 0;
        if (escaped) {
            char *read = record, *write = record;
            while (*read) {
                if (read[0] == '\\' && read[1] == 'n') { *write++ = '\n'; read += 2; }
                else if (read[0] == '\\' && read[1] == '\\') { *write++ = '\\'; read += 2; }
                else *write++ = *read++;
            }
            *write = 0;
        }
        environment[index++] = record;
        if (last) break;
        record = cursor + 1;
    }
    return environment;
}

static void hl_native_supervised_environment_free(char **environment) {
    if (environment == NULL) return;
    free(environment[0]);
    free(environment);
}

static int hl_native_supervised_wait(int listener, int leader_pidfd, pid_t leader,
                                     const hl_options *options, int *guest_signal) {
    int refused_number, refused_error;
    if (hl_native_supervised_refusal(options, &refused_number, &refused_error) != 0) return 70;
    struct seccomp_notif_sizes sizes = {0};
    if (syscall(SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &sizes) != 0) return 70;
    struct seccomp_notif *request = calloc(1, sizes.seccomp_notif);
    struct seccomp_notif_resp *response = calloc(1, sizes.seccomp_notif_resp);
    if (request == NULL || response == NULL) { free(request); free(response); return 70; }
    int leader_result = 70, leader_done = 0;
    int diagnostics = hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL;
    const char *notification_receipt = hl_options_get(options, "HL_NATIVE_NOTIFY_TEST_RECEIPT");
    int count_notifications = diagnostics || notification_receipt != NULL;
    unsigned long idle_timeouts = 0, notifications = 0, open_notifications = 0;
    int listener_active = listener;
    volatile uint32_t *trigger = NULL;
    uint32_t trigger_seen = 0;
    int trigger_descriptor = hl_ckpt_trigger_descriptor();
    int trigger_wake = hl_ckpt_broker_descriptor();
    if (trigger_descriptor >= 0) {
        void *mapping = mmap(NULL, sizeof(uint32_t), PROT_READ | PROT_WRITE, MAP_SHARED, trigger_descriptor, 0);
        if (mapping != MAP_FAILED) { trigger = mapping; trigger_seen = *trigger; }
    }
    *guest_signal = 0;
    for (;;) {
        int status;
        pid_t waited;
        while ((waited = waitpid(-1, &status, WNOHANG)) > 0) {
            if (waited == leader) {
                leader_done = 1;
                if (WIFEXITED(status)) leader_result = WEXITSTATUS(status);
                else if (WIFSIGNALED(status)) {
                    *guest_signal = WTERMSIG(status);
                    /* finish_process authenticates the signal record against this worker status. */
                    leader_result = 128 + *guest_signal;
                }
            }
        }
        if (waited < 0 && errno == ECHILD && leader_done) {
            const char *idle_receipt = hl_options_get(options, "HL_NATIVE_CKPT_TEST_IDLE_RECEIPT");
            if (idle_receipt != NULL) {
                char line[64];
                snprintf(line, sizeof(line), "periodic_wakeups=%lu\n", idle_timeouts);
                (void)hl_native_supervised_write_text(idle_receipt, line);
            }
            if (diagnostics)
                fprintf(stderr, "[hl-native-supervised]\tnotifications=%lu open=%lu\n", notifications,
                        open_notifications);
            if (notification_receipt != NULL) {
                char line[64];
                snprintf(line, sizeof(line), "notifications=%lu open=%lu\n", notifications, open_notifications);
                (void)hl_native_supervised_write_text(notification_receipt, line);
            }
            free(request); free(response); return leader_result;
        }
        if (waited < 0 && errno != EINTR && errno != ECHILD) { free(request); free(response); return 70; }
        struct pollfd events[3] = {
            {listener_active, POLLIN, 0}, {leader_pidfd, POLLIN, 0}, {trigger_wake, POLLIN, 0}};
        int polled = poll(events, trigger_wake < 0 ? (leader_pidfd < 0 ? 1 : 2) : 3, -1);
        if (polled < 0) { if (errno == EINTR) continue; free(request); free(response); return 70; }
        if (polled == 0) { ++idle_timeouts; continue; }
        if (trigger_wake >= 0 && (events[2].revents & POLLIN)) {
            unsigned char wakes[64];
            while (recv(trigger_wake, wakes, sizeof(wakes), MSG_DONTWAIT) > 0) {}
            if (trigger != NULL && *trigger != trigger_seen) {
                trigger_seen = *trigger;
                pid_t workload = -1;
                (void)hl_native_supervised_single_child(leader, &workload);
                (void)hl_native_supervised_checkpoint_phase1(workload, trigger_seen, options);
            }
        }
        if (events[0].revents & (POLLHUP | POLLNVAL)) listener_active = -1;
        if (!(events[0].revents & POLLIN)) continue;
        memset(request, 0, sizes.seccomp_notif);
        if (ioctl(listener, SECCOMP_IOCTL_NOTIF_RECV, request) != 0) {
            if (errno == EINTR || errno == ENOENT) continue;
            free(request); free(response); return 70;
        }
        memset(response, 0, sizes.seccomp_notif_resp);
        response->id = request->id;
        int number = (int)request->data.nr;
        if (count_notifications) {
            ++notifications;
            if (number == SYS_open) ++open_notifications;
        }
        if (number == refused_number) {
            response->error = -refused_error;
#ifdef SYS_clone3
        } else if (number == SYS_clone3) {
            response->error = -ENOSYS;
#endif
        } else if (hl_native_supervised_denied(number) ||
                   (number == SYS_ioctl && !hl_native_supervised_ioctl_allowed(request->data.args[1])) ||
                   (number == SYS_clone && hl_native_supervised_clone_namespaces(request->data.args[0]))
                   ) {
            response->error = -EPERM;
        } else {
            response->flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
        }
        if (ioctl(listener, SECCOMP_IOCTL_NOTIF_SEND, response) != 0 && errno != ENOENT) {
            free(request); free(response); return 70;
        }
    }
}

static int32_t hl_native_supervised_run(const hl_host_services *host, hl_linux_abi *box,
                                        const hl_engine_config *config,
                                        hl_host_handle executable_handle, uint32_t argc, char *const argv[],
                                        const hl_options *options, int activation_ready, int *guest_signal) {
#if defined(HL_NATIVE_TEST_HOOKS)
    /* Every selected integration scenario is also a structural gate: rebuilding the translated ABI makes
     * the native-only suite fail instead of silently preserving behavior through the old heavyweight seam. */
    if (box != NULL) return 70;
#endif
    if (argv == NULL || argv[0] == NULL) return 70;
    if (host == NULL || host->posix_attachment == NULL || host->posix_attachment->borrow_file_at_least == NULL ||
        host->posix_attachment->release == NULL) return 70;
    char **exec_argv = calloc((size_t)argc + 1, sizeof(char *));
    if (exec_argv == NULL) return 70;
    for (uint32_t index = 0; index < argc; ++index) exec_argv[index] = argv[index];
    const char *policy_rejection = hl_native_supervised_policy_rejection(config);
    if (policy_rejection != NULL) {
        if (hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL)
            fprintf(stderr, "[hl-native-supervised]\tunsupported-policy=%s\n", policy_rejection);
        return 70;
    }
    if (hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL)
        fprintf(stderr, "[hl-native-supervised]\tselected=1 translated_abi=%d constructed=%llu destroyed=%llu\n",
                box != NULL, (unsigned long long)hl_linux_abi_constructed(),
                (unsigned long long)hl_linux_abi_destroyed());
    char **environment = hl_native_supervised_environment(options);
    if (environment == NULL) { free(exec_argv); return 70; }
    hl_host_result executable_attachment =
        host->posix_attachment->borrow_file_at_least(host->context, executable_handle, 64);
    if (executable_attachment.status != HL_STATUS_OK || executable_attachment.value > INT_MAX) {
        hl_native_supervised_environment_free(environment); free(exec_argv); return 70;
    }
    int executable = (int)executable_attachment.value;
    int borrowed[3] = {-1, -1, -1};
    for (hl_linux_fd fd = 0; fd < 3; ++fd) {
        hl_linux_fd_snapshot snapshot = {0};
        if (box != NULL) {
            if (hl_linux_fd_snapshot_get(box, fd, &snapshot) != HL_STATUS_OK) goto attachment_failed;
        } else {
            snapshot.host_handle = HL_HOST_HANDLE_INVALID;
            for (uint32_t index = 0; index < config->fd_binding_count; ++index)
                if (config->fd_bindings[index].guest_fd == fd) {
                    snapshot.host_handle = config->fd_bindings[index].host_handle;
                    break;
                }
            if (snapshot.host_handle == HL_HOST_HANDLE_INVALID) goto attachment_failed;
        }
        hl_host_result attached = host->posix_attachment->borrow_file_at_least(host->context, snapshot.host_handle, 64);
        if (attached.status != HL_STATUS_OK || attached.value > INT_MAX) goto attachment_failed;
        borrowed[fd] = (int)attached.value;
    }
    int planted_high_fd = -1;
    const char *test_refusal = hl_options_get(options, "HL_NATIVE_SUPERVISED_REFUSE");
    if (test_refusal != NULL && strcmp(test_refusal, "999:38") == 0) {
        int source = open("/dev/null", O_RDONLY | O_CLOEXEC);
        if (source < 0 || dup2(source, 1048575) != 1048575) { if (source >= 0) close(source); goto attachment_failed; }
        close(source);
        planted_high_fd = 1048575;
    }
    hl_native_supervised_bootstrap *bootstrap = mmap(NULL, sizeof(*bootstrap), PROT_READ | PROT_WRITE,
                                                     MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (bootstrap == MAP_FAILED) goto attachment_failed;
    atomic_init(&bootstrap->listener, -1);
    atomic_init(&bootstrap->target_pid, -1);
    atomic_init(&bootstrap->acknowledged, 0);
    atomic_init(&bootstrap->result_signal, 0);
    atomic_init(&bootstrap->projected_overlay, 0);
    atomic_init(&bootstrap->clone_stages, 0);
#if defined(HL_NATIVE_TEST_HOOKS)
    atomic_init(&bootstrap->listener_wakes, 0);
#endif
    if (prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0) {
        munmap(bootstrap, sizeof(*bootstrap)); goto attachment_failed;
    }
    unsigned guest_uid = (unsigned)(config->box->uid < 0 ? 0 : config->box->uid);
    unsigned guest_gid = (unsigned)(config->box->gid < 0 ? 0 : config->box->gid);
    char uid_map[16384], gid_map[16384];
    int mapping[2] = {-1, -1};
    int leader_pidfd = -1;
    if (config->box->file_owners == NULL) {
        if (snprintf(uid_map, sizeof(uid_map), "%u %u 1\n", guest_uid, (unsigned)geteuid()) <= 0 ||
            snprintf(gid_map, sizeof(gid_map), "%u %u 1\n", guest_gid, (unsigned)getegid()) <= 0)
            goto clone_failed;
    } else if (hl_native_supervised_id_map(uid_map, sizeof(uid_map), guest_uid, config->box->file_owners, 0) != 0 ||
               hl_native_supervised_id_map(gid_map, sizeof(gid_map), guest_gid, config->box->file_owners, 1) != 0) {
        goto clone_failed;
    }
    if (config->box->file_owners != NULL && socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, mapping) != 0)
        goto clone_failed;
    uint64_t network_namespace = (config->box->flags & HL_ENGINE_BOX_NETWORK_ISOLATED) != 0 ? CLONE_NEWNET : 0;
    struct clone_args clone = {
        .flags = CLONE_NEWNS | CLONE_NEWPID | network_namespace | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_PIDFD,
        .pidfd = (uint64_t)(uintptr_t)&leader_pidfd,
        .exit_signal = SIGCHLD,
    };
#if defined(HL_NATIVE_TEST_HOOKS)
    const char *stage_fail = test_refusal != NULL && strcmp(test_refusal, "998:38") == 0 ? "clone" :
                             test_refusal != NULL && strcmp(test_refusal, "997:38") == 0 ? "mapping" :
                             test_refusal != NULL && strcmp(test_refusal, "996:38") == 0 ? "listener" : NULL;
#else
    const char *stage_fail = NULL;
#endif
    pid_t child = stage_fail != NULL && strcmp(stage_fail, "clone") == 0
                      ? (errno = ENOSYS, (pid_t)-1)
                      : (pid_t)syscall(SYS_clone3, &clone, sizeof(clone));
    if (child < 0) goto clone_failed;
    if (child == 0) {
        atomic_fetch_add_explicit(&bootstrap->clone_stages, 1, memory_order_relaxed);
        if (mapping[0] >= 0) close(mapping[0]);
        for (int fd = 0; fd < 3; ++fd) {
            if (borrowed[fd] < 0) continue;
            if (dup2(borrowed[fd], fd) < 0) _exit(70);
            if (borrowed[fd] != fd) close(borrowed[fd]);
        }
        if (fcntl(executable, F_SETFD, 0) != 0) _exit(70);
        hl_native_supervised_volumes volumes;
        if (hl_native_supervised_volumes_open(config->box->volumes, &volumes) != 0 ||
            hl_native_supervised_project_container(config, options, bootstrap, &volumes, mapping[1], uid_map,
                                                    gid_map) != 0 ||
            hl_native_supervised_close_except(executable) != 0) {
            if (hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL)
                fprintf(stderr, "[hl-native-supervised]\tprojector_errno=%d\n", errno);
            _exit(70);
        }
        int listener = stage_fail != NULL && strcmp(stage_fail, "listener") == 0
                           ? (errno = EIO, -1)
                           : hl_native_supervised_create_listener(options);
        if (listener < 0) _exit(70);
#if defined(HL_NATIVE_TEST_HOOKS)
        if (test_refusal != NULL && (strcmp(test_refusal, "993:38") == 0 || strcmp(test_refusal, "995:38") == 0))
            usleep(10000);
#endif
        atomic_store_explicit(&bootstrap->listener, listener, memory_order_release);
        int listeners_woken = (int)syscall(SYS_futex, &bootstrap->listener, FUTEX_WAKE, 1, NULL, NULL, 0);
#if defined(HL_NATIVE_TEST_HOOKS)
        atomic_store_explicit(&bootstrap->listener_wakes, listeners_woken + 1, memory_order_release);
#else
        (void)listeners_woken;
#endif
        while (!atomic_load_explicit(&bootstrap->acknowledged, memory_order_acquire)) {
            if (syscall(SYS_futex, &bootstrap->acknowledged, FUTEX_WAIT, 0, NULL, NULL, 0) != 0 &&
                errno != EAGAIN && errno != EINTR)
                _exit(70);
        }
        close(listener);
        pid_t workload = fork();
        if (workload < 0) _exit(70);
        if (workload > 0) {
            atomic_fetch_add_explicit(&bootstrap->clone_stages, 1, memory_order_relaxed);
            int leader_status = 0;
            int status;
            pid_t waited;
            while ((waited = waitpid(-1, &status, 0)) > 0)
                if (waited == workload) leader_status = status;
            if (WIFSIGNALED(leader_status)) {
                atomic_store_explicit(&bootstrap->result_signal, WTERMSIG(leader_status), memory_order_release);
                _exit(128 + WTERMSIG(leader_status));
            }
            _exit(WIFEXITED(leader_status) ? WEXITSTATUS(leader_status) : 70);
        }
        if (fcntl(executable, F_SETFD, FD_CLOEXEC) != 0) _exit(70);
        execveat(executable, "", exec_argv, environment, AT_EMPTY_PATH);
        if (hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL)
            fprintf(stderr, "[hl-native-supervised]\texecveat_errno=%d\n", errno);
        _exit(errno == ENOENT ? 127 : 126);
    }
    if (mapping[1] >= 0) close(mapping[1]);
    if (mapping[0] < 0 && stage_fail != NULL && strcmp(stage_fail, "mapping") == 0) {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        goto clone_failed;
    }
    if (mapping[0] >= 0) {
        char byte;
        struct pollfd ready = {.fd = mapping[0], .events = POLLIN};
        if (poll(&ready, 1, 10000) != 1 || read(mapping[0], &byte, 1) != 1 ||
            (stage_fail != NULL && strcmp(stage_fail, "mapping") == 0) ||
            hl_native_supervised_write_process_text(child, "setgroups", "deny") != 0 ||
            hl_native_supervised_write_process_text(child, "uid_map", uid_map) != 0 ||
            hl_native_supervised_write_process_text(child, "gid_map", gid_map) != 0 || write(mapping[0], "1", 1) != 1) {
            close(mapping[0]);
            kill(child, SIGKILL);
            waitpid(child, NULL, 0);
            goto clone_failed;
        }
        close(mapping[0]);
    }
    if (planted_high_fd >= 0) { close(planted_high_fd); planted_high_fd = -1; }
    for (int fd = 0; fd < 3; ++fd) {
        if (borrowed[fd] >= 0) (void)host->posix_attachment->release(host->context, (uint64_t)borrowed[fd]);
        borrowed[fd] = -1;
    }
    (void)host->posix_attachment->release(host->context, (uint64_t)executable);
    executable = -1;
    int listener = leader_pidfd < 0 ? -1 : hl_native_supervised_listener_wait(bootstrap, leader_pidfd, options);
    if (listener >= 0) {
        atomic_store_explicit(&bootstrap->acknowledged, 1, memory_order_release);
        (void)syscall(SYS_futex, &bootstrap->acknowledged, FUTEX_WAKE, 1, NULL, NULL, 0);
    }
    if (listener < 0) {
        (void)kill(child, SIGKILL); (void)waitpid(child, NULL, 0);
        hl_native_supervised_projection_cleanup(bootstrap);
        if (leader_pidfd >= 0) close(leader_pidfd);
        munmap(bootstrap, sizeof(*bootstrap));
        hl_native_supervised_environment_free(environment); free(exec_argv); return 70;
    }
    unsigned char ready = 1;
    if (write(activation_ready, &ready, sizeof(ready)) != (ssize_t)sizeof(ready)) {
        close(listener); (void)kill(child, SIGKILL); (void)waitpid(child, NULL, 0);
        hl_native_supervised_projection_cleanup(bootstrap);
        if (leader_pidfd >= 0) close(leader_pidfd);
        munmap(bootstrap, sizeof(*bootstrap));
        hl_native_supervised_environment_free(environment); free(exec_argv); return 70;
    }
    int result = hl_native_supervised_wait(listener, leader_pidfd, child, options, guest_signal);
#if defined(HL_NATIVE_TEST_HOOKS)
    if (atomic_load_explicit(&bootstrap->clone_stages, memory_order_relaxed) != 2) result = 70;
#endif
    int result_signal = atomic_load_explicit(&bootstrap->result_signal, memory_order_acquire);
    if (result_signal != 0) *guest_signal = result_signal;
    hl_native_supervised_projection_cleanup(bootstrap);
    munmap(bootstrap, sizeof(*bootstrap));
    if (leader_pidfd >= 0) close(leader_pidfd);
    close(listener);
    hl_native_supervised_environment_free(environment);
    free(exec_argv);
    return result;
clone_failed:
    if (mapping[0] >= 0) close(mapping[0]);
    if (mapping[1] >= 0) close(mapping[1]);
    if (leader_pidfd >= 0) close(leader_pidfd);
    hl_native_supervised_projection_cleanup(bootstrap);
    munmap(bootstrap, sizeof(*bootstrap));
    goto attachment_failed;
attachment_failed:
    if (planted_high_fd >= 0) close(planted_high_fd);
    for (int fd = 0; fd < 3; ++fd)
        if (borrowed[fd] >= 0) (void)host->posix_attachment->release(host->context, (uint64_t)borrowed[fd]);
    if (executable >= 0) (void)host->posix_attachment->release(host->context, (uint64_t)executable);
    hl_native_supervised_environment_free(environment);
    free(exec_argv);
    return 70;
}
#else
static int hl_native_supervised_available(void) { return 0; }
static int32_t hl_native_supervised_run(const hl_host_services *host, hl_linux_abi *box,
                                        const hl_engine_config *config,
                                        hl_host_handle executable_handle, uint32_t argc, char *const argv[],
                                        const hl_options *options, int activation_ready, int *guest_signal) {
    (void)host; (void)box; (void)config; (void)executable_handle; (void)argc; (void)argv; (void)options;
    (void)activation_ready;
    *guest_signal = 0; return 70;
}
#endif
