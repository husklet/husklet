#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static void fail(const char *operation) {
    perror(operation);
    _exit(70);
}

static void write_all(const char *text) {
    size_t length = 0;
    while (text[length] != 0) length++;
    while (length != 0) {
        ssize_t written = write(STDOUT_FILENO, text, length);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) fail("write");
        text += written;
        length -= (size_t)written;
    }
}

int main(void) {
    struct termios mode;
    if (!isatty(STDIN_FILENO) || tcgetattr(STDIN_FILENO, &mode) != 0) fail("terminal");
    mode.c_lflag |= ISIG;
    mode.c_cc[VINTR] = 3;
    if (tcsetattr(STDIN_FILENO, TCSANOW, &mode) != 0) fail("tcsetattr");
    if (setpgid(0, 0) != 0 && errno != EACCES && errno != EPERM) fail("setpgid-parent");

    pid_t child = fork();
    if (child < 0) fail("fork");
    if (child == 0) {
        if (setpgid(0, 0) != 0) fail("setpgid-child");
        for (;;) {
            usleep(100000);
            write_all("CHILD-ALIVE\n");
        }
    }
    if (setpgid(child, child) != 0 && errno != EACCES) fail("setpgid-child-parent");
    if (tcsetpgrp(STDIN_FILENO, child) != 0) fail("tcsetpgrp-child");
    write_all("SLEEPING\n");

    int status = 0;
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) fail("waitpid");
    }
    if (!WIFSIGNALED(status) || WTERMSIG(status) != SIGINT) {
        write_all("WRONG-CHILD-STATUS\n");
        return 71;
    }
    if (tcsetpgrp(STDIN_FILENO, getpgrp()) != 0) fail("tcsetpgrp-parent");
    write_all("PROMPT-SURVIVED\n");
    return 0;
}
