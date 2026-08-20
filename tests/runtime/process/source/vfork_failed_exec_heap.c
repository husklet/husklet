// A failed exec in a vfork child must not roll a multithreaded parent's heap back to the
// child process's fork-time snapshot. Keep another thread allocating while the child is
// alive, then prove both the allocations and a later healthy child remain usable.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

enum { ALLOCATIONS = 512, ALLOCATION_SIZE = 4096 };

static int child_ready[2];
static int allocations_ready[2];
static unsigned char *allocations[ALLOCATIONS];
static unsigned char *concurrent;

static void *allocate_while_child_lives(void *unused) {
    (void)unused;
    unsigned char byte;
    if (read(child_ready[0], &byte, 1) != 1) return (void *)(uintptr_t)1;
    memset(concurrent, 0xa5, ALLOCATION_SIZE);
    for (size_t index = 0; index < ALLOCATIONS; index++) {
        allocations[index] = malloc(ALLOCATION_SIZE);
        if (allocations[index] == NULL) return (void *)(uintptr_t)2;
        memset(allocations[index], (int)(index & 0xff), ALLOCATION_SIZE);
    }
    byte = 1;
    return write(allocations_ready[1], &byte, 1) == 1 ? NULL : (void *)(uintptr_t)3;
}

static int failed_child(void) {
    pid_t child = vfork();
    if (child == 0) {
        unsigned char byte = 1;
        if (write(child_ready[1], &byte, 1) != 1 || read(allocations_ready[0], &byte, 1) != 1) _exit(125);
        char *arguments[] = {(char *)"missing", NULL};
        execve("/no/such/husklet-child", arguments, environ);
        _exit(errno == ENOENT ? 42 : 124);
    }
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 42;
}

static int healthy_child(void) {
    pid_t child = fork();
    if (child == 0) _exit(17);
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 17;
}

int main(void) {
    if (pipe(child_ready) != 0 || pipe(allocations_ready) != 0) return 2;
    concurrent = malloc(ALLOCATION_SIZE);
    if (concurrent == NULL) return 3;
    memset(concurrent, 0, ALLOCATION_SIZE);
    pthread_t worker;
    if (pthread_create(&worker, NULL, allocate_while_child_lives, NULL) != 0) return 4;
    int failed_isolated = failed_child();
    void *worker_result = NULL;
    int joined = pthread_join(worker, &worker_result) == 0 && worker_result == NULL;
    int heap_ok = joined;
    for (size_t offset = 0; offset < ALLOCATION_SIZE; offset += 257)
        heap_ok &= concurrent[offset] == 0xa5;
    for (size_t index = 0; index < ALLOCATIONS && heap_ok; index++) {
        for (size_t offset = 0; offset < ALLOCATION_SIZE; offset += 257)
            heap_ok &= allocations[index][offset] == (unsigned char)(index & 0xff);
    }
    for (size_t index = 0; index < ALLOCATIONS; index++)
        free(allocations[index]);
    free(concurrent);
    void *probe = malloc(2 * ALLOCATION_SIZE);
    heap_ok &= probe != NULL;
    free(probe);
    int retry_ok = healthy_child();
    printf("vfork_failed_exec_heap failed_isolated=%d heap_ok=%d retry_ok=%d\n", failed_isolated, heap_ok, retry_ok);
    return failed_isolated && heap_ok && retry_ok ? 0 : 1;
}
