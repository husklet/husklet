#include "../process.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <sys/event.h>
#include <unistd.h>

int hl_host_process_open(pid_t pid) {
    int descriptor = kqueue();
    if (descriptor < 0) return -1;
    struct kevent event;
    EV_SET(&event, (uintptr_t)pid, EVFILT_PROC, EV_ADD, NOTE_EXIT, 0, NULL);
    if (kevent(descriptor, &event, 1, NULL, 0, NULL) != 0) {
        int error = errno;
        close(descriptor);
        errno = error;
        return -1;
    }
    (void)fcntl(descriptor, F_SETFD, FD_CLOEXEC);
    return descriptor;
}
