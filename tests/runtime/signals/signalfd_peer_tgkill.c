#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <poll.h>
#include <sys/select.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int signal_fd, epoll_fd, command_pipe[2], result_pipe[2], ready_pipe[2];
static volatile pid_t worker_tid;
static pid_t main_tid;
static volatile pid_t info_tid;
static volatile sig_atomic_t info_done;
static volatile sig_atomic_t info_ok;
static int blocking_fd;
static volatile pid_t blocking_tid;
static int cleanup_command[2], cleanup_result[2];
static volatile pid_t cleanup_tid;

static void *cleanup_worker(void *unused) {
    (void)unused;
    cleanup_tid = (pid_t)syscall(SYS_gettid);
    char command = 0;
    if (read(cleanup_command[0], &command, 1) != 1 || command != 'r') return NULL;
    int count = 0;
    int spins = 0;
    while (count < 128 && spins < 1000000) {
        struct signalfd_siginfo info = {0};
        ssize_t amount = read(signal_fd, &info, sizeof(info));
        if (amount == sizeof(info) && info.ssi_signo == SIGRTMIN)
            ++count;
        else if (amount < 0 && errno == EAGAIN) {
            ++spins;
            sched_yield();
        } else
            break;
    }
    struct signalfd_siginfo extra;
    errno = 0;
    int ok = count == 128 && read(signal_fd, &extra, sizeof(extra)) == -1 && errno == EAGAIN;
    (void)write(cleanup_result[1], &ok, sizeof(ok));
    return NULL;
}

static void *blocking_worker(void *unused) {
    (void)unused;
    blocking_tid = (pid_t)syscall(SYS_gettid);
    struct signalfd_siginfo info = {0};
    ssize_t amount = read(blocking_fd, &info, sizeof(info));
    return (void *)(uintptr_t)!(amount == sizeof(info) && info.ssi_signo == SIGUSR2 && info.ssi_code == SI_TKILL);
}

static void info_handler(int signal, siginfo_t *info, void *context) {
    (void)context;
    info_ok = signal == SIGWINCH && info->si_signo == SIGWINCH && info->si_code == SI_TKILL &&
              info->si_pid == getpid() && info->si_uid == getuid() && syscall(SYS_gettid) == info_tid;
    info_done = 1;
}

static void *info_worker(void *unused) {
    (void)unused;
    info_tid = (pid_t)syscall(SYS_gettid);
    while (!info_done)
        sched_yield();
    return NULL;
}

struct sender_args {
    int signal;
    int count;
};

static void *sender(void *opaque) {
    const struct sender_args *args = opaque;
    for (int index = 0; index < args->count; ++index)
        if (syscall(SYS_tgkill, getpid(), main_tid, args->signal) != 0) return (void *)1;
    return NULL;
}

