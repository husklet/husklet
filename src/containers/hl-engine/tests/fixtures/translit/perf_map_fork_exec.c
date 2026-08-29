#include <stdio.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static _Atomic int worker_stop;
static _Atomic int worker_ready;

__attribute__((noinline)) static int worker_tick(int value) {
    return value + 1;
}

static void *worker(void *unused) {
    (void)unused;
    volatile int value = 0;
    while (!atomic_load_explicit(&worker_stop, memory_order_acquire)) {
        value = worker_tick(value);
        atomic_store_explicit(&worker_ready, 1, memory_order_release);
        sched_yield();
    }
    return NULL;
}

__attribute__((noinline)) static int profiled_target(int value) {
    return value + 35;
}

__attribute__((noinline)) static int profiled_caller(int value) {
    return profiled_target(value);
}

__attribute__((noinline)) static int post_rollover_target(int value) {
    return value * 6;
}

__attribute__((noinline)) static int post_rollover_caller(int value) {
    return post_rollover_target(value);
}

int main(int argc, char **argv) {
    volatile int seed = 7;
    if (argc == 2 && strcmp(argv[1], "post-exec") == 0) {
        pthread_t thread;
        if (pthread_create(&thread, NULL, worker, NULL) != 0) return 7;
        while (!atomic_load_explicit(&worker_ready, memory_order_acquire)) sched_yield();
        int answer = profiled_caller(seed);
        int after = post_rollover_caller(seed);
        atomic_store_explicit(&worker_stop, 1, memory_order_release);
        if (pthread_join(thread, NULL) != 0) return 9;
        printf("post-exec pid=%d answer=%d after=%d caller=%p target=%p\n", (int)getpid(), answer, after,
               (void *)profiled_caller, (void *)post_rollover_caller);
        return answer == 42 && after == 42 ? 0 : 8;
    }

    pid_t child = fork();
    if (child < 0) return 2;
    if (child == 0) {
        int answer = 0;
        for (int i = 0; i < 64; i++) answer = profiled_caller(seed);
        if (answer != 42) _exit(3);
        execl(argv[0], argv[0], "post-exec", (char *)NULL);
        _exit(4);
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child) return 5;
    printf("parent pid=%d child=%d status=%d\n", (int)getpid(), (int)child,
           WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 6;
}
