// Chrome performs pathname syscalls from many worker threads while its address
// space changes. Valid stack and heap pointers must never fail with EFAULT.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#if defined(__aarch64__)
enum { WORKERS = 24, ITERATIONS = 2000 };
#else
enum { WORKERS = 48, ITERATIONS = 2000 };
#endif

struct result {
    int stat;
    int stat_efault;
    int unlink;
    int unlink_efault;
};

static pthread_barrier_t start;

static void *paths(void *opaque) {
    struct result *result = opaque;
    pthread_barrier_wait(&start);
    char *heap_path = strdup("/tmp/hl-path-contention-missing");
    if (heap_path == NULL) {
        result->stat++;
        return NULL;
    }
    for (int iteration = 0; iteration < ITERATIONS; ++iteration) {
        char stack_path[] = "/tmp";
        struct stat status;
        if (stat(stack_path, &status) != 0 || !S_ISDIR(status.st_mode)) {
            result->stat++;
            result->stat_efault += errno == EFAULT;
        }

        errno = 0;
        if (unlink(heap_path) != -1 || errno != ENOENT) {
            result->unlink++;
            result->unlink_efault += errno == EFAULT;
        }

        if ((iteration & 31) == 0) sched_yield();
    }
    free(heap_path);
    return NULL;
}

static void *protections(void *opaque) {
    (void)opaque;
    pthread_barrier_wait(&start);
    for (int iteration = 0; iteration < ITERATIONS; ++iteration) {
        void *page = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (page == MAP_FAILED) continue;
        (void)mprotect(page, 4096, PROT_READ | PROT_WRITE);
        (void)mprotect(page, 4096, PROT_READ);
        (void)munmap(page, 4096);
    }
    return NULL;
}

int main(void) {
    pthread_t workers[WORKERS], churn;
    struct result results[WORKERS + 1] = {0};
    if (pthread_barrier_init(&start, NULL, WORKERS + 2) != 0) {
        perror("pthread_barrier_init");
        return 2;
    }
    for (int index = 0; index < WORKERS; ++index) {
        if (pthread_create(&workers[index], NULL, paths, &results[index]) != 0) {
            perror("pthread_create paths");
            return 2;
        }
    }
    if (pthread_create(&churn, NULL, protections, &results[WORKERS]) != 0) {
        perror("pthread_create protections");
        return 2;
    }
    pthread_barrier_wait(&start);
    for (int index = 0; index < WORKERS; ++index) {
        if (pthread_join(workers[index], NULL) != 0) {
            perror("pthread_join paths");
            return 2;
        }
    }
    if (pthread_join(churn, NULL) != 0) {
        perror("pthread_join protections");
        return 2;
    }
    pthread_barrier_destroy(&start);

    struct result total = {0};
    for (int index = 0; index <= WORKERS; ++index) {
        total.stat += results[index].stat;
        total.stat_efault += results[index].stat_efault;
        total.unlink += results[index].unlink;
        total.unlink_efault += results[index].unlink_efault;
    }
    printf("path_thread_contention stat=%d/%d unlink=%d/%d\n", total.stat, total.stat_efault, total.unlink,
           total.unlink_efault);
    return total.stat == 0 && total.unlink == 0 ? 0 : 1;
}
