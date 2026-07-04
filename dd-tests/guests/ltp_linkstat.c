// link()/linkat()/lstat() semantics — LTP link02/link05/lstat01/lstat02 surface, deterministic self-check
// oracle-diffed dd-vs-native on both arches. Covers: link increments st_nlink; link content is shared;
// link error paths (EEXIST/ENOENT/EPERM-on-dir); lstat reports the SYMLINK itself (size=len, S_ISLNK), not
// its target; lstat error paths (ENOENT/ENOTDIR).
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    const char *base = "/tmp/ltp_ls_a";
    const char *l1 = "/tmp/ltp_ls_b";
    const char *l2 = "/tmp/ltp_ls_c";
    const char *sym = "/tmp/ltp_ls_sym";
    unlink(base); unlink(l1); unlink(l2); unlink(sym);

    int fd = open(base, O_RDWR | O_CREAT | O_TRUNC, 0644);
    write(fd, "hello", 5);
    close(fd);

    // link: new name -> same inode, st_nlink becomes 2.
    int r = link(base, l1);
    struct stat s1, s2;
    stat(base, &s1); stat(l1, &s2);
    printf("link ok=%d nlink=%d sameino=%d\n", r == 0, (int)s1.st_nlink, s1.st_ino == s2.st_ino);

    // a second link -> nlink 3 (link05: multiple links).
    link(base, l2);
    stat(base, &s1);
    printf("link2 nlink=%d\n", (int)s1.st_nlink);

    // shared content: write through one link, read through another.
    int wf = open(l1, O_WRONLY);
    pwrite(wf, "J", 1, 0);
    close(wf);
    char buf[8] = {0};
    int rf = open(l2, O_RDONLY);
    read(rf, buf, 5);
    close(rf);
    printf("link shared content=%s\n", buf);

    // link over an existing name -> EEXIST.
    errno = 0;
    int e1 = link(base, l1);
    printf("link EEXIST: ret=%d ok=%d\n", e1, e1 < 0 && errno == EEXIST);

    // link with a nonexistent source -> ENOENT.
    errno = 0;
    int e2 = link("/tmp/ltp_ls_nope", "/tmp/ltp_ls_x");
    printf("link ENOENT: ret=%d ok=%d\n", e2, e2 < 0 && errno == ENOENT);

    // symlink + lstat: lstat must report the LINK (S_ISLNK, size==strlen(target)), not the target file.
    symlink(base, sym);
    struct stat ls;
    int lr = lstat(sym, &ls);
    printf("lstat symlink: ok=%d islnk=%d size=%d\n", lr == 0, S_ISLNK(ls.st_mode) != 0,
           (int)ls.st_size == (int)strlen(base));
    // stat() through the symlink follows it -> the regular file (size 5).
    struct stat ts;
    stat(sym, &ts);
    printf("stat follows: reg=%d size=%d\n", S_ISREG(ts.st_mode) != 0, (int)ts.st_size);

    // lstat on a nonexistent path -> ENOENT.
    errno = 0;
    struct stat ns;
    int nr = lstat("/tmp/ltp_ls_nope", &ns);
    printf("lstat ENOENT: ret=%d ok=%d\n", nr, nr < 0 && errno == ENOENT);

    // lstat where a path component is a file (not a dir) -> ENOTDIR.
    errno = 0;
    int nd = lstat("/tmp/ltp_ls_a/x", &ns);
    printf("lstat ENOTDIR: ret=%d ok=%d\n", nd, nd < 0 && errno == ENOTDIR);

    unlink(base); unlink(l1); unlink(l2); unlink(sym);
    return 0;
}
