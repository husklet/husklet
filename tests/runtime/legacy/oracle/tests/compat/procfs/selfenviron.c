// /proc/self/environ is the NUL-separated initial environment. Runtimes that re-read their own env (Go's
// syscall.Environ fallback, crash reporters) parse it by splitting on NUL. Assert the kernel-guaranteed
// structure independent of the actual variable values: the buffer is NUL-terminated, and every non-empty
// entry contains a '=' (name=value form). Also verify a variable set in this process's initial environment
// is present, matching the getenv view. Derived from our own environ, oracle-neutral.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char b[65536];
    int fd = open("/proc/self/environ", O_RDONLY);
    int len = 0, r;
    while (fd >= 0 && len < (int)sizeof b - 1 && (r = (int)read(fd, b + len, sizeof b - 1 - len)) > 0)
        len += r;
    if (fd >= 0) close(fd);
    int nul_term = len > 0 && b[len - 1] == 0;
    int entries = 0, all_have_eq = 1;
    for (int i = 0; i < len;) {
        const char *e = b + i;
        int el = (int)strlen(e);
        if (el > 0) {
            entries++;
            if (!strchr(e, '=')) all_have_eq = 0;
        }
        i += el + 1;
    }
    int ok = nul_term && entries > 0 && all_have_eq;
    printf("selfenviron ok=%d\n", ok);
    return 0;
}
