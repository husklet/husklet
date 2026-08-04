// fallocate(2): default allocation, FALLOC_FL_KEEP_SIZE, and FALLOC_FL_PUNCH_HOLE.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_fallocate_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);

    // Default fallocate extends the file size.
    int def = fallocate(fd, 0, 0, 8192) == 0;
    struct stat s1;
    fstat(fd, &s1);
    int grew = def && s1.st_size == 8192 && s1.st_blocks > 0;

    // KEEP_SIZE reserves beyond EOF without changing the logical size.
    int keep = fallocate(fd, FALLOC_FL_KEEP_SIZE, 8192, 8192) == 0;
    struct stat s2;
    fstat(fd, &s2);
    int keep_ok = keep && s2.st_size == 8192;

    // Fill the first region so its blocks are certainly backed, then punch a hole.
    char buf[8192];
    memset(buf, 'q', sizeof buf);
    pwrite(fd, buf, sizeof buf, 0);
    blkcnt_t before;
    struct stat s3;
    fstat(fd, &s3);
    before = s3.st_blocks;
    int punch = fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, 0, 4096) == 0;
    struct stat s4;
    fstat(fd, &s4);
    int punched = punch && s4.st_size == 8192 && s4.st_blocks <= before;
    // Punched region reads back as zeros.
    char rb[4096];
    memset(rb, 'x', sizeof rb);
    pread(fd, rb, sizeof rb, 0);
    int zeros = 1;
    for (size_t i = 0; i < sizeof rb; i++) if (rb[i] != 0) { zeros = 0; break; }

    close(fd);
    unlink(path);
    rmdir(dir);
    printf("fallocate-modes grew=%d keep-size=%d punch=%d hole-zeros=%d\n",
           grew, keep_ok, punched, zeros);
    return 0;
}
