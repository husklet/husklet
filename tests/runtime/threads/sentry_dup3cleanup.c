#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/eventfd.h>
#include <unistd.h>

int main(void) {
    int stale = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    char path[] = "/tmp/hl-dup3-cleanup.XXXXXX";
    int source = mkstemp(path);
    if (stale < 0 || source < 0) return 10;
    unlink(path);

    int replaced = dup3(source, stale, O_CLOEXEC) == stale;
    int file_ok = replaced && write(stale, "x", 1) == 1;
    int close_ok = close(stale) == 0 && close(source) == 0;

    int fresh = eventfd(0, EFD_NONBLOCK);
    uint64_t one = 1;
    uint64_t value = 0;
    int event_ok = fresh >= 0 && write(fresh, &one, sizeof one) == sizeof one &&
                   read(fresh, &value, sizeof value) == sizeof value && value == 1;
    int final_close = fresh >= 0 && close(fresh) == 0;
    printf("sentry_dup3_cleanup replaced=%d file_ok=%d close_ok=%d event_ok=%d final_close=%d\n", replaced, file_ok,
           close_ok, event_ok, final_close);
    return replaced && file_ok && close_ok && event_ok && final_close ? 0 : 1;
}
