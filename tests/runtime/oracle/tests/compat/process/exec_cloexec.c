// Descriptor inheritance across execve: plain descriptors survive, O_CLOEXEC ones are closed,
// F_DUPFD_CLOEXEC produces a close-on-exec duplicate while dup2 clears the flag, and clearing
// FD_CLOEXEC with F_SETFD before exec makes the descriptor survive after all.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int alive(int fd) { return fcntl(fd, F_GETFD) != -1; }

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "child") == 0) {
        printf("child plain=%d cloexec=%d dupcloexec=%d dup2=%d cleared=%d\n",
               alive(20), alive(21), alive(22), alive(23), alive(24));
        return 0;
    }
    int base = open("/dev/null", O_RDONLY);
    dup2(base, 20);                                  // plain, no CLOEXEC
    int c = open("/dev/null", O_RDONLY | O_CLOEXEC); // close-on-exec
    dup3(base, 21, O_CLOEXEC);
    int dc = fcntl(base, F_DUPFD_CLOEXEC, 22);       // 22: cloexec dup
    dup2(c, 23);                                     // dup2 clears CLOEXEC
    dup3(base, 24, O_CLOEXEC);
    fcntl(24, F_SETFD, 0);                           // explicitly cleared again
    int flags20 = fcntl(20, F_GETFD), flags21 = fcntl(21, F_GETFD);
    int flags22 = fcntl(22, F_GETFD), flags23 = fcntl(23, F_GETFD), flags24 = fcntl(24, F_GETFD);
    printf("parent dc=%d f20=%d f21=%d f22=%d f23=%d f24=%d\n",
           dc == 22, flags20, flags21, flags22, flags23, flags24);
    fflush(stdout);
    pid_t p = fork();
    if (p == 0) {
        char *av[] = {argv[0], (char *)"child", NULL};
        execv("/proc/self/exe", av);
        _exit(127);
    }
    int st = 0;
    waitpid(p, &st, 0);
    printf("exit=%d\n", WEXITSTATUS(st));
    return 0;
}
