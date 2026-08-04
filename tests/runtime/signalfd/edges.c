#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <unistd.h>

static volatile sig_atomic_t handled;
static void handler(int signal) { handled += signal == SIGALRM; }

int signalfd_edges(void) {
    struct sigaction action = { .sa_handler = handler };
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGALRM, &action, 0) != 0 || raise(SIGALRM) != 0) return 20;
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigaddset(&mask, SIGUSR2);
    sigaddset(&mask, SIGRTMIN);
    if (sigprocmask(SIG_BLOCK, &mask, 0) != 0) return 21;
    int fd = signalfd(-1, &mask, SFD_NONBLOCK | SFD_CLOEXEC);
    if (fd < 0) return 22;
    struct signalfd_siginfo info[2];
    errno = 0;
    ssize_t empty = read(fd, info, sizeof(info[0]));
    int empty_errno = errno;
    if (kill(getpid(), SIGUSR1) != 0) return 23;
    errno = 0;
    ssize_t fault = syscall(SYS_read, fd, (void *)1, sizeof(info[0]));
    int fault_errno = errno;
    ssize_t first = read(fd, info, sizeof(info[0]));
    int process_info = first == 128 && info[0].ssi_signo == SIGUSR1 &&
        info[0].ssi_code == SI_USER && info[0].ssi_pid == (uint32_t)getpid() &&
        info[0].ssi_uid == (uint32_t)getuid();
    pid_t tid = (pid_t)syscall(SYS_gettid);
    if (syscall(SYS_tkill, tid, SIGUSR2) != 0 ||
        syscall(SYS_tgkill, getpid(), tid, SIGRTMIN) != 0) return 24;
    ssize_t directed = read(fd, &info[0], sizeof(info[0]));
    ssize_t grouped = read(fd, &info[1], sizeof(info[1]));
    int tkill_info = directed == 128 && info[0].ssi_signo == SIGUSR2 &&
        info[0].ssi_code == SI_TKILL && info[0].ssi_pid == (uint32_t)getpid();
    int tgkill_info = grouped == 128 && info[1].ssi_signo == SIGRTMIN &&
        info[1].ssi_code == SI_TKILL && info[1].ssi_pid == (uint32_t)getpid();
    int cloexec = (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0;
    printf("signalfd_edges handler=%d empty=%ld eagain=%d fault=%ld efault=%d kill=%d directed=%ld tkill=%d grouped=%ld tgkill=%d cloexec=%d\n",
        handled == 1, (long)empty, empty_errno == EAGAIN, (long)fault,
        fault_errno == EFAULT, process_info, (long)directed, tkill_info,
        (long)grouped, tgkill_info, cloexec);
    return 0;
}
