// openpty + login_tty via forkpty: child's stdout is the pty; parent reads it back through the master.
#include <pty.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <termios.h>

int main(void) {
    int m, s;
    int rc = openpty(&m, &s, NULL, NULL, NULL);
    int opened = rc == 0 && m >= 0 && s >= 0;
    if (!opened) { printf("openptyt opened=0\n"); return 0; }
    // Both ends are ttys.
    int mtty = isatty(m);
    int stty = isatty(s);
    close(m);
    close(s);

    int master;
    pid_t pid = forkpty(&master, NULL, NULL, NULL);
    if (pid == 0) {
        write(STDOUT_FILENO, "HI\n", 3); // child's stdout == slave pty
        _exit(0);
    }
    char buf[64] = {0};
    ssize_t total = 0;
    // Read until we see the payload or the child closes the pty.
    for (int i = 0; i < 100 && total < 2; i++) {
        ssize_t n = read(master, buf + total, sizeof buf - 1 - total);
        if (n > 0) total += n;
        else break;
    }
    int got = strstr(buf, "HI") != NULL;
    close(master);
    int status = 0;
    waitpid(pid, &status, 0);
    printf("openptyt mtty=%d stty=%d child=%d\n", mtty, stty, got);
    return 0;
}
