// readlink/readlinkat buffer edges: truncation to the buffer size, no NUL terminator,
// exact-fit, and EINVAL on a non-symlink.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_readlink_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    const char target[] = "abcdefghij";   // 10 bytes
    symlinkat(target, dfd, "link");
    close(openat(dfd, "regular", O_CREAT | O_WRONLY, 0644));

    // Small buffer: readlink returns the buffer size and truncates (no NUL written).
    char small[4];
    memset(small, '#', sizeof small);
    ssize_t s = readlinkat(dfd, "link", small, sizeof small);
    int trunc_ok = s == 4 && memcmp(small, "abcd", 4) == 0;

    // Exact-fit buffer: full content, still no terminator.
    char exact[10];
    memset(exact, '#', sizeof exact);
    ssize_t e = readlinkat(dfd, "link", exact, sizeof exact);
    int exact_ok = e == 10 && memcmp(exact, target, 10) == 0;

    // Roomy buffer: caller must terminate using the returned length.
    char roomy[32];
    ssize_t g = readlinkat(dfd, "link", roomy, sizeof roomy);
    int roomy_ok = g == 10;
    if (g >= 0) roomy[g] = 0;
    int content_ok = roomy_ok && strcmp(roomy, target) == 0;

    // Non-symlink -> EINVAL.
    errno = 0;
    ssize_t bad = readlinkat(dfd, "regular", roomy, sizeof roomy);
    int einval_ok = bad == -1 && errno == EINVAL;

    unlinkat(dfd, "link", 0);
    unlinkat(dfd, "regular", 0);
    close(dfd);
    rmdir(dir);
    printf("readlink-edges trunc=%d exact=%d content=%d einval=%d\n",
           trunc_ok, exact_ok, content_ok, einval_ok);
    return 0;
}
