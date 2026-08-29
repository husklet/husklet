#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

__attribute__((noinline, visibility("hidden"))) int translated_after_fork(int value) {
    return value == 7 ? value + 35 : 0;
}

__attribute__((noinline, visibility("hidden"))) int direct_call_caller(int value) {
    return translated_after_fork(value);
}

int main(void) {
    volatile int seed = 7;
    int warm = 0;
    for (int i = 0; i < 256; i++) warm += direct_call_caller(seed);
    pid_t child = fork();
    if (child < 0) return 2;
    int answer = direct_call_caller(seed);
    if (child == 0) {
        int second = direct_call_caller(seed);
        _exit(answer == 42 && second == 42 ? 0 : 3);
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child) return 4;
    printf("fork-map=%d warm=%d child=%d parent-pid=%d child-pid=%d caller=%p target=%p\n", answer, warm,
           WIFEXITED(status) ? WEXITSTATUS(status) : -1, (int)getpid(), (int)child, (void *)direct_call_caller,
           (void *)translated_after_fork);
    return warm == 256 * 42 && answer == 42 && WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 5;
}
