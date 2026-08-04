#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <unistd.h>

static int child_mode(void) {
    char input[16] = {0};
    ssize_t n = read(STDIN_FILENO, input, sizeof input);
    if (n != 5 || memcmp(input, "hello", 5) != 0) return 21;
    return write(STDOUT_FILENO, "handoff-ok\n", 11) == 11 ? 0 : 22;
}

static int pipeline(const char *self, int slave, int result) {
    if (setsid() < 0 || ioctl(slave, TIOCSCTTY, 0) < 0) return 10;
    if (dup2(slave, 0) != 0 || dup2(slave, 1) != 1 || dup2(slave, 2) != 2) return 11;
    if (slave > 2) close(slave);

    int input[2], output[2];
    if (pipe2(input, O_CLOEXEC) || pipe2(output, O_CLOEXEC)) return 12;
    pid_t command = fork();
    if (command < 0) return 13;
    if (command == 0) {
        if (dup3(input[0], 0, 0) != 0 || dup3(output[1], 1, 0) != 1) _exit(14);
        int saved = fcntl(2, F_DUPFD, 10);
        if (saved < 10) _exit(15);
        if (close(2) || dup3(saved, 2, 0) != 2 || close(saved)) _exit(16);
        execl(self, self, "--child", NULL);
        _exit(17);
    }
    close(input[0]);
    close(output[1]);
    if (write(input[1], "hello", 5) != 5) return 18;
    close(input[1]);
    char buf[32] = {0};
    ssize_t n = read(output[0], buf, sizeof buf);
    close(output[0]);
    int status = 0;
    if (waitpid(command, &status, 0) != command) return 19;
    int ok = WIFEXITED(status) && WEXITSTATUS(status) == 0 && n == 11 && !memcmp(buf, "handoff-ok\n", 11);
    dprintf(result, "pty-fd-handoff=%s\n", ok ? "ok" : "bad");
    return ok ? 0 : 20;
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--child")) return child_mode();
    int result[2];
    if (pipe(result)) return 1;
    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0 || grantpt(master) || unlockpt(master)) return 2;
    char *name = ptsname(master);
    int slave = name ? open(name, O_RDWR | O_NOCTTY) : -1;
    if (slave < 0) return 3;
    pid_t shell = fork();
    if (shell < 0) return 4;
    if (shell == 0) {
        close(result[0]);
        _exit(pipeline(argv[0], slave, result[1]));
    }
    close(result[1]);
    close(slave);
    alarm(5);
    char line[64] = {0};
    ssize_t n = read(result[0], line, sizeof line);
    int status = 0;
    waitpid(shell, &status, 0);
    alarm(0);
    close(master);
    if (n > 0 && write(STDOUT_FILENO, line, (size_t)n) != n) return 6;
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 5;
}
