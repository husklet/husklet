#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <unistd.h>

enum { WORKERS = 6, ITERATIONS = 2000 };
static atomic_int start;
static atomic_int failures;

static void *allocator(void *unused) {
    (void)unused;
    while (!atomic_load_explicit(&start, memory_order_acquire)) {}
    for (int i = 0; i < ITERATIONS; ++i) {
        int fd = open("/dev/zero", O_RDONLY | O_CLOEXEC);
        if (fd < 0) {
            atomic_fetch_add(&failures, 1);
            continue;
        }
        int copy = dup(fd);
        unsigned char byte = 1;
        if (copy < 0 || read(copy, &byte, 1) != 1 || byte != 0) atomic_fetch_add(&failures, 1);
        if (copy >= 0) close(copy);
        close(fd);
    }
    return NULL;
}

int main(void) {
    pthread_t threads[WORKERS];
    for (int i = 0; i < WORKERS; ++i) pthread_create(&threads[i], NULL, allocator, NULL);
    atomic_store_explicit(&start, 1, memory_order_release);
    for (int i = 0; i < ITERATIONS; ++i) {
        DIR *directory = opendir("/tmp");
        if (directory == NULL) {
            atomic_fetch_add(&failures, 1);
        } else {
            closedir(directory);
        }
        int source = open("/dev/zero", O_RDONLY | O_CLOEXEC);
        if (source >= 0) {
            int target = 97;
            int replaced = dup2(source, target);
            if (replaced != target) atomic_fetch_add(&failures, 1);
            unsigned char byte = 1;
            if (replaced >= 0 && (read(replaced, &byte, 1) != 1 || byte != 0)) atomic_fetch_add(&failures, 1);
            if (replaced >= 0) close(replaced);
            close(source);
        } else {
            atomic_fetch_add(&failures, 1);
        }
    }
    for (int i = 0; i < WORKERS; ++i) pthread_join(threads[i], NULL);

    int held[16];
    for (int i = 0; i < 16; ++i) held[i] = open("/dev/null", O_RDONLY | O_CLOEXEC);
    int expected = held[5];
    close(held[5]);
    int lowest = dup(held[0]);
    int lowest_ok = lowest == expected;
    if (lowest >= 0) close(lowest);
    for (int i = 0; i < 16; ++i)
        if (i != 5 && held[i] >= 0) close(held[i]);

    printf("fd-shadow-contention failures=%d lowest=%d\n", atomic_load(&failures), lowest_ok);
    return atomic_load(&failures) == 0 && lowest_ok ? 0 : 1;
}
