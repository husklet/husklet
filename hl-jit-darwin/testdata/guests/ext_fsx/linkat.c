// dirfd-relative name ops: symlinkat / readlinkat / linkat / renameat / mkdirat.
// Portable POSIX -> golden verdict on every engine.
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_linkat_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "target", O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(fd, "payload", 7);
    close(fd);

    int sym = symlinkat("target", dfd, "link") == 0;
    char buf[64] = {0};
    long rl = readlinkat(dfd, "link", buf, sizeof buf - 1);
    int readlink_ok = rl == 6 && strcmp(buf, "target") == 0;
    int ln = linkat(dfd, "target", dfd, "hard", 0) == 0;
    struct stat st;
    fstatat(dfd, "target", &st, 0);
    int nlink2 = st.st_nlink == 2;
    int ren = renameat(dfd, "hard", dfd, "hard2") == 0;
    int oldgone = faccessat(dfd, "hard", F_OK, 0) != 0;
    int newhas = faccessat(dfd, "hard2", F_OK, 0) == 0;
    int md = mkdirat(dfd, "sub", 0755) == 0;
    struct stat ds;
    int isdir = fstatat(dfd, "sub", &ds, 0) == 0 && S_ISDIR(ds.st_mode);

    unlinkat(dfd, "link", 0);
    unlinkat(dfd, "target", 0);
    unlinkat(dfd, "hard2", 0);
    unlinkat(dfd, "sub", AT_REMOVEDIR);
    close(dfd);
    rmdir(dir);
    printf("linkat sym=%d readlink=%d link=%d nlink2=%d rename=%d oldgone=%d newhas=%d mkdir=%d isdir=%d\n",
           sym, readlink_ok, ln, nlink2, ren, oldgone, newhas, md, isdir);
    return 0;
}
