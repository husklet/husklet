#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

__attribute__((noinline)) static int translated_after_fork(int value) {
    return value == 7 ? value + 35 : 0;
}

int main(void) {
    pid_t child = fork();
    if (child < 0) return 2;
    int answer = translated_after_fork(7);
    if (child == 0) _exit(answer == 42 ? 0 : 3);
    int status = 0;
    if (waitpid(child, &status, 0) != child) return 4;
    printf("fork-map=%d child=%d\n", answer, WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    return answer == 42 && WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 5;
}
