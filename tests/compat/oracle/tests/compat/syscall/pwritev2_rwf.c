// pwritev2(2) RWF_APPEND is a semantic requirement, not a hint: it must write at end-of-file and ignore
// the supplied offset. A translator that drops the flag writes at the offset instead, corrupting the file.
// preadv2 positioned reads must also leave the file position unchanged.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/uio.h>

int main(void) {
    char path[64];
    snprintf(path, sizeof path, "/tmp/hl_pwritev2_%d", (int)getpid());
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { perror("open"); return 1; }

    char a[] = "hello", b[] = "world";
    struct iovec wv[2] = {{a, 5}, {b, 5}};
    ssize_t w = pwritev2(fd, wv, 2, 0, 0);
    printf("pwritev2 w=%zd\n", w);

    // RWF_APPEND: offset 0 is ignored, "XYZ" lands at end -> "helloworldXYZ".
    char c[] = "XYZ";
    struct iovec av = {c, 3};
    ssize_t wa = pwritev2(fd, &av, 1, 0, RWF_APPEND);
    printf("pwritev2 append w=%zd\n", wa);

    char buf[64] = {0};
    ssize_t r = pread(fd, buf, sizeof buf, 0);
    printf("content=[%.*s] r=%zd\n", (int)(r > 0 ? r : 0), buf, r);

    // preadv2 positioned read must not move the file position.
    lseek(fd, 0, SEEK_SET);
    char rb[5] = {0};
    struct iovec rv = {rb, 5};
    ssize_t pr = preadv2(fd, &rv, 1, 5, 0);
    off_t pos = lseek(fd, 0, SEEK_CUR);
    printf("preadv2 at5=[%.*s] pr=%zd pos=%ld\n", (int)(pr > 0 ? pr : 0), rb, pr, (long)pos);

    close(fd);
    unlink(path);
    return 0;
}
