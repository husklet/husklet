#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef CLOSE_RANGE_UNSHARE
#define CLOSE_RANGE_UNSHARE (1U << 1)
#endif
#define CHILDREN 63

int main(void) {
    int ready[2], release[2];
    if (pipe(ready) != 0 || pipe(release) != 0) return 10;
    for (int i = 0; i < CHILDREN; i++) {
        pid_t child = fork();
        if (child < 0) return 11;
        if (child == 0) {
            close(ready[0]);
            close(release[1]);
            int unshared =
                syscall(SYS_close_range, 1000u, 1000u, CLOSE_RANGE_UNSHARE) == 0;
            unsigned char value = unshared ? 1 : 0;
            if (write(ready[1], &value, 1) != 1) _exit(20);
            if (read(release[0], &value, 1) != 1) _exit(21);
            _exit(unshared ? 0 : 22);
        }
    }
    close(ready[1]);
    close(release[0]);
    int children_ready = 1;
    for (int i = 0; i < CHILDREN; i++) {
        unsigned char value = 0;
        children_ready &= read(ready[0], &value, 1) == 1 && value == 1;
    }
    int parent_unshare =
        syscall(SYS_close_range, 1000u, 1000u, CLOSE_RANGE_UNSHARE) == 0;
    errno = 0;
    int exhausted =
        syscall(SYS_close_range, 1000u, 1000u, CLOSE_RANGE_UNSHARE) == -1 &&
        errno == ENOMEM;

    unsigned char value = 1;
    int woke = write(release[1], &value, 1) == 1;
    int status = 0;
    pid_t reaped = wait(&status);
    int child_released =
        reaped > 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int recovered =
        syscall(SYS_close_range, 1000u, 1000u, CLOSE_RANGE_UNSHARE) == 0;

    for (int i = 1; i < CHILDREN; i++) (void)write(release[1], &value, 1);
    close(release[1]);
    close(ready[0]);
    while (wait(NULL) > 0) {}
    printf("sentry_table_exhaustion ready=%d parent=%d exhausted=%d released=%d recovered=%d\n",
           children_ready, parent_unshare, exhausted, woke && child_released, recovered);
    return children_ready && parent_unshare && exhausted && woke && child_released && recovered
               ? 0
               : 1;
}