static void *worker(void *unused) {
    (void)unused;
    worker_tid = (pid_t)syscall(SYS_gettid);
    char command;
    if (read(command_pipe[0], &command, 1) != 1) return NULL;
    struct timespec before, after_poll, after_select, after;
    clock_gettime(CLOCK_MONOTONIC, &before);
    struct pollfd poll_event[2] = {{.fd = signal_fd, .events = POLLIN}, {.fd = ready_pipe[0], .events = POLLIN}};
    int poll_ready = poll(poll_event, 2, 1000) == 2 && (poll_event[0].revents & POLLIN) &&
                     (poll_event[1].revents & POLLIN);
    clock_gettime(CLOCK_MONOTONIC, &after_poll);
    fd_set read_set;
    FD_ZERO(&read_set);
    FD_SET(signal_fd, &read_set);
    FD_SET(ready_pipe[0], &read_set);
    struct timeval select_timeout = {.tv_sec = 1};
    int select_bound = signal_fd > ready_pipe[0] ? signal_fd + 1 : ready_pipe[0] + 1;
    int select_ready = select(select_bound, &read_set, NULL, NULL, &select_timeout) == 2 &&
                       FD_ISSET(signal_fd, &read_set) && FD_ISSET(ready_pipe[0], &read_set);
    clock_gettime(CLOCK_MONOTONIC, &after_select);
    struct epoll_event event;
    int ready = epoll_wait(epoll_fd, &event, 1, 1000);
    int epoll_ready = ready == 1 && event.data.u64 == 0x51 && (event.events & EPOLLIN);
    int oneshot_quiet = epoll_wait(epoll_fd, &event, 1, 0) == 0;
    struct epoll_event rearm = {.events = EPOLLIN | EPOLLONESHOT, .data.u64 = 0x51};
    int oneshot_rearmed = epoll_ctl(epoll_fd, EPOLL_CTL_MOD, signal_fd, &rearm) == 0 &&
                          epoll_wait(epoll_fd, &event, 1, 0) == 1;
    clock_gettime(CLOCK_MONOTONIC, &after);
    long elapsed_ms = (after.tv_sec - before.tv_sec) * 1000L + (after.tv_nsec - before.tv_nsec) / 1000000L;
    struct signalfd_siginfo info = {0};
    ssize_t count = read(signal_fd, &info, sizeof(info));
    int ok = elapsed_ms < 500 && poll_ready && select_ready && epoll_ready && oneshot_quiet && oneshot_rearmed &&
             count == sizeof(info) &&
             info.ssi_signo == SIGUSR1 && info.ssi_code == SI_TKILL &&
             info.ssi_pid == (uint32_t)getpid() && info.ssi_uid == (uint32_t)getuid();
    if (!ok) fprintf(stderr, "worker poll=%d/%ld select=%d/%ld epoll=%d/%ld read=%zd signo=%u\n",
                     poll_ready, (after_poll.tv_sec-before.tv_sec)*1000+(after_poll.tv_nsec-before.tv_nsec)/1000000,
                     select_ready, (after_select.tv_sec-after_poll.tv_sec)*1000+(after_select.tv_nsec-after_poll.tv_nsec)/1000000,
                     epoll_ready, (after.tv_sec-after_select.tv_sec)*1000+(after.tv_nsec-after_select.tv_nsec)/1000000,
                     count, info.ssi_signo);
    if (write(result_pipe[1], &ok, sizeof(ok)) != sizeof(ok)) return NULL;
    return NULL;
}

