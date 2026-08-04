// splice(2) file->pipe with an explicit off_in reads from and advances *off_in while leaving the input
// file position unchanged.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char path[64];
    snprintf(path, sizeof path, "/tmp/hl_splice_%d", (int)getpid());
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    write(fd, "abcdefghij", 10);
    lseek(fd, 1, SEEK_SET);
    int p[2];
    pipe(p);
    off_t off = 4;
    ssize_t n = splice(fd, &off, p[1], NULL, 3, 0);
    off_t fpos = lseek(fd, 0, SEEK_CUR);
    char buf[16] = {0};
    read(p[0], buf, n > 0 ? n : 0);
    printf("splice n=%zd off=%ld fpos=%ld data=[%.*s]\n", n, (long)off, (long)fpos,
           (int)(n > 0 ? n : 0), buf);
    close(fd);
    close(p[0]);
    close(p[1]);
    unlink(path);
    return 0;
}
