#include <stdio.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static _Atomic int worker_stop;

static void *worker(void *unused) {
    (void)unused;
    while (!atomic_load_explicit(&worker_stop, memory_order_acquire)) sched_yield();
    return NULL;
}

__attribute__((noinline)) static int profiled_target(int value) {
    return value + 35;
}

__attribute__((noinline)) static int profiled_caller(int value) {
    return profiled_target(value);
}

int main(int argc, char **argv) {
    volatile int seed = 7;
    if (argc == 2 && strcmp(argv[1], "post-exec") == 0) {
        pthread_t thread;
        if (pthread_create(&thread, NULL, worker, NULL) != 0) return 7;
        int answer = 0;
        for (int i = 0; i < 64; i++) answer = profiled_caller(seed);
        atomic_store_explicit(&worker_stop, 1, memory_order_release);
        if (pthread_join(thread, NULL) != 0) return 9;
        printf("post-exec pid=%d answer=%d caller=%p target=%p\n", (int)getpid(), answer,
               (void *)profiled_caller, (void *)profiled_target);
        return answer == 42 ? 0 : 8;
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
