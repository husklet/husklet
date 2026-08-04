// /proc/thread-self is the magic symlink to <pid>/task/<tid> for the calling thread. glibc, tcmalloc
// and profilers use it to reach the current thread's per-task files without a gettid syscall. On the
// main thread tid==pid, so it must read back "<pid>/task/<pid>". A synthesized link that ignores the
// live caller identity fails. Derived + deterministic.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    char link[128];
    ssize_t n = readlink("/proc/thread-self", link, sizeof link - 1);
    int ok = 0;
    if (n > 0) {
        link[n] = 0;
        char want[128];
        long tid = syscall(SYS_gettid);
        snprintf(want, sizeof want, "%d/task/%ld", (int)getpid(), tid);
        ok = strcmp(link, want) == 0;
    }
    printf("threadself ok=%d\n", ok);
    return 0;
}
