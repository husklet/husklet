#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

enum {
    ALLOCATIONS = 32,
    DESCRIPTORS = 64,
    THREADS = 8,
    ALLOCATION_SIZE = 1024 * 1024,
    /* First-use translation and thread-runtime arenas remain reusable after teardown. */
    RETAINED_RSS_LIMIT = 2048
};

static long count_directory(const char *path) {
    long count = 0;
    DIR *directory = opendir(path);
    if (directory == NULL) return -1;
    for (;;) {
        struct dirent *entry = readdir(directory);
        if (entry == NULL) break;
        if (entry->d_name[0] != '.' ||
            (entry->d_name[1] != '\0' && (entry->d_name[1] != '.' || entry->d_name[2] != '\0')))
            ++count;
    }
    return closedir(directory) == 0 ? count : -1;
}

static long wait_for_count(const char *path, long expected) {
    long count = count_directory(path);
    for (unsigned attempt = 0; count != expected && attempt < 10000; ++attempt) {
        sched_yield();
        count = count_directory(path);
    }
    return count;
}

static long rss_pages(void) {
    long total;
    long resident;
    FILE *file = fopen("/proc/self/statm", "r");
    if (file == NULL) return -1;
    int fields = fscanf(file, "%ld %ld", &total, &resident);
    return fclose(file) == 0 && fields == 2 ? resident : -1;
}

static void *worker(void *argument) {
    int fd = *(int *)argument;
    char byte;
    while (read(fd, &byte, 1) < 0 && errno == EINTR) {}
    return NULL;
}

int main(void) {
    void *memory[ALLOCATIONS] = {0};
    int descriptors[DESCRIPTORS];
    pthread_t threads[THREADS];
    int gate[2];
    long fd_base = count_directory("/proc/self/fd");
    long thread_base = count_directory("/proc/self/task");
    long rss_base = rss_pages();
    long page_size = sysconf(_SC_PAGESIZE);
    if (fd_base < 0 || thread_base < 0 || rss_base < 0 || page_size <= 0 || pipe(gate) != 0) return 1;
    for (unsigned i = 0; i < DESCRIPTORS; ++i) {
        descriptors[i] = open("/dev/null", O_RDONLY | O_CLOEXEC);
        if (descriptors[i] < 0) return 1;
    }
    for (unsigned i = 0; i < ALLOCATIONS; ++i) {
        memory[i] = mmap(NULL, ALLOCATION_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (memory[i] == MAP_FAILED) return 1;
        for (size_t offset = 0; offset < ALLOCATION_SIZE; offset += (size_t)page_size)
            ((char *)memory[i])[offset] = 1;
    }
    for (unsigned i = 0; i < THREADS; ++i)
        if (pthread_create(&threads[i], NULL, worker, &gate[0]) != 0) return 1;
    long fd_peak = count_directory("/proc/self/fd");
    long thread_peak = count_directory("/proc/self/task");
    long rss_peak = rss_pages();
    close(gate[1]);
    for (unsigned i = 0; i < THREADS; ++i)
        if (pthread_join(threads[i], NULL) != 0) return 1;
    close(gate[0]);
    for (unsigned i = 0; i < DESCRIPTORS; ++i)
        close(descriptors[i]);
    for (unsigned i = 0; i < ALLOCATIONS; ++i)
        munmap(memory[i], ALLOCATION_SIZE);
    long fd_end = count_directory("/proc/self/fd");
    long thread_end = wait_for_count("/proc/self/task", thread_base);
    long rss_end = rss_pages();
    int ok = fd_peak >= fd_base + DESCRIPTORS && thread_peak == thread_base + THREADS && rss_peak > rss_base &&
             fd_end == fd_base && thread_end == thread_base && rss_end <= rss_base + RETAINED_RSS_LIMIT;
    printf("resource ok=%d fd=%ld/%ld/%ld threads=%ld/%ld/%ld rss_pages=%ld/%ld/%ld\n", ok, fd_base, fd_peak, fd_end,
           thread_base, thread_peak, thread_end, rss_base, rss_peak, rss_end);
    return ok ? 0 : 1;
}
