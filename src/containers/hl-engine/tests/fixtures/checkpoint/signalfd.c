#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/epoll.h>
#include <sys/select.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <unistd.h>

static int signal_fd, process_signal_fd, epoll_fd;
static const char *release_path, *final_release_path;
static volatile pid_t worker_tid;
static volatile sig_atomic_t low_active, high_seen, high_nested;
static _Atomic int worker_done;

static long raw_write_marker(const char *text, size_t length) {
#if defined(__aarch64__)
    register long x0 __asm__("x0") = STDOUT_FILENO;
    register const char *x1 __asm__("x1") = text;
    register size_t x2 __asm__("x2") = length;
    register long x8 __asm__("x8") = SYS_write;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
#elif defined(__x86_64__)
    register long rax __asm__("rax") = SYS_write;
    register long rdi __asm__("rdi") = STDOUT_FILENO;
    register const char *rsi __asm__("rsi") = text;
    register size_t rdx __asm__("rdx") = length;
    __asm__ volatile("syscall" : "+a"(rax) : "D"(rdi), "S"(rsi), "d"(rdx) : "rcx", "r11", "memory");
    return rax;
#endif
}

static void marker(const char *text, size_t length) {
    if (raw_write_marker(text, length) != (long)length) _exit(45);
}

static long raw_unblock_high(const uint64_t *mask) {
#if defined(__aarch64__)
    register long x0 __asm__("x0") = SIG_UNBLOCK;
    register const uint64_t *x1 __asm__("x1") = mask;
    register long x2 __asm__("x2") = 0;
    register long x3 __asm__("x3") = 8;
    register long x8 __asm__("x8") = SYS_rt_sigprocmask;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x8) : "memory");
    return x0;
#elif defined(__x86_64__)
    register long rax __asm__("rax") = SYS_rt_sigprocmask;
    register long rdi __asm__("rdi") = SIG_UNBLOCK;
    register const uint64_t *rsi __asm__("rsi") = mask;
    register long rdx __asm__("rdx") = 0;
    register long r10 __asm__("r10") = 8;
    __asm__ volatile("syscall" : "+a"(rax) : "D"(rdi), "S"(rsi), "d"(rdx), "r"(r10) : "rcx", "r11", "memory");
    return rax;
#else
#error unsupported checkpoint fixture architecture
#endif
}

static void high_handler(int signal) {
    if (signal == SIGRTMAX) {
        high_seen++;
        if (low_active) high_nested++;
    }
}

static void low_handler(int signal) {
    if (signal != SIGUSR1) return;
    low_active = 1;
    static const char ready[] = "CYCLE-READY\n";
    marker(ready, sizeof ready - 1);
    while (access(final_release_path, F_OK) != 0) {
        if (errno != ENOENT) _exit(41);
        sched_yield();
    }
    static const char released[] = "LOW-RELEASE\n";
    marker(released, sizeof released - 1);
    uint64_t highest = UINT64_C(1) << 63;
    static const char before[] = "BEFORE-INNER-SYSCALL\n";
    if (raw_write_marker(before, sizeof before - 1) != (long)(sizeof before - 1)) _exit(44);
    (void)raw_unblock_high(&highest);
    low_active = 0;
}

static int readiness(int expected) {
    struct pollfd poll_event = {.fd = signal_fd, .events = POLLIN};
    int poll_ready = poll(&poll_event, 1, 0) == expected;
    fd_set read_set;
    FD_ZERO(&read_set);
    FD_SET(signal_fd, &read_set);
    struct timeval timeout = {0};
    int select_ready = select(signal_fd + 1, &read_set, NULL, NULL, &timeout) == expected;
    struct epoll_event event;
    int epoll_ready = epoll_wait(epoll_fd, &event, 1, 0) == expected;
    return poll_ready && select_ready && epoll_ready;
}

