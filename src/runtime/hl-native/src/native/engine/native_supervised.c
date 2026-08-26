static int hl_native_supervised_selected(const hl_options *options) {
    const char *value = hl_options_get(options, "HL_NATIVE_SUPERVISED");
    return value != NULL && value[0] != 0 && value[0] != '0';
}

#if defined(__linux__) && defined(__x86_64__)
#include <linux/audit.h>
#include <linux/filter.h>
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
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <sys/uio.h>
#include <termios.h>

static int hl_native_supervised_available(void) { return 1; }

typedef struct {
    _Atomic int listener;
    _Atomic int target_pid;
    _Atomic int acknowledged;
    _Atomic int result_signal;
} hl_native_supervised_bootstrap;

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
        int host_root = open("/", O_PATH | O_DIRECTORY | O_CLOEXEC);
        struct open_how source_how = {.flags = O_PATH | O_DIRECTORY | O_CLOEXEC,
                                      .resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS};
        int source = host_root < 0 ? -1 : (int)syscall(SYS_openat2, host_root, colon + 1, &source_how, sizeof(source_how));
        if (host_root >= 0) close(host_root);
        if (source < 0) goto failed;
        int tree = (int)syscall(SYS_open_tree, AT_FDCWD, colon, OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC);
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
        if (tree < 0 || syscall(SYS_mount_setattr, tree, "", AT_EMPTY_PATH, &attributes, sizeof(attributes)) != 0 ||
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

static int hl_native_supervised_policy_supported(const hl_engine_config *config) {
    const hl_engine_box_config *box = config->box;
    if (geteuid() != 0 || getegid() != 0 || config->rootfs == NULL || box == NULL ||
        config->memory_limit != 0 || config->pid_limit != 0 ||
        config->cpu_limit != 0 || box->uid != -1 || box->gid != -1 || box->lower_layers != NULL ||
        box->publish_count != 0 || box->limits != NULL || box->network_bridge != NULL ||
        box->network_namespace != NULL || box->ip != NULL || box->egress_proxy != NULL ||
        box->filesystem_generation != NULL || box->file_owners != NULL || box->checkpoint_mode != 0 ||
        box->checkpoint_policy != 0 ||
        (box->flags & ~(HL_ENGINE_BOX_ROOTFS_READ_ONLY | HL_ENGINE_BOX_NETWORK_ISOLATED |
                        HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED)) != 0)
        return 0;
    return 1;
}

static int hl_native_supervised_project_container(const hl_engine_config *config,
                                                  hl_native_supervised_bootstrap *bootstrap,
                                                  const hl_native_supervised_volumes *volumes) {
    const hl_engine_box_config *box = config->box;
    uid_t host_uid = geteuid();
    gid_t host_gid = getegid();
    if (unshare(CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET | CLONE_NEWUTS | CLONE_NEWIPC) != 0)
        return -1;
    pid_t init = fork();
    if (init < 0) return -1;
    if (init > 0) {
        atomic_store_explicit(&bootstrap->target_pid, init, memory_order_release);
        int status;
        if (waitpid(init, &status, 0) != init) _exit(70);
        int result_signal = atomic_load_explicit(&bootstrap->result_signal, memory_order_acquire);
        if (result_signal != 0) {
            sigset_t signals;
            sigemptyset(&signals);
            sigaddset(&signals, result_signal);
            sigprocmask(SIG_UNBLOCK, &signals, NULL);
            signal(result_signal, SIG_DFL);
            raise(result_signal);
        }
        _exit(WIFEXITED(status) ? WEXITSTATUS(status) : 70);
    }
    if (mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL) != 0) return -1;
    if (strcmp(config->rootfs, "/") != 0 && mount(config->rootfs, config->rootfs, NULL, MS_BIND, NULL) != 0) return -1;
    if (hl_native_supervised_volumes_mount(config->rootfs, volumes) != 0) return -1;
    char proc_target[PATH_MAX];
    if (snprintf(proc_target, sizeof(proc_target), "%s%s", config->rootfs, "/proc") >= (int)sizeof(proc_target)) return -1;
    if (umount2(proc_target, MNT_DETACH) != 0 && errno != EINVAL && errno != ENOENT) return -1;
    if (mount("proc", proc_target, "proc", MS_NOSUID | MS_NODEV | MS_NOEXEC, NULL) != 0) return -1;
    if ((box->flags & HL_ENGINE_BOX_ROOTFS_READ_ONLY) != 0 &&
        mount(NULL, config->rootfs, NULL, MS_BIND | MS_REMOUNT | MS_RDONLY, NULL) != 0)
        return -1;
    if (box->hostname != NULL && sethostname(box->hostname, strlen(box->hostname)) != 0) return -1;
    if (prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0 || unshare(CLONE_NEWUSER) != 0 ||
        prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0)
        return -1;
    char uid_map[64], gid_map[64];
    if (snprintf(uid_map, sizeof(uid_map), "0 %u 1\n", (unsigned)host_uid) <= 0 ||
        snprintf(gid_map, sizeof(gid_map), "0 %u 1\n", (unsigned)host_gid) <= 0 ||
        hl_native_supervised_write_text("/proc/self/setgroups", "deny") != 0 ||
        hl_native_supervised_write_text("/proc/self/uid_map", uid_map) != 0 ||
        hl_native_supervised_write_text("/proc/self/gid_map", gid_map) != 0)
        return -1;
    if (chroot(config->rootfs) != 0) return -1;
    if (chdir(box->working_directory == NULL ? "/" : box->working_directory) != 0) return -1;
    for (int capability = 0; capability <= CAP_LAST_CAP; ++capability)
        if (prctl(PR_CAPBSET_DROP, capability, 0, 0, 0) != 0) return -1;
    if (setresgid(box->gid < 0 ? 0 : box->gid, box->gid < 0 ? 0 : box->gid, box->gid < 0 ? 0 : box->gid) != 0 ||
        setresuid(box->uid < 0 ? 0 : box->uid, box->uid < 0 ? 0 : box->uid, box->uid < 0 ? 0 : box->uid) != 0)
        return -1;
    struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
    struct __user_cap_data_struct data[2] = {{0}};
    if (syscall(SYS_capset, &header, data) != 0 || prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) return -1;
    return 0;
}

