#if defined(__linux__) && defined(__x86_64__)
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>

extern char **environ;

static int hl_native_supervised_selected(const hl_options *options) {
    const char *value = hl_options_get(options, "HL_NATIVE_SUPERVISED");
    return value != NULL && value[0] != 0 && value[0] != '0';
}

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
    struct sock_filter instructions[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_sendmsg, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
    };
    struct sock_fprog program = {(unsigned short)(sizeof(instructions) / sizeof(instructions[0])), instructions};
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) return -1;
    return (int)syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_NEW_LISTENER, &program);
}

#define HL_NATIVE_SUPERVISED_COPY_CHUNK (64u * 1024u)

static int64_t hl_native_supervised_stream(hl_linux_abi *box, const struct seccomp_notif *request) {
    int write_call = request->data.nr == SYS_write;
    hl_linux_fd fd = (hl_linux_fd)request->data.args[0];
    uint64_t address = request->data.args[1];
    uint64_t remaining = request->data.args[2];
    unsigned char *buffer = malloc(remaining < HL_NATIVE_SUPERVISED_COPY_CHUNK
                                       ? (size_t)remaining
                                       : HL_NATIVE_SUPERVISED_COPY_CHUNK);
    if (buffer == NULL && remaining != 0) return -ENOMEM;
    uint64_t completed = 0;
    do {
        size_t chunk = remaining < HL_NATIVE_SUPERVISED_COPY_CHUNK ? (size_t)remaining
                                                                   : HL_NATIVE_SUPERVISED_COPY_CHUNK;
        int64_t result;
        if (write_call) {
            if (chunk != 0) {
                struct iovec local = {buffer, chunk};
                struct iovec remote = {(void *)(uintptr_t)(address + completed), chunk};
                if (process_vm_readv((pid_t)request->pid, &local, 1, &remote, 1, 0) != (ssize_t)chunk) {
                    result = -EFAULT;
                    goto finished;
                }
            }
            result = hl_linux_write(box, fd, buffer, chunk);
        } else {
            result = hl_linux_read(box, fd, buffer, chunk);
            if (result > 0) {
                struct iovec local = {buffer, (size_t)result};
                struct iovec remote = {(void *)(uintptr_t)(address + completed), (size_t)result};
                if (process_vm_writev((pid_t)request->pid, &local, 1, &remote, 1, 0) != result) result = -EFAULT;
            }
        }
finished:
        if (result < 0) { free(buffer); return completed != 0 ? (int64_t)completed : result; }
        completed += (uint64_t)result;
        remaining -= (uint64_t)result;
        if ((size_t)result != chunk || chunk == 0) break;
    } while (remaining != 0);
    free(buffer);
    return (int64_t)completed;
}

static int hl_native_supervised_wait(hl_linux_abi *box, int listener, pid_t leader) {
    int status;
    for (;;) {
        pid_t waited = waitpid(leader, &status, WNOHANG);
        if (waited == leader)
            return WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
        if (waited < 0 && errno != EINTR) return 70;
        struct pollfd event = {listener, POLLIN, 0};
        int polled = poll(&event, 1, 10);
        if (polled < 0) { if (errno == EINTR) continue; return 70; }
        if (polled == 0 || !(event.revents & POLLIN)) continue;
        struct seccomp_notif request = {0};
        if (ioctl(listener, SECCOMP_IOCTL_NOTIF_RECV, &request) != 0) {
            if (errno == EINTR || errno == ENOENT) continue;
            return 70;
        }
        struct seccomp_notif_resp response = {.id = request.id};
        if ((request.data.nr == SYS_read || request.data.nr == SYS_write) && request.data.args[0] <= STDERR_FILENO) {
            int64_t result = hl_native_supervised_stream(box, &request);
            if (result < 0)
                response.error = (int32_t)result;
            else
                response.val = result;
        } else {
            response.flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
        }
        if (ioctl(listener, SECCOMP_IOCTL_NOTIF_SEND, &response) != 0 && errno != ENOENT) return 70;
    }
}

static int32_t hl_native_supervised_run(hl_linux_abi *box, const char *rootfs, char *const argv[], int activation_ready) {
    if (argv == NULL || argv[0] == NULL) return 70;
    if (rootfs == NULL) rootfs = "/";
    int channel[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, channel) != 0) return 70;
    pid_t child = fork();
    if (child < 0) { close(channel[0]); close(channel[1]); return 70; }
    if (child == 0) {
        close(channel[0]);
        int listener = hl_native_supervised_create_listener();
        if (listener < 0 || hl_native_supervised_send_listener(channel[1], listener) != 0) _exit(70);
        close(listener);
        close(channel[1]);
        if (chroot(rootfs) != 0 || chdir("/") != 0) _exit(70);
        execve(argv[0], argv, environ);
        _exit(errno == ENOENT ? 127 : 126);
    }
    close(channel[1]);
    int listener = hl_native_supervised_receive_listener(channel[0]);
    close(channel[0]);
    if (listener < 0) { (void)kill(child, SIGKILL); (void)waitpid(child, NULL, 0); return 70; }
    unsigned char ready = 1;
    if (write(activation_ready, &ready, sizeof(ready)) != (ssize_t)sizeof(ready)) {
        close(listener); (void)kill(child, SIGKILL); (void)waitpid(child, NULL, 0); return 70;
    }
    int result = hl_native_supervised_wait(box, listener, child);
    close(listener);
    return result;
}
#else
static int hl_native_supervised_selected(const hl_options *options) { (void)options; return 0; }
static int32_t hl_native_supervised_run(hl_linux_abi *box, const char *rootfs, char *const argv[], int activation_ready) {
    (void)box; (void)rootfs; (void)argv; (void)activation_ready; return 70;
}
#endif
