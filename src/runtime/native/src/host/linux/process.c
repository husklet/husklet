#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "../process.h"

#include <errno.h>
#include <sys/syscall.h>
#include <unistd.h>

int hl_host_process_open(pid_t pid) {
#ifdef SYS_pidfd_open
    return (int)syscall(SYS_pidfd_open, pid, 0u);
#else
    (void)pid;
    errno = ENOSYS;
    return -1;
#endif
}
