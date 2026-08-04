// statx(2): request STATX_BASIC_STATS and verify the returned mask and derived fields.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <linux/stat.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_statx_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0640);
    char body[4096];
    memset(body, 'z', sizeof body);
    if (write(fd, body, sizeof body) != (ssize_t)sizeof body) return 1;

    struct statx sx;
    memset(&sx, 0x5a, sizeof sx);
    int rc = statx(AT_FDCWD, path, 0, STATX_BASIC_STATS, &sx);
    int ok = rc == 0;
    int size_ok = (sx.stx_mask & STATX_SIZE) && sx.stx_size == 4096;
    int type_ok = (sx.stx_mask & STATX_TYPE) && S_ISREG(sx.stx_mode);
    int mode_ok = (sx.stx_mask & STATX_MODE) && (sx.stx_mode & 07777) == 0640;
    int nlink_ok = (sx.stx_mask & STATX_NLINK) && sx.stx_nlink == 1;
    int block_ok = sx.stx_blksize > 0;

    // AT_EMPTY_PATH against the open fd resolves the same inode.
    struct statx sf;
    int fdrc = statx(fd, "", AT_EMPTY_PATH, STATX_INO, &sf);
    int empty_ok = fdrc == 0 && sf.stx_ino == sx.stx_ino;

    close(fd);
    unlink(path);
    rmdir(dir);
    printf("statx-probe ok=%d size=%d type=%d mode=%d nlink=%d blksize=%d empty-path=%d\n",
           ok, size_ok, type_ok, mode_ok, nlink_ok, block_ok, empty_ok);
    return 0;
}
