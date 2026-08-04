// dup2/dup3 edge semantics: dup2(fd,fd) is a no-op returning fd, dup3(fd,fd) is EINVAL,
// dup3 rejects unknown flags, and O_CLOEXEC is per-descriptor not shared.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/null", O_RDONLY);
    int a = dup2(fd, fd);
    int b = dup3(fd, fd, 0);
    int eb = (b == -1) ? errno : 0;
    int c = dup3(fd, 200, 0x40000000);
    int ec = (c == -1) ? errno : 0;
    int d = dup3(fd, 201, O_CLOEXEC);
    int f0 = fcntl(fd, F_GETFD);
    int f1 = fcntl(201, F_GETFD);
    int e = dup2(fd, 202);
    int f2 = fcntl(202, F_GETFD);
    int g = dup2(-1, 203);
    int eg = (g == -1) ? errno : 0;
    printf("a=%d b=%d eb=%d c=%d ec=%d d=%d f0=%d f1=%d e=%d f2=%d g=%d eg=%d\n",
           a == fd, b, eb, c, ec, d, f0, f1, e, f2, g, eg);
    return 0;
}
