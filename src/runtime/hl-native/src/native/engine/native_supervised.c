static int hl_native_supervised_selected(const hl_options *options) {
    const char *value = hl_options_get(options, "HL_NATIVE_SUPERVISED");
    return value != NULL && value[0] != 0 && value[0] != '0';
}

#if defined(__linux__) && defined(__x86_64__)
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>

static int hl_native_supervised_available(void) { return 1; }

static int hl_native_supervised_send_listener(int socket, int listener) {
    char byte = 1;
    struct iovec vector = {&byte, sizeof(byte)};
    union { struct cmsghdr align; char bytes[CMSG_SPACE(sizeof(int))]; } control = {0};
    struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1,
                             .msg_control = control.bytes, .msg_controllen = sizeof(control.bytes)};
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    header->cmsg_level = SOL_SOCKET;
    header->cmsg_type = SCM_RIGHTS;
    header->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(header), &listener, sizeof(listener));
    return sendmsg(socket, &message, 0) == (ssize_t)sizeof(byte) ? 0 : -1;
}

static int hl_native_supervised_receive_listener(int socket) {
    char byte;
    struct iovec vector = {&byte, sizeof(byte)};
    union { struct cmsghdr align; char bytes[CMSG_SPACE(sizeof(int))]; } control = {0};
    struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1,
                             .msg_control = control.bytes, .msg_controllen = sizeof(control.bytes)};
    if (recvmsg(socket, &message, 0) != (ssize_t)sizeof(byte)) return -1;
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    if (header == NULL || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_RIGHTS ||
        header->cmsg_len != CMSG_LEN(sizeof(int))) return -1;
    int listener;
    memcpy(&listener, CMSG_DATA(header), sizeof(listener));
    return listener;
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
        HL_NATIVE_NOTIFY(SYS_execve), HL_NATIVE_NOTIFY(SYS_clone), HL_NATIVE_NOTIFY(SYS_fork),
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
        HL_NATIVE_NOTIFY(SYS_seccomp),
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
        if ((int)request->data.nr == refused_number) {
            response->error = -refused_error;
        } else {
            response->flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
        }
        if (ioctl(listener, SECCOMP_IOCTL_NOTIF_SEND, response) != 0 && errno != ENOENT) {
            free(request); free(response); return 70;
        }
    }
}

static int32_t hl_native_supervised_run(const hl_host_services *host, hl_linux_abi *box, const char *rootfs,
                                        char *const argv[],
                                        const hl_options *options, int activation_ready, int *guest_signal) {
    if (argv == NULL || argv[0] == NULL) return 70;
    if (rootfs == NULL) rootfs = "/";
    if (host == NULL || host->posix_attachment == NULL || host->posix_attachment->borrow_file == NULL ||
        host->posix_attachment->release == NULL) return 70;
    char **environment = hl_native_supervised_environment(options);
    if (environment == NULL) return 70;
    int borrowed[3] = {-1, -1, -1};
    for (hl_linux_fd fd = 0; fd < 3; ++fd) {
        hl_linux_fd_snapshot snapshot = {0};
        if (hl_linux_fd_snapshot_get(box, fd, &snapshot) != HL_STATUS_OK) continue;
        hl_host_result attached = host->posix_attachment->borrow_file(host->context, snapshot.host_handle);
        if (attached.status != HL_STATUS_OK || attached.value > INT_MAX) goto attachment_failed;
        borrowed[fd] = (int)attached.value;
    }
    int channel[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, channel) != 0) goto attachment_failed;
    if (prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0) {
        close(channel[0]); close(channel[1]); goto attachment_failed;
    }
    pid_t child = fork();
    if (child < 0) { close(channel[0]); close(channel[1]); goto attachment_failed; }
    if (child == 0) {
        close(channel[0]);
        for (int fd = 0; fd < 3; ++fd) {
            if (borrowed[fd] < 0) continue;
            if (dup2(borrowed[fd], fd) < 0) _exit(70);
            if (borrowed[fd] != fd) close(borrowed[fd]);
        }
        int listener = hl_native_supervised_create_listener();
        if (listener < 0 || hl_native_supervised_send_listener(channel[1], listener) != 0) _exit(70);
        close(listener);
        close(channel[1]);
        if (chroot(rootfs) != 0 || chdir("/") != 0) _exit(70);
        execve(argv[0], argv, environment);
        _exit(errno == ENOENT ? 127 : 126);
    }
    for (int fd = 0; fd < 3; ++fd) {
        if (borrowed[fd] >= 0) (void)host->posix_attachment->release(host->context, (uint64_t)borrowed[fd]);
        borrowed[fd] = -1;
    }
    close(channel[1]);
    int listener = hl_native_supervised_receive_listener(channel[0]);
    close(channel[0]);
    if (listener < 0) {
        (void)kill(child, SIGKILL); (void)waitpid(child, NULL, 0);
        hl_native_supervised_environment_free(environment); return 70;
    }
    unsigned char ready = 1;
    if (write(activation_ready, &ready, sizeof(ready)) != (ssize_t)sizeof(ready)) {
        close(listener); (void)kill(child, SIGKILL); (void)waitpid(child, NULL, 0);
        hl_native_supervised_environment_free(environment); return 70;
    }
    int result = hl_native_supervised_wait(listener, child, options, guest_signal);
    close(listener);
    hl_native_supervised_environment_free(environment);
    return result;
attachment_failed:
    for (int fd = 0; fd < 3; ++fd)
        if (borrowed[fd] >= 0) (void)host->posix_attachment->release(host->context, (uint64_t)borrowed[fd]);
    hl_native_supervised_environment_free(environment);
    return 70;
}
#else
static int hl_native_supervised_available(void) { return 0; }
static int32_t hl_native_supervised_run(const hl_host_services *host, hl_linux_abi *box, const char *rootfs,
                                        char *const argv[],
                                        const hl_options *options, int activation_ready, int *guest_signal) {
    (void)host; (void)box; (void)rootfs; (void)argv; (void)options; (void)activation_ready;
    *guest_signal = 0; return 70;
}
#endif
