/* End-to-end fixture for hl-engine's checkpoint composition API. */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile unsigned long state;
static volatile sig_atomic_t delivered;
static volatile sig_atomic_t helper_delivered;
static volatile sig_atomic_t helper_on_altstack;
static pthread_mutex_t held_mutex;
static unsigned char helper_altstack[65536];

struct helper_context {
    const char *release;
    const char *final_release;
    _Atomic int locked;
    _Atomic int first_restored;
    _Atomic int error;
    _Atomic unsigned long turns;
};

static void *helper_fail(struct helper_context *context, long code) {
    atomic_store_explicit(&context->error, (int)code, memory_order_release);
    return (void *)code;
}

static void notice(int signal) {
    if (signal == SIGUSR2) {
        unsigned char marker;
        helper_delivered++;
        helper_on_altstack = &marker >= helper_altstack && &marker < helper_altstack + sizeof helper_altstack;
    } else {
        delivered++;
    }
}

static void *helper(void *opaque) {
    struct helper_context *context = opaque;
    struct sigaction action = {0};
    stack_t alternate = {.ss_sp = helper_altstack, .ss_size = sizeof helper_altstack};
    sigset_t blocked, pending, observed;
    action.sa_handler = notice;
    action.sa_flags = SA_ONSTACK;
    sigemptyset(&action.sa_mask);
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR2);
    if (sigaction(SIGUSR2, &action, NULL) != 0 || sigaltstack(&alternate, NULL) != 0 ||
        pthread_sigmask(SIG_BLOCK, &blocked, NULL) != 0 || pthread_kill(pthread_self(), SIGUSR2) != 0)
        return helper_fail(context, 4);
    if (pthread_mutex_lock(&held_mutex) != 0) return helper_fail(context, 1);
    atomic_store_explicit(&context->locked, 1, memory_order_release);
    while (access(context->release, F_OK) != 0) {
        if (errno != ENOENT) return helper_fail(context, 2);
        atomic_fetch_add_explicit(&context->turns, 1, memory_order_relaxed);
    }
    stack_t restored;
    int mask_result = pthread_sigmask(SIG_SETMASK, NULL, &observed);
    int pending_result = sigpending(&pending);
    int stack_result = sigaltstack(NULL, &restored);
    if (mask_result != 0 || !sigismember(&observed, SIGUSR2) || pending_result != 0 ||
        !sigismember(&pending, SIGUSR2) || stack_result != 0 || restored.ss_sp != alternate.ss_sp ||
        restored.ss_size != alternate.ss_size || restored.ss_flags != 0)
        return helper_fail(context, 5);
    if (pthread_sigmask(SIG_UNBLOCK, &blocked, NULL) != 0) return helper_fail(context, 6);
    for (int attempt = 0; attempt < 1000 && helper_delivered != 1; ++attempt)
        usleep(1000);
    if (helper_delivered != 1 || !helper_on_altstack) return helper_fail(context, 7);
    if (pthread_sigmask(SIG_BLOCK, &blocked, NULL) != 0 || pthread_kill(pthread_self(), SIGUSR2) != 0)
        return helper_fail(context, 8);
    atomic_store_explicit(&context->first_restored, 1, memory_order_release);
    while (access(context->final_release, F_OK) != 0) {
        if (errno != ENOENT) return helper_fail(context, 9);
        atomic_fetch_add_explicit(&context->turns, 1, memory_order_relaxed);
    }
    if (pthread_sigmask(SIG_SETMASK, NULL, &observed) != 0 || !sigismember(&observed, SIGUSR2) ||
        sigpending(&pending) != 0 || !sigismember(&pending, SIGUSR2) || sigaltstack(NULL, &restored) != 0 ||
        restored.ss_sp != alternate.ss_sp || restored.ss_size != alternate.ss_size || restored.ss_flags != 0)
        return helper_fail(context, 10);
    if (pthread_sigmask(SIG_UNBLOCK, &blocked, NULL) != 0) return helper_fail(context, 11);
    for (int attempt = 0; attempt < 1000 && helper_delivered != 2; ++attempt)
        usleep(1000);
    if (helper_delivered != 2 || !helper_on_altstack) return helper_fail(context, 12);
    if (pthread_mutex_unlock(&held_mutex) != 0) return helper_fail(context, 3);
    return NULL;
}

