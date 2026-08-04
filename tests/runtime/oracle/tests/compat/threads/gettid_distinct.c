// Per-thread kernel tid distinctness and consistency: 20 threads each read gettid(); every tid must be
// distinct from every other and from the main thread's, all share the process getpid(), and a second
// read of gettid() within the same thread is stable. Reported as counts/booleans (no raw ids).
#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#define N 20
static pid_t tids[N];
static pid_t shared_pid;
static atomic_int stable = 0, same_pid = 0;

static void *w(void *arg) {
    long idx = (long)arg;
    pid_t a = (pid_t)syscall(SYS_gettid);
    pid_t b = (pid_t)syscall(SYS_gettid);
    if (a == b) atomic_fetch_add(&stable, 1);
    if (getpid() == shared_pid) atomic_fetch_add(&same_pid, 1);
    tids[idx] = a;
    return 0;
}

int main(void) {
    shared_pid = getpid();
    pid_t main_tid = (pid_t)syscall(SYS_gettid);
    pthread_t t[N];
    for (long i = 0; i < N; i++) pthread_create(&t[i], 0, w, (void *)i);
    for (int i = 0; i < N; i++) pthread_join(t[i], 0);

    int distinct = 1;
    for (int i = 0; i < N; i++) {
        if (tids[i] == main_tid) distinct = 0;
        for (int j = i + 1; j < N; j++) if (tids[i] == tids[j]) distinct = 0;
    }
    printf("gettid_distinct distinct=%d stable=%d same_pid=%d\n",
           distinct, atomic_load(&stable) == N, atomic_load(&same_pid) == N);
    return 0;
}
