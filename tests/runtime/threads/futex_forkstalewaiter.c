#define _GNU_SOURCE
#include <errno.h>
#include <linux/futex.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static _Atomic int word;
static _Atomic int ready;

static long futex(int operation, int value) {
    return syscall(SYS_futex, &word, operation | FUTEX_PRIVATE_FLAG, value, NULL, NULL, 0);
}

static void *waiter(void *unused) {
    (void)unused;
    atomic_store(&ready, 1);
    while (atomic_load(&word) == 0 && futex(FUTEX_WAIT, 0) < 0 && errno == EINTR) {}
    return NULL;
}

int main(void) {
    pthread_t parent_waiter;
    pthread_create(&parent_waiter, NULL, waiter, NULL);
    while (!atomic_load(&ready)) sched_yield();
    usleep(10000);

    pid_t child = fork();
    if (child == 0) {
        atomic_store(&ready, 0);
        pthread_t child_waiter;
        pthread_create(&child_waiter, NULL, waiter, NULL);
        while (!atomic_load(&ready)) sched_yield();
        usleep(10000);
        atomic_store(&word, 1);
        long woke = futex(FUTEX_WAKE, 1);
        pthread_join(child_waiter, NULL);
        _exit(woke == 1 ? 0 : 2);
    }

    int status = 0;
    waitpid(child, &status, 0);
    atomic_store(&word, 1);
    futex(FUTEX_WAKE, 1);
    pthread_join(parent_waiter, NULL);
    int clean = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    printf("futex-fork-stale-waiter clean=%d\n", clean);
    return clean ? 0 : 1;
}
