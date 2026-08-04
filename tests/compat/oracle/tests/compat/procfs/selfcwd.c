// /proc/self/cwd is the magic link to the process working directory. It must track chdir(2) live: shells,
// build tools and daemons resolve it to report or restore their cwd. Assert it equals getcwd() before and
// again after a chdir("/"), proving the link is not a stale snapshot. Derived + deterministic.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int cwd_matches(void) {
    char link[4096], real[4096];
    ssize_t n = readlink("/proc/self/cwd", link, sizeof link - 1);
    if (n <= 0) return 0;
    link[n] = 0;
    if (!getcwd(real, sizeof real)) return 0;
    return strcmp(link, real) == 0;
}

int main(void) {
    int before = cwd_matches();
    int changed = chdir("/") == 0;
    char link[4096];
    ssize_t n = readlink("/proc/self/cwd", link, sizeof link - 1);
    int is_root = 0;
    if (n > 0) { link[n] = 0; is_root = strcmp(link, "/") == 0; }
    int ok = before && changed && is_root;
    printf("selfcwd ok=%d\n", ok);
    return 0;
}
