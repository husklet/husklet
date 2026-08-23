#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/eventfd.h>
#include <unistd.h>

#define OPEN_COUNT 1021

static int event_works(int descriptor) {
    uint64_t one = 1;
    uint64_t value = 0;
    return write(descriptor, &one, sizeof one) == sizeof one &&
           read(descriptor, &value, sizeof value) == sizeof value && value == 1;
}

int main(void) {
    int descriptors[OPEN_COUNT];
    for (int i = 0; i < OPEN_COUNT; i++) {
        descriptors[i] = eventfd(0, EFD_NONBLOCK);
        if (descriptors[i] < 0) return 10;
    }
    errno = 0;
    int create_full = eventfd(0, EFD_NONBLOCK);
    int create_emfile = create_full == -1 && errno == EMFILE;
    errno = 0;
    int duplicate_full = fcntl(descriptors[0], F_DUPFD, 0);
    int duplicate_emfile = duplicate_full == -1 && errno == EMFILE;

    int released = close(descriptors[OPEN_COUNT - 1]) == 0;
    int duplicate = fcntl(descriptors[0], F_DUPFD, 0);
    int duplicate_ok = duplicate >= 0 && event_works(duplicate);
    int duplicate_close = duplicate >= 0 && close(duplicate) == 0;
    int fresh = eventfd(0, EFD_NONBLOCK);
    int fresh_ok = fresh >= 0 && event_works(fresh);
    int fresh_close = fresh >= 0 && close(fresh) == 0;
    int cleanup = 1;
    for (int i = 0; i < OPEN_COUNT - 1; i++)
        cleanup &= close(descriptors[i]) == 0;

    printf("sentry_fd_emfile create=%d duplicate=%d recovered_dup=%d recovered_new=%d cleanup=%d\n", create_emfile,
           duplicate_emfile, released && duplicate_ok && duplicate_close, fresh_ok && fresh_close, cleanup);
    return create_emfile && duplicate_emfile && released && duplicate_ok && duplicate_close && fresh_ok &&
                   fresh_close && cleanup
               ? 0
               : 1;
}
