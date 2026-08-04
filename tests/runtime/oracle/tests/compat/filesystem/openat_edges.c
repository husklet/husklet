// openat/*at path resolution edges: an empty path is ENOENT without AT_EMPTY_PATH, a relative
// path against an O_PATH directory fd resolves, dirfd on a non-directory is ENOTDIR, AT_FDCWD
// with an absolute path ignores the dirfd, O_DIRECTORY on a file is ENOTDIR, and O_CREAT|O_EXCL
// on an existing entry is EEXIST.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[] = "/tmp/hl-openat-XXXXXX";
    if (!mkdtemp(dir)) return 1;
    char file[512];
    snprintf(file, sizeof file, "%s/f", dir);
    close(open(file, O_CREAT | O_WRONLY, 0600));

    int dfd = open(dir, O_PATH | O_DIRECTORY);
    int a = openat(dfd, "f", O_RDONLY);
    int ea = (a == -1) ? errno : 0;
    int b = openat(dfd, "", O_RDONLY);
    int eb = (b == -1) ? errno : 0;
    int c = openat(dfd, "", O_RDONLY | O_PATH); // AT_EMPTY_PATH analogue for open is O_PATH+""
    int ec = (c == -1) ? errno : 0;
    int ffd = open(file, O_RDONLY);
    int d = openat(ffd, "x", O_RDONLY);
    int ed = (d == -1) ? errno : 0;
    int e = openat(ffd, "/dev/null", O_RDONLY); // absolute path ignores dirfd
    int ee = (e == -1) ? errno : 0;
    int f = openat(AT_FDCWD, file, O_RDONLY | O_DIRECTORY);
    int ef = (f == -1) ? errno : 0;
    int g = openat(AT_FDCWD, file, O_CREAT | O_EXCL | O_WRONLY, 0600);
    int eg = (g == -1) ? errno : 0;
    int h = openat(-1, "f", O_RDONLY);
    int eh = (h == -1) ? errno : 0;
    struct stat st;
    int i = fstatat(dfd, "", &st, AT_EMPTY_PATH);
    int isdir = S_ISDIR(st.st_mode);
    int j = fstatat(dfd, "", &st, 0);
    int ej = (j == -1) ? errno : 0;
    int k = faccessat(dfd, "f", R_OK, 0);
    int l = unlinkat(dfd, "f", AT_REMOVEDIR);
    int el = (l == -1) ? errno : 0;
    int m = unlinkat(dfd, "f", 0);
    rmdir(dir);
    printf("a=%d ea=%d b=%d eb=%d cok=%d ec=%d d=%d ed=%d eok=%d ee=%d f=%d ef=%d g=%d eg=%d h=%d eh=%d i=%d isdir=%d j=%d ej=%d k=%d l=%d el=%d m=%d\n",
           a >= 0, ea, b, eb, c >= 0, ec, d, ed, e >= 0, ee, f, ef, g, eg, h, eh, i, isdir, j, ej, k, l, el, m);
    return 0;
}
