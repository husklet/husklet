#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

struct race {
    void *address;
    int fd;
    _Atomic int failed;
};

static void *worker(void *opaque) {
    struct race *race = opaque;
    for (unsigned i = 0; i < 100; ++i) {
        void *mapped = mmap(race->address, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, race->fd, 4096);
        if (mapped != race->address || munmap(race->address, 4096) != 0) {
            atomic_store_explicit(&race->failed, 1, memory_order_release);
            break;
        }
    }
    return NULL;
}

int main(void) {
    int fd = (int)syscall(SYS_memfd_create, "logical-transition", 0u);
    if (fd < 0 || ftruncate(fd, 16384) != 0) return 1;
    void *reservation = mmap(NULL, 16384, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reservation == MAP_FAILED || munmap(reservation, 16384) != 0) return 1;
    struct race race = {.address = reservation, .fd = fd};
    pthread_t threads[2];
    if (pthread_create(&threads[0], NULL, worker, &race) != 0 || pthread_create(&threads[1], NULL, worker, &race) != 0)
        return 1;
    pthread_join(threads[0], NULL);
    pthread_join(threads[1], NULL);
    (void)munmap(reservation, 4096);
    close(fd);
    printf("logical-transition-race deadlock-free=%d\n", !atomic_load_explicit(&race.failed, memory_order_acquire));
    return atomic_load_explicit(&race.failed, memory_order_acquire) ? 1 : 0;
}
