// O_APPEND forces every write to the current end regardless of the file offset; a seek
// before the write does not move where appended bytes land.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_append_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);

    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    write(fd, "AAAA", 4);
    close(fd);

    // Reopen with O_APPEND, seek to the start, and write: bytes still land at EOF.
    fd = open(path, O_WRONLY | O_APPEND);
    lseek(fd, 0, SEEK_SET);
    write(fd, "BBBB", 4);
    off_t after = lseek(fd, 0, SEEK_CUR);
    close(fd);

    int rf = open(path, O_RDONLY);
    char buf[16] = {0};
    ssize_t r = read(rf, buf, sizeof buf - 1);
    close(rf);
    int appended = r == 8 && memcmp(buf, "AAAABBBB", 8) == 0;
    int offset_at_end = after == 8;

    unlink(path);
    rmdir(dir);
    printf("append-mode appended=%d offset-at-end=%d content=%.8s\n",
           appended, offset_at_end, buf);
    return 0;
}
