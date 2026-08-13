#define _GNU_SOURCE
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

/* Linux exposes this siginfo si_code in asm-generic/siginfo.h, but some
 * cross-libc header sets omit the public spelling.  The UAPI value is stable
 * and shared by both supported guest architectures. */
#ifndef SYS_SECCOMP
#define SYS_SECCOMP 1
#endif

static int filter(unsigned action, unsigned flags) {
    struct sock_filter code[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getppid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, action),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {.len = 4, .filter = code};
    return syscall(__NR_seccomp, SECCOMP_SET_MODE_FILTER, flags, &program);
}

static int status_exit(pid_t child, int code) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == code;
}

static int status_signal(pid_t child, int signal) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFSIGNALED(status) && WTERMSIG(status) == signal;
}

static int permission_case(void) {
    pid_t child = fork();
    if (child == 0) {
        errno = 0;
        _exit(filter(SECCOMP_RET_ALLOW, 0) == -1 && errno == EACCES ? 0 : 1);
    }
    return status_exit(child, 0);
}

static int invalid_case(void) {
    pid_t child = fork();
    if (child == 0) {
        prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        struct sock_filter code[] = {BPF_JUMP(BPF_JMP | BPF_JA, 8, 0, 0)};
        struct sock_fprog program = {.len = 1, .filter = code};
        errno = 0;
        _exit(syscall(__NR_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program) == -1
              && errno == EINVAL ? 0 : 1);
    }
    return status_exit(child, 0);
}

static int strict_case(void) {
    pid_t child = fork();
    if (child == 0) {
        prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        if (prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT, 0, 0, 0) != 0) _exit(2);
        syscall(__NR_getpid);
        _exit(3);
    }
    return status_signal(child, SIGKILL);
}

static volatile sig_atomic_t trap_seen;
static void trap_handler(int signal, siginfo_t *info, void *context) {
    (void)context;
#if defined(__aarch64__)
    const unsigned expected_arch = AUDIT_ARCH_AARCH64;
#else
    const unsigned expected_arch = AUDIT_ARCH_X86_64;
#endif
    trap_seen = signal == SIGSYS && info->si_code == SYS_SECCOMP
        && info->si_syscall == __NR_getppid && (unsigned)info->si_arch == expected_arch;
}

static int trap_case(void) {
    pid_t child = fork();
    if (child == 0) {
        struct sigaction action = {.sa_sigaction = trap_handler, .sa_flags = SA_SIGINFO};
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGSYS, &action, 0) || prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)
            || filter(SECCOMP_RET_TRAP | 17, 0)) _exit(2);
        syscall(__NR_getppid);
        _exit(trap_seen ? 0 : 3);
    }
    return status_exit(child, 0);
}

static int kill_process_case(void) {
    pid_t child = fork();
    if (child == 0) {
        prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        if (filter(SECCOMP_RET_KILL_PROCESS, 0)) _exit(2);
        syscall(__NR_getppid); _exit(3);
    }
    return status_signal(child, SIGSYS);
}

static void *killed_thread(void *unused) {
    (void)unused;
    if (filter(SECCOMP_RET_KILL_THREAD, 0)) return (void *)1;
    syscall(__NR_getppid);
    return (void *)2;
}

static int kill_thread_case(void) {
    pid_t child = fork();
    if (child == 0) {
        if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)) _exit(2);
        pthread_t thread;
        if (pthread_create(&thread, 0, killed_thread, 0)) _exit(3);
        void *result = (void *)9;
        if (pthread_join(thread, &result)) _exit(4);
        _exit(0);
    }
    return status_exit(child, 0);
}

static atomic_int tsync_go;
static void *tsync_thread(void *unused) {
    (void)unused;
    while (!atomic_load_explicit(&tsync_go, memory_order_acquire)) sched_yield();
    errno = 0;
    return (void *)(long)(syscall(__NR_getppid) == -1 && errno == 23);
}

static int tsync_case(void) {
    pid_t child = fork();
    if (child == 0) {
        pthread_t thread; atomic_store(&tsync_go, 0);
        if (pthread_create(&thread, 0, tsync_thread, 0)) _exit(2);
        if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)
            || filter(SECCOMP_RET_ERRNO | 23, SECCOMP_FILTER_FLAG_TSYNC)) _exit(3);
        atomic_store_explicit(&tsync_go, 1, memory_order_release);
        void *result = 0;
        if (pthread_join(thread, &result)) _exit(4);
        _exit((long)result == 1 ? 0 : 5);
    }
    return status_exit(child, 0);
}

static int stack_case(void) {
    pid_t child = fork();
    if (child == 0) {
        if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)
            || filter(SECCOMP_RET_ERRNO | 7, 0)
            || filter(SECCOMP_RET_ERRNO | 9, 0)) _exit(2);
        errno = 0;
        _exit(syscall(__NR_getppid) == -1 && errno == 9 ? 0 : 3);
    }
    return status_exit(child, 0);
}

static int clone_case(void) {
    pid_t child = fork();
    if (child == 0) {
        if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)
            || filter(SECCOMP_RET_ERRNO | 31, 0)) _exit(2);
        pid_t inherited = fork();
        if (inherited == 0) {
            errno = 0; _exit(syscall(__NR_getppid) == -1 && errno == 31 ? 0 : 1);
        }
        _exit(status_exit(inherited, 0) ? 0 : 3);
    }
    return status_exit(child, 0);
}

int main(void) {
    int permission = permission_case(), invalid = invalid_case(), strict = strict_case();
    int trap = trap_case(), kill_thread = kill_thread_case(), kill_process = kill_process_case();
    int tsync = tsync_case(), stack = stack_case(), clone = clone_case();
    printf("seccomp permission=%d invalid=%d strict=%d trap=%d kill_thread=%d kill_process=%d tsync=%d stack=%d clone=%d\n",
           permission, invalid, strict, trap, kill_thread, kill_process, tsync, stack, clone);
    return permission && invalid && strict && trap && kill_thread && kill_process
        && tsync && stack && clone ? 0 : 1;
}
