#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <unistd.h>

static void fail(const char *operation) {
    perror(operation);
    _exit(70);
}

int main(void) {
    if (!isatty(STDIN_FILENO)) fail("controlling-terminal");
    pid_t foreground = tcgetpgrp(STDIN_FILENO);
    if (foreground <= 0) fail("controlling-foreground");

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0 || grantpt(master) != 0 || unlockpt(master) != 0) fail("secondary-master");
    char *name = ptsname(master);
    if (name == NULL) fail("secondary-name");
    int secondary = open(name, O_RDWR | O_NOCTTY);
    if (secondary < 0) fail("secondary-slave");

    errno = 0;
    if (ioctl(secondary, TIOCSPGRP, &foreground) == 0 || errno != ENOTTY) {
        dprintf(STDERR_FILENO, "secondary TIOCSPGRP result did not preserve ENOTTY: %d\n", errno);
        return 71;
    }
    if (tcgetpgrp(STDIN_FILENO) != foreground) return 72;
    dprintf(STDOUT_FILENO, "SECONDARY-PTY-PRESERVED\n");
    return 0;
}
