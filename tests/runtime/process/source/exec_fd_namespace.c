// The engine hoists its OWN host descriptors into a private band above the guest fd ceiling so they can
// never collide with a guest fd number, and the execve descriptor sweep must not make them visible either.
// This pins both halves from the guest side, before and after an execve:
//   - /proc/self/fd shows exactly the guest's own descriptors and nothing else;
//   - a guest descriptor may be placed at the very top of its RLIMIT_NOFILE ceiling, which is inside the
//     band an unseparated engine descriptor would have occupied;
//   - the same holds in the exec'd image, so the sweep neither leaks an engine fd nor closes a plain one.
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

// Count entries in /proc/self/fd, excluding the directory descriptor doing the counting.
static int visible(int *highest) {
    DIR *directory = opendir("/proc/self/fd");
    struct dirent *item;
    int own = dirfd(directory);
    int total = 0;
    *highest = -1;
    if (directory == NULL) return -1;
    while ((item = readdir(directory)) != NULL) {
        char *end = NULL;
        long value = strtol(item->d_name, &end, 10);
        if (end == item->d_name || *end != '\0') continue;
        if ((int)value == own) continue;
        if ((int)value > *highest) *highest = (int)value;
        total++;
    }
    closedir(directory);
    return total;
}

int main(int argc, char **argv) {
    struct rlimit limit;
    int highest = -1;
    int count;
    if (getrlimit(RLIMIT_NOFILE, &limit) != 0) return 1;
    int ceiling = (int)limit.rlim_cur - 1;
    if (argc > 1 && strcmp(argv[1], "child") == 0) {
        count = visible(&highest);
        // stdio plus the plain descriptor the parent placed at the ceiling, and nothing the engine owns.
        // Report positions relative to the ceiling, not raw numbers: the guest ceiling is derived from the
        // host RLIMIT_NOFILE and is not a constant, but the SEPARATION being pinned here is.
        printf("child visible=%d top=%d alive=%d\n", count, highest == ceiling, fcntl(ceiling, F_GETFD) != -1);
        return 0;
    }
    count = visible(&highest);
    printf("parent visible=%d low=%d\n", count, highest >= 0 && highest < 3);
    // The top of the guest ceiling must be free: an engine-private descriptor there would fail this dup2.
    int base = open("/dev/null", O_RDONLY);
    printf("top=%d\n", dup2(base, ceiling) == ceiling);
    close(base);
    fflush(stdout);
    pid_t child = fork();
    if (child == 0) {
        char *arguments[] = {argv[0], (char *)"child", NULL};
        execv("/proc/self/exe", arguments);
        _exit(127);
    }
    int status = 0;
    waitpid(child, &status, 0);
    printf("exit=%d\n", WEXITSTATUS(status));
    return 0;
}
