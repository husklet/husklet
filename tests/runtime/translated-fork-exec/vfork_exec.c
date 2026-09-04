#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "vchild") == 0) return 33;
    volatile int sentinel = 0xABC;
    pid_t pid = vfork();
    if (pid == 0) {
        char *cargv[] = {argv[0], (char *)"vchild", NULL};
        execve(argv[0], cargv, environ);
        _exit(120);
    }
    int status = 0;
    int reaped = waitpid(pid, &status, 0) == pid;
    int exit33 = WIFEXITED(status) && WEXITSTATUS(status) == 33;
    int memory_ok = sentinel == 0xABC;
    printf("vfork_exec reaped=%d exit33=%d mem_ok=%d\n", reaped, exit33, memory_ok);
    return !(reaped && exit33 && memory_ok);
}
