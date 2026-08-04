// vfork(): child shares the parent's address space until it _exit()s or execs. We only _exit() in the
// child (the sole vfork-legal action besides exec), then the parent reaps its status.
// Portable POSIX -> golden verdict on every engine.
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    pid_t pid = vfork();
    if (pid == 0) {
        _exit(9);                 // only _exit / exec are permitted post-vfork
    }
    int status = 0;
    pid_t r = waitpid(pid, &status, 0);
    int reaped = r == pid;
    int exit9 = WIFEXITED(status) && WEXITSTATUS(status) == 9;
    printf("vfork reaped=%d exit9=%d\n", reaped, exit9);
    return 0;
}
