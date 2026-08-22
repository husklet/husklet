#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef CLOSE_RANGE_UNSHARE
#define CLOSE_RANGE_UNSHARE (1U << 1)
#endif

struct close_request {
    int descriptor;
    int closed;
};

static void *close_private_copy(void *opaque) {
    struct close_request *request = opaque;
    long result =
        syscall(SYS_close_range, (unsigned)request->descriptor, (unsigned)request->descriptor, CLOSE_RANGE_UNSHARE);
    request->closed = result == 0 && fcntl(request->descriptor, F_GETFD) == -1 && errno == EBADF;
    return NULL;
}

int main(void) {
    int descriptor = eventfd(0, EFD_NONBLOCK);
    if (descriptor < 0) return 10;
    uint64_t one = 1;
    if (write(descriptor, &one, sizeof one) != sizeof one) return 11;

    struct close_request request = {.descriptor = descriptor, .closed = 0};
    pthread_t thread;
    if (pthread_create(&thread, NULL, close_private_copy, &request) != 0) return 12;
    if (pthread_join(thread, NULL) != 0) return 13;

    uint64_t value = 0;
    int owner_alive = read(descriptor, &value, sizeof value) == sizeof value && value == 1;
    int owner_close = close(descriptor) == 0;

    printf("close_range_unshare thread_closed=%d owner_alive=%d owner_close=%d\n", request.closed, owner_alive,
           owner_close);
    return request.closed && owner_alive && owner_close ? 0 : 1;
}
