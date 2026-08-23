#define _GNU_SOURCE
#include <pty.h>
#include <sys/ioctl.h>
#include <unistd.h>

int main(int argc, char **argv) {
    int master = -1;
    int slave = -1;
    if (argc != 3 || openpty(&master, &slave, NULL, NULL, NULL) != 0 || setsid() < 0 ||
        ioctl(slave, TIOCSCTTY, 0) != 0 || dup2(slave, STDIN_FILENO) < 0)
        return 126;
    close(slave);
    execl(argv[1], argv[1], argv[2], (char *)NULL);
    return 127;
}
