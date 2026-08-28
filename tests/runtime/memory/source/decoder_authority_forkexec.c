#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int child_work(void) {
    volatile uint64_t value = 1;
    for (uint64_t i = 1; i <= 200000; ++i) value = (value * 33u) ^ i;
    puts("decoder-authority-forkexec child=1");
    return value == 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "child") == 0) return child_work();
    pid_t child = fork();
    if (child == 0) {
        char *const args[] = {argv[0], "child", NULL};
        execv("/proc/self/exe", args);
        _exit(127);
    }
    int status = 0;
    int ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    printf("decoder-authority-forkexec parent=%d\n", ok);
    return ok ? 0 : 1;
}
