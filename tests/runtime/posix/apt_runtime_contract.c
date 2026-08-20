#define _DEFAULT_SOURCE
#define _XOPEN_SOURCE 600
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int wait_intr(pid_t child, int *status) {
    pid_t result;
    do {
        result = waitpid(child, status, 0);
    } while (result < 0 && errno == EINTR);
    return result == child;
}

int main(void) {
    struct stat tmp_status;
    int tmp = stat("/tmp", &tmp_status) == 0 && S_ISDIR(tmp_status.st_mode) && (tmp_status.st_mode & 07777) == 01777;
    char temporary[] = "/tmp/hl-apt-contract-XXXXXX";
    int temporary_fd = mkstemp(temporary);
    int writable = temporary_fd >= 0 && write(temporary_fd, "package\n", 8) == 8 && fsync(temporary_fd) == 0;
    if (temporary_fd >= 0) close(temporary_fd);

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    int ptn = -1;
    int devpts =
        master >= 0 && grantpt(master) == 0 && unlockpt(master) == 0 && ioctl(master, TIOCGPTN, &ptn) == 0 && ptn >= 0;
    char slave_path[64];
    snprintf(slave_path, sizeof slave_path, "/dev/pts/%d", ptn);
    int slave = devpts ? open(slave_path, O_RDWR | O_NOCTTY) : -1;
    devpts = devpts && slave >= 0 && isatty(slave);
    if (slave >= 0) close(slave);
    if (master >= 0) close(master);

    int exit_status = 0;
    pid_t exiting = fork();
    if (exiting == 0) _exit(42);
    int exit_code =
        exiting > 0 && wait_intr(exiting, &exit_status) && WIFEXITED(exit_status) && WEXITSTATUS(exit_status) == 42;

    int signal_status = 0;
    pid_t signalled = fork();
    if (signalled == 0) {
        for (;;)
            pause();
    }
    int signal_code = signalled > 0 && kill(signalled, SIGTERM) == 0 && wait_intr(signalled, &signal_status) &&
                      WIFSIGNALED(signal_status) && WTERMSIG(signal_status) == SIGTERM;

    char installed[128];
    snprintf(installed, sizeof installed, "%s.installed", temporary);
    int postinstall = writable && rename(temporary, installed) == 0;
    int directory = open("/tmp", O_RDONLY | O_DIRECTORY);
    postinstall = postinstall && directory >= 0 && fsync(directory) == 0;
    if (directory >= 0) close(directory);
    char contents[8] = {0};
    int installed_fd = open(installed, O_RDONLY);
    postinstall = postinstall && installed_fd >= 0 && read(installed_fd, contents, sizeof contents) == 8 &&
                  memcmp(contents, "package\n", 8) == 0;
    if (installed_fd >= 0) close(installed_fd);
    unlink(temporary);
    unlink(installed);

    printf("apt-runtime tmp=%d writable=%d devpts=%d exit=%d signal=%d postinstall=%d\n", tmp, writable, devpts,
           exit_code, signal_code, postinstall);
    return !(tmp && writable && devpts && exit_code && signal_code && postinstall);
}
