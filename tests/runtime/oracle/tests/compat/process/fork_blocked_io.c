#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static int descriptors[2];
static _Atomic int entering;

static void *reader(void *unused) {
    char byte = 0;
    (void)unused;
    atomic_store_explicit(&entering, 1, memory_order_release);
    return (void *)(intptr_t)(read(descriptors[0], &byte, 1) == 1 && byte == 'x' ? 0 : 1);
}

int main(void) {
    pthread_t thread;
    if (pipe(descriptors) != 0 || pthread_create(&thread, NULL, reader, NULL) != 0) return 2;
    while (!atomic_load_explicit(&entering, memory_order_acquire)) sched_yield();
    usleep(10000); /* reader is now blocked inside the typed host operation */
    pid_t child = fork();
    if (child == 0) _exit(0);
    int status = 0;
    int fork_ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int wake_ok = write(descriptors[1], "x", 1) == 1;
    void *result = (void *)(intptr_t)1;
    wake_ok = wake_ok && pthread_join(thread, &result) == 0 && result == NULL;
    printf("fork-blocked-io fork=%d wake=%d\n", fork_ok, wake_ok);
    close(descriptors[0]);
    close(descriptors[1]);
    return fork_ok && wake_ok ? 0 : 3;
}
