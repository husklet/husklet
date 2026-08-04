// /proc/self/task/<tid> is the per-thread directory the JVM, Go runtime and top(1) -H enumerate to see a
// process's threads. After spawning N worker threads that are all live, the directory must contain exactly
// N+1 numeric entries, /proc/self/status "Threads:" must read N+1, and the calling thread's own tid must
// have an entry whose task/<tid>/comm is readable. A synthesized task dir that reports a fixed or stale
// thread set fails. Derived from the threads this process creates, oracle-neutral.
#define _GNU_SOURCE
#include <dirent.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>
#include "pf.h"

enum { WORKERS = 3 };
static pthread_barrier_t start, hold;

static void *worker(void *arg) {
    (void)arg;
    pthread_barrier_wait(&start); // all threads now live
    pthread_barrier_wait(&hold);  // keep alive until main has sampled
    return NULL;
}

static int task_count(void) {
    DIR *d = opendir("/proc/self/task");
    if (!d) return -1;
    int c = 0;
    for (struct dirent *e; (e = readdir(d));)
        if (e->d_name[0] != '.') c++;
    closedir(d);
    return c;
}

int main(void) {
    pthread_barrier_init(&start, NULL, WORKERS + 1);
    pthread_barrier_init(&hold, NULL, WORKERS + 1);
    pthread_t th[WORKERS];
    for (int i = 0; i < WORKERS; i++) pthread_create(&th[i], NULL, worker, NULL);
    pthread_barrier_wait(&start);

    int count_ok = task_count() == WORKERS + 1;
    char b[8192], v[64];
    pf_read("/proc/self/status", b, sizeof b);
    int threads_ok = pf_line_val(b, "Threads:", v, sizeof v) && atoi(v) == WORKERS + 1;
    char p[64];
    snprintf(p, sizeof p, "/proc/self/task/%ld/comm", (long)syscall(SYS_gettid));
    int self_task_ok = pf_read(p, b, sizeof b) > 0;

    pthread_barrier_wait(&hold);
    for (int i = 0; i < WORKERS; i++) pthread_join(th[i], NULL);

    int ok = count_ok && threads_ok && self_task_ok;
    printf("selftask ok=%d\n", ok);
    return 0;
}
