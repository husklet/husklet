#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static _Atomic int *clear_word;
static int ready_pipe[2];

struct robust_head {
    void *list;
    long futex_offset;
    void *list_op_pending;
};

struct robust_node {
    void *next;
    _Atomic int futex;
};

static struct robust_head readonly_head;
static struct robust_node *readonly_node;
static _Atomic int readonly_tid;

static int publish_ready(void) {
    return write(ready_pipe[1], "x", 1) == 1;
}

static void *clear_shared(void *unused) {
    (void)unused;
    long tid = syscall(SYS_gettid);
    atomic_store(clear_word, (int)tid);
    (void)syscall(SYS_set_tid_address, clear_word);
    (void)publish_ready();
    return NULL;
}

static void *clear_malformed(void *unused) {
    (void)unused;
    (void)syscall(SYS_set_tid_address, (void *)(uintptr_t)1);
    (void)publish_ready();
    return NULL;
}

static void *robust_readonly(void *unused) {
    (void)unused;
    readonly_node->next = &readonly_head;
    int tid = (int)syscall(SYS_gettid);
    atomic_store(&readonly_tid, tid);
    atomic_store(&readonly_node->futex, tid);
    readonly_head.list = readonly_node;
    readonly_head.futex_offset = (char *)&readonly_node->futex - (char *)readonly_node;
    readonly_head.list_op_pending = NULL;
    (void)syscall(SYS_set_robust_list, &readonly_head, sizeof readonly_head);
    (void)mprotect(readonly_node, 4096, PROT_READ);
    (void)publish_ready();
    return NULL;
}

static int start_detached(void *(*routine)(void *)) {
    pthread_t thread;
    if (pthread_create(&thread, NULL, routine, NULL) != 0) return 0;
    return pthread_detach(thread) == 0;
}

static int wait_ready(void) {
    char byte;
    return read(ready_pipe[0], &byte, 1) == 1;
}

int main(void) {
    char path[] = "/tmp/hl-clear-tid-XXXXXX";
    int fd = mkstemp(path);
    unlink(path);
    if (fd < 0 || ftruncate(fd, 4096) != 0 || pipe(ready_pipe) != 0) return 2;
    clear_word = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (clear_word == MAP_FAILED) return 3;
    readonly_node = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (readonly_node == MAP_FAILED) return 4;

    int shared_started = start_detached(clear_shared);
    int shared_ready = shared_started && wait_ready();
    struct timespec pause = {.tv_sec = 0, .tv_nsec = 1000000};
    int shared_cleared = 0;
    for (int attempt = 0; attempt < 2000; ++attempt) {
        if (atomic_load(clear_word) == 0) {
            shared_cleared = 1;
            break;
        }
        nanosleep(&pause, NULL);
    }

    int malformed_started = start_detached(clear_malformed);
    int malformed_ready = malformed_started && wait_ready();
    int readonly_started = start_detached(robust_readonly);
    int readonly_ready = readonly_started && wait_ready();
    struct timespec settle = {.tv_sec = 0, .tv_nsec = 50000000};
    nanosleep(&settle, NULL);
    int readonly_unchanged = atomic_load(&readonly_node->futex) == atomic_load(&readonly_tid);
    printf("shared-clear=%s malformed-survives=%s readonly-robust-survives=%s readonly-robust-unchanged=%s\n",
           shared_ready && shared_cleared ? "ok" : "BAD", malformed_ready ? "ok" : "BAD", readonly_ready ? "ok" : "BAD",
           readonly_unchanged ? "ok" : "BAD");
    return shared_ready && shared_cleared && malformed_ready && readonly_ready && readonly_unchanged ? 0 : 1;
}
