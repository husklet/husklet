#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static unsigned char *page;
static atomic_int request, done;

static void *protector(void *unused) {
    (void)unused;
    for (int generation = 1; generation <= 2000; ++generation) {
        while (atomic_load_explicit(&request, memory_order_acquire) != generation) {}
        if (mprotect(page, 4096, (generation & 1) ? PROT_NONE : PROT_READ | PROT_WRITE) != 0) return (void *)1;
        atomic_store_explicit(&done, generation, memory_order_release);
    }
    return NULL;
}

int main(void) {
    page = mmap(NULL, 3 * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) return 2;
    int fd[2];
    if (pipe(fd) != 0) return 3;
    page[0] = 7;
    /* Prime this thread's same-page negative entry before another thread
       changes the logical protection generation. */
    if (write(fd[1], page, 1) != 1) return 4;
    pthread_t thread;
    if (pthread_create(&thread, NULL, protector, NULL) != 0) return 5;
    int fault = 1, clear = 1;
    for (int generation = 1; generation <= 2000; ++generation) {
        atomic_store_explicit(&request, generation, memory_order_release);
        while (atomic_load_explicit(&done, memory_order_acquire) != generation) {}
        errno = 0;
        ssize_t result = write(fd[1], page, 1);
        if (generation & 1)
            fault &= result == -1 && errno == EFAULT;
        else
            clear &= result == 1;
    }
    void *status = NULL;
    pthread_join(thread, &status);
    int split = mprotect(page, 3 * 4096, PROT_NONE) == 0 &&
                mprotect(page + 4096, 4096, PROT_READ | PROT_WRITE) == 0 &&
                write(fd[1], page + 4096, 1) == 1 &&
                write(fd[1], page, 1) == -1;
    printf("gna-negative-cache fault=%d clear=%d split=%d thread=%d\n",
           fault, clear, split, status == NULL);
    return !(fault && clear && split && status == NULL);
}
