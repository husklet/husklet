// Regression: a guest-bus stop-the-world transition must not wait on peer threads that are parked
// inside a guest futex wait.
//
// The engine dispatcher publishes a per-thread `in_translated` flag, and a bus transition
// (stw_mapping_begin / stw_force_dispatch_flush in src/translator/cache.c) spins until every peer with
// in_translated=1 acknowledges the new dispatch epoch. If the dispatcher keeps in_translated asserted
// across host service (syscall handling), a thread that blocks indefinitely in a guest FUTEX_WAIT never
// reaches the next dispatcher safepoint, and the transitioning thread spins forever: whole-process
// deadlock with zero output. That is exactly what a Go runtime looks like -- most Ms parked on futexes
// while one thread mutates mappings.
//
// This test builds that shape directly: WORKERS threads block on a condition variable (a real guest
// FUTEX_WAIT) after having executed translated code, while the main thread performs ROUNDS file-mapping
// resizes -- each ftruncate of an mmap'd file drives the engine's bound-mapping bus transition. Under the
// bug the process hangs; correct behaviour finishes promptly with a deterministic line.
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define WORKERS 6
#define ROUNDS 24
#define PAGE 4096

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_cond = PTHREAD_COND_INITIALIZER;
static int g_release;    // set once the main thread has finished all bus transitions
static int g_parked;     // number of workers confirmed blocked in the futex wait
static long g_sum;       // deterministic per-worker contribution, guarded by g_lock

static long spin_work(long seed) {
    // A little translated code so the worker is a fully warmed guest thread before it parks.
    long acc = seed;
    for (int i = 1; i <= 4096; i++) acc = (acc + (long)i * 7) % 1000003;
    return acc;
}

static void *worker(void *arg) {
    long id = (long)arg;
    long value = spin_work(id + 1);
    pthread_mutex_lock(&g_lock);
    g_parked++;
    pthread_cond_broadcast(&g_cond);
    // Block in a guest futex wait for the whole duration of the main thread's bus transitions.
    while (!g_release) pthread_cond_wait(&g_cond, &g_lock);
    g_sum += value;
    pthread_mutex_unlock(&g_lock);
    return NULL;
}

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/tmp/hl_stwfutex_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) {
        printf("stwfutex open failed\n");
        return 1;
    }
    if (ftruncate(fd, PAGE) != 0) {
        printf("stwfutex ftruncate failed\n");
        return 1;
    }
    // A live shared file mapping is what puts the file under the engine's bound-mapping bus ledger, so a
    // later resize has to take the stop-the-world mapping transition.
    unsigned char *map = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) {
        printf("stwfutex mmap failed\n");
        return 1;
    }
    map[0] = 1;

    pthread_t threads[WORKERS];
    for (long i = 0; i < WORKERS; i++) pthread_create(&threads[i], NULL, worker, (void *)i);

    // Wait until every worker is genuinely blocked in the futex before disturbing mappings.
    pthread_mutex_lock(&g_lock);
    while (g_parked < WORKERS) pthread_cond_wait(&g_cond, &g_lock);
    pthread_mutex_unlock(&g_lock);

    long checksum = 0;
    for (int round = 0; round < ROUNDS; round++) {
        // Grow then shrink: both directions publish a new mapping window through the bus transition.
        size_t grown = (size_t)PAGE * (size_t)(2 + (round % 3));
        if (ftruncate(fd, (off_t)grown) != 0) {
            printf("stwfutex resize failed\n");
            return 1;
        }
        map[0] = (unsigned char)(round & 0xff);
        checksum += map[0];
        if (ftruncate(fd, (off_t)PAGE) != 0) {
            printf("stwfutex reset failed\n");
            return 1;
        }
        checksum += (long)round;
    }

    pthread_mutex_lock(&g_lock);
    g_release = 1;
    pthread_cond_broadcast(&g_cond);
    pthread_mutex_unlock(&g_lock);
    for (int i = 0; i < WORKERS; i++) pthread_join(threads[i], NULL);

    munmap(map, PAGE);
    close(fd);
    unlink(path);
    printf("stwfutex workers=%d rounds=%d checksum=%ld sum=%ld\n", WORKERS, ROUNDS, checksum, g_sum);
    return 0;
}
