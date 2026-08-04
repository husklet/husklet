// fsync/fdatasync/syncfs: durability calls succeed and data survives an fd reopen.
// EBADF on a bad fd; EINVAL/EROFS-free path on a normal file.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_fsync_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);

    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(fd, "durable", 7);
    int fsync_ok = fsync(fd) == 0;

    pwrite(fd, "-more", 5, 7);
    int fdatasync_ok = fdatasync(fd) == 0;

    int syncfs_ok = syncfs(fd) == 0;

    // fsync on an O_PATH fd is rejected with EBADF.
    int pfd = open(path, O_PATH);
    errno = 0;
    int ebadf_ok = fsync(pfd) == -1 && errno == EBADF;
    close(pfd);

    // Data is intact on reopen.
    close(fd);
    int rf = open(path, O_RDONLY);
    char buf[16] = {0};
    ssize_t r = read(rf, buf, sizeof buf - 1);
    int content_ok = r == 12 && memcmp(buf, "durable-more", 12) == 0;
    close(rf);

    unlink(path);
    rmdir(dir);
    printf("fsync-probe fsync=%d fdatasync=%d syncfs=%d opath-ebadf=%d content=%d\n",
           fsync_ok, fdatasync_ok, syncfs_ok, ebadf_ok, content_ok);
    return 0;
}
