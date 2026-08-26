#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/socket.h>
#include <sys/mount.h>
#include <sys/ioctl.h>
#include <sys/ptrace.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/statvfs.h>
#include <linux/capability.h>
#include <linux/sched.h>
#include <sched.h>
#include <sys/wait.h>
#include <unistd.h>
#include <signal.h>
#include <pthread.h>

static void *thread_return(void *argument) { return argument; }

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "output")) {
        fputs("native-supervised", stdout);
        return 23;
    }
    if (argc > 1 && !strcmp(argv[1], "descendant")) {
        pid_t child = fork();
        if (child < 0) return 31;
        if (child == 0) {
            errno = 0;
            long result = syscall(SYS_getpid);
            _exit(result == -1 && errno == ENOSYS ? 37 : 38);
        }
        int status;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 37) return 32;
        fputs("descendant-supervised", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "orphan")) {
        pid_t child = fork();
        if (child < 0) return 41;
        if (child == 0) {
            usleep(20000);
            errno = 0;
            long result = syscall(SYS_getpid);
            if (result == -1 && errno == ENOSYS && write(1, "orphan-supervised", 17) != 17) _exit(43);
            _exit(0);
        }
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "environment")) {
        const char *value = getenv("NATIVE_SUPERVISED_ENV");
        const char *leak = getenv("HOME");
        if (value == NULL || strcmp(value, "line1\nline2\\tail") || leak != NULL) return 42;
        fputs("environment-exact", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "streams")) {
        char input[5] = {0};
        if (read(0, input, 4) != 4 || memcmp(input, "pipe", 4)) return 51;
        struct iovec output[] = {{"write", 5}, {"v", 1}};
        if (writev(1, output, 2) != 6) return 52;
        if (write(2, "stderr", 6) != 6) return 53;
        if (dup2(1, 7) != 7 || write(7, "-dup", 4) != 4) return 54;
    }
    if (argc > 1 && !strcmp(argv[1], "signal")) raise(SIGTERM);
    if (argc > 1 && !strcmp(argv[1], "sendmsg-denied")) {
        char byte = 1;
        struct iovec vector = {&byte, 1};
        struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1};
        errno = 0;
        if (sendmsg(1, &message, 0) != -1 || errno != EPERM) return 62;
        fputs("sendmsg-denied", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "secure-jail")) {
        for (int fd = 3; fd < 64; ++fd)
            if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) return 70;
        if (fcntl(1048575, F_GETFD) != -1 || errno != EBADF) return 70;
        if (prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1) return 71;
        errno = 0;
        if (mount("none", "/tmp", "tmpfs", 0, NULL) != -1 || errno != EPERM) return 72;
        errno = 0;
        if (unshare(CLONE_NEWNS) != -1 || errno != EPERM) return 73;
        errno = 0;
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != -1 || errno != EPERM) return 74;
        errno = 0;
        if (socket(AF_INET, SOCK_STREAM, 0) != -1 || errno != EPERM) return 75;
        struct __user_cap_header_struct cap_header = {_LINUX_CAPABILITY_VERSION_3, 0};
        struct __user_cap_data_struct cap_data[2] = {{0}};
        if (syscall(SYS_capget, &cap_header, cap_data) != 0 || cap_data[0].effective || cap_data[0].permitted ||
            cap_data[0].inheritable || cap_data[1].effective || cap_data[1].permitted || cap_data[1].inheritable)
            return 76;
        char status_text[4096] = {0};
        int status_fd = open("/proc/self/status", O_RDONLY);
        ssize_t status_size = status_fd < 0 ? -1 : read(status_fd, status_text, sizeof(status_text) - 1);
        if (status_fd >= 0) close(status_fd);
        if (status_size <= 0 || strstr(status_text, "CapInh:\t0000000000000000") == NULL ||
            strstr(status_text, "CapPrm:\t0000000000000000") == NULL ||
            strstr(status_text, "CapEff:\t0000000000000000") == NULL ||
            strstr(status_text, "CapBnd:\t0000000000000000") == NULL ||
            strstr(status_text, "CapAmb:\t0000000000000000") == NULL)
            return 76;
        errno = 0;
        if (syscall(SYS_clone, CLONE_NEWNS | SIGCHLD, 0, 0, 0, 0) != -1 || errno != EPERM) return 77;