static int worker(const char *release, const char *final_release, int role) {
    pid_t original_pid = getpid();
    pid_t original_ppid = getppid();
    char file_path[1024];
    char deleted_path[1024];
    char expected[32];
    char observed[32] = {0};
    int descriptors[2];
    int sockets[2];
    int file;
    int independent_file;
    int duplicate_file;
    int deleted_write_file;
    struct sigaction action = {0};
    sigset_t blocked, previous;
    ssize_t expected_size;
    pthread_mutexattr_t mutex_attributes;
    pthread_t helper_thread;
    struct helper_context helper_state = {.release = release, .final_release = final_release};
    char directory[256], original_cwd[256], restored_cwd[256];

    if (snprintf(directory, sizeof directory, "/tmp/husklet-checkpoint-cwd-%ld-%d", (long)original_pid, role) >=
        (int)sizeof directory)
        return 24 + role;
    if (mkdir(directory, 0700) < 0 && errno != EEXIST) return 27 + role;
    if (chdir(directory) < 0 || getcwd(original_cwd, sizeof original_cwd) == NULL) return 30 + role;

    if (snprintf(file_path, sizeof file_path, "%s.file.%d", release, role) >= (int)sizeof file_path) return 10 + role;
    if (snprintf(deleted_path, sizeof deleted_path, "%s.deleted.%d", release, role) >= (int)sizeof deleted_path)
        return 10 + role;
    file = open(file_path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (file < 0 || write(file, "offset", 6) != 6 || lseek(file, role, SEEK_SET) != role) return 10 + role;
    independent_file = open(file_path, O_RDONLY);
    duplicate_file = dup(file);
    if (independent_file < 0 || duplicate_file < 0 || lseek(independent_file, 0, SEEK_SET) != 0 ||
        fcntl(duplicate_file, F_SETFD, FD_CLOEXEC) != 0 ||
        fcntl(duplicate_file, F_SETFL, fcntl(duplicate_file, F_GETFL) | O_APPEND | O_NONBLOCK) != 0)
        return 10 + role;
    deleted_write_file = open(deleted_path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    if (deleted_write_file < 0 || write(deleted_write_file, "secret", 6) != 6 ||
        lseek(deleted_write_file, 1, SEEK_SET) != 1 || unlink(deleted_path) != 0)
        return 10 + role;
    if (pipe(descriptors) != 0) return 10 + role;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) != 0) return 10 + role;
    expected_size = snprintf(expected, sizeof expected, "pipe-%d", role);
    if (expected_size <= 0 || write(descriptors[1], expected, (size_t)expected_size) != expected_size) return 10 + role;
    if (write(sockets[0], expected, (size_t)expected_size) != expected_size) return 10 + role;
    close(descriptors[1]);
    action.sa_handler = notice;
    sigemptyset(&action.sa_mask);
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR1);
    if (sigaction(SIGUSR1, &action, NULL) != 0 || sigprocmask(SIG_BLOCK, &blocked, &previous) != 0 ||
        kill(original_pid, SIGUSR1) != 0)
        return 10 + role;
    if (pthread_mutexattr_init(&mutex_attributes) != 0 ||
        pthread_mutexattr_setrobust(&mutex_attributes, PTHREAD_MUTEX_ROBUST) != 0 ||
        pthread_mutex_init(&held_mutex, &mutex_attributes) != 0 || pthread_mutexattr_destroy(&mutex_attributes) != 0 ||
        pthread_create(&helper_thread, NULL, helper, &helper_state) != 0)
        return 10 + role;
    while (!atomic_load_explicit(&helper_state.locked, memory_order_acquire))
        usleep(1000);
    state = 1000003ul * (unsigned long)role;
    dprintf(STDOUT_FILENO, "READY %d %ld %ld\n", role, (long)original_pid, (long)original_ppid);
    for (;;) {
        state += (unsigned long)(role * 2 + 1);
        if (access(release, F_OK) == 0) break;
        if (errno != ENOENT) return 30 + role;
    }
    if (getpid() != original_pid || getppid() != original_ppid) return 40 + role;
    if (getcwd(restored_cwd, sizeof restored_cwd) == NULL || strcmp(restored_cwd, original_cwd) != 0) return 43 + role;
    if (state <= 1000003ul * (unsigned long)role) return 50 + role;
    if ((fcntl(descriptors[0], F_GETFL) & O_NONBLOCK) != 0) return 56 + role;
    if (atomic_load_explicit(&helper_state.turns, memory_order_relaxed) == 0) return 55 + role;
    if (read(file, observed, 1) != 1 || observed[0] != "offset"[role]) return 60 + role;
    if (read(duplicate_file, observed, 1) != 1 || observed[0] != "offset"[role + 1]) return 63 + role;
    if (read(independent_file, observed, 1) != 1 || observed[0] != 'o') return 66 + role;
    if ((fcntl(file, F_GETFL) & (O_APPEND | O_NONBLOCK)) != (O_APPEND | O_NONBLOCK) ||
        (fcntl(duplicate_file, F_GETFL) & (O_APPEND | O_NONBLOCK)) != (O_APPEND | O_NONBLOCK) ||
        (fcntl(independent_file, F_GETFL) & (O_APPEND | O_NONBLOCK)) != 0 || (fcntl(file, F_GETFD) & FD_CLOEXEC) != 0 ||
        (fcntl(duplicate_file, F_GETFD) & FD_CLOEXEC) == 0 || (fcntl(independent_file, F_GETFD) & FD_CLOEXEC) != 0)
        return 69 + role;
    struct stat deleted_status;
    char byte;
    errno = 0;
    if ((fcntl(deleted_write_file, F_GETFL) & O_ACCMODE) != O_WRONLY ||
        (fcntl(deleted_write_file, F_GETFD) & FD_CLOEXEC) == 0 || pread(deleted_write_file, &byte, 1, 0) != -1 ||
        errno != EBADF || lseek(deleted_write_file, 0, SEEK_CUR) != 1 || write(deleted_write_file, "X", 1) != 1 ||
        fstat(deleted_write_file, &deleted_status) != 0 || deleted_status.st_nlink != 0 || deleted_status.st_size != 6)
        return 68 + role;
    memset(observed, 0, sizeof observed);
    if (read(descriptors[0], observed, sizeof observed) != expected_size ||
        memcmp(observed, expected, (size_t)expected_size))
        return 70 + role;
    memset(observed, 0, sizeof observed);
    if (read(sockets[1], observed, sizeof observed) != expected_size ||
        memcmp(observed, expected, (size_t)expected_size))
        return 75 + role;
    if (sigprocmask(SIG_SETMASK, &previous, NULL) != 0) return 80 + role;
    for (int attempt = 0; attempt < 1000 && delivered != 1; ++attempt)
        usleep(1000);
    if (delivered != 1) {
        dprintf(STDOUT_FILENO, "SIGNAL-ERROR %d main=%d helper=%d helper_alt=%d\n", role, delivered, helper_delivered,
                helper_on_altstack);
        return 90 + role;
    }
    if (sigprocmask(SIG_BLOCK, &blocked, &previous) != 0 || kill(original_pid, SIGUSR1) != 0) return 100 + role;
    while (!atomic_load_explicit(&helper_state.first_restored, memory_order_acquire) &&
           !atomic_load_explicit(&helper_state.error, memory_order_acquire))
        usleep(1000);
    if (atomic_load_explicit(&helper_state.error, memory_order_acquire)) {
        int error = atomic_load_explicit(&helper_state.error, memory_order_relaxed);
        dprintf(STDOUT_FILENO, "HELPER-ERROR %d %d\n", role, error);
        return 160 + error;
    }
    dprintf(STDOUT_FILENO, "CYCLE-READY %d\n", role);
    while (access(final_release, F_OK) != 0) {
        if (errno != ENOENT) return 110 + role;
        state += (unsigned long)(role * 2 + 1);
    }
    if (read(file, observed, 1) != 1 || observed[0] != "offset"[role + 2]) return 120 + role;
    if (read(independent_file, observed, 1) != 1 || observed[0] != 'f') return 123 + role;
    if (lseek(deleted_write_file, 0, SEEK_CUR) != 2 || write(deleted_write_file, "Y", 1) != 1 ||
        fstat(deleted_write_file, &deleted_status) != 0 || deleted_status.st_nlink != 0 || deleted_status.st_size != 6)
        return 126 + role;
    if (read(descriptors[0], observed, sizeof observed) != 0) return 130 + role;
    if (sigprocmask(SIG_SETMASK, &previous, NULL) != 0) return 140 + role;
    for (int attempt = 0; attempt < 1000 && delivered != 2; ++attempt)
        usleep(1000);
    if (delivered != 2) return 150 + role;
    void *helper_result = NULL;
    if (pthread_join(helper_thread, &helper_result) != 0 || helper_result != NULL ||
        pthread_mutex_lock(&held_mutex) != 0 || pthread_mutex_unlock(&held_mutex) != 0)
        return 155 + role;
    close(file);
    close(independent_file);
    close(duplicate_file);
    close(deleted_write_file);
    close(descriptors[0]);
    close(sockets[0]);
    close(sockets[1]);
    if (getcwd(restored_cwd, sizeof restored_cwd) == NULL || strcmp(restored_cwd, original_cwd) != 0) return 153 + role;
    if (chdir("/") < 0 || rmdir(directory) < 0) return 46 + role;
    dprintf(STDOUT_FILENO, "RESTORED %d %ld %ld %lu\n", role, (long)getpid(), (long)getppid(), state);
    return 20 + role;
}

