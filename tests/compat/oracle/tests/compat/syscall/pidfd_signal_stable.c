#define _GNU_SOURCE
#include <signal.h>
#include <sys/syscall.h>
#include <unistd.h>

static int stable_kill(pid_t process, int signal_number) {
    if (signal_number == SIGKILL) usleep(100000);
    return (int)syscall(SYS_kill, process, signal_number);
}

#define main legacy_pidfd_signal_main
#define kill stable_kill
#include "pidfd_signal.c"
#undef kill
#undef main

int main(void) { return legacy_pidfd_signal_main(); }
