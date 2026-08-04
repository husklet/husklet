#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

struct thread_case {
    unsigned char *writer;
    unsigned char *reader;
    _Atomic int ready;
    int observed;
};

static void *thread_writer(void *opaque) {
    struct thread_case *test = opaque;
    test->writer[7] = 0x6c;
    atomic_store_explicit(&test->ready, 1, memory_order_release);
    return NULL;
}

static void *thread_reader(void *opaque) {
    struct thread_case *test = opaque;
    while (!atomic_load_explicit(&test->ready, memory_order_acquire)) {}
    test->observed = test->reader[7] == 0x6c;
    return NULL;
}

int main(void) {
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "offset-alias", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)(page * 3)) != 0) return 2;

    unsigned char *readable = mmap(NULL, page, PROT_READ, MAP_SHARED, fd, (off_t)page);
    unsigned char *writable = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)page);
    if (readable == MAP_FAILED || writable == MAP_FAILED) return 3;

    writable[0] = 0x0b;
    writable[page - 1] = 0x7d;
    int alias_coherent = readable[0] == 0x0b && readable[page - 1] == 0x7d;

    unsigned char *fixed = mmap(NULL, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (fixed == MAP_FAILED ||
        mmap(fixed, page, PROT_NONE, MAP_SHARED | MAP_FIXED, fd, (off_t)page) != fixed)
        return 4;
    int protected = mprotect(fixed, page, PROT_READ | PROT_EXEC) == 0;
    writable[1] = 0x36;
    writable[page - 2] = 0x9a;
    int fixed_coherent = protected && fixed[1] == 0x36 && fixed[page - 2] == 0x9a;

    writable[2] = 0x47;
    writable[page - 3] = 0xa5;
    int writable_publish_coherent = fixed[2] == 0x47 && fixed[page - 3] == 0xa5;

    unsigned char *third = mmap(NULL, page, PROT_READ, MAP_SHARED, fd, (off_t)page);
    if (close(fd) != 0) return 5;
    int third_coherent = third != MAP_FAILED && third[2] == 0x47 && third[page - 3] == 0xa5;
    struct thread_case threaded = {.writer = writable, .reader = fixed};
    pthread_t writer_thread, reader_thread;
    int thread_coherent = pthread_create(&reader_thread, NULL, thread_reader, &threaded) == 0 &&
                          pthread_create(&writer_thread, NULL, thread_writer, &threaded) == 0 &&
                          pthread_join(writer_thread, NULL) == 0 && pthread_join(reader_thread, NULL) == 0 &&
                          threaded.observed;

    int fixed_unmapped = munmap(fixed, page) == 0;
    unsigned char *reused = mmap(fixed, page, PROT_READ | PROT_WRITE,
                                 MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reused != MAP_FAILED) reused[7] = 0x91;
    int va_reuse = fixed_unmapped && reused == fixed && reused[7] == 0x91;
    int unmapped = munmap(readable, page) == 0 && munmap(writable, page) == 0 && munmap(reused, page) == 0 &&
                   munmap(third, page) == 0;
    printf("memfd-offset-alias alias=%d fixed=%d writable-publish=%d third=%d thread=%d reuse=%d unmapped=%d\n",
           alias_coherent, fixed_coherent, writable_publish_coherent, third_coherent, thread_coherent, va_reuse,
           unmapped);
    return alias_coherent && fixed_coherent && writable_publish_coherent && third_coherent && thread_coherent &&
                   va_reuse && unmapped
               ? 0
               : 1;
}