int main(int argc, char **argv) {
    pid_t first, second;
    int first_status, second_status;
    char output[1024];
    int fd;
    if (argc != 3) return 2;
    if (snprintf(output, sizeof output, "%s.output", argv[1]) >= (int)sizeof output) return 2;
    fd = open(output, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (fd < 0 || dup2(fd, STDOUT_FILENO) < 0 || dup2(fd, STDERR_FILENO) < 0) return 2;
    if (fd > STDERR_FILENO) close(fd);
    fd = open("/dev/null", O_RDONLY);
    if (fd < 0 || dup2(fd, STDIN_FILENO) < 0) return 2;
    if (fd > STDERR_FILENO) close(fd);
    first = fork();
    if (first < 0) return 3;
    if (first == 0) return worker(argv[1], argv[2], 1);
    second = fork();
    if (second < 0) return 4;
    if (second == 0) return worker(argv[1], argv[2], 2);
    {
        int result = worker(argv[1], argv[2], 3);
        if (result != 23) return result;
    }
    if (waitpid(first, &first_status, 0) != first || waitpid(second, &second_status, 0) != second) return 60;
    if (!WIFEXITED(first_status) || WEXITSTATUS(first_status) != 21 || !WIFEXITED(second_status) ||
        WEXITSTATUS(second_status) != 22)
        return 61;
    dprintf(STDOUT_FILENO, "TREE-RESTORED %ld %ld\n", (long)first, (long)second);
    return 0;
}
