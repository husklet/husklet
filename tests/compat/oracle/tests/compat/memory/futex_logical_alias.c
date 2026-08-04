#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#define FUTEX_WAIT 0
#define FUTEX_WAKE 1
#define FUTEX_PRIVATE_FLAG 128

struct wait_arg {
    int *word;
    int private;
    _Atomic int ready;
    _Atomic int done;
    long result;
};

static void *waiter(void *opaque) {
    struct wait_arg *arg = opaque;
    atomic_store_explicit(&arg->ready, 1, memory_order_release);
    arg->result = syscall(SYS_futex, arg->word, FUTEX_WAIT | arg->private, 0, NULL, NULL, 0);
    atomic_store_explicit(&arg->done, 1, memory_order_release);
    return NULL;
}

static void wait_ready(struct wait_arg *arg) {
    while (!atomic_load_explicit(&arg->ready, memory_order_acquire))
        sched_yield();
    usleep(20000);
}

int main(void) {
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "futex-alias", 0u);
    if (fd < 0 || ftruncate(fd, 2 * (off_t)page) != 0) return 2;
    unsigned char *r0 = mmap(NULL, 2 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned char *r1 = mmap(NULL, 2 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (r0 == MAP_FAILED || r1 == MAP_FAILED) return 3;
    int *a = mmap(r0 + page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, page);
    int *b = mmap(r1 + page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, page);
    if (a != (int *)(r0 + page) || b != (int *)(r1 + page)) return 4;
    *a = 0;

    struct wait_arg shared = {.word = a};
    pthread_t thread;
    if (pthread_create(&thread, NULL, waiter, &shared) != 0) return 5;
    wait_ready(&shared);
    long shared_woke = syscall(SYS_futex, b, FUTEX_WAKE, 1, NULL, NULL, 0);
    pthread_join(thread, NULL);
    int shared_ok = shared_woke == 1 && shared.result == 0;

    *a = 0;
    struct wait_arg private = {.word = a, .private = FUTEX_PRIVATE_FLAG};
    if (pthread_create(&thread, NULL, waiter, &private) != 0) return 6;
    wait_ready(&private);
    long wrong_alias = syscall(SYS_futex, b, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0);
    usleep(20000);
    int stayed_asleep = !atomic_load_explicit(&private.done, memory_order_acquire);
    long right_alias = syscall(SYS_futex, a, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0);
    pthread_join(thread, NULL);
    int private_ok = wrong_alias == 0 && stayed_asleep && right_alias == 1 && private.result == 0;

    printf("futex-logical-alias shared=%d private-distinct=%d\n", shared_ok, private_ok);
    return shared_ok && private_ok ? 0 : 1;
}
