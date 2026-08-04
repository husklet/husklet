// pread/pwrite: positioned I/O leaves the file offset untouched; preadv/pwritev scatter-gather.
// Portable -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>
#include <sys/stat.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_preadwrite_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);

    write(fd, "................................", 32);   // 32 dots
    off_t before = lseek(fd, 5, SEEK_SET);

    // pwrite at offset 10 must not move the offset held at 5.
    ssize_t w = pwrite(fd, "WXYZ", 4, 10);
    off_t after_w = lseek(fd, 0, SEEK_CUR);
    int offset_kept = before == 5 && after_w == 5;

    char buf[8] = {0};
    ssize_t r = pread(fd, buf, 4, 10);
    off_t after_r = lseek(fd, 0, SEEK_CUR);
    int read_ok = r == 4 && memcmp(buf, "WXYZ", 4) == 0 && after_r == 5;

    // Scatter-gather positioned write then read-back.
    struct iovec wv[2] = {{"AB", 2}, {"CD", 2}};
    ssize_t wv_n = pwritev(fd, wv, 2, 20);
    char p0[3] = {0}, p1[3] = {0};
    struct iovec rv[2] = {{p0, 2}, {p1, 2}};
    ssize_t rv_n = preadv(fd, rv, 2, 20);
    int iov_ok = wv_n == 4 && rv_n == 4 && memcmp(p0, "AB", 2) == 0 && memcmp(p1, "CD", 2) == 0;

    close(fd);
    unlink(path);
    rmdir(dir);
    printf("pread-pwrite offset-kept=%d read=%d iov=%d bytes=%.4s\n",
           offset_kept, read_ok, iov_ok, buf);
    return 0;
}
