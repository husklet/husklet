#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

struct state {
    _Atomic uint32_t word, ack, child_ready;
};

static void pause_briefly(void) {
    struct timespec t = {0, 1000000};
    nanosleep(&t, NULL);
}

static int wait_file(const char *path) {
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        if (access(path, F_OK) == 0) return 0;
        pause_briefly();
    }
    return -1;
}

static int wait_sleeping(pid_t process) {
    char path[64], line[512];
    snprintf(path, sizeof path, "/proc/%d/stat", (int)process);
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        int descriptor = open(path, O_RDONLY);
        ssize_t count = descriptor < 0 ? -1 : read(descriptor, line, sizeof line - 1);
        if (descriptor >= 0) close(descriptor);
        if (count > 0) {
            line[count] = 0;
            char *end = strrchr(line, ')');
            if (end != NULL && end[1] == ' ' && end[2] == 'S') return 0;
        }
        pause_briefly();
    }
    return -1;
}

static int reap_bounded(pid_t child) {
    int status = 0;
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        if (waitpid(child, &status, WNOHANG) == child) return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
        pause_briefly();
    }
    kill(child, SIGKILL);
    for (unsigned attempt = 0; attempt < 5000; ++attempt) {
        if (waitpid(child, &status, WNOHANG) == child) break;
        pause_briefly();
    }
    return -1;
}

static int fail_child(pid_t child, int result) {
    kill(child, SIGKILL);
    (void)reap_bounded(child);
    return result;
}

static int futex_wait(_Atomic uint32_t *word, uint32_t expected) {
    struct timespec timeout = {30, 0};
    return (int)syscall(SYS_futex, word, FUTEX_WAIT, expected, &timeout, NULL, 0);
}

static int futex_wake(_Atomic uint32_t *word) {
    return (int)syscall(SYS_futex, word, FUTEX_WAKE, 1, NULL, NULL, 0);
}

int main(int argc, char **argv) {
    if (argc != 2) return 10;
    char output[1024], cycle1[1024], cycle2[1024], finish[1024];
    snprintf(output, sizeof output, "%s/output", argv[1]);
    snprintf(cycle1, sizeof cycle1, "%s/cycle1", argv[1]);
    snprintf(cycle2, sizeof cycle2, "%s/cycle2", argv[1]);
    snprintf(finish, sizeof finish, "%s/finish", argv[1]);
    int log = open(output, O_WRONLY | O_CREAT | O_APPEND, 0600);
    int fd = (int)syscall(SYS_memfd_create, "checkpoint-shared-futex", 0u);
    long page = sysconf(_SC_PAGESIZE);
    if (log < 0 || fd < 0 || page <= 0 || ftruncate(fd, 2 * page) != 0) return 11;
    struct state *parent_view = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, page);
    struct state *child_view = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, page);
    if (parent_view == MAP_FAILED || child_view == MAP_FAILED || parent_view == child_view) return 12;
    dprintf(log, "BOOT\n");
    pid_t child = fork();
    if (child < 0) return 13;
    if (child == 0) {
        atomic_store_explicit(&child_view->child_ready, 1, memory_order_release);
        for (uint32_t generation = 1; generation <= 2; ++generation) {
            uint32_t expected = generation - 1;
            while (atomic_load_explicit(&child_view->word, memory_order_acquire) == expected) {
                if (futex_wait(&child_view->word, expected) != 0 && errno != EAGAIN && errno != EINTR) _exit(20);
            }
            if (atomic_load_explicit(&child_view->word, memory_order_acquire) != generation) _exit(21);
            atomic_store_explicit(&child_view->ack, generation, memory_order_release);
        }
        if (wait_file(finish) != 0) _exit(22);
        _exit(0);
    }
    unsigned ready_attempts = 0;
    while (atomic_load_explicit(&parent_view->child_ready, memory_order_acquire) != 1 && ready_attempts++ < 30000)
        pause_briefly();
    if (ready_attempts >= 30000) return fail_child(child, 14);
    if (wait_sleeping(child) != 0) return fail_child(child, 15);
    dprintf(log, "READY\n");
    const char *cycles[2] = {cycle1, cycle2};
    for (uint32_t generation = 1; generation <= 2; ++generation) {
        if (wait_file(cycles[generation - 1]) != 0) return fail_child(child, 15);
        atomic_store_explicit(&parent_view->word, generation, memory_order_release);
        if (futex_wake(&parent_view->word) != 1) return fail_child(child, 16);
        unsigned attempts = 0;
        while (atomic_load_explicit(&parent_view->ack, memory_order_acquire) != generation && attempts++ < 30000)
            pause_briefly();
        if (attempts >= 30000) return fail_child(child, 17);
        if (generation == 1 && wait_sleeping(child) != 0) return fail_child(child, 18);
        dprintf(log, generation == 1 ? "CYCLE 1\n" : "DONE shared-futex-ok\n");
    }
    if (wait_file(finish) != 0) return fail_child(child, 19);
    int status = 0;
    (void)status;
    return reap_bounded(child) == 0 ? 0 : 20;
}
