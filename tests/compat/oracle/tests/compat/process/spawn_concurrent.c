#include <errno.h>
#include <pthread.h>
#include <spawn.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

enum { THREADS = 8, ROUNDS = 12 };

static const char *g_program;
static _Atomic int g_spawned;
static _Atomic int g_spawn_errors;
static _Atomic int g_wait_errors;
static _Atomic int g_bad_status;

static void *worker(void *opaque) {
    (void)opaque;
    for (int round = 0; round < ROUNDS; ++round) {
        pid_t child = -1;
        char *const argv[] = {(char *)g_program, (char *)"child", NULL};
        int error = posix_spawn(&child, g_program, NULL, NULL, argv, environ);
        if (error != 0) {
            atomic_fetch_add_explicit(&g_spawn_errors, 1, memory_order_relaxed);
            continue;
        }
        atomic_fetch_add_explicit(&g_spawned, 1, memory_order_relaxed);
        int status = 0;
        pid_t waited;
        do {
            waited = waitpid(child, &status, 0);
        } while (waited < 0 && errno == EINTR);
        if (waited != child)
            atomic_fetch_add_explicit(&g_wait_errors, 1, memory_order_relaxed);
        else if (!WIFEXITED(status) || WEXITSTATUS(status) != 7)
            atomic_fetch_add_explicit(&g_bad_status, 1, memory_order_relaxed);
    }
    return NULL;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "child") == 0) return 7;
    g_program = argv[0];
    pthread_t threads[THREADS];
    int created = 0;
    for (; created < THREADS; ++created)
        if (pthread_create(&threads[created], NULL, worker, NULL) != 0) break;
    for (int index = 0; index < created; ++index) pthread_join(threads[index], NULL);

    int status = 0;
    errno = 0;
    pid_t leftover = waitpid(-1, &status, WNOHANG);
    int clean = leftover == -1 && errno == ECHILD;
    int spawned = atomic_load_explicit(&g_spawned, memory_order_relaxed);
    int spawn_errors = atomic_load_explicit(&g_spawn_errors, memory_order_relaxed);
    int wait_errors = atomic_load_explicit(&g_wait_errors, memory_order_relaxed);
    int bad_status = atomic_load_explicit(&g_bad_status, memory_order_relaxed);
    printf("spawn-concurrent complete=%d spawned=%d spawn-errors=%d wait-errors=%d bad-status=%d clean=%d\n",
           created == THREADS, spawned, spawn_errors, wait_errors, bad_status, clean);
    return created == THREADS && spawned == THREADS * ROUNDS && spawn_errors == 0 && wait_errors == 0 &&
                   bad_status == 0 && clean
               ? 0
               : 1;
}
