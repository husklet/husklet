#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

struct io_arg {
    int fd;
    unsigned char *buffer;
    int write;
    _Atomic int started;
    ssize_t result;
    int error;
};

static void *io_thread(void *opaque) {
    struct io_arg *arg = opaque;
    atomic_store_explicit(&arg->started, 1, memory_order_release);
    arg->result = arg->write ? write(arg->fd, arg->buffer, 1) : read(arg->fd, arg->buffer, 1);
    arg->error = errno;
    return NULL;
}

static void wait_started(struct io_arg *arg) {
    while (!atomic_load_explicit(&arg->started, memory_order_acquire))
        sched_yield();
    usleep(5000);
}

static unsigned char *remap_alias(unsigned char *address, int fd, off_t offset) {
    return mmap(address, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, offset);
}

int main(void) {
    const size_t page = 4096;
    int memfd = (int)syscall(SYS_memfd_create, "logical-race", 0u);
    if (memfd < 0 || ftruncate(memfd, 2 * (off_t)page) != 0) return 2;
    unsigned char *reservation = mmap(NULL, 2 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reservation == MAP_FAILED) return 3;
    unsigned char *address = reservation + page;
    if (remap_alias(address, memfd, page) != address) return 4;

    int safe = 1;
    for (int iteration = 0; iteration < 32 && safe; ++iteration) {
        int pipefd[2];
        if (pipe(pipefd) != 0) return 5;
        struct io_arg arg = {.fd = pipefd[0], .buffer = address};
        pthread_t thread;
        if (pthread_create(&thread, NULL, io_thread, &arg) != 0) return 6;
        wait_started(&arg);
        if (munmap(address, page) != 0 || remap_alias(address, memfd, page) != address) return 7;
        *address = 0xa5;
        if (write(pipefd[1], "R", 1) != 1) return 8;
        pthread_join(thread, NULL);
        safe &= (arg.result == 1 || (arg.result < 0 && arg.error == EFAULT));
        /*
         * Linux may pin before sleeping (replacement stays 0xa5) or resolve
         * the user page after pipe wakeup (replacement receives 'R'). Both
         * are legal races; any other byte indicates stale/corrupt storage.
         */
        safe &= (*address == 0xa5 || *address == (unsigned char)'R');
        close(pipefd[0]);
        close(pipefd[1]);
    }

    int write_safe = 1;
    for (int iteration = 0; iteration < 16 && write_safe; ++iteration) {
        int pipefd[2];
        if (pipe(pipefd) != 0) return 9;
        int flags = fcntl(pipefd[1], F_GETFL);
        if (flags < 0 || fcntl(pipefd[1], F_SETFL, flags | O_NONBLOCK) != 0) return 10;
        unsigned char fill[4096];
        memset(fill, 'F', sizeof(fill));
        while (write(pipefd[1], fill, sizeof(fill)) > 0) {}
        if (errno != EAGAIN || fcntl(pipefd[1], F_SETFL, flags & ~O_NONBLOCK) != 0) return 11;

        *address = 'O';
        struct io_arg arg = {.fd = pipefd[1], .buffer = address, .write = 1};
        pthread_t thread;
        if (pthread_create(&thread, NULL, io_thread, &arg) != 0) return 12;
        wait_started(&arg);
        if (munmap(address, page) != 0 || remap_alias(address, memfd, page) != address) return 13;
        *address = 'N';
        if (read(pipefd[0], fill, sizeof(fill)) <= 0) return 14; /* make room for the blocked byte */
        pthread_join(thread, NULL);
        write_safe &= (arg.result == 1 || (arg.result < 0 && arg.error == EFAULT));
        close(pipefd[0]);
        close(pipefd[1]);
    }

    printf("syscall-logical-unmap-race read-safe=%d write-safe=%d\n", safe, write_safe);
    return safe && write_safe ? 0 : 1;
}
