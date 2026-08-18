#include <errno.h>
#include <stdio.h>
#include <sys/types.h>
#include <unistd.h>

int main(void) {
    errno = 0;
    pid_t child = fork();
    if (child != -1 || errno != EAGAIN) return 1;
    return puts("fork pids limit diagnostic ok") == EOF;
}