static void *worker(void *unused) {
    (void)unused;
    worker_tid = (pid_t)syscall(SYS_gettid);
    while (access(release_path, F_OK) != 0) {
        if (errno != ENOENT) return (void *)1;
        sched_yield();
    }
    int ready = readiness(1);
    struct signalfd_siginfo info = {0};
    ssize_t count = read(signal_fd, &info, sizeof info);
    struct signalfd_siginfo process_info = {0};
    ssize_t process_count = read(process_signal_fd, &process_info, sizeof process_info);
    int failed = !(ready && count == sizeof info && info.ssi_signo == (uint32_t)SIGRTMAX &&
                   info.ssi_code == SI_TKILL && process_count == sizeof process_info &&
                   process_info.ssi_signo == (uint32_t)SIGUSR2 && process_info.ssi_code == SI_USER);
    atomic_store_explicit(&worker_done, failed ? -1 : 1, memory_order_release);
    return (void *)(uintptr_t)failed;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    release_path = argv[1];
    final_release_path = argv[2];
    char output[1024];
    if (snprintf(output, sizeof output, "%s.output", release_path) >= (int)sizeof output) return 2;
    int output_fd = open(output, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (output_fd < 0 || dup2(output_fd, STDOUT_FILENO) < 0 || dup2(output_fd, STDERR_FILENO) < 0) return 2;
    if (output_fd > STDERR_FILENO) close(output_fd);

    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigaddset(&mask, SIGUSR2);
    sigaddset(&mask, SIGRTMAX);
    if (pthread_sigmask(SIG_BLOCK, &mask, NULL) != 0) return 3;
    sigset_t targeted_mask;
    sigemptyset(&targeted_mask);
    sigaddset(&targeted_mask, SIGUSR1);
    sigaddset(&targeted_mask, SIGRTMAX);
    sigset_t process_mask;
    sigemptyset(&process_mask);
    sigaddset(&process_mask, SIGUSR2);
    signal_fd = signalfd(-1, &targeted_mask, SFD_NONBLOCK | SFD_CLOEXEC);
    process_signal_fd = signalfd(-1, &process_mask, SFD_NONBLOCK | SFD_CLOEXEC);
    epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event registration = {.events = EPOLLIN, .data.u64 = 0x7369676e616c6664ULL};
    if (signal_fd < 0 || process_signal_fd < 0 || epoll_fd < 0 ||
        epoll_ctl(epoll_fd, EPOLL_CTL_ADD, signal_fd, &registration) != 0)
        return 4;

    pthread_t thread;
    if (pthread_create(&thread, NULL, worker, NULL) != 0) return 5;
    while (!worker_tid) sched_yield();
    if (syscall(SYS_tgkill, getpid(), worker_tid, SIGRTMAX) != 0 || kill(getpid(), SIGUSR2) != 0) return 6;
    errno = 0;
    struct signalfd_siginfo wrong_info;
    int wrong_read = read(signal_fd, &wrong_info, sizeof wrong_info) == -1 && errno == EAGAIN;
    int wrong_ready = readiness(0);
    dprintf(STDOUT_FILENO, "READY targeted_wrong_read=%d targeted_wrong_ready=%d\n", wrong_read, wrong_ready);

    while (atomic_load_explicit(&worker_done, memory_order_acquire) == 0) sched_yield();
    void *thread_result = (void *)1;
    pthread_join(thread, &thread_result);
    if (thread_result != NULL) return 7;
    dprintf(STDOUT_FILENO, "TARGETED-RESTORED\n");

    struct sigaction low = {.sa_handler = low_handler};
    struct sigaction high = {.sa_handler = high_handler};
    sigemptyset(&low.sa_mask);
    sigemptyset(&high.sa_mask);
    if (sigaction(SIGUSR1, &low, NULL) != 0 || sigaction(SIGRTMAX, &high, NULL) != 0) return 8;
    if (syscall(SYS_tgkill, getpid(), syscall(SYS_gettid), SIGUSR1) != 0 ||
        syscall(SYS_tgkill, getpid(), syscall(SYS_gettid), SIGRTMAX) != 0) return 9;
    sigset_t lower;
    sigemptyset(&lower);
    sigaddset(&lower, SIGUSR1);
    if (pthread_sigmask(SIG_UNBLOCK, &lower, NULL) != 0) return 10;

    sigset_t highest;
    sigemptyset(&highest);
    sigaddset(&highest, SIGRTMAX);
    if (pthread_sigmask(SIG_UNBLOCK, &highest, NULL) != 0) return 11;
    dprintf(STDOUT_FILENO, "HIGH-UNBLOCK\n");
    for (int attempt = 0; attempt < 1000 && high_seen != 1; ++attempt) usleep(1000);
    dprintf(STDOUT_FILENO, "DEFER-RESTORED seen=%d nested=%d\n", high_seen == 1, high_nested == 0);
    return !(wrong_read && wrong_ready && high_seen == 1 && high_nested == 0);
}
