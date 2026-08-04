#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static _Atomic unsigned command;
static _Atomic unsigned done;
static _Atomic uint32_t observed;
static uint32_t (*entry)(void);

static void emit_return(unsigned char *code, uint32_t value) {
    unsigned char bytes[16] = {
        0xb8, 0, 0, 0, 0,
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0xc3,
    };
    memcpy(bytes + 1, &value, sizeof(value));
    memcpy(code, bytes, sizeof(bytes));
}

static void *executor(void *unused) {
    (void)unused;
    unsigned seen = 0;
    for (;;) {
        unsigned requested;
        while ((requested = atomic_load_explicit(&command, memory_order_acquire)) == seen)
            ;
        if (requested == UINT32_MAX) break;
        uint32_t value = entry();
        atomic_store_explicit(&observed, value, memory_order_relaxed);
        atomic_store_explicit(&done, requested, memory_order_release);
        seen = requested;
    }
    return NULL;
}

int main(void) {
#if !defined(__x86_64__)
    puts("memfd-exec-alias-race skipped=1");
    return 0;
#else
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "exec-alias-race", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)page) != 0) return 2;
    unsigned char *rw = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    unsigned char *rx = mmap(NULL, page, PROT_READ | PROT_EXEC, MAP_SHARED, fd, 0);
    close(fd);
    if (rw == MAP_FAILED || rx == MAP_FAILED) return 3;
    entry = (uint32_t (*)(void))rx;

    pthread_t thread;
    unsigned char code[16];
    emit_return(code, 1);
    memcpy(rw, code, sizeof(code));
    if (pthread_create(&thread, NULL, executor, NULL) != 0) return 4;

    int stale = 0;
    for (unsigned generation = 1; generation <= 256; ++generation) {
        uint32_t expected = generation * 3u + 7u;
        emit_return(code, expected);
        memcpy(rw, code, sizeof(code));
        atomic_store_explicit(&command, generation, memory_order_release);
        while (atomic_load_explicit(&done, memory_order_acquire) != generation)
            ;
        if (atomic_load_explicit(&observed, memory_order_relaxed) != expected) {
            stale = 1;
            break;
        }
    }
    atomic_store_explicit(&command, UINT32_MAX, memory_order_release);
    pthread_join(thread, NULL);
    printf("memfd-exec-alias-race stale=%d iterations=%u\n", stale, stale ? 0u : 256u);
    munmap(rx, page);
    munmap(rw, page);
    return stale ? 1 : 0;
#endif
}