#ifdef SYS_clone3
        struct clone_args clone_args = {.flags = CLONE_NEWUTS, .exit_signal = SIGCHLD};
        errno = 0;
        if (syscall(SYS_clone3, &clone_args, sizeof(clone_args)) != -1 || errno != EPERM) return 78;
#endif
        errno = 0;
        if (ioctl(1, 0xdeadbeefUL, 0) != -1 || errno != EPERM) return 79;
        fputs("secure-jail", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "root-contract")) {
        char cwd[64];
        if (getcwd(cwd, sizeof(cwd)) == NULL || strcmp(cwd, "/tmp")) return 81;
        char hostname[64];
        if (gethostname(hostname, sizeof(hostname)) != 0 || strcmp(hostname, "husklet-native")) return 81;
        errno = 0;
        if (open("/etc/hostname", O_RDONLY) != -1 || errno != ENOENT) return 82;
        errno = 0;
        if (open("/proc/hostile", O_RDONLY) != -1 || errno != ENOENT) return 83;
        char namespace[128];
        if (readlink("/proc/self/ns/mnt", namespace, sizeof(namespace)) <= 0) return 84;
        errno = 0;
        if (open("/blocked", O_WRONLY | O_CREAT, 0600) != -1 || errno != EROFS) return 85;
        fputs("root-contract", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "volumes")) {
        char source[16] = {0};
        int input = open("/src/input.c", O_RDONLY);
        if (input < 0 || read(input, source, sizeof(source)) != 7 || memcmp(source, "source\n", 7)) return 91;
        close(input);
        errno = 0;
        if (open("/src/blocked", O_WRONLY | O_CREAT, 0600) != -1 || errno != EROFS) return 92;
        errno = 0;
        if (open("/src/nested/blocked", O_WRONLY | O_CREAT, 0600) != -1 || errno != EROFS) return 92;
        struct statvfs mounted;
        if (statvfs("/src/nested", &mounted) != 0 || !(mounted.f_flag & ST_RDONLY) ||
            !(mounted.f_flag & ST_NOSUID) || !(mounted.f_flag & ST_NODEV)) return 92;
        int output = open("/out/result.o", O_WRONLY | O_CREAT | O_TRUNC, 0600);
        if (output < 0 || write(output, "object\n", 7) != 7 || close(output) != 0) return 93;
        fputs("volumes", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "identity-limits")) {
        if (getuid() != 1234 || geteuid() != 1234 || getgid() != 2345 || getegid() != 2345 || getgroups(0, NULL) != 0)
            return 94;
        struct rlimit nofile, core;
        if (getrlimit(RLIMIT_NOFILE, &nofile) != 0 || nofile.rlim_cur != 32 || nofile.rlim_max != 32 ||
            getrlimit(RLIMIT_CORE, &core) != 0 || core.rlim_cur != 0 || core.rlim_max != 0)
            return 95;
        fputs("identity-limits", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "namespaces")) {
        const char *names[] = {"mnt", "pid", "net", "uts", "ipc"};
        const char *keys[] = {"HOST_MNT_NS", "HOST_PID_NS", "HOST_NET_NS", "HOST_UTS_NS", "HOST_IPC_NS"};
        for (size_t index = 0; index < 5; ++index) {
            char path[64], value[128] = {0};
            snprintf(path, sizeof(path), "/proc/self/ns/%s", names[index]);
            if (readlink(path, value, sizeof(value) - 1) <= 0 || getenv(keys[index]) == NULL ||
                strcmp(value, getenv(keys[index])) == 0)
                return 96;
        }
        fputs("namespaces", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "pthread")) {
        pthread_t thread;
        void *result = NULL;
        if (pthread_create(&thread, NULL, thread_return, (void *)0x1234) != 0 ||
            pthread_join(thread, &result) != 0 || result != (void *)0x1234)
            return 97;
        fputs("pthread", stdout);
    }
    return 0;
}
