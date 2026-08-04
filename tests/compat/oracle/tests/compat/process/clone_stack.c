// clone(2) with a non-null child stack but without CLONE_VM must still enter
// the libc trampoline on that stack and invoke the callback.
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static int child(void *argument) {
    return *(int *)argument;
}

int main(void) {
    const size_t size = 64 * 1024;
    void *stack = malloc(size);
    if (stack == NULL) return 2;

    int expected = 23;
    pid_t pid = clone(child, (char *)stack + size, SIGCHLD, &expected);
    if (pid < 0) return 3;

    int status = 0;
    int passed = waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == expected;
    printf("clone_stack callback=%d\n", passed);
    free(stack);
    return passed ? 0 : 1;
}