static int hl_native_supervised_create_listener(void) {
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
#undef HL_NATIVE_NOTIFY
    struct sock_fprog program = {(unsigned short)(sizeof(instructions) / sizeof(instructions[0])), instructions};
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
    return number == SYS_sendmsg || number == SYS_ptrace || number == SYS_seccomp || number == SYS_mount ||
           number == SYS_umount2 || number == SYS_pivot_root || number == SYS_chroot || number == SYS_setns ||
           number == SYS_unshare || number == SYS_socket || number == SYS_socketpair || number == SYS_connect ||
           number == SYS_bind || number == SYS_listen || number == SYS_accept || number == SYS_accept4;
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

static int hl_native_supervised_wait(int listener, pid_t leader, const hl_options *options, int *guest_signal) {
    int refused_number, refused_error;
    if (hl_native_supervised_refusal(options, &refused_number, &refused_error) != 0) return 70;
    struct seccomp_notif_sizes sizes = {0};
    if (syscall(SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &sizes) != 0) return 70;
    struct seccomp_notif *request = calloc(1, sizes.seccomp_notif);
    struct seccomp_notif_resp *response = calloc(1, sizes.seccomp_notif_resp);
    if (request == NULL || response == NULL) { free(request); free(response); return 70; }
    int leader_result = 70, leader_done = 0;
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
            free(request); free(response); return leader_result;
        }
        if (waited < 0 && errno != EINTR && errno != ECHILD) { free(request); free(response); return 70; }
        struct pollfd event = {listener, POLLIN, 0};
        int polled = poll(&event, 1, 10);
        if (polled < 0) { if (errno == EINTR) continue; free(request); free(response); return 70; }
        if (polled == 0 || !(event.revents & POLLIN)) continue;
        memset(request, 0, sizes.seccomp_notif);
        if (ioctl(listener, SECCOMP_IOCTL_NOTIF_RECV, request) != 0) {
            if (errno == EINTR || errno == ENOENT) continue;
            free(request); free(response); return 70;
        }
        memset(response, 0, sizes.seccomp_notif_resp);
        response->id = request->id;
        int number = (int)request->data.nr;
        if (number == refused_number) {
            response->error = -refused_error;
        } else if (hl_native_supervised_denied(number) ||
                   (number == SYS_ioctl && !hl_native_supervised_ioctl_allowed(request->data.args[1])) ||
                   (number == SYS_clone && hl_native_supervised_clone_namespaces(request->data.args[0]))
#ifdef SYS_clone3
                   || number == SYS_clone3
#endif
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
    if (argv == NULL || argv[0] == NULL) return 70;
    if (host == NULL || host->posix_attachment == NULL || host->posix_attachment->borrow_file_at_least == NULL ||
        host->posix_attachment->release == NULL) return 70;
    char **exec_argv = calloc((size_t)argc + 1, sizeof(char *));
    if (exec_argv == NULL) return 70;
    for (uint32_t index = 0; index < argc; ++index) exec_argv[index] = argv[index];
    if (!hl_native_supervised_policy_supported(config)) return 70;
    if (hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL)
        fprintf(stderr, "[hl-native-supervised]\tselected=1\n");
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
        if (hl_linux_fd_snapshot_get(box, fd, &snapshot) != HL_STATUS_OK) goto attachment_failed;
        hl_host_result attached = host->posix_attachment->borrow_file_at_least(host->context, snapshot.host_handle, 64);
        if (attached.status != HL_STATUS_OK || attached.value > INT_MAX) goto attachment_failed;
        borrowed[fd] = (int)attached.value;
    }
    hl_native_supervised_bootstrap *bootstrap = mmap(NULL, sizeof(*bootstrap), PROT_READ | PROT_WRITE,
                                                     MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (bootstrap == MAP_FAILED) goto attachment_failed;
    atomic_init(&bootstrap->listener, -1);
    atomic_init(&bootstrap->target_pid, -1);
    atomic_init(&bootstrap->acknowledged, 0);
    atomic_init(&bootstrap->result_signal, 0);
    if (prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0) {
        munmap(bootstrap, sizeof(*bootstrap)); goto attachment_failed;
    }
    pid_t child = fork();
    if (child < 0) { munmap(bootstrap, sizeof(*bootstrap)); goto attachment_failed; }
    if (child == 0) {
        for (int fd = 0; fd < 3; ++fd) {
            if (borrowed[fd] < 0) continue;
            if (dup2(borrowed[fd], fd) < 0) _exit(70);
            if (borrowed[fd] != fd) close(borrowed[fd]);
        }
        if (fcntl(executable, F_SETFD, 0) != 0) _exit(70);
        if (hl_native_supervised_close_except(executable) != 0) _exit(70);
        hl_native_supervised_volumes volumes;
        if (hl_native_supervised_volumes_open(config->box->volumes, &volumes) != 0 ||
            hl_native_supervised_project_container(config, bootstrap, &volumes) != 0) {
            if (hl_options_get(options, "HL_C_DIAGNOSTICS") != NULL)
                fprintf(stderr, "[hl-native-supervised]\tprojector_errno=%d\n", errno);
            _exit(70);
        }
        int listener = hl_native_supervised_create_listener();
        if (listener < 0) _exit(70);
        atomic_store_explicit(&bootstrap->listener, listener, memory_order_release);
        while (!atomic_load_explicit(&bootstrap->acknowledged, memory_order_acquire)) {}
        close(listener);
        pid_t workload = fork();
        if (workload < 0) _exit(70);
        if (workload > 0) {
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
    for (int fd = 0; fd < 3; ++fd) {
        if (borrowed[fd] >= 0) (void)host->posix_attachment->release(host->context, (uint64_t)borrowed[fd]);
        borrowed[fd] = -1;
    }
    (void)host->posix_attachment->release(host->context, (uint64_t)executable);
    executable = -1;
    int target_pid = -1;
    for (int attempt = 0; attempt < 5000 && target_pid < 0; ++attempt) {
        target_pid = atomic_load_explicit(&bootstrap->target_pid, memory_order_acquire);
        if (target_pid < 0) usleep(1000);
    }
    int pidfd = target_pid < 0 ? -1 : (int)syscall(SYS_pidfd_open, target_pid, 0);
    int listener = -1;
    if (pidfd >= 0) {
        struct pollfd death = {pidfd, POLLIN, 0};
        for (int attempt = 0; attempt < 5000; ++attempt) {
            int remote = atomic_load_explicit(&bootstrap->listener, memory_order_acquire);
            if (remote >= 0) {
                listener = (int)syscall(SYS_pidfd_getfd, pidfd, remote, 0);
                break;
            }
            if (poll(&death, 1, 1) != 0) break;
        }
    }
    if (listener >= 0) atomic_store_explicit(&bootstrap->acknowledged, 1, memory_order_release);
    if (pidfd >= 0) close(pidfd);
    munmap(bootstrap, sizeof(*bootstrap));
    if (listener < 0) {
        (void)kill(child, SIGKILL); (void)waitpid(child, NULL, 0);
        hl_native_supervised_environment_free(environment); free(exec_argv); return 70;
    }
    unsigned char ready = 1;
    if (write(activation_ready, &ready, sizeof(ready)) != (ssize_t)sizeof(ready)) {
        close(listener); (void)kill(child, SIGKILL); (void)waitpid(child, NULL, 0);
        hl_native_supervised_environment_free(environment); free(exec_argv); return 70;
    }
    int result = hl_native_supervised_wait(listener, child, options, guest_signal);
    close(listener);
    hl_native_supervised_environment_free(environment);
    free(exec_argv);
    return result;
attachment_failed:
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
