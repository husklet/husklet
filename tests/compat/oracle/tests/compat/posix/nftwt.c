// nftw walks a tree depth-consistently with FTW_PHYS; type flags for dir/file/symlink are correct.
#define _XOPEN_SOURCE 700
#include <ftw.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>
#include <sys/stat.h>

static int files = 0, dirs = 0, links = 0, depth_ok = 1;

static int cb(const char *path, const struct stat *sb, int type, struct FTW *ftw) {
    (void)path; (void)sb;
    if (type == FTW_F) files++;
    else if (type == FTW_D || type == FTW_DP) dirs++;
    else if (type == FTW_SL) links++;
    if (ftw->level < 0) depth_ok = 0;
    return 0;
}

int main(void) {
    char root[64];
    snprintf(root, sizeof root, "/tmp/hl_nftw_%d", (int)getpid());
    char sub[128], f1[160], f2[160], ln[160];
    snprintf(sub, sizeof sub, "%s/sub", root);
    snprintf(f1, sizeof f1, "%s/top.txt", root);
    snprintf(f2, sizeof f2, "%s/sub/inner.txt", root);
    snprintf(ln, sizeof ln, "%s/link", root);
    mkdir(root, 0755);
    mkdir(sub, 0755);
    int fd;
    fd = open(f1, O_CREAT | O_WRONLY, 0644); if (fd >= 0) close(fd);
    fd = open(f2, O_CREAT | O_WRONLY, 0644); if (fd >= 0) close(fd);
    symlink("top.txt", ln);

    int rc = nftw(root, cb, 16, FTW_PHYS);
    // 2 dirs (root, sub), 2 regular files, 1 symlink (FTW_PHYS keeps it a link).
    int counts = rc == 0 && dirs == 2 && files == 2 && links == 1 && depth_ok;

    unlink(f1); unlink(f2); unlink(ln); rmdir(sub); rmdir(root);
    printf("nftwt counts=%d dirs=%d files=%d links=%d\n",
           counts, dirs == 2, files == 2, links == 1);
    return 0;
}
