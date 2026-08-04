// truncate/ftruncate: grow zero-fills, shrink discards, read-back is exact.
// Portable POSIX -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_truncate_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(fd, "0123456789", 10);

    // Grow via ftruncate; the gap reads back as NUL bytes.
    int grow = ftruncate(fd, 20) == 0;
    struct stat gs;
    fstat(fd, &gs);
    char buf[20] = {0};
    pread(fd, buf, 20, 0);
    int tail_zero = 1;
    for (int i = 10; i < 20; i++) if (buf[i] != 0) { tail_zero = 0; break; }
    int head_ok = memcmp(buf, "0123456789", 10) == 0;

    // Shrink via path truncate; the data past the new end is gone.
    int shrink = truncate(path, 4) == 0;
    struct stat ss;
    fstat(fd, &ss);
    char sb[8] = {0};
    ssize_t r = pread(fd, sb, 8, 0);

    close(fd);
    unlink(path);
    rmdir(dir);
    printf("truncate-grow grow=%d size20=%d head=%d tail-zero=%d shrink=%d size4=%d read=%zd data=%.4s\n",
           grow, (int)(gs.st_size == 20), head_ok, tail_zero, shrink,
           (int)(ss.st_size == 4), r, sb);
    return 0;
}
