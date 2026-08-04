#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

enum { THREADS = 4, ROUNDS = 20000 };
static _Atomic unsigned long *counter;

#if defined(__aarch64__)
static void exclusive_increment(_Atomic unsigned long *value) {
    unsigned long old, next;
    unsigned status;
    __asm__ volatile(
        "1: ldxr %x[old], [%x[value]]\n"
        "add %x[next], %x[old], #1\n"
        "stxr %w[status], %x[next], [%x[value]]\n"
        "cbnz %w[status], 1b\n"
        : [old] "=&r"(old), [next] "=&r"(next), [status] "=&r"(status)
        : [value] "r"(value)
        : "memory");
}

static void stolen_exclusive_increment(_Atomic unsigned long *value) {
    __asm__ volatile(
        "1: ldxr x16, [%x[value]]\n"
        "add x17, x16, #1\n"
        "stxr w15, x17, [%x[value]]\n"
        "cbnz w15, 1b\n"
        :
        : [value] "r"(value)
        : "x15", "x16", "x17", "memory");
}
#else
static void exclusive_increment(_Atomic unsigned long *value) {
    atomic_fetch_add_explicit(value, 1, memory_order_relaxed);
}
static void stolen_exclusive_increment(_Atomic unsigned long *value) {
    atomic_fetch_add_explicit(value, 1, memory_order_relaxed);
}
#endif

static void *increment(void *unused) {
    (void)unused;
    for (int i = 0; i < ROUNDS; ++i)
        exclusive_increment(counter);
    stolen_exclusive_increment(counter);
    return NULL;
}

int main(void) {
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "logical-atomics", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)(page * 2)) != 0) return 2;
    counter = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)page);
    if (counter == MAP_FAILED) return 3;
    atomic_init(counter, 0);

    pthread_t threads[THREADS];
    for (int i = 0; i < THREADS; ++i)
        if (pthread_create(&threads[i], NULL, increment, NULL) != 0) return 4;
    for (int i = 0; i < THREADS; ++i)
        if (pthread_join(threads[i], NULL) != 0) return 5;

    unsigned long value = atomic_load_explicit(counter, memory_order_relaxed);
    printf("aarch64-logical-atomics value=%lu expected=%d\n", value, THREADS * (ROUNDS + 1));
    return value == THREADS * (ROUNDS + 1) ? 0 : 1;
}
