// vfork()+execve semantics: the child borrows the parent's address space and the parent is suspended
// until the child execs (or _exit()s). After the child's successful exec the parent resumes and reaps
// the exec'd image's exit status. Also verifies the parent's memory is intact after resume.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "vchild") == 0) return 33;

    volatile int sentinel = 0xABC;      // parent memory that must be unchanged after vfork+exec
    pid_t pid = vfork();
    if (pid == 0) {
        // Only exec is legal here besides _exit. Replace image immediately.
        char *cargv[] = { argv[0], (char *)"vchild", NULL };
        execve(argv[0], cargv, environ);
        _exit(120);                     // exec failed
    }
    int st = 0;
    int reaped = waitpid(pid, &st, 0) == pid;
    int exit33 = WIFEXITED(st) && WEXITSTATUS(st) == 33;
    int mem_ok = sentinel == 0xABC;
    printf("vfork_exec reaped=%d exit33=%d mem_ok=%d\n", reaped, exit33, mem_ok);
    return 0;
}
