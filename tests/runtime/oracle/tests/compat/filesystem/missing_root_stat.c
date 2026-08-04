#include <errno.h>
#include <stdio.h>
#include <sys/stat.h>

int main(void) {
    struct stat status;
    errno = 0;
    int result = stat("/.git", &status);
    printf("missing-root-stat result=%d errno=%d\n", result, errno);
    return result == -1 && errno == ENOENT ? 0 : 1;
}
