// signalfd delivers blocked signals as readable struct signalfd_siginfo records instead of
// invoking a handler. Block SIGUSR1, raise it, and read the fd: ssi_signo must match and no
// handler runs.
#include <signal.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/signalfd.h>
#include <unistd.h>

static volatile sig_atomic_t handler_ran;
static void h(int s) { (void)s; handler_ran++; }

int main(void) {
    signal(SIGUSR1, h);
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigprocmask(SIG_BLOCK, &mask, NULL);

    int fd = signalfd(-1, &mask, 0);
    if (fd < 0) { printf("signalfd_basic fd_fail\n"); return 1; }

    raise(SIGUSR1);

    struct signalfd_siginfo si;
    ssize_t n = read(fd, &si, sizeof si);
    int ok = n == (ssize_t)sizeof si && si.ssi_signo == (uint32_t)SIGUSR1;
    printf("signalfd_basic read_ok=%d no_handler=%d\n", ok, handler_ran == 0);
    return 0;
}
