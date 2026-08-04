// chown/fchown/fchownat: chowning to the current owner is always permitted and preserved,
// a (-1,-1) request is a no-op, and AT_SYMLINK_NOFOLLOW targets the link inode. No test
// requires elevated privilege, so the verdict is deterministic regardless of the run uid.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_chown_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "file", O_CREAT | O_RDWR, 0644);
    struct stat s0;
    fstat(fd, &s0);
    uid_t uid = s0.st_uid;
    gid_t gid = s0.st_gid;

    // chown to the same owner: permitted, values preserved.
    int same = fchown(fd, uid, gid) == 0;
    struct stat s1;
    fstat(fd, &s1);
    int preserved = s1.st_uid == uid && s1.st_gid == gid;

    // (-1,-1) is an explicit no-op.
    int noop = fchownat(dfd, "file", (uid_t)-1, (gid_t)-1, 0) == 0;
    struct stat s2;
    fstatat(dfd, "file", &s2, 0);
    int noop_ok = noop && s2.st_uid == uid && s2.st_gid == gid;

    // A symlink chown with AT_SYMLINK_NOFOLLOW targets the link inode.
    symlinkat("file", dfd, "link");
    int link_ok = fchownat(dfd, "link", uid, gid, AT_SYMLINK_NOFOLLOW) == 0;
    struct stat ls;
    fstatat(dfd, "link", &ls, AT_SYMLINK_NOFOLLOW);
    int link_owner = ls.st_uid == uid;

    close(fd);
    unlinkat(dfd, "link", 0);
    unlinkat(dfd, "file", 0);
    close(dfd);
    rmdir(dir);
    printf("chown-selfown same=%d preserved=%d noop=%d link=%d link-owner=%d\n",
           same, preserved, noop_ok, link_ok, link_owner);
    return 0;
}
