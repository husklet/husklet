#define _GNU_SOURCE
#include <errno.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int install(void) {
    struct sock_filter code[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getppid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EACCES),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {.len = 4, .filter = code};
    return prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program, 0, 0);
}

static int waited(pid_t child, int code) {
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == code;
}

int main(int argc, char **argv) {
    if (argc == 2) {
        errno = 0;
        _exit(syscall(__NR_getppid) == -1 && errno == EACCES ? 61 : 62);
    }
    int initial = prctl(PR_GET_SECCOMP, 0, 0, 0, 0) == SECCOMP_MODE_DISABLED;
    int nnp = prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0;
    int installed = install() == 0;
    int mode = prctl(PR_GET_SECCOMP, 0, 0, 0, 0) == SECCOMP_MODE_FILTER;
    errno = 0;
    int deny = syscall(__NR_getppid) == -1 && errno == EACCES;
    int allow = getpid() > 0;
    pid_t child = fork();
    if (child == 0) {
        errno = 0;
        _exit(syscall(__NR_getppid) == -1 && errno == EACCES ? 63 : 64);
    }
    int inherit = waited(child, 63);
    child = fork();
    if (child == 0) {
        char *args[] = {argv[0], (char *)"exec", 0};
        execve("/proc/self/exe", args, environ);
        _exit(65);
    }
    int exec = waited(child, 61);
    printf("seccomp initial=%d nnp=%d install=%d mode=%d deny=%d allow=%d inherit=%d exec=%d\n", initial, nnp,
           installed, mode, deny, allow, inherit, exec);
    return initial && nnp && installed && mode && deny && allow && inherit && exec ? 0 : 1;
}
