#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <linux/limits.h>
#include <pthread.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

struct robust_node {
    struct robust_node *next;
    uint32_t futex;
};

struct robust_head {
    struct robust_node *next;
    long offset;
    struct robust_node *pending;
};

struct shared_state {
    struct robust_head head;
    struct robust_node node;
    uint32_t clear_tid;
    uint32_t ready;
    uint32_t gate;
};

static char self[PATH_MAX];

static int futex_wait(uint32_t *address, uint32_t value) {
    return (int)syscall(SYS_futex, address, FUTEX_WAIT, value, NULL, NULL, 0);
}

static int futex_wait_timed(uint32_t *address, uint32_t value) {
    struct timespec timeout = {.tv_sec = 2, .tv_nsec = 0};
    return (int)syscall(SYS_futex, address, FUTEX_WAIT, value, &timeout, NULL, 0);
}

static void futex_wake(uint32_t *address) {
    syscall(SYS_futex, address, FUTEX_WAKE, INT32_MAX, NULL, NULL, 0);
}

static void caught(int signal) { (void)signal; }

static int copy_self(const char *path) {
    char bytes[65536];
    int input = open(self, O_RDONLY);
    int output = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    ssize_t count;
    int ok = input >= 0 && output >= 0;
    while (ok && (count = read(input, bytes, sizeof bytes)) > 0)
        ok = write(output, bytes, (size_t)count) == count;
    if (input >= 0) close(input);
    if (output >= 0) close(output);
    return ok && chmod(path, 0755) == 0;
}

static int write_file(const char *path, const char *bytes, size_t length) {
    int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    int ok = fd >= 0 && write(fd, bytes, length) == (ssize_t)length;
    if (fd >= 0) close(fd);
    return ok && chmod(path, 0755) == 0;
}

static int failed_exec(const char *path, char *const arguments[]) {
    char *environment[] = {NULL};
    execve(path, arguments, environment);
    return errno;
}

static int failed_raw(const char *path, uintptr_t arguments) {
    char *environment[] = {NULL};
    syscall(SYS_execve, path, arguments, environment);
    return errno;
}

static void *owner_thread(void *opaque) {
    struct shared_state *shared = opaque;
    pid_t tid = (pid_t)syscall(SYS_gettid);
    shared->head.next = &shared->node;
    shared->head.offset = (char *)&shared->node.futex - (char *)&shared->node;
    shared->head.pending = NULL;
    shared->node.next = (struct robust_node *)&shared->head;
    shared->node.futex = (uint32_t)tid | FUTEX_WAITERS;
    shared->clear_tid = (uint32_t)tid;
    syscall(SYS_set_robust_list, &shared->head, sizeof shared->head);
    syscall(SYS_set_tid_address, &shared->clear_tid);
    __atomic_store_n(&shared->ready, 1, __ATOMIC_RELEASE);
    futex_wake(&shared->ready);
    while (__atomic_load_n(&shared->gate, __ATOMIC_ACQUIRE) == 0)
        futex_wait(&shared->gate, 0);
    return NULL;
}

static int probe_state(void) {
    sigset_t pending;
    struct sigaction caught_action, ignored_action;
    stack_t stack;
    sigpending(&pending);
    sigaction(SIGTERM, NULL, &caught_action);
    sigaction(SIGUSR1, NULL, &ignored_action);
    sigaltstack(NULL, &stack);
    int value = 0;
    if (sigismember(&pending, SIGUSR2) == 1) value |= 1;
    if (caught_action.sa_handler == SIG_DFL) value |= 2;
    if (ignored_action.sa_handler == SIG_IGN) value |= 4;
    if ((stack.ss_flags & SS_DISABLE) != 0) value |= 8;
    return 64 | value;
}