int main(void) {
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigaddset(&mask, SIGUSR2);
    sigaddset(&mask, SIGRTMIN);
    sigaddset(&mask, SIGRTMAX);
    if (pthread_sigmask(SIG_BLOCK, &mask, NULL) != 0) return 1;
    main_tid = (pid_t)syscall(SYS_gettid);
    if (syscall(SYS_tgkill, getpid(), main_tid, SIGUSR2)) return 2;
    signal_fd = signalfd(-1, &mask, SFD_NONBLOCK | SFD_CLOEXEC);
    epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    if (signal_fd < 0 || epoll_fd < 0 || pipe(command_pipe) || pipe(result_pipe) || pipe(ready_pipe)) return 2;
    if (write(ready_pipe[1], "r", 1) != 1) return 2;
    struct epoll_event registration = {.events = EPOLLIN | EPOLLONESHOT, .data.u64 = 0x51};
    if (epoll_ctl(epoll_fd, EPOLL_CTL_ADD, signal_fd, &registration)) return 3;
    struct signalfd_siginfo existing = {0};
    int preexisting = read(signal_fd, &existing, sizeof(existing)) == sizeof(existing) &&
                      existing.ssi_signo == SIGUSR2 && existing.ssi_code == SI_TKILL &&
                      existing.ssi_pid == (uint32_t)getpid() && existing.ssi_uid == (uint32_t)getuid();
    pthread_t thread;
    if (pthread_create(&thread, NULL, worker, NULL)) return 4;
    while (!worker_tid)
        sched_yield();
    if (syscall(SYS_tgkill, getpid(), worker_tid, SIGUSR1)) return 5;
    struct signalfd_siginfo wrong;
    errno = 0;
    int wrong_read = read(signal_fd, &wrong, sizeof(wrong)) == -1 && errno == EAGAIN;
    struct epoll_event wrong_event;
    int wrong_epoll = epoll_wait(epoll_fd, &wrong_event, 1, 0) == 0;
    struct pollfd wrong_poll_event = {.fd = signal_fd, .events = POLLIN};
    int wrong_poll = poll(&wrong_poll_event, 1, 0) == 0;
    fd_set wrong_read_set;
    FD_ZERO(&wrong_read_set);
    FD_SET(signal_fd, &wrong_read_set);
    struct timeval no_wait = {0};
    int wrong_select = select(signal_fd + 1, &wrong_read_set, NULL, NULL, &no_wait) == 0;
    if (write(command_pipe[1], "x", 1) != 1) return 6;
    int worker_ok = 0;
    if (read(result_pipe[0], &worker_ok, sizeof(worker_ok)) != sizeof(worker_ok)) return 6;
    pthread_join(thread, NULL);
    blocking_fd = signalfd(-1, &mask, 0);
    if (blocking_fd < 0 || pthread_create(&thread, NULL, blocking_worker, NULL)) return 7;
    while (!blocking_tid) sched_yield();
    usleep(20000);
    if (syscall(SYS_tgkill, getpid(), blocking_tid, SIGUSR2)) return 7;
    void *blocking_result = (void *)1;
    pthread_join(thread, &blocking_result);
    int blocking = blocking_result == NULL;
    struct sender_args realtime = {.signal = SIGRTMIN, .count = 32};
    if (pthread_create(&thread, NULL, sender, &realtime)) return 7;
    int concurrent = 0;
    while (concurrent < realtime.count) {
        struct signalfd_siginfo queued;
        ssize_t amount = read(signal_fd, &queued, sizeof(queued));
        if (amount == sizeof(queued) && queued.ssi_signo == (uint32_t)realtime.signal)
            ++concurrent;
        else if (amount < 0 && errno == EAGAIN)
            sched_yield();
        else
            return 8;
    }
    void *sender_result = NULL;
    pthread_join(thread, &sender_result);
    int concurrent_ok = sender_result == NULL;
    enum { QUEUE_CAPACITY = 128 };
    int highest_sent = 0;
    while (highest_sent < QUEUE_CAPACITY &&
           syscall(SYS_tgkill, getpid(), main_tid, SIGRTMAX) == 0)
        ++highest_sent;
    errno = 0;
    int highest_overflow = syscall(SYS_tgkill, getpid(), main_tid, SIGRTMAX) == -1 && errno == EAGAIN;
    sigset_t pending;
    int rtmax_pending = sigpending(&pending) == 0 && sigismember(&pending, SIGRTMAX) == 1;
    int max_count = 0;
    int max_spins = 0;
    while (max_count < QUEUE_CAPACITY && max_spins < 1000000) {
        struct signalfd_siginfo max_info = {0};
        ssize_t max_amount = read(signal_fd, &max_info, sizeof(max_info));
        if (max_amount == sizeof(max_info) && max_info.ssi_signo == (uint32_t)SIGRTMAX)
            ++max_count;
        else if (max_amount < 0 && errno == EAGAIN) {
            ++max_spins;
            sched_yield();
        } else
            break;
    }
    struct signalfd_siginfo overflow_info;
    errno = 0;
    int overflow_empty = read(signal_fd, &overflow_info, sizeof(overflow_info)) == -1 && errno == EAGAIN;
    int rtmax = rtmax_pending && highest_sent == QUEUE_CAPACITY && highest_overflow &&
                max_count == QUEUE_CAPACITY && overflow_empty;
    if (pipe(cleanup_command) || pipe(cleanup_result)) return 7;
    cleanup_tid = 0;
    if (pthread_create(&thread, NULL, cleanup_worker, NULL)) return 7;
    while (!cleanup_tid) sched_yield();
    int dead_target_sent = 0;
    while (dead_target_sent < QUEUE_CAPACITY &&
           syscall(SYS_tgkill, getpid(), cleanup_tid, SIGRTMIN) == 0)
        ++dead_target_sent;
    errno = 0;
    int dead_target_full = syscall(SYS_tgkill, getpid(), cleanup_tid, SIGRTMIN) == -1 && errno == EAGAIN;
    struct signalfd_siginfo dead_wrong;
    errno = 0;
    int dead_wrong_empty = read(signal_fd, &dead_wrong, sizeof(dead_wrong)) == -1 && errno == EAGAIN;
    if (kill(getpid(), SIGUSR1) != 0 || write(cleanup_command[1], "x", 1) != 1) return 7;
    pthread_join(thread, NULL);
    struct signalfd_siginfo process_info = {0};
    int process_retained = read(signal_fd, &process_info, sizeof(process_info)) == sizeof(process_info) &&
                           process_info.ssi_signo == SIGUSR1;
    cleanup_tid = 0;
    if (pthread_create(&thread, NULL, cleanup_worker, NULL)) return 7;
    while (!cleanup_tid) sched_yield();
    int replacement_sent = 0;
    while (replacement_sent < QUEUE_CAPACITY &&
           syscall(SYS_tgkill, getpid(), cleanup_tid, SIGRTMIN) == 0)
        ++replacement_sent;
    errno = 0;
    int replacement_full = syscall(SYS_tgkill, getpid(), cleanup_tid, SIGRTMIN) == -1 && errno == EAGAIN;
    if (write(cleanup_command[1], "r", 1) != 1) return 7;
    int replacement_read = 0;
    if (read(cleanup_result[0], &replacement_read, sizeof(replacement_read)) != sizeof(replacement_read)) return 7;
    pthread_join(thread, NULL);
    int cleanup = dead_target_sent == QUEUE_CAPACITY && dead_target_full && dead_wrong_empty && process_retained &&
                  replacement_sent == QUEUE_CAPACITY && replacement_full && replacement_read;
    struct sigaction action = {0};
    action.sa_sigaction = info_handler;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGWINCH, &action, NULL) != 0 || pthread_create(&thread, NULL, info_worker, NULL)) return 7;
    while (!info_tid)
        sched_yield();
    if (syscall(SYS_tgkill, getpid(), info_tid, SIGWINCH)) return 7;
    pthread_join(thread, NULL);
    int siginfo = info_done && info_ok;
    if (syscall(SYS_tgkill, getpid(), syscall(SYS_gettid), SIGUSR1)) return 7;
    pid_t child = fork();
    if (child < 0) return 8;
    if (child == 0) {
        struct signalfd_siginfo child_info;
        errno = 0;
        _exit(read(signal_fd, &child_info, sizeof(child_info)) == -1 && errno == EAGAIN ? 0 : 1);
    }
    int child_status = 0;
    waitpid(child, &child_status, 0);
    struct signalfd_siginfo parent_info;
    int parent_retained =
        read(signal_fd, &parent_info, sizeof(parent_info)) == sizeof(parent_info) && parent_info.ssi_signo == SIGUSR1;
    int child_empty = WIFEXITED(child_status) && WEXITSTATUS(child_status) == 0;
    printf("signalfd_peer preexisting=%d wrong_read=%d wrong_epoll=%d wrong_poll=%d wrong_select=%d worker=%d blocking=%d concurrent=%d rtmax=%d cleanup=%d siginfo=%d child_empty=%d parent_retained=%d\n",
           preexisting, wrong_read, wrong_epoll, wrong_poll, wrong_select, worker_ok, blocking, concurrent_ok, rtmax, cleanup, siginfo,
           child_empty, parent_retained);
    return !(preexisting && wrong_read && wrong_epoll && wrong_poll && wrong_select && worker_ok && blocking && concurrent_ok && rtmax && cleanup && siginfo &&
             child_empty && parent_retained);
}
