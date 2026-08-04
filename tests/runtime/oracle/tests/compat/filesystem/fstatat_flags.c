// fstatat AT_* flags: AT_SYMLINK_NOFOLLOW stats the link itself, following stats the target,
// and AT_EMPTY_PATH on an fd stats the open inode.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_fstatat_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "target", O_CREAT | O_RDWR, 0644);
    write(fd, "0123456789", 10);   // 10-byte target
    symlinkat("target", dfd, "link");

    // Following the link reports the target (regular, size 10).
    struct stat follow;
    fstatat(dfd, "link", &follow, 0);
    int follow_ok = S_ISREG(follow.st_mode) && follow.st_size == 10;

    // NOFOLLOW reports the symlink inode itself.
    struct stat nofollow;
    fstatat(dfd, "link", &nofollow, AT_SYMLINK_NOFOLLOW);
    int nofollow_ok = S_ISLNK(nofollow.st_mode) && nofollow.st_size == (off_t)strlen("target");

    // AT_EMPTY_PATH against the open fd matches the target inode.
    struct stat empty;
    int empty_rc = fstatat(fd, "", &empty, AT_EMPTY_PATH);
    int empty_ok = empty_rc == 0 && empty.st_ino == follow.st_ino && empty.st_size == 10;

    close(fd);
    unlinkat(dfd, "link", 0);
    unlinkat(dfd, "target", 0);
    close(dfd);
    rmdir(dir);
    printf("fstatat-flags follow=%d nofollow=%d empty-path=%d\n",
           follow_ok, nofollow_ok, empty_ok);
    return 0;
}