static int image_state(struct shared_state *shared) {
    pid_t child = fork();
    if (child == 0) {
        pthread_t owner;
        if (pthread_create(&owner, NULL, owner_thread, shared) != 0) _exit(40);
        while (__atomic_load_n(&shared->ready, __ATOMIC_ACQUIRE) == 0)
            futex_wait(&shared->ready, 0);
        struct sigaction action = {.sa_handler = caught};
        sigemptyset(&action.sa_mask);
        sigaction(SIGTERM, &action, NULL);
        signal(SIGUSR1, SIG_IGN);
        sigset_t mask;
        sigemptyset(&mask);
        sigaddset(&mask, SIGUSR2);
        sigprocmask(SIG_BLOCK, &mask, NULL);
        raise(SIGUSR2);
        char memory[SIGSTKSZ];
        stack_t stack = {.ss_sp = memory, .ss_size = sizeof memory, .ss_flags = 0};
        sigaltstack(&stack, NULL);
        char *arguments[] = {self, (char *)"probe", NULL};
        char *environment[] = {NULL};
        execve(self, arguments, environment);
        _exit(41);
    }
    if (child < 0) {
        printf("robust=0 clear=0 wake=0 pending=0 caught=0 ignored=0 altstack=0\n");
        return 0;
    }
    while (__atomic_load_n(&shared->ready, __ATOMIC_ACQUIRE) == 0)
        futex_wait(&shared->ready, 0);
    int woke = 0;
    while (__atomic_load_n(&shared->clear_tid, __ATOMIC_ACQUIRE) != 0) {
        uint32_t value = shared->clear_tid;
        if (futex_wait_timed(&shared->clear_tid, value) == 0) woke = 1;
        else if (errno != EAGAIN && errno != EINTR) break;
    }
    int status = 0;
    waitpid(child, &status, 0);
    int probe = WIFEXITED(status) ? WEXITSTATUS(status) : 0;
    int robust = (shared->node.futex & (FUTEX_OWNER_DIED | FUTEX_WAITERS))
        == (FUTEX_OWNER_DIED | FUTEX_WAITERS);
    int clear = shared->clear_tid == 0;
    printf("robust=%d clear=%d wake=%d pending=%d caught=%d ignored=%d altstack=%d\n",
           robust, clear, woke, !!(probe & 1), !!(probe & 2),
           !!(probe & 4), !!(probe & 8));
    return probe == 79 && robust && clear && woke;
}

static struct shared_state *shared_mapping(void) {
    int fd = (int)syscall(SYS_memfd_create, "exec-edges", 0);
    if (fd < 0 || ftruncate(fd, sizeof(struct shared_state)) != 0) {
        if (fd >= 0) close(fd);
        return MAP_FAILED;
    }
    struct shared_state *shared = mmap(NULL, sizeof *shared,
        PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    return shared;
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "probe") == 0) return probe_state();
    ssize_t length = readlink("/proc/self/exe", self, sizeof self - 1);
    if (length <= 0) snprintf(self, sizeof self, "%s", argv[0]);
    else self[length] = 0;
    char root[128], writable[160], malformed[160], scripts[6][160];
    snprintf(root, sizeof root, "/tmp/hl_exec_edges_%d", (int)getpid());
    mkdir(root, 0755);
    snprintf(writable, sizeof writable, "%s/writable", root);
    snprintf(malformed, sizeof malformed, "%s/malformed", root);
    int copied = copy_self(writable);
    int lease = open(writable, O_WRONLY);
    char *plain[] = {writable, NULL};
    int etxtbsy = copied && lease >= 0 && failed_exec(writable, plain) == ETXTBSY;
    close(lease);
    int malformed_ok = write_file(malformed, "#!\n", 3)
        && failed_exec(malformed, plain) == ENOEXEC;
    for (int index = 0; index < 6; ++index)
        snprintf(scripts[index], sizeof scripts[index], "%s/s%d", root, index);
    int chain = 1;
    for (int index = 0; index < 6; ++index) {
        char line[256];
        int size = snprintf(line, sizeof line, "#!%s\n", scripts[(index + 1) % 6]);
        chain &= write_file(scripts[index], line, (size_t)size);
    }
    int recursion = chain && failed_exec(scripts[0], plain) == ELOOP;
    int efault = failed_raw(self, 1) == EFAULT;
    int precedence = failed_raw("/no/such/exec-edge", 1) == ENOENT;
    char *huge = malloc(140000);
    memset(huge, 'x', 139999); huge[139999] = 0;
    char *large_arguments[] = {self, huge, NULL};
    int e2big = failed_exec(self, large_arguments) == E2BIG;
    free(huge);
    printf("etxtbsy=%d malformed=%d recursion=%d efault=%d precedence=%d e2big=%d\n",
           etxtbsy, malformed_ok, recursion, efault, precedence, e2big);
    fflush(stdout);
    struct shared_state *shared = shared_mapping();
    int state = shared != MAP_FAILED && image_state(shared);
    if (shared == MAP_FAILED)
        printf("robust=0 clear=0 wake=0 pending=0 caught=0 ignored=0 altstack=0\n");
    if (shared != MAP_FAILED) munmap(shared, sizeof *shared);
    for (int index = 0; index < 6; ++index) unlink(scripts[index]);
    unlink(malformed); unlink(writable); rmdir(root);
    return !(etxtbsy && malformed_ok && recursion && efault && precedence && e2big && state);
}
